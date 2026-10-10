//! Request-local SSE event trace for translated Claude streams.
//!
//! Metadata only: event names, item types, tool names, stop/finish reasons
//! and terminal counters. No payload content (text deltas, tool arguments,
//! prompts, raw frames) is ever retained. The trace is bounded: at most
//! `MAX_ENTRIES` ordered entries of `MAX_ENTRY_CHARS` chars, plus overflow
//! counters, so a runaway stream cannot grow the request journal row.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use crate::core::translator::registry::Format;

const MAX_ENTRIES: usize = 64;
const MAX_ENTRY_CHARS: usize = 64;
const MAX_TOOL_NAMES: usize = 8;
/// Hard cap on the number of distinct counted keys per map. The Claude
/// vocabulary is small (<30 event types); anything beyond this is anomaly
/// territory and gets folded into `overflowed`.
const MAX_KEYS: usize = 48;

#[derive(Clone, Debug, Default)]
pub(crate) struct StreamEventTrace {
    state: Arc<Mutex<TraceState>>,
}

#[derive(Debug, Default)]
struct TraceState {
    upstream_counts: BTreeMap<String, u32>,
    emitted_counts: BTreeMap<String, u32>,
    item_type_counts: BTreeMap<String, u32>,
    tool_names: Vec<String>,
    entries: Vec<String>,
    overflowed: u32,
    stop_reason: Option<String>,
    finish_reason_emitted: Option<String>,
    completed_count: u32,
    error_count: u32,
    frames_after_completed: u32,
    done_sent: bool,
}

impl StreamEventTrace {
    /// Record one upstream SSE frame. `event` is the `event:` field when
    /// present, otherwise JSON `type` supplies the kind; `payload` is the parsed data JSON
    /// when available. Content fields are read only for structure (block
    /// type, tool name, stop reason), never copied.
    pub(crate) fn observe_upstream(&self, event: Option<&str>, payload: Option<&Value>) {
        let Some(event) = event.or_else(|| payload?.get("type")?.as_str()) else {
            return;
        };
        let mut key = event.to_string();
        let payload = payload.cloned();
        match event {
            "content_block_start" => {
                if let Some(block_type) = payload
                    .as_ref()
                    .and_then(|value| value.pointer("/content_block/type"))
                    .and_then(Value::as_str)
                {
                    key.push(':');
                    key.push_str(block_type);
                }
            }
            "message_delta" => {
                if let Some(stop_reason) = payload
                    .as_ref()
                    .and_then(|value| value.pointer("/delta/stop_reason"))
                    .and_then(Value::as_str)
                {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.stop_reason = Some(stop_reason.chars().take(MAX_ENTRY_CHARS).collect());
                    key.push_str(":stop_reason=");
                    key.push_str(stop_reason);
                }
            }
            _ => {}
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        bump_key(&mut state, |state| &mut state.upstream_counts, key.clone());
        push_entry(&mut state, format!("u:{key}"));
    }

    /// Record one emitted downstream chunk (a full SSE frame string such as
    /// `event: response.completed\ndata: {...}\n\n` or `data: [DONE]\n\n`).
    pub(crate) fn observe_emitted(&self, chunk: &str) {
        let (name, item_type, tool_name) = match parse_emitted_event(chunk) {
            Some(parsed) => parsed,
            None => return,
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.done_sent || state.completed_count > 0 {
            state.frames_after_completed = state.frames_after_completed.saturating_add(1);
        }
        match name.as_str() {
            "response.completed" => {
                state.completed_count = state.completed_count.saturating_add(1);
            }
            "error" => {
                state.error_count = state.error_count.saturating_add(1);
            }
            "[DONE]" => {
                state.done_sent = true;
            }
            _ => {}
        }
        if let Some(item_type) = item_type.as_deref() {
            bump_key(
                &mut state,
                |state| &mut state.item_type_counts,
                item_type.to_string(),
            );
        }
        if let Some(tool_name) = tool_name.as_deref() {
            if state.tool_names.len() < MAX_TOOL_NAMES
                && !state
                    .tool_names
                    .iter()
                    .any(|existing| existing == tool_name)
            {
                state
                    .tool_names
                    .push(tool_name.chars().take(MAX_ENTRY_CHARS).collect());
            }
        }
        let mut key = name.to_string();
        if let Some(item_type) = item_type.as_deref() {
            key.push(':');
            key.push_str(item_type);
        }
        bump_key(&mut state, |state| &mut state.emitted_counts, key.clone());
        push_entry(&mut state, format!("d:{key}"));
    }

    /// Record a finish reason emitted downstream (OpenAI `finish_reason`
    /// inside a chat chunk on the pivot path).
    pub(crate) fn observe_finish_reason(&self, reason: &str) {
        if reason.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.finish_reason_emitted = Some(reason.chars().take(MAX_ENTRY_CHARS).collect());
    }

    /// Terminal error emitted via `streaming_error` (not an SSE frame from
    /// the translator, so it needs an explicit note).
    pub(crate) fn observe_stream_error(&self, code: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.error_count = state.error_count.saturating_add(1);
        if state.done_sent || state.completed_count > 0 {
            state.frames_after_completed = state.frames_after_completed.saturating_add(1);
        }
        let key = format!("error:{code}");
        bump_key(&mut state, |state| &mut state.emitted_counts, key.clone());
        push_entry(&mut state, format!("d:{key}"));
    }

    /// Persistable projection. Returns `None` when nothing was observed
    /// (empty trace is not written).
    pub(crate) fn snapshot(&self) -> Option<Value> {
        let state = self.state.lock().ok()?;
        if state.entries.is_empty() && state.upstream_counts.is_empty() {
            return None;
        }
        let mut root = Map::new();
        root.insert("version".into(), json!(1));
        root.insert(
            "upstreamEvents".into(),
            counts_value(&state.upstream_counts),
        );
        root.insert("emittedEvents".into(), counts_value(&state.emitted_counts));
        root.insert("itemTypes".into(), counts_value(&state.item_type_counts));
        if !state.tool_names.is_empty() {
            root.insert(
                "toolNames".into(),
                Value::Array(
                    state
                        .tool_names
                        .iter()
                        .map(|name| Value::String(name.clone()))
                        .collect(),
                ),
            );
        }
        if let Some(stop_reason) = &state.stop_reason {
            root.insert("stopReason".into(), json!(stop_reason));
        }
        if let Some(finish) = &state.finish_reason_emitted {
            root.insert("finishReason".into(), json!(finish));
        }
        root.insert("completedCount".into(), json!(state.completed_count));
        root.insert("errorCount".into(), json!(state.error_count));
        root.insert(
            "framesAfterCompleted".into(),
            json!(state.frames_after_completed),
        );
        root.insert("doneSent".into(), json!(state.done_sent));
        if state.overflowed > 0 {
            root.insert("overflowed".into(), json!(state.overflowed));
        }
        root.insert(
            "entries".into(),
            Value::Array(
                state
                    .entries
                    .iter()
                    .map(|entry| Value::String(entry.clone()))
                    .collect(),
            ),
        );
        Some(Value::Object(root))
    }
}

fn bump_key(
    state: &mut TraceState,
    map: impl Fn(&mut TraceState) -> &mut BTreeMap<String, u32>,
    key: String,
) {
    if map(state).len() >= MAX_KEYS && !map(state).contains_key(&key) {
        state.overflowed = state.overflowed.saturating_add(1);
        return;
    }
    *map(state).entry(key).or_insert(0) += 1;
}

fn push_entry(state: &mut TraceState, entry: String) {
    let bounded: String = entry.chars().take(MAX_ENTRY_CHARS).collect();
    if state.entries.len() >= MAX_ENTRIES {
        state.overflowed = state.overflowed.saturating_add(1);
        return;
    }
    if state.entries.last().is_some_and(|last| last == &bounded) {
        return; // consecutive dedup
    }
    state.entries.push(bounded);
}

/// Extract `(event_name, item_type, tool_name)` from one emitted SSE frame.
/// `item_type` is set only for `response.output_item.added`; `tool_name`
/// only for function/custom tool call items.
fn parse_emitted_event(chunk: &str) -> Option<(String, Option<String>, Option<String>)> {
    let mut lines = chunk.split('\n');
    let first = lines.next()?.trim();
    if first == "data: [DONE]" {
        return Some(("[DONE]".to_string(), None, None));
    }
    let event = if let Some(event) = first.strip_prefix("event: ") {
        event.trim().to_string()
    } else if first.starts_with("data: ") {
        // Data-only frame: parse the JSON to find the type.
        let data = first.strip_prefix("data: ")?.trim();
        if data == "[DONE]" {
            return Some(("[DONE]".to_string(), None, None));
        }
        let value: Value = serde_json::from_str(data).ok()?;
        value
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string)?
    } else {
        return None;
    };
    if event != "response.output_item.added" {
        return Some((event, None, None));
    }
    let data = lines
        .find_map(|line| line.trim().strip_prefix("data: "))
        .and_then(|data| serde_json::from_str::<Value>(data.trim()).ok());
    let item_type = data
        .as_ref()
        .and_then(|value| value.pointer("/item/type"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let tool_name = data
        .as_ref()
        .and_then(|value| value.pointer("/item/name"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Some((event, item_type, tool_name))
}

fn counts_value(map: &BTreeMap<String, u32>) -> Value {
    Value::Object(
        map.iter()
            .map(|(key, count)| (key.clone(), json!(count)))
            .collect(),
    )
}

/// Whether tracing applies to this stream: translated streams whose source
/// is Claude (the incident class). Passthrough and dashboard streams are
/// excluded, as are non-Claude sources.
pub(crate) fn trace_applies(source: Format, target: Format, translation_active: bool) -> bool {
    translation_active && source == Format::Claude && target != Format::Claude
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace() -> StreamEventTrace {
        StreamEventTrace::default()
    }

    #[test]
    fn snapshot_is_none_until_first_observation() {
        assert!(trace().snapshot().is_none());
    }

    #[test]
    fn upstream_events_are_counted_with_block_type_and_stop_reason() {
        let t = trace();
        t.observe_upstream(
            Some("message_start"),
            Some(&json!({"type":"message_start","message":{"id":"m1"}})),
        );
        t.observe_upstream(
            Some("content_block_start"),
            Some(&json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}})),
        );
        t.observe_upstream(
            Some("message_delta"),
            Some(&json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}})),
        );
        let snapshot = t.snapshot().unwrap();
        assert_eq!(snapshot["upstreamEvents"]["message_start"], 1);
        assert_eq!(snapshot["upstreamEvents"]["content_block_start:text"], 1);
        assert_eq!(
            snapshot["upstreamEvents"]["message_delta:stop_reason=end_turn"],
            1
        );
        assert_eq!(snapshot["stopReason"], "end_turn");
        assert_eq!(snapshot["entries"][0], "u:message_start");
    }

    #[test]
    fn emitted_events_track_completion_items_and_done() {
        let t = trace();
        t.observe_emitted("event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"message\",\"id\":\"msg_1\"}}\n\n");
        t.observe_emitted("event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n");
        t.observe_emitted("data: [DONE]\n\n");
        t.observe_emitted("event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\"}\n\n");
        let snapshot = t.snapshot().unwrap();
        assert_eq!(snapshot["completedCount"], 1);
        assert_eq!(snapshot["doneSent"], true);
        assert_eq!(snapshot["itemTypes"]["message"], 1);
        // [DONE] and the trailing delta both arrive after completion.
        assert_eq!(snapshot["framesAfterCompleted"], 2);
        assert_eq!(snapshot["entries"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn data_only_refusal_records_kind_and_stop_reason() {
        let t = trace();
        t.observe_upstream(
            None,
            Some(&json!({"type":"message_delta","delta":{"stop_reason":"refusal"}})),
        );
        let snapshot = t.snapshot().unwrap();
        assert_eq!(snapshot["stopReason"], "refusal");
        assert_eq!(
            snapshot["upstreamEvents"]["message_delta:stop_reason=refusal"],
            1
        );
        // An explicit SSE event keeps precedence over a conflicting JSON type.
        t.observe_upstream(Some("message_stop"), Some(&json!({"type":"error"})));
        assert_eq!(t.snapshot().unwrap()["upstreamEvents"]["message_stop"], 1);
    }

    #[test]
    fn tool_call_items_capture_sanitized_name() {
        let t = trace();
        t.observe_emitted("event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"call_id\":\"fc_1\",\"name\":\"read\"}}\n\n");
        let snapshot = t.snapshot().unwrap();
        assert_eq!(snapshot["itemTypes"]["function_call"], 1);
        assert_eq!(snapshot["toolNames"][0], "read");
    }

    #[test]
    fn entries_are_capped_and_overflow_counted() {
        let t = trace();
        for i in 0..100 {
            t.observe_upstream(Some(&format!("event_{i}")), None);
        }
        let snapshot = t.snapshot().unwrap();
        let entries = snapshot["entries"].as_array().unwrap();
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert!(snapshot["overflowed"].as_u64().unwrap() > 0);
    }

    #[test]
    fn consecutive_duplicate_entries_dedup() {
        let t = trace();
        t.observe_upstream(Some("content_block_delta"), None);
        t.observe_upstream(Some("content_block_delta"), None);
        let snapshot = t.snapshot().unwrap();
        let entries = snapshot["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(snapshot["upstreamEvents"]["content_block_delta"], 2);
    }

    #[test]
    fn stream_error_after_completion_counts_as_post_completed_frame() {
        let t = trace();
        t.observe_emitted("event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n");
        t.observe_stream_error("upstream_stream_truncated");
        let snapshot = t.snapshot().unwrap();
        assert_eq!(snapshot["errorCount"], 1);
        assert_eq!(snapshot["framesAfterCompleted"], 1);
        assert!(snapshot["emittedEvents"]["error:upstream_stream_truncated"] == 1);
    }

    #[test]
    fn finish_reason_recorded() {
        let t = trace();
        t.observe_finish_reason("stop");
        // snapshot() skips empty traces; observe one event first.
        t.observe_upstream(Some("message_stop"), None);
        let snapshot = t.snapshot().unwrap();
        assert_eq!(snapshot["finishReason"], "stop");
    }

    #[test]
    fn trace_applies_only_to_translated_claude_sources() {
        assert!(trace_applies(Format::Claude, Format::OpenAiResponses, true));
        assert!(trace_applies(Format::Claude, Format::OpenAi, true));
        assert!(!trace_applies(
            Format::Claude,
            Format::OpenAiResponses,
            false
        ));
        assert!(!trace_applies(
            Format::OpenAi,
            Format::OpenAiResponses,
            true
        ));
        assert!(!trace_applies(Format::Claude, Format::Claude, true));
    }
}
