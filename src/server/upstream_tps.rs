//! Request-local original-protocol usage and monotonic timing evidence.
//!
//! No payloads survive observation. Choice/candidate finality uses two bounded
//! bitsets; cumulative usage is latest-known, never summed across snapshots.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{json, Value};

use crate::core::executor::generation_timing::GenerationTiming;
use crate::core::translator::registry::Format;

#[derive(Clone, Debug, Default)]
pub(crate) struct UpstreamTpsObservation {
    timing: GenerationTiming,
    state: Arc<Mutex<ObservationState>>,
}

#[derive(Debug, Default)]
struct ObservationState {
    output: Option<u64>,
    thoughts: Option<u64>,
    // Known cumulative counters are not necessarily final usage evidence.
    final_output: Option<u64>,
    final_thoughts: Option<u64>,
    generated_output: Option<u64>,
    add_thoughts: bool,
    seen: u128,
    finished: u128,
    final_delta: bool,
    failed: bool,
    terminal: bool,
    end: Option<(Instant, &'static str)>,
    cursor: u64,
    last_read: Option<Instant>,
}

impl UpstreamTpsObservation {
    pub(crate) fn timing(&self) -> GenerationTiming {
        self.timing.clone()
    }

    /// Only complete, positive-duration original observations are persistable.
    pub(crate) fn snapshot(&self) -> Option<Value> {
        let state = self.state.lock().ok()?;
        if state.failed || !state.terminal {
            return None;
        }
        let output = state.generated_output?;
        let (end, kind) = state.end?;
        let micros = u64::try_from(
            end.checked_duration_since(self.timing.started()?)?
                .as_micros(),
        )
        .ok()
        .filter(|micros| *micros > 0)?;
        Some(
            json!({"version":1,"generatedOutputTokens":output,"elapsedMicros":micros,"endKind":kind}),
        )
    }

    pub(crate) fn invalidate(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.failed = true;
        }
    }

    /// Segment replay at scalar preflight boundaries, using the caller's ONE
    /// existing framer. A terminal in an earlier prefix has unknowable timing.
    pub(crate) fn feed_segments<E>(
        &self,
        chunk: &[u8],
        read_at: Instant,
        mut feed: impl FnMut(&[u8], Option<Instant>) -> Result<(), E>,
    ) -> Result<(), E> {
        let prefix = self.timing.prefix();
        let mut offset = 0;
        while offset < chunk.len() {
            let (length, at) = {
                let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                let (boundary, at) = match prefix.as_ref() {
                    Some(prefix) if state.cursor < prefix.last_chunk_start => {
                        (prefix.last_chunk_start, None)
                    }
                    Some(prefix) if state.cursor < prefix.total_bytes => {
                        (prefix.total_bytes, Some(prefix.last_read_at))
                    }
                    _ => (u64::MAX, Some(read_at)),
                };
                let length = usize::try_from(boundary.saturating_sub(state.cursor))
                    .unwrap_or(usize::MAX)
                    .min(chunk.len() - offset);
                state.cursor = state.cursor.saturating_add(length as u64);
                state.last_read = at;
                (length, at)
            };
            feed(&chunk[offset..offset + length], at)?;
            offset += length;
        }
        Ok(())
    }

    pub(crate) fn last_read_at(&self) -> Option<Instant> {
        self.state.lock().ok().and_then(|state| state.last_read)
    }

    /// Called for an original framed payload BEFORE any translator or yield.
    pub(crate) fn observe_payload(
        &self,
        format: Format,
        event: Option<&str>,
        payload: Option<&str>,
        at: Option<Instant>,
    ) {
        let Some(payload) = payload else {
            if event == Some("error") {
                self.invalidate();
            }
            return;
        };
        if payload.trim() == "[DONE]" {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if format == Format::OpenAi && state.choices_complete() {
                state.complete(at, "protocol_terminal");
            }
            return;
        }
        match serde_json::from_str::<Value>(payload) {
            Ok(value) => self.observe_value(format, event, &value, at, false),
            Err(_) => self.invalidate(),
        }
    }

    pub(crate) fn observe_json(&self, format: Format, value: &Value, at: Instant) {
        self.observe_value(format, None, value, Some(at), true);
    }

    pub(crate) fn clean_eof(&self, format: Format, at: Instant) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        // Responses, Claude and Ollama require their explicit terminal, not EOF.
        if matches!(
            format,
            Format::OpenAi | Format::Gemini | Format::Vertex | Format::Antigravity
        ) && state.choices_complete()
        {
            state.complete(Some(at), "clean_eof");
        }
    }

    fn observe_value(
        &self,
        format: Format,
        event: Option<&str>,
        original: &Value,
        at: Option<Instant>,
        body: bool,
    ) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let value = if format == Format::Antigravity {
            original.get("response").unwrap_or(original)
        } else {
            original
        };
        let kind = value.get("type").and_then(Value::as_str).or(event);
        if matches!(
            event,
            Some("error" | "response.failed" | "response.incomplete")
        ) || matches!(
            kind,
            Some("error" | "response.failed" | "response.incomplete")
        ) || value.get("error").is_some_and(|error| !error.is_null())
            || original.get("error").is_some_and(|error| !error.is_null())
        {
            state.failed = true;
        }
        // Late protocol failures suppress even an already observed terminal.
        if state.failed {
            return;
        }
        let end_kind = if body {
            "json_body"
        } else {
            "protocol_terminal"
        };
        match format {
            Format::OpenAi => {
                if value.pointer("/usage/estimated").and_then(Value::as_bool) == Some(true) {
                    state.failed = true;
                    return;
                }
                let mut invalid = false;
                merge_count(
                    &mut state.output,
                    value.pointer("/usage/completion_tokens"),
                    &mut invalid,
                );
                state.failed |= invalid;
                let seen = state.seen;
                state.observe_choices(value.get("choices"), "finish_reason", false);
                if seen != state.seen {
                    state.final_output = None;
                }
                if state.choices_complete() {
                    merge_count(
                        &mut state.final_output,
                        value.pointer("/usage/completion_tokens"),
                        &mut invalid,
                    );
                    if body {
                        state.complete(at, end_kind);
                    }
                }
            }
            Format::OpenAiResponses | Format::OpenAiResponse | Format::Codex => {
                let response = value.get("response").unwrap_or(value);
                if response.get("error").is_some_and(|error| !error.is_null())
                    || matches!(
                        response.get("status").and_then(Value::as_str),
                        Some("failed" | "incomplete" | "cancelled")
                    )
                {
                    state.failed = true;
                    return;
                }
                let mut invalid = false;
                merge_count(
                    &mut state.output,
                    response.pointer("/usage/output_tokens"),
                    &mut invalid,
                );
                state.failed |= invalid;
                let completed = response.get("status").and_then(Value::as_str) == Some("completed")
                    && (body || kind == Some("response.completed"))
                    && (body || event.is_none_or(|event| event == "response.completed"))
                    && (body
                        || value
                            .get("type")
                            .is_none_or(|kind| kind.as_str() == Some("response.completed")))
                    && response
                        .get("incomplete_details")
                        .is_none_or(Value::is_null)
                    && response.get("output").is_none_or(|output| {
                        output.as_array().is_some_and(|items| {
                            items.iter().all(|item| {
                                item.get("status")
                                    .is_none_or(|status| status.as_str() == Some("completed"))
                            })
                        })
                    });
                if completed {
                    merge_count(
                        &mut state.final_output,
                        response.pointer("/usage/output_tokens"),
                        &mut invalid,
                    );
                    state.complete(at, end_kind);
                }
            }
            Format::Claude => {
                if body {
                    let mut invalid = false;
                    merge_count(
                        &mut state.output,
                        value.pointer("/usage/output_tokens"),
                        &mut invalid,
                    );
                    state.failed |= invalid;
                    if value.get("type").and_then(Value::as_str) == Some("message")
                        && value
                            .get("stop_reason")
                            .and_then(Value::as_str)
                            .is_some_and(|reason| !reason.is_empty())
                    {
                        merge_count(
                            &mut state.final_output,
                            value.pointer("/usage/output_tokens"),
                            &mut invalid,
                        );
                        state.complete(at, end_kind);
                    }
                } else if kind == Some("message_delta") {
                    let mut invalid = false;
                    merge_count(
                        &mut state.output,
                        value.pointer("/usage/output_tokens"),
                        &mut invalid,
                    );
                    state.failed |= invalid;
                    let final_delta = value
                        .pointer("/delta/stop_reason")
                        .and_then(Value::as_str)
                        .is_some_and(|reason| !reason.is_empty());
                    if final_delta {
                        state.final_delta = true;
                        merge_count(
                            &mut state.final_output,
                            value.pointer("/usage/output_tokens"),
                            &mut invalid,
                        );
                    }
                } else if kind == Some("message_stop") && state.final_delta {
                    state.complete(at, end_kind);
                }
            }
            Format::Gemini | Format::Vertex | Format::Antigravity => {
                state.add_thoughts = true;
                let mut invalid = false;
                merge_count(
                    &mut state.output,
                    value.pointer("/usageMetadata/candidatesTokenCount"),
                    &mut invalid,
                );
                merge_count(
                    &mut state.thoughts,
                    value.pointer("/usageMetadata/thoughtsTokenCount"),
                    &mut invalid,
                );
                state.failed |= invalid;
                if value
                    .pointer("/promptFeedback/blockReason")
                    .is_some_and(|reason| !reason.is_null())
                {
                    state.failed = true;
                }
                let seen = state.seen;
                state.observe_choices(value.get("candidates"), "finishReason", true);
                if seen != state.seen {
                    state.final_output = None;
                    state.final_thoughts = None;
                }
                // Omitted components remain unknown. No total-minus-input or
                // model-name inference can establish non-thinking semantics.
                if state.choices_complete() {
                    merge_count(
                        &mut state.final_output,
                        value.pointer("/usageMetadata/candidatesTokenCount"),
                        &mut invalid,
                    );
                    merge_count(
                        &mut state.final_thoughts,
                        value.pointer("/usageMetadata/thoughtsTokenCount"),
                        &mut invalid,
                    );
                    // Stream usage-only trailers can follow finished candidates.
                    // Their counts and read time belong to the final observation.
                    if body {
                        state.complete(at, end_kind);
                    }
                }
            }
            Format::Ollama => {
                let mut invalid = false;
                merge_count(&mut state.output, value.get("eval_count"), &mut invalid);
                state.failed |= invalid;
                if value.get("done").and_then(Value::as_bool) == Some(true) {
                    merge_count(
                        &mut state.final_output,
                        value.get("eval_count"),
                        &mut invalid,
                    );
                    state.complete(at, end_kind);
                }
            }
        }
    }
}

fn merge_count(target: &mut Option<u64>, value: Option<&Value>, invalid: &mut bool) {
    match value {
        None | Some(Value::Null) => {}
        Some(value) => match value.as_u64() {
            Some(count) => *target = Some(count),
            None => *invalid = true,
        },
    }
}

impl ObservationState {
    fn choices_complete(&self) -> bool {
        self.seen != 0 && self.finished == self.seen
    }

    fn complete(&mut self, at: Option<Instant>, kind: &'static str) {
        if !self.terminal {
            let output = if self.add_thoughts {
                let (Some(output), Some(thoughts)) = (self.final_output, self.final_thoughts)
                else {
                    return;
                };
                let Some(output) = output.checked_add(thoughts) else {
                    self.failed = true;
                    return;
                };
                output
            } else {
                let Some(output) = self.final_output else {
                    return;
                };
                output
            };
            self.generated_output = Some(output);
            self.terminal = true;
            self.end = at.map(|at| (at, kind));
        }
    }

    fn observe_choices(&mut self, choices: Option<&Value>, finish_field: &str, gemini: bool) {
        let Some(choices) = choices.and_then(Value::as_array) else {
            return;
        };
        if choices.len() > 128 {
            self.failed = true;
            return;
        }
        for (position, choice) in choices.iter().enumerate() {
            let index = match choice.get("index") {
                None => position as u64,
                Some(index) => match index.as_u64() {
                    Some(index) => index,
                    None => {
                        self.failed = true;
                        continue;
                    }
                },
            };
            if index >= 128 {
                self.failed = true;
                continue;
            }
            let bit = 1u128 << index;
            self.seen |= bit;
            if let Some(reason) = choice
                .get(finish_field)
                .and_then(Value::as_str)
                .filter(|reason| !reason.is_empty())
            {
                if (gemini && !matches!(reason, "STOP" | "MAX_TOKENS"))
                    || (!gemini
                        && !matches!(reason, "stop" | "length" | "tool_calls" | "function_call"))
                {
                    self.failed = true;
                } else {
                    self.finished |= bit;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::executor::generation_timing::{mark_generation_send, mark_prefix_read};
    use crate::core::stream_framing::SseFramer;
    use std::time::Duration;

    async fn started() -> UpstreamTpsObservation {
        let observation = UpstreamTpsObservation::default();
        observation
            .timing()
            .scope(async {
                mark_generation_send();
            })
            .await;
        observation
    }

    fn at(observation: &UpstreamTpsObservation) -> Instant {
        observation.timing.started().unwrap() + Duration::from_secs(2)
    }

    fn payload(observation: &UpstreamTpsObservation, format: Format, value: Value) {
        observation.observe_payload(
            format,
            None,
            Some(&value.to_string()),
            Some(at(observation)),
        );
    }

    fn assert_count(observation: &UpstreamTpsObservation, count: u64, kind: &str) {
        assert_eq!(
            observation.snapshot().unwrap(),
            json!({
                "version":1,"generatedOutputTokens":count,"elapsedMicros":2_000_000,"endKind":kind
            })
        );
    }

    #[tokio::test]
    async fn tps_chat_requires_all_choices_and_preserves_latest_zero_and_null() {
        let observation = started().await;
        payload(
            &observation,
            Format::OpenAi,
            json!({"choices":[{"index":0},{"index":1}],"usage":{"completion_tokens":9}}),
        );
        payload(
            &observation,
            Format::OpenAi,
            json!({"choices":[{"index":0,"finish_reason":"stop"}]}),
        );
        observation.observe_payload(Format::OpenAi, None, Some("[DONE]"), Some(at(&observation)));
        assert!(observation.snapshot().is_none());
        payload(
            &observation,
            Format::OpenAi,
            json!({"choices":[{"index":1,"finish_reason":"tool_calls"}],"usage":{"completion_tokens":0}}),
        );
        payload(
            &observation,
            Format::OpenAi,
            json!({"choices":[],"usage":{"completion_tokens":null}}),
        );
        observation.observe_payload(Format::OpenAi, None, Some("[DONE]"), Some(at(&observation)));
        assert_count(&observation, 0, "protocol_terminal");
    }

    #[tokio::test]
    async fn tps_chat_clean_eof_needs_finality_and_never_total_minus_input() {
        for value in [
            json!({"choices":[{"index":0}],"usage":{"completion_tokens":7}}),
            json!({"choices":[{"index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"total_tokens":10}}),
        ] {
            let observation = started().await;
            payload(&observation, Format::OpenAi, value);
            observation.clean_eof(Format::OpenAi, at(&observation));
            assert!(observation.snapshot().is_none());
        }
        let observation = started().await;
        payload(
            &observation,
            Format::OpenAi,
            json!({"choices":[{"index":0,"finish_reason":"length"}],"usage":{"completion_tokens":7}}),
        );
        observation.clean_eof(Format::OpenAi, at(&observation));
        assert_count(&observation, 7, "clean_eof");
    }

    #[tokio::test]
    async fn tps_responses_validates_completed_and_does_not_double_add_reasoning() {
        for format in [
            Format::Codex,
            Format::OpenAiResponse,
            Format::OpenAiResponses,
        ] {
            let observation = started().await;
            payload(
                &observation,
                format,
                json!({"type":"response.created","response":{"status":"in_progress","usage":{"output_tokens":10}}}),
            );
            assert!(observation.snapshot().is_none());
            payload(
                &observation,
                format,
                json!({"type":"response.completed","response":{"status":"completed","output":[],"usage":{"output_tokens":8,"output_tokens_details":{"reasoning_tokens":3}}}}),
            );
            assert_count(&observation, 8, "protocol_terminal");
        }
        for response in [
            json!({"status":"in_progress","output":[],"usage":{"output_tokens":8}}),
            json!({"status":"incomplete","output":[],"usage":{"output_tokens":8}}),
            json!({"status":"failed","output":[],"usage":{"output_tokens":8}}),
            json!({"status":"completed","output":[{"status":"in_progress"}],"usage":{"output_tokens":8}}),
            json!({"status":"completed","output":[],"incomplete_details":{},"usage":{"output_tokens":8}}),
        ] {
            let observation = started().await;
            payload(
                &observation,
                Format::Codex,
                json!({"type":"response.completed","response":response}),
            );
            observation.clean_eof(Format::Codex, at(&observation));
            assert!(observation.snapshot().is_none());
        }
    }

    #[tokio::test]
    async fn tps_claude_start_is_provisional_and_final_delta_needs_stop() {
        let observation = started().await;
        payload(
            &observation,
            Format::Claude,
            json!({"type":"message_start","message":{"usage":{"output_tokens":99}}}),
        );
        payload(
            &observation,
            Format::Claude,
            json!({"type":"message_delta","usage":{"output_tokens":2},"delta":{}}),
        );
        observation.clean_eof(Format::Claude, at(&observation));
        assert!(observation.snapshot().is_none());
        payload(
            &observation,
            Format::Claude,
            json!({"type":"message_delta","usage":{"output_tokens":0},"delta":{"stop_reason":"end_turn"}}),
        );
        payload(
            &observation,
            Format::Claude,
            json!({"type":"message_delta","usage":{"output_tokens":null},"delta":{}}),
        );
        assert!(observation.snapshot().is_none());
        payload(&observation, Format::Claude, json!({"type":"message_stop"}));
        assert_count(&observation, 0, "protocol_terminal");
    }

    #[tokio::test]
    async fn tps_gemini_checked_components_cumulative_partial_and_usage_trailers() {
        for format in [Format::Gemini, Format::Vertex, Format::Antigravity] {
            let observation = started().await;
            let wrap = |value: Value| {
                if format == Format::Antigravity {
                    json!({"response":value})
                } else {
                    value
                }
            };
            payload(
                &observation,
                format,
                wrap(
                    json!({"candidates":[{"index":0}],"usageMetadata":{"candidatesTokenCount":20,"thoughtsTokenCount":4}}),
                ),
            );
            payload(
                &observation,
                format,
                wrap(
                    json!({"candidates":[{"index":0}],"usageMetadata":{"candidatesTokenCount":12}}),
                ),
            );
            payload(
                &observation,
                format,
                wrap(json!({"candidates":[{"index":0,"finishReason":"STOP"}]})),
            );
            assert!(observation.snapshot().is_none());
            payload(
                &observation,
                format,
                wrap(
                    json!({"usageMetadata":{"candidatesTokenCount":10,"thoughtsTokenCount":null}}),
                ),
            );
            payload(
                &observation,
                format,
                wrap(json!({"usageMetadata":{"candidatesTokenCount":10}})),
            );
            assert!(observation.snapshot().is_none());
            // Provisional thoughts cannot become final merely through STOP.
            observation.clean_eof(format, at(&observation));
            assert!(observation.snapshot().is_none());
            let trailer = started().await;
            payload(
                &trailer,
                format,
                wrap(json!({"candidates":[{"index":0,"finishReason":"STOP"}]})),
            );
            payload(
                &trailer,
                format,
                wrap(json!({"usageMetadata":{"candidatesTokenCount":7,"thoughtsTokenCount":0}})),
            );
            payload(
                &trailer,
                format,
                wrap(json!({"usageMetadata":{"candidatesTokenCount":5,"thoughtsTokenCount":null}})),
            );
            assert!(trailer.snapshot().is_none());
            trailer.clean_eof(format, at(&trailer));
            assert_count(&trailer, 5, "clean_eof");
        }
        for usage in [
            json!({"candidatesTokenCount":7}),
            json!({"thoughtsTokenCount":7}),
            json!({"promptTokenCount":3,"totalTokenCount":10}),
            json!({"candidatesTokenCount":u64::MAX,"thoughtsTokenCount":1}),
        ] {
            let observation = started().await;
            payload(
                &observation,
                Format::Gemini,
                json!({"candidates":[{"finishReason":"STOP"}],"usageMetadata":usage}),
            );
            observation.clean_eof(Format::Gemini, at(&observation));
            assert!(observation.snapshot().is_none());
        }
    }

    #[tokio::test]
    async fn tps_json_policies_and_ollama_done_finality() {
        for (format, body, count) in [
            (
                Format::OpenAi,
                json!({"choices":[{"finish_reason":"stop"}],"usage":{"completion_tokens":0}}),
                0,
            ),
            (
                Format::Claude,
                json!({"type":"message","stop_reason":"tool_use","usage":{"output_tokens":7}}),
                7,
            ),
            (
                Format::Codex,
                json!({"status":"completed","output":[],"usage":{"output_tokens":8}}),
                8,
            ),
            (
                Format::Gemini,
                json!({"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"candidatesTokenCount":7,"thoughtsTokenCount":3}}),
                10,
            ),
            (Format::Ollama, json!({"done":true,"eval_count":3}), 3),
        ] {
            let observation = started().await;
            observation.observe_json(format, &body, at(&observation));
            assert_count(&observation, count, "json_body");
        }
        let observation = started().await;
        payload(
            &observation,
            Format::Ollama,
            json!({"done":false,"eval_count":12}),
        );
        observation.clean_eof(Format::Ollama, at(&observation));
        assert!(observation.snapshot().is_none());
        payload(
            &observation,
            Format::Ollama,
            json!({"done":true,"eval_count":0}),
        );
        assert_count(&observation, 0, "protocol_terminal");
    }

    #[tokio::test]
    async fn tps_terminal_cannot_promote_provisional_usage() {
        for final_usage in [json!({}), json!({"output_tokens":null})] {
            for format in [
                Format::Codex,
                Format::OpenAiResponse,
                Format::OpenAiResponses,
            ] {
                let observation = started().await;
                payload(
                    &observation,
                    format,
                    json!({"type":"response.created","response":{"status":"in_progress","usage":{"output_tokens":9}}}),
                );
                payload(
                    &observation,
                    format,
                    json!({"type":"response.completed","response":{"status":"completed","output":[],"usage":final_usage}}),
                );
                observation.clean_eof(format, at(&observation));
                assert!(observation.snapshot().is_none(), "{format:?}");
            }
        }
        for count in [None, Some(Value::Null)] {
            let observation = started().await;
            payload(
                &observation,
                Format::Ollama,
                json!({"done":false,"eval_count":9}),
            );
            let mut terminal = json!({"done":true});
            if let Some(count) = count.clone() {
                terminal["eval_count"] = count;
            }
            payload(&observation, Format::Ollama, terminal);
            observation.clean_eof(Format::Ollama, at(&observation));
            assert!(observation.snapshot().is_none());

            let observation = started().await;
            payload(
                &observation,
                Format::Claude,
                json!({"type":"message_delta","delta":{},"usage":{"output_tokens":9}}),
            );
            let mut terminal =
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{}});
            if let Some(count) = count.clone() {
                terminal["usage"]["output_tokens"] = count;
            }
            payload(&observation, Format::Claude, terminal);
            payload(&observation, Format::Claude, json!({"type":"message_stop"}));
            assert!(observation.snapshot().is_none());

            for done in [false, true] {
                let observation = started().await;
                payload(
                    &observation,
                    Format::OpenAi,
                    json!({"choices":[{"index":0}],"usage":{"completion_tokens":9}}),
                );
                let mut terminal =
                    json!({"choices":[{"index":0,"finish_reason":"stop"}],"usage":{}});
                if let Some(count) = count.clone() {
                    terminal["usage"]["completion_tokens"] = count;
                }
                payload(&observation, Format::OpenAi, terminal);
                payload(
                    &observation,
                    Format::OpenAi,
                    json!({"choices":[],"usage":{"completion_tokens":null}}),
                );
                if done {
                    observation.observe_payload(
                        Format::OpenAi,
                        None,
                        Some("[DONE]"),
                        Some(at(&observation)),
                    );
                }
                observation.clean_eof(Format::OpenAi, at(&observation));
                assert!(observation.snapshot().is_none());
            }

            for format in [Format::Gemini, Format::Vertex, Format::Antigravity] {
                let observation = started().await;
                let wrap = |value: Value| {
                    if format == Format::Antigravity {
                        json!({"response":value})
                    } else {
                        value
                    }
                };
                payload(
                    &observation,
                    format,
                    wrap(
                        json!({"candidates":[{"index":0}],"usageMetadata":{"candidatesTokenCount":9,"thoughtsTokenCount":2}}),
                    ),
                );
                payload(
                    &observation,
                    format,
                    wrap(json!({"candidates":[{"index":0,"finishReason":"STOP"}]})),
                );
                observation.clean_eof(format, at(&observation));
                assert!(observation.snapshot().is_none());
            }
        }
    }

    #[tokio::test]
    async fn tps_responses_event_and_type_must_agree() {
        for (event, kind) in [
            ("response.created", "response.completed"),
            ("ping", "response.completed"),
            ("response.completed", "response.created"),
        ] {
            let observation = started().await;
            observation.observe_payload(Format::Codex, Some(event), Some(&json!({"type":kind,"response":{"status":"completed","output":[],"usage":{"output_tokens":9}}}).to_string()), Some(at(&observation)));
            observation.clean_eof(Format::Codex, at(&observation));
            assert!(observation.snapshot().is_none(), "{event} / {kind}");
        }
        let observation = started().await;
        observation.observe_payload(Format::Codex, Some("response.completed"), Some(&json!({"type":"response.completed","response":{"status":"completed","output":[],"usage":{"output_tokens":0}}}).to_string()), Some(at(&observation)));
        assert_count(&observation, 0, "protocol_terminal");
    }

    #[tokio::test]
    async fn tps_gemini_final_trailer_and_eof_determine_numerator_and_time() {
        for format in [Format::Gemini, Format::Vertex, Format::Antigravity] {
            let observation = started().await;
            let wrap = |value: Value| {
                if format == Format::Antigravity {
                    json!({"response":value})
                } else {
                    value
                }
            };
            payload(
                &observation,
                format,
                wrap(
                    json!({"candidates":[{"index":0,"finishReason":"STOP"}],"usageMetadata":{"candidatesTokenCount":9,"thoughtsTokenCount":3}}),
                ),
            );
            assert!(observation.snapshot().is_none());
            observation.observe_payload(format, None, Some(&wrap(json!({"usageMetadata":{"candidatesTokenCount":4,"thoughtsTokenCount":null}})).to_string()), Some(at(&observation) + Duration::from_secs(1)));
            observation.observe_payload(
                format,
                None,
                Some(&wrap(json!({"usageMetadata":{"thoughtsTokenCount":0}})).to_string()),
                Some(at(&observation) + Duration::from_secs(2)),
            );
            assert!(observation.snapshot().is_none());
            observation.clean_eof(format, at(&observation) + Duration::from_secs(3));
            assert_eq!(
                observation.snapshot().unwrap(),
                json!({"version":1,"generatedOutputTokens":4,"elapsedMicros":5_000_000,"endKind":"clean_eof"})
            );
        }
    }

    #[tokio::test]
    async fn tps_estimated_usage_is_never_original_evidence() {
        for body in [false, true] {
            let observation = started().await;
            let value = json!({"choices":[{"index":0,"finish_reason":"stop"}],"usage":{"completion_tokens":9,"estimated":true}});
            if body {
                observation.observe_json(Format::OpenAi, &value, at(&observation));
            } else {
                payload(&observation, Format::OpenAi, value);
                observation.observe_payload(
                    Format::OpenAi,
                    None,
                    Some("[DONE]"),
                    Some(at(&observation)),
                );
            }
            assert!(observation.snapshot().is_none());
        }
    }

    #[tokio::test]
    async fn tps_errors_malformed_and_invalid_counts_suppress_even_late_terminal() {
        for error in [
            json!({"error":{"message":"HTTP200 failure"}}),
            json!({"type":"response.failed"}),
            json!({"type":"response.incomplete"}),
        ] {
            let observation = started().await;
            observation.observe_json(
                Format::Ollama,
                &json!({"done":true,"eval_count":8}),
                at(&observation),
            );
            payload(&observation, Format::Ollama, error);
            assert!(observation.snapshot().is_none());
        }
        for value in [json!(-1), json!(1.5), json!("7")] {
            let observation = started().await;
            observation.observe_json(
                Format::OpenAi,
                &json!({"choices":[{"finish_reason":"stop"}],"usage":{"completion_tokens":value}}),
                at(&observation),
            );
            assert!(observation.snapshot().is_none());
        }
        let observation = started().await;
        observation.observe_json(
            Format::Ollama,
            &json!({"done":true,"eval_count":8}),
            at(&observation),
        );
        observation.observe_payload(Format::Ollama, None, Some("{"), Some(at(&observation)));
        assert!(observation.snapshot().is_none());
    }

    #[tokio::test]
    async fn tps_snapshot_requires_send_and_known_positive_monotonic_duration() {
        let observation = UpstreamTpsObservation::default();
        observation.observe_json(
            Format::Ollama,
            &json!({"done":true,"eval_count":1}),
            Instant::now(),
        );
        assert!(observation.snapshot().is_none());
        for offset in [Duration::ZERO, Duration::from_nanos(500)] {
            let observation = started().await;
            observation.observe_json(
                Format::Ollama,
                &json!({"done":true,"eval_count":0}),
                observation.timing.started().unwrap() + offset,
            );
            assert!(observation.snapshot().is_none());
        }
        let observation = started().await;
        observation.observe_json(
            Format::Ollama,
            &json!({"done":true,"eval_count":1}),
            observation.timing.started().unwrap() - Duration::from_secs(1),
        );
        assert!(observation.snapshot().is_none());
    }

    #[tokio::test]
    async fn tps_preflight_boundaries_are_independent_of_rechunking() {
        let terminal = b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[],\"usage\":{\"output_tokens\":9}}}\n\n";
        let ping = b": ping\n\n";
        for placement in 0..3 {
            let bytes = [ping.as_slice(), terminal.as_slice(), ping.as_slice()].concat();
            for split in 1..bytes.len() {
                let observation = UpstreamTpsObservation::default();
                observation
                    .timing()
                    .scope(async {
                        mark_generation_send();
                        match placement {
                            0 => {
                                mark_prefix_read(ping.len() + terminal.len());
                                mark_prefix_read(ping.len());
                            }
                            1 => {
                                mark_prefix_read(ping.len());
                                mark_prefix_read(terminal.len() + ping.len());
                            }
                            _ => {
                                mark_prefix_read(ping.len());
                                mark_prefix_read(terminal.len() / 2);
                            }
                        }
                    })
                    .await;
                let saved = observation.timing.prefix().unwrap().last_read_at;
                let live = saved + Duration::from_secs(1);
                let mut framer = SseFramer::new();
                for chunk in [&bytes[..split], &bytes[split..]] {
                    observation
                        .feed_segments(chunk, live, |segment, at| {
                            framer.feed(segment, |event| {
                                observation.observe_payload(
                                    Format::Codex,
                                    event.event(),
                                    event.data(),
                                    at,
                                )
                            })
                        })
                        .unwrap();
                }
                if placement == 0 {
                    assert!(observation.snapshot().is_none());
                } else {
                    let expected = if placement == 1 { saved } else { live };
                    assert_eq!(observation.state.lock().unwrap().end.unwrap().0, expected);
                }
            }
        }
    }
}
