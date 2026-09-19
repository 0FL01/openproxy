//! Stream-to-JSON converter for chat responses (#306).
//!
//! When a provider forces streaming but the client requested non-streaming,
//! this module converts the SSE stream back to a single `chat.completion`
//! JSON response.
//!
//! Two input formats are supported:
//! - **Chat Completions SSE** (`data: {...}` lines with `delta.content` /
//!   `delta.tool_calls`)
//! - **Responses API SSE** (`event:` / `data:` pairs from the OpenAI Responses
//!   API, which providers like Codex use on the wire)

use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::core::stream_framing::{SseEvent, SseFramer};
use crate::core::translator::limits::{
    checked_add_u64, checked_append, checked_retain, wire_index, StreamLimitError,
    MAX_RESPONSES_OUTPUT_ITEMS, MAX_STREAM_ACCUMULATED_BYTES, MAX_STREAM_CHOICES,
    MAX_STREAM_TOOL_ARGUMENT_BYTES, MAX_STREAM_TOOL_CALLS,
};

/// Quick check whether raw bytes look like SSE data (start with `data:` or
/// `event:` or an SSE comment line `:`).
///
/// Normal JSON responses never begin with these, so a positive result is a
/// reliable indicator that the response body is SSE rather than a single JSON
/// object.
pub fn looks_like_sse(input: &[u8]) -> bool {
    if input.is_empty() || input.len() < 5 {
        return false;
    }
    let s = String::from_utf8_lossy(input);
    let trimmed = s.trim();
    trimmed.starts_with("data:") || trimmed.starts_with("event:") || trimmed.starts_with(':')
}

/// Convert SSE stream bytes to a single `chat.completion` JSON response.
///
/// Automatically detects the SSE format:
/// - OpenAI Chat Completions SSE (`data: {...}` lines)
/// - OpenAI Responses API SSE (`event:` / `data:` pairs)
///
/// Returns `None` when the input does not look like valid SSE or when parsing
/// yields no content.
pub fn sse_stream_to_json(
    input: &[u8],
    fallback_model: Option<&str>,
) -> Result<Option<Value>, StreamLimitError> {
    let input_str = String::from_utf8_lossy(input);
    let input_str = input_str.trim();

    if input_str.is_empty() || !looks_like_sse(input) {
        return Ok(None);
    }

    if input_str.starts_with("event:") || input_str.contains("\nevent:") {
        convert_responses_api_stream(input_str, fallback_model)
    } else {
        convert_chat_completion_stream(input_str, fallback_model)
    }
}

// ---------------------------------------------------------------------------
// Incremental forced SSE accumulator (C33).
//
// Feeds completed C32 `SseEvent` frames directly into C31-bounded state
// without retaining the full raw SSE history. Only the parsed accumulator
// (≤16 MiB retained state) plus the final JSON output survive. Wire bytes
// are counted by the caller against the C29 success-body limit.
// ---------------------------------------------------------------------------

/// Which SSE family the forced stream belongs to. Decided by the first
/// non-empty event: an `event:` field means Responses, otherwise Chat.
/// Pure comment/empty frames leave the kind undecided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForcedKind {
    Chat,
    Responses,
}

/// Request-local incremental accumulator for forced SSE→JSON conversion.
#[derive(Debug, Default)]
pub struct ForcedSseAccumulator {
    kind: Option<ForcedKind>,
    saw_any_event: bool,
    chat: ChatForcedState,
    responses: ResponsesForcedState,
}

#[derive(Debug, Default)]
struct ChatForcedState {
    id: Option<String>,
    created: Option<i64>,
    model: Option<String>,
    usage: Option<Value>,
    choices: BTreeMap<u64, ChoiceAccum>,
    tool_call_count: usize,
    retained_bytes: usize,
}

#[derive(Debug)]
struct ResponsesForcedState {
    summary: ResponsesStreamSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsesSearchOutput {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsesSearchError {
    pub code: String,
    pub message: String,
}

impl Default for ResponsesForcedState {
    fn default() -> Self {
        Self {
            summary: ResponsesStreamSummary {
                response_id: String::new(),
                created: None,
                status: "in_progress".to_string(),
                output: BTreeMap::new(),
                usage: json!({"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}),
                retained_bytes: 0,
                error: None,
            },
        }
    }
}

impl ForcedSseAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// True once any SSE frame (including comment-only) has been observed.
    /// Used to distinguish bare non-SSE JSON (fallback path) from SSE with
    /// no convertible content (wrapper error path).
    pub fn saw_any_event(&self) -> bool {
        self.saw_any_event
    }

    pub fn ingest(&mut self, event: &SseEvent<'_>) -> Result<(), StreamLimitError> {
        // Bare JSON tails dispatched by the framer have no SSE fields and
        // must not mark the wire as SSE; otherwise the bare-JSON fallback in
        // the caller could never trigger.
        let is_sse = event.data().is_some() || event.event().is_some() || event.comment_count() > 0;
        if !is_sse {
            return Ok(());
        }
        self.saw_any_event = true;
        if self.kind.is_none() {
            if event.event().is_some() {
                self.kind = Some(ForcedKind::Responses);
            } else if event.data().is_some() {
                self.kind = Some(ForcedKind::Chat);
            } else {
                // Comment-only frame: SSE confirmed but no payload.
                return Ok(());
            }
        }
        match self.kind {
            Some(ForcedKind::Chat) => {
                let Some(data_str) = event.data() else {
                    return Ok(());
                };
                self.chat.ingest_data_str(data_str)
            }
            Some(ForcedKind::Responses) => self
                .responses
                .ingest_event(event.event(), event.data().unwrap_or("")),
            None => Ok(()),
        }
    }

    pub fn finish(self, fallback_model: Option<&str>) -> Result<Option<Value>, StreamLimitError> {
        match self.kind {
            None => Ok(None),
            Some(ForcedKind::Chat) => self.chat.finish(fallback_model),
            Some(ForcedKind::Responses) => self.responses.finish(fallback_model),
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.kind == Some(ForcedKind::Responses) && self.responses.summary.status != "in_progress"
    }

    pub fn finish_responses_search(self) -> Result<ResponsesSearchOutput, ResponsesSearchError> {
        if self.kind != Some(ForcedKind::Responses) {
            return Err(ResponsesSearchError {
                code: "upstream_response_invalid".to_string(),
                message: "Codex returned a non-Responses stream".to_string(),
            });
        }
        project_responses_search(self.responses.summary)
    }

    pub fn ingest_responses_json(&mut self, value: &Value) -> Result<(), StreamLimitError> {
        self.kind = Some(ForcedKind::Responses);
        let response = if value.get("type").and_then(Value::as_str) == Some("response.completed") {
            value.get("response")
        } else {
            Some(value)
        }
        .ok_or_else(|| StreamLimitError {
            code: "upstream_response_invalid",
            message: "Codex response JSON omitted response".to_string(),
        })?;
        let status = response
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("completed");
        self.responses.summary.status = status.to_string();
        let event = json!({"response": response});
        self.responses.summary.capture_response_metadata(&event)?;
        self.responses.summary.capture_usage(response.get("usage"));
        if status != "completed" {
            self.responses.summary.error = Some(response_event_error(
                &event,
                if status == "incomplete" {
                    "response_incomplete"
                } else {
                    "response_failed"
                },
            ));
        }
        Ok(())
    }
}

impl ChatForcedState {
    fn ingest_data_str(&mut self, data_str: &str) -> Result<(), StreamLimitError> {
        if data_str == "[DONE]" {
            return Ok(());
        }
        let Ok(data) = serde_json::from_str::<Value>(data_str) else {
            return Ok(());
        };
        // Capture metadata from the very first data frame.
        if self.id.is_none() {
            if let Some(value) = data.get("id").and_then(Value::as_str) {
                replace_bounded_string(
                    &mut self.id,
                    value,
                    &mut self.retained_bytes,
                    "response id",
                )?;
            }
            self.created = data.get("created").and_then(|v| v.as_i64());
            if let Some(value) = data.get("model").and_then(Value::as_str) {
                replace_bounded_string(&mut self.model, value, &mut self.retained_bytes, "model")?;
            }
        }
        if self.usage.is_none() {
            if let Some(u) = data.get("usage") {
                if !u.is_null() {
                    let next = self
                        .retained_bytes
                        .checked_add(data_str.len())
                        .ok_or_else(|| StreamLimitError::arithmetic("usage state"))?;
                    if next > MAX_STREAM_ACCUMULATED_BYTES {
                        return Err(StreamLimitError::bytes(
                            "retained state",
                            MAX_STREAM_ACCUMULATED_BYTES,
                        ));
                    }
                    self.usage = Some(u.clone());
                    self.retained_bytes = next;
                }
            }
        }
        let Some(choices_arr) = data.get("choices").and_then(|v| v.as_array()) else {
            return Ok(());
        };
        for choice_val in choices_arr {
            let idx = wire_index(choice_val.get("index"), "choices[].index")?;
            if !self.choices.contains_key(&idx) && self.choices.len() >= MAX_STREAM_CHOICES {
                return Err(StreamLimitError::too_many(
                    "response choices",
                    MAX_STREAM_CHOICES,
                ));
            }
            let entry = self.choices.entry(idx).or_default();
            if let Some(reason) = choice_val.get("finish_reason") {
                if reason.is_string() {
                    let r = reason.as_str().unwrap();
                    if !r.is_empty() && r != "null" {
                        replace_bounded_string(
                            &mut entry.finish_reason,
                            r,
                            &mut self.retained_bytes,
                            "finish reason",
                        )?;
                    }
                }
            }
            let Some(delta) = choice_val.get("delta") else {
                continue;
            };
            if entry.role.is_none() {
                if let Some(role) = delta.get("role").and_then(|v| v.as_str()) {
                    replace_bounded_string(
                        &mut entry.role,
                        role,
                        &mut self.retained_bytes,
                        "role",
                    )?;
                }
            }
            if let Some(content) = delta.get("content") {
                if content.is_string() {
                    checked_append(
                        &mut entry.content,
                        content.as_str().unwrap(),
                        MAX_STREAM_ACCUMULATED_BYTES,
                        &mut self.retained_bytes,
                        "content",
                    )?;
                }
            }
            if let Some(refusal) = delta.get("refusal").and_then(|v| v.as_str()) {
                checked_append(
                    &mut entry.refusal,
                    refusal,
                    MAX_STREAM_ACCUMULATED_BYTES,
                    &mut self.retained_bytes,
                    "refusal",
                )?;
            }
            if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    let tc_idx = wire_index(tc.get("index"), "tool_calls[].index")?;
                    if !entry.tool_calls.contains_key(&tc_idx) {
                        if self.tool_call_count >= MAX_STREAM_TOOL_CALLS {
                            return Err(StreamLimitError::too_many(
                                "tool calls",
                                MAX_STREAM_TOOL_CALLS,
                            ));
                        }
                        self.tool_call_count = self
                            .tool_call_count
                            .checked_add(1)
                            .ok_or_else(|| StreamLimitError::arithmetic("tool call"))?;
                    }
                    let tool = entry.tool_calls.entry(tc_idx).or_default();
                    if let Some(tc_id) = tc.get("id").and_then(|v| v.as_str()) {
                        replace_bounded_string(
                            &mut tool.id,
                            tc_id,
                            &mut self.retained_bytes,
                            "tool id",
                        )?;
                    }
                    if let Some(tc_type) = tc.get("type").and_then(|v| v.as_str()) {
                        replace_bounded_string(
                            &mut tool.call_type,
                            tc_type,
                            &mut self.retained_bytes,
                            "tool type",
                        )?;
                    }
                    if let Some(func) = tc.get("function") {
                        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                            replace_bounded_string(
                                &mut tool.name,
                                name,
                                &mut self.retained_bytes,
                                "tool name",
                            )?;
                        }
                        if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                            checked_append(
                                &mut tool.arguments,
                                args,
                                MAX_STREAM_TOOL_ARGUMENT_BYTES,
                                &mut self.retained_bytes,
                                "tool arguments",
                            )?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(self, fallback_model: Option<&str>) -> Result<Option<Value>, StreamLimitError> {
        if self.choices.is_empty() {
            return Ok(None);
        }
        let mut response_choices: Vec<Value> = Vec::new();
        for (idx, accum) in &self.choices {
            let mut message = serde_json::Map::new();
            message.insert(
                "role".to_string(),
                Value::String(
                    accum
                        .role
                        .clone()
                        .unwrap_or_else(|| "assistant".to_string()),
                ),
            );
            if !accum.tool_calls.is_empty() {
                message.insert("content".to_string(), Value::Null);
                let mut call_arr = Vec::new();
                for tool in accum.tool_calls.values() {
                    let id = tool.id.as_ref().ok_or_else(|| StreamLimitError {
                        code: "upstream_stream_invalid_tool_call",
                        message: "Upstream tool call ended without an id".to_string(),
                    })?;
                    let name = tool.name.as_ref().ok_or_else(|| StreamLimitError {
                        code: "upstream_stream_invalid_tool_call",
                        message: "Upstream tool call ended without a function name".to_string(),
                    })?;
                    let mut tc_obj = serde_json::Map::new();
                    tc_obj.insert("id".to_string(), Value::String(id.clone()));
                    tc_obj.insert(
                        "type".to_string(),
                        Value::String(
                            tool.call_type
                                .clone()
                                .unwrap_or_else(|| "function".to_string()),
                        ),
                    );
                    let mut func_obj = serde_json::Map::new();
                    func_obj.insert("name".to_string(), Value::String(name.clone()));
                    func_obj.insert(
                        "arguments".to_string(),
                        Value::String(tool.arguments.clone()),
                    );
                    tc_obj.insert("function".to_string(), Value::Object(func_obj));
                    call_arr.push(Value::Object(tc_obj));
                }
                message.insert("tool_calls".to_string(), Value::Array(call_arr));
            } else {
                message.insert("content".to_string(), Value::String(accum.content.clone()));
            }
            response_choices.push(json!({
                "index": *idx,
                "message": Value::Object(message),
                "finish_reason": accum.finish_reason.clone().unwrap_or_else(|| "stop".to_string()),
            }));
        }
        let final_model = self
            .model
            .or_else(|| fallback_model.map(String::from))
            .unwrap_or_else(|| "unknown".to_string());
        Ok(Some(json!({
            "id": self.id.unwrap_or_else(|| {
                format!("chatcmpl-{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0000"))
            }),
            "object": "chat.completion",
            "created": self.created.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64
            }),
            "model": final_model,
            "choices": response_choices,
            "usage": self.usage.unwrap_or_else(|| json!({
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "total_tokens": 0,
            })),
        })))
    }
}

impl ResponsesForcedState {
    fn ingest_event(
        &mut self,
        event_name: Option<&str>,
        data_str: &str,
    ) -> Result<(), StreamLimitError> {
        let Some(event) = event_name else {
            return Ok(());
        };
        if data_str == "[DONE]" {
            return Ok(());
        }
        let recognized = matches!(
            event,
            "response.created"
                | "response.output_item.done"
                | "response.completed"
                | "response.failed"
                | "response.incomplete"
                | "error"
        );
        let parsed = match serde_json::from_str::<Value>(data_str) {
            Ok(parsed) => parsed,
            Err(_) if !recognized => return Ok(()),
            Err(_) => {
                return Err(StreamLimitError {
                    code: "upstream_stream_invalid_event",
                    message: format!("Codex returned malformed {event} event data"),
                });
            }
        };
        match event {
            "response.created" => {
                if let Some(id_val) = parsed.pointer("/response/id").and_then(|v| v.as_str()) {
                    replace_bounded_plain_string(
                        &mut self.summary.response_id,
                        id_val,
                        &mut self.summary.retained_bytes,
                        "response id",
                    )?;
                }
                if let Some(t) = parsed
                    .pointer("/response/created_at")
                    .and_then(|v| v.as_i64())
                {
                    self.summary.created = Some(t);
                }
            }
            "response.output_item.done" => {
                if let Some(item) = parsed.get("item") {
                    let idx = wire_index(parsed.get("output_index"), "output_index")?;
                    self.summary
                        .insert_output(idx, item.clone(), data_str.len())?;
                }
            }
            "response.completed" => {
                self.summary.status = "completed".to_string();
                self.summary.capture_response_metadata(&parsed)?;
                self.summary
                    .capture_usage(parsed.pointer("/response/usage"));
            }
            "response.failed" => {
                self.summary.status = "failed".to_string();
                self.summary.error = Some(response_event_error(&parsed, "response_failed"));
            }
            "response.incomplete" => {
                self.summary.status = "incomplete".to_string();
                self.summary.error = Some(response_event_error(&parsed, "response_incomplete"));
            }
            "error" => {
                self.summary.status = "error".to_string();
                self.summary.error = Some(response_event_error(&parsed, "upstream_error"));
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self, fallback_model: Option<&str>) -> Result<Option<Value>, StreamLimitError> {
        if self.summary.response_id.is_empty() {
            return Ok(None);
        }
        assemble_responses_summary(self.summary, fallback_model)
    }
}

/// Feed one complete SSE wire into a fresh accumulator. Test/oracle helper
/// that shares the incremental ingest path with live streaming.
#[allow(dead_code)]
pub fn accumulate_sse_bytes(
    input: &[u8],
    fallback_model: Option<&str>,
) -> Result<Option<Value>, StreamLimitError> {
    // This helper already receives a fully buffered body. Live streaming uses
    // the normal 1 MiB frame cap before reaching the accumulator; allow the
    // retained-state bound here so the in-memory converter keeps its existing
    // tool-argument contract without a second Responses parser.
    let mut framer = SseFramer::with_max_frame_bytes(MAX_STREAM_ACCUMULATED_BYTES);
    let mut accumulator = ForcedSseAccumulator::new();
    let mut ingest_error: Option<StreamLimitError> = None;
    let feed_result = framer.feed(input, |event| {
        if ingest_error.is_some() {
            return;
        }
        if let Err(error) = accumulator.ingest(&event) {
            ingest_error = Some(error);
        }
    });
    if let Some(error) = ingest_error {
        return Err(error);
    }
    feed_result.map_err(|frame| StreamLimitError {
        code: frame.code(),
        message: frame.to_string(),
    })?;
    let mut finish_error: Option<StreamLimitError> = None;
    let finish_result = framer.finish(|event| {
        if finish_error.is_some() {
            return;
        }
        if let Err(error) = accumulator.ingest(&event) {
            finish_error = Some(error);
        }
    });
    if let Some(error) = finish_error {
        return Err(error);
    }
    finish_result.map_err(|frame| StreamLimitError {
        code: frame.code(),
        message: frame.to_string(),
    })?;
    accumulator.finish(fallback_model)
}

// ---------------------------------------------------------------------------
// Chat Completions SSE  →  chat.completion JSON
// ---------------------------------------------------------------------------

/// Accumulator state for a single choice index.
#[derive(Debug, Default)]
struct ChoiceAccum {
    role: Option<String>,
    content: String,
    /// Tool calls keyed by their SSE-index within this choice.
    /// Each entry maps metadata keys (id, type, function_name, function_arguments)
    /// to their accumulated values.
    tool_calls: BTreeMap<u64, ToolCallAccum>,
    finish_reason: Option<String>,
    refusal: String,
}

#[derive(Debug, Default)]
struct ToolCallAccum {
    id: Option<String>,
    call_type: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// Convert OpenAI Chat Completions SSE (`data: {...}` lines) to a single
/// `chat.completion` JSON response.
///
/// Input looks like:
/// ```text
/// data: {"id":"chatcmpl-xxx","object":"chat.completion.chunk","created":123,...
/// data: {"choices":[{"index":0,"delta":{"content":"Hello"}}]}
/// data: [DONE]
/// ```
fn convert_chat_completion_stream(
    sse: &str,
    fallback_model: Option<&str>,
) -> Result<Option<Value>, StreamLimitError> {
    let mut id: Option<String> = None;
    let mut created: Option<i64> = None;
    let mut model: Option<String> = None;
    let mut usage: Option<Value> = None;
    let mut choices: BTreeMap<u64, ChoiceAccum> = BTreeMap::new();
    let mut tool_call_count = 0usize;
    let mut retained_bytes = 0usize;

    // Split by blank lines (SSE frame delimiter).
    for frame in sse.split("\n\n") {
        let frame = frame.trim();
        if frame.is_empty() {
            continue;
        }

        // Extract the `data:` line (skip `event:` lines if present).
        let data_str = frame
            .lines()
            .find(|line| line.trim().starts_with("data: "))
            .and_then(|line| line.trim().strip_prefix("data: "));

        let Some(data_str) = data_str else {
            continue;
        };

        if data_str == "[DONE]" {
            continue;
        }

        let Ok(data) = serde_json::from_str::<Value>(data_str) else {
            continue;
        };

        // Capture metadata from the very first data frame.
        if id.is_none() {
            if let Some(value) = data.get("id").and_then(Value::as_str) {
                replace_bounded_string(&mut id, value, &mut retained_bytes, "response id")?;
            }
            created = data.get("created").and_then(|v| v.as_i64());
            if let Some(value) = data.get("model").and_then(Value::as_str) {
                replace_bounded_string(&mut model, value, &mut retained_bytes, "model")?;
            }
        }

        // Usage may appear in the final frames.
        if usage.is_none() {
            if let Some(u) = data.get("usage") {
                if !u.is_null() {
                    let next = retained_bytes
                        .checked_add(data_str.len())
                        .ok_or_else(|| StreamLimitError::arithmetic("usage state"))?;
                    if next > MAX_STREAM_ACCUMULATED_BYTES {
                        return Err(StreamLimitError::bytes(
                            "retained state",
                            MAX_STREAM_ACCUMULATED_BYTES,
                        ));
                    }
                    usage = Some(u.clone());
                    retained_bytes = next;
                }
            }
        }

        // Process the choices array.
        let Some(choices_arr) = data.get("choices").and_then(|v| v.as_array()) else {
            continue;
        };

        for choice_val in choices_arr {
            let idx = wire_index(choice_val.get("index"), "choices[].index")?;
            if !choices.contains_key(&idx) && choices.len() >= MAX_STREAM_CHOICES {
                return Err(StreamLimitError::too_many(
                    "response choices",
                    MAX_STREAM_CHOICES,
                ));
            }
            let entry = choices.entry(idx).or_default();

            // Finish reason — last non-null/non-empty value wins.
            if let Some(reason) = choice_val.get("finish_reason") {
                if reason.is_string() {
                    let r = reason.as_str().unwrap();
                    if !r.is_empty() && r != "null" {
                        replace_bounded_string(
                            &mut entry.finish_reason,
                            r,
                            &mut retained_bytes,
                            "finish reason",
                        )?;
                    }
                }
            }

            let Some(delta) = choice_val.get("delta") else {
                continue;
            };

            // Role (only present in the very first chunk for each choice).
            if entry.role.is_none() {
                if let Some(role) = delta.get("role").and_then(|v| v.as_str()) {
                    replace_bounded_string(&mut entry.role, role, &mut retained_bytes, "role")?;
                }
            }

            // Content delta — append to accumulator.
            if let Some(content) = delta.get("content") {
                if content.is_string() {
                    checked_append(
                        &mut entry.content,
                        content.as_str().unwrap(),
                        MAX_STREAM_ACCUMULATED_BYTES,
                        &mut retained_bytes,
                        "content",
                    )?;
                }
            }

            // Refusal delta.
            if let Some(refusal) = delta.get("refusal").and_then(|v| v.as_str()) {
                checked_append(
                    &mut entry.refusal,
                    refusal,
                    MAX_STREAM_ACCUMULATED_BYTES,
                    &mut retained_bytes,
                    "refusal",
                )?;
            }

            // Tool calls delta — each chunk carries the full tool_call object
            // for its index, and we accumulate the function arguments across
            // chunks (same pattern as concat-ing content).
            if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    let tc_idx = wire_index(tc.get("index"), "tool_calls[].index")?;
                    if !entry.tool_calls.contains_key(&tc_idx) {
                        if tool_call_count >= MAX_STREAM_TOOL_CALLS {
                            return Err(StreamLimitError::too_many(
                                "tool calls",
                                MAX_STREAM_TOOL_CALLS,
                            ));
                        }
                        tool_call_count = tool_call_count
                            .checked_add(1)
                            .ok_or_else(|| StreamLimitError::arithmetic("tool call"))?;
                    }
                    let tool = entry.tool_calls.entry(tc_idx).or_default();

                    if let Some(tc_id) = tc.get("id").and_then(|v| v.as_str()) {
                        replace_bounded_string(
                            &mut tool.id,
                            tc_id,
                            &mut retained_bytes,
                            "tool id",
                        )?;
                    }
                    if let Some(tc_type) = tc.get("type").and_then(|v| v.as_str()) {
                        replace_bounded_string(
                            &mut tool.call_type,
                            tc_type,
                            &mut retained_bytes,
                            "tool type",
                        )?;
                    }

                    if let Some(func) = tc.get("function") {
                        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                            replace_bounded_string(
                                &mut tool.name,
                                name,
                                &mut retained_bytes,
                                "tool name",
                            )?;
                        }
                        if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                            checked_append(
                                &mut tool.arguments,
                                args,
                                MAX_STREAM_TOOL_ARGUMENT_BYTES,
                                &mut retained_bytes,
                                "tool arguments",
                            )?;
                        }
                    }
                }
            }
        }
    }

    if choices.is_empty() {
        return Ok(None);
    }

    // Build the response choices array.
    let mut response_choices: Vec<Value> = Vec::new();
    for (idx, accum) in &choices {
        let mut message = serde_json::Map::new();
        message.insert(
            "role".to_string(),
            Value::String(
                accum
                    .role
                    .clone()
                    .unwrap_or_else(|| "assistant".to_string()),
            ),
        );

        if !accum.tool_calls.is_empty() {
            message.insert("content".to_string(), Value::Null);

            let mut call_arr = Vec::new();
            for tool in accum.tool_calls.values() {
                let id = tool.id.as_ref().ok_or_else(|| StreamLimitError {
                    code: "upstream_stream_invalid_tool_call",
                    message: "Upstream tool call ended without an id".to_string(),
                })?;
                let name = tool.name.as_ref().ok_or_else(|| StreamLimitError {
                    code: "upstream_stream_invalid_tool_call",
                    message: "Upstream tool call ended without a function name".to_string(),
                })?;
                let mut tc_obj = serde_json::Map::new();
                tc_obj.insert("id".to_string(), Value::String(id.clone()));
                tc_obj.insert(
                    "type".to_string(),
                    Value::String(
                        tool.call_type
                            .clone()
                            .unwrap_or_else(|| "function".to_string()),
                    ),
                );

                let mut func_obj = serde_json::Map::new();
                func_obj.insert("name".to_string(), Value::String(name.clone()));
                func_obj.insert(
                    "arguments".to_string(),
                    Value::String(tool.arguments.clone()),
                );
                tc_obj.insert("function".to_string(), Value::Object(func_obj));

                call_arr.push(Value::Object(tc_obj));
            }
            message.insert("tool_calls".to_string(), Value::Array(call_arr));
        } else {
            message.insert("content".to_string(), Value::String(accum.content.clone()));
        }

        response_choices.push(json!({
            "index": *idx,
            "message": Value::Object(message),
            "finish_reason": accum.finish_reason.clone().unwrap_or_else(|| "stop".to_string()),
        }));
    }

    let final_model = model
        .or_else(|| fallback_model.map(String::from))
        .unwrap_or_else(|| "unknown".to_string());

    Ok(Some(json!({
        "id": id.unwrap_or_else(|| {
            format!("chatcmpl-{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0000"))
        }),
        "object": "chat.completion",
        "created": created.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64
        }),
        "model": final_model,
        "choices": response_choices,
        "usage": usage.unwrap_or_else(|| json!({
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0,
        })),
    })))
}

fn replace_bounded_string(
    target: &mut Option<String>,
    value: &str,
    retained_bytes: &mut usize,
    kind: &str,
) -> Result<(), StreamLimitError> {
    let old_len = target.as_ref().map_or(0, String::len);
    let without_old = retained_bytes
        .checked_sub(old_len)
        .ok_or_else(|| StreamLimitError::arithmetic("retained state"))?;
    let next = without_old
        .checked_add(value.len())
        .ok_or_else(|| StreamLimitError::arithmetic("retained state"))?;
    if next > MAX_STREAM_ACCUMULATED_BYTES {
        return Err(StreamLimitError::bytes(
            "retained state",
            MAX_STREAM_ACCUMULATED_BYTES,
        ));
    }
    let mut replacement = String::new();
    replacement
        .try_reserve(value.len())
        .map_err(|_| StreamLimitError::capacity(kind))?;
    replacement.push_str(value);
    *target = Some(replacement);
    *retained_bytes = next;
    Ok(())
}

fn replace_bounded_plain_string(
    target: &mut String,
    value: &str,
    retained_bytes: &mut usize,
    kind: &str,
) -> Result<(), StreamLimitError> {
    let without_old = retained_bytes
        .checked_sub(target.len())
        .ok_or_else(|| StreamLimitError::arithmetic("retained state"))?;
    let next = without_old
        .checked_add(value.len())
        .ok_or_else(|| StreamLimitError::arithmetic("retained state"))?;
    if next > MAX_STREAM_ACCUMULATED_BYTES {
        return Err(StreamLimitError::bytes(
            "retained state",
            MAX_STREAM_ACCUMULATED_BYTES,
        ));
    }
    target
        .try_reserve(value.len())
        .map_err(|_| StreamLimitError::capacity(kind))?;
    target.clear();
    target.push_str(value);
    *retained_bytes = next;
    Ok(())
}

// ---------------------------------------------------------------------------
// Responses API SSE  →  chat.completion JSON
// ---------------------------------------------------------------------------

/// Parsed summary of a Responses API SSE stream.
#[derive(Debug)]
struct ResponsesStreamSummary {
    response_id: String,
    created: Option<i64>,
    status: String,
    output: BTreeMap<u64, (Value, usize)>,
    usage: Value,
    retained_bytes: usize,
    error: Option<ResponsesSearchError>,
}

impl ResponsesStreamSummary {
    fn insert_output(
        &mut self,
        index: u64,
        item: Value,
        charge: usize,
    ) -> Result<(), StreamLimitError> {
        if !self.output.contains_key(&index) && self.output.len() >= MAX_RESPONSES_OUTPUT_ITEMS {
            return Err(StreamLimitError::too_many(
                "response output items",
                MAX_RESPONSES_OUTPUT_ITEMS,
            ));
        }
        let old_charge = self.output.get(&index).map_or(0, |(_, bytes)| *bytes);
        let next = self
            .retained_bytes
            .checked_sub(old_charge)
            .and_then(|bytes| bytes.checked_add(charge))
            .ok_or_else(|| StreamLimitError::arithmetic("response output state"))?;
        if next > MAX_STREAM_ACCUMULATED_BYTES {
            return Err(StreamLimitError::bytes(
                "response output state",
                MAX_STREAM_ACCUMULATED_BYTES,
            ));
        }
        self.output.insert(index, (item, charge));
        self.retained_bytes = next;
        Ok(())
    }

    fn capture_response_metadata(&mut self, event: &Value) -> Result<(), StreamLimitError> {
        let Some(response) = event.get("response") else {
            return Err(StreamLimitError {
                code: "upstream_stream_invalid_event",
                message: "Codex response.completed event omitted response".to_string(),
            });
        };
        if let Some(id) = response.get("id").and_then(Value::as_str) {
            replace_bounded_plain_string(
                &mut self.response_id,
                id,
                &mut self.retained_bytes,
                "response id",
            )?;
        }
        if let Some(created) = response.get("created_at").and_then(Value::as_i64) {
            self.created = Some(created);
        }
        if let Some(output) = response.get("output").and_then(Value::as_array) {
            for (index, item) in output.iter().enumerate() {
                let index = u64::try_from(index)
                    .map_err(|_| StreamLimitError::arithmetic("response output index"))?;
                if self.output.contains_key(&index) {
                    continue;
                }
                let charge = serde_json::to_vec(item)
                    .map_err(|_| StreamLimitError {
                        code: "upstream_stream_invalid_event",
                        message: "Codex returned an invalid completed output item".to_string(),
                    })?
                    .len();
                self.insert_output(index, item.clone(), charge)?;
            }
        }
        Ok(())
    }

    fn capture_usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else {
            return;
        };
        let mut map = serde_json::Map::new();
        for key in &[
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "cache_read_input_tokens",
            "cached_tokens",
            "cache_creation_input_tokens",
        ] {
            map.insert(
                key.to_string(),
                usage.get(*key).cloned().unwrap_or(json!(0)),
            );
        }
        self.usage = Value::Object(map);
    }
}

fn response_event_error(event: &Value, fallback_code: &str) -> ResponsesSearchError {
    let error = event
        .pointer("/response/error")
        .or_else(|| event.get("error"));
    let code = error
        .and_then(|value| value.get("code"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_code)
        .to_string();
    let message = error
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .or_else(|| event.get("message").and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .unwrap_or("Codex web search did not complete")
        .to_string();
    ResponsesSearchError { code, message }
}

/// Convert an OpenAI Responses API SSE stream to a single `chat.completion`
/// JSON response.
///
/// Input looks like:
/// ```text
/// event: response.created
/// data: {"type":"response.created","response":{"id":"resp_xxx",...}}
///
/// event: response.output_item.done
/// data: ...
///
/// event: response.completed
/// data: {"type":"response.completed","response":{"usage":{...}}}
/// ```
fn convert_responses_api_stream(
    sse: &str,
    fallback_model: Option<&str>,
) -> Result<Option<Value>, StreamLimitError> {
    accumulate_sse_bytes(sse.as_bytes(), fallback_model)
}

fn project_responses_search(
    summary: ResponsesStreamSummary,
) -> Result<ResponsesSearchOutput, ResponsesSearchError> {
    if summary.status != "completed" {
        return Err(summary.error.unwrap_or_else(|| ResponsesSearchError {
            code: "upstream_response_incomplete".to_string(),
            message: "Codex web search ended before response.completed".to_string(),
        }));
    }

    let mut messages = Vec::new();
    let mut sources: Vec<(String, String)> = Vec::new();
    for (item, _) in summary.output.values() {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(parts) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        let mut message = String::new();
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("output_text") {
                continue;
            }
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                append_search_output(&mut message, text)?;
            }
            let Some(annotations) = part.get("annotations").and_then(Value::as_array) else {
                continue;
            };
            for annotation in annotations {
                if annotation.get("type").and_then(Value::as_str) != Some("url_citation") {
                    continue;
                }
                let Some(url) = annotation
                    .get("url")
                    .and_then(Value::as_str)
                    .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
                else {
                    continue;
                };
                let title = annotation
                    .get("title")
                    .and_then(Value::as_str)
                    .map(normalize_source_title)
                    .unwrap_or_default();
                if let Some((_, existing_title)) =
                    sources.iter_mut().find(|(existing, _)| existing == url)
                {
                    if existing_title.is_empty() && !title.is_empty() {
                        *existing_title = title;
                    }
                } else {
                    sources.push((url.to_string(), title));
                }
            }
        }
        if !message.is_empty() {
            messages.push(message);
        }
    }

    if messages.is_empty() {
        return Err(ResponsesSearchError {
            code: "upstream_response_empty".to_string(),
            message: "Codex web search completed without answer text".to_string(),
        });
    }

    let mut text = String::new();
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            append_search_output(&mut text, "\n\n")?;
        }
        append_search_output(&mut text, message)?;
    }
    if !sources.is_empty() {
        append_search_output(&mut text, "\n\nSources:")?;
        for (index, (url, title)) in sources.iter().enumerate() {
            append_search_output(&mut text, "\n")?;
            append_search_output(&mut text, &(index + 1).to_string())?;
            append_search_output(&mut text, ". ")?;
            if !title.is_empty() {
                append_search_output(&mut text, title)?;
                append_search_output(&mut text, ": ")?;
            }
            append_search_output(&mut text, url)?;
        }
    }

    Ok(ResponsesSearchOutput { text })
}

fn append_search_output(target: &mut String, value: &str) -> Result<(), ResponsesSearchError> {
    let next = target
        .len()
        .checked_add(value.len())
        .ok_or_else(search_output_limit_error)?;
    if next > MAX_STREAM_ACCUMULATED_BYTES {
        return Err(search_output_limit_error());
    }
    target.push_str(value);
    Ok(())
}

fn search_output_limit_error() -> ResponsesSearchError {
    ResponsesSearchError {
        code: "mcp_result_too_large".to_string(),
        message: "Codex web search result exceeds the 16 MiB MCP limit".to_string(),
    }
}

fn normalize_source_title(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn assemble_responses_summary(
    summary: ResponsesStreamSummary,
    fallback_model: Option<&str>,
) -> Result<Option<Value>, StreamLimitError> {
    if summary.response_id.is_empty() {
        return Ok(None);
    }
    // Extract text and function calls from output items.
    let mut content_text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    tool_calls
        .try_reserve(summary.output.len())
        .map_err(|_| StreamLimitError::capacity("tool calls"))?;
    // Parsing and final assembly are one operation and therefore share one
    // retained-state budget. The parsed output remains live while final fields
    // are assembled, so assembly starts with the parser's current charge.
    let mut final_retained_bytes = summary.retained_bytes;
    for (item, _) in summary.output.values() {
        if let Some(item_type) = item.get("type").and_then(|v| v.as_str()) {
            if item_type == "message" {
                if let Some(content_arr) = item.get("content").and_then(|v| v.as_array()) {
                    for part in content_arr {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                checked_append(
                                    &mut content_text,
                                    text,
                                    MAX_STREAM_ACCUMULATED_BYTES,
                                    &mut final_retained_bytes,
                                    "response text",
                                )?;
                            }
                        }
                    }
                }
            } else if item_type == "function_call" {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| StreamLimitError {
                        code: "upstream_stream_invalid_tool_call",
                        message: "Responses function call ended without call_id".to_string(),
                    })?;
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| StreamLimitError {
                        code: "upstream_stream_invalid_tool_call",
                        message: "Responses function call ended without a name".to_string(),
                    })?;
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .ok_or_else(|| StreamLimitError {
                        code: "upstream_stream_invalid_tool_call",
                        message: "Responses function call ended without arguments".to_string(),
                    })?;
                if arguments.len() > MAX_STREAM_TOOL_ARGUMENT_BYTES {
                    return Err(StreamLimitError::bytes(
                        "tool arguments",
                        MAX_STREAM_TOOL_ARGUMENT_BYTES,
                    ));
                }
                let metadata_bytes = call_id
                    .len()
                    .checked_add(name.len())
                    .and_then(|bytes| bytes.checked_add(arguments.len()))
                    .ok_or_else(|| StreamLimitError::arithmetic("tool state"))?;
                checked_retain(&mut final_retained_bytes, metadata_bytes, "tool state")?;
                tool_calls.push(json!({
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments,
                    },
                }));
            }
        }
    }

    let input_tokens = summary
        .usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = summary
        .usage
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // 9router sseToJsonHandler.js: `input_tokens` EXCLUDES cached tokens on
    // cache-capable upstreams. Fold cache_read (cached_tokens) + cache_creation
    // into the client-facing prompt_tokens and surface them in
    // prompt_tokens_details so a client can tell a cache hit from a small prompt.
    let cache_read = summary
        .usage
        .get("cache_read_input_tokens")
        .or_else(|| summary.usage.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_create = summary
        .usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let folded_input = checked_add_u64(
        checked_add_u64(input_tokens, cache_read, "prompt token")?,
        cache_create,
        "prompt token",
    )?;
    let total_tokens = summary
        .usage
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .map(Ok)
        .unwrap_or_else(|| checked_add_u64(folded_input, output_tokens, "total token"))?;

    // prompt_tokens_details: only when a cache counter is non-zero.
    let mut usage_json = json!({
        "prompt_tokens": folded_input,
        "completion_tokens": output_tokens,
        "total_tokens": total_tokens,
    });
    if cache_read > 0 || cache_create > 0 {
        let mut details = serde_json::Map::new();
        if cache_read > 0 {
            details.insert("cached_tokens".into(), json!(cache_read));
        }
        if cache_create > 0 {
            details.insert("cache_creation_tokens".into(), json!(cache_create));
        }
        usage_json["prompt_tokens_details"] = Value::Object(details);
    }

    let model = fallback_model.unwrap_or("unknown");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let mut message = json!({
        "role": "assistant",
        "content": if tool_calls.is_empty() || !content_text.is_empty() {
            Value::String(content_text)
        } else {
            Value::Null
        },
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    let finish_reason = if summary.status != "completed" {
        "error"
    } else if message.get("tool_calls").is_some() {
        "tool_calls"
    } else {
        "stop"
    };

    Ok(Some(json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0000")),
        "object": "chat.completion",
        "created": summary.created.unwrap_or(now),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": usage_json,
    })))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_sse(chunks: impl IntoIterator<Item = Value>) -> String {
        chunks
            .into_iter()
            .map(|chunk| format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap()))
            .collect::<String>()
            + "data: [DONE]\n\n"
    }

    #[test]
    fn test_looks_like_sse_true_data() {
        assert!(looks_like_sse(b"data: {\"test\": 1}"));
    }

    #[test]
    fn test_looks_like_sse_true_event() {
        assert!(looks_like_sse(b"event: foo"));
    }

    #[test]
    fn test_looks_like_sse_true_comment() {
        assert!(looks_like_sse(b": keepalive"));
    }

    #[test]
    fn test_looks_like_sse_false_json() {
        assert!(!looks_like_sse(b"{\"test\": 1}"));
    }

    #[test]
    fn test_looks_like_sse_false_empty() {
        assert!(!looks_like_sse(b""));
        assert!(!looks_like_sse(b"  "));
    }

    #[test]
    fn test_chat_stream_simple_completion() {
        let sse = concat!(
            "data: {\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion.chunk\",\"created\":1712345678,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion.chunk\",\"created\":1712345678,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion.chunk\",\"created\":1712345678,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion.chunk\",\"created\":1712345678,\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let result = sse_stream_to_json(sse.as_bytes(), None).unwrap().unwrap();
        assert_eq!(result["id"], "chatcmpl-abc");
        assert_eq!(result["object"], "chat.completion");
        assert_eq!(result["choices"][0]["message"]["content"], "Hello world");
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
        assert_eq!(result["model"], "gpt-4");
        assert_eq!(result["created"], 1712345678);
    }

    #[test]
    fn test_chat_stream_with_tool_calls() {
        // Build the SSE programmatically to avoid escaping issues.
        let chunks = [
            json!({"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1712345678,"model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}),
            json!({"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1712345678,"model":"gpt-4","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"location\":\"SF\"}"}}]},"finish_reason":null}]}),
            json!({"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1712345678,"model":"gpt-4","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ];
        let sse: String = chunks
            .iter()
            .map(|c| format!("data: {}\n\n", serde_json::to_string(c).unwrap()))
            .collect::<Vec<_>>()
            .join("")
            + "data: [DONE]\n\n";

        let result = sse_stream_to_json(sse.as_bytes(), None).unwrap().unwrap();
        assert_eq!(result["object"], "chat.completion");
        let msg = &result["choices"][0]["message"];
        assert!(msg["content"].is_null());
        let tcs = msg["tool_calls"].as_array().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0]["id"], "call_1");
        assert_eq!(tcs[0]["type"], "function");
        assert_eq!(tcs[0]["function"]["name"], "get_weather");
        assert!(tcs[0]["function"]["arguments"]
            .as_str()
            .unwrap()
            .contains("SF"));
        assert_eq!(result["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn test_chat_stream_with_usage() {
        let chunks = [
            json!({"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1712345678,"model":"gpt-4","choices":[{"index":0,"delta":{"role":"assistant","content":"Hi"},"finish_reason":null}]}),
            json!({"id":"chatcmpl-abc","object":"chat.completion.chunk","created":1712345678,"model":"gpt-4","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}),
        ];
        let sse: String = chunks
            .iter()
            .map(|c| format!("data: {}\n\n", serde_json::to_string(c).unwrap()))
            .collect::<Vec<_>>()
            .join("")
            + "data: [DONE]\n\n";

        let result = sse_stream_to_json(sse.as_bytes(), None).unwrap().unwrap();
        assert_eq!(result["usage"]["prompt_tokens"], 10);
        assert_eq!(result["usage"]["completion_tokens"], 5);
        assert_eq!(result["usage"]["total_tokens"], 15);
        assert_eq!(result["choices"][0]["message"]["content"], "Hi");
    }

    #[test]
    fn test_responses_api_stream_to_chat() {
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_123\",\"created_at\":1712345678}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world\"}],\"role\":\"assistant\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_123\",\"status\":\"completed\",\"usage\":{\"input_tokens\":15,\"output_tokens\":25,\"total_tokens\":40}}}\n\n",
            "data: [DONE]\n\n",
        );

        let result = sse_stream_to_json(sse.as_bytes(), Some("claude-sonnet-4"))
            .unwrap()
            .unwrap();
        assert_eq!(result["object"], "chat.completion");
        assert_eq!(result["model"], "claude-sonnet-4");
        assert_eq!(result["choices"][0]["message"]["content"], "Hello world");
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
        assert_eq!(result["usage"]["prompt_tokens"], 15);
        assert_eq!(result["usage"]["completion_tokens"], 25);
        assert_eq!(result["usage"]["total_tokens"], 40);
    }

    #[test]
    fn test_responses_api_function_call_to_chat() {
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_tool\",\"created_at\":1712345678}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"test_tool\",\"arguments\":\"{}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7}}}\n\n",
        );

        let result = sse_stream_to_json(sse.as_bytes(), Some("gpt-5.6-luna"))
            .unwrap()
            .unwrap();
        let message = &result["choices"][0]["message"];
        assert!(message["content"].is_null());
        assert_eq!(message["tool_calls"][0]["id"], "call_1");
        assert_eq!(message["tool_calls"][0]["function"]["name"], "test_tool");
        assert_eq!(result["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn test_responses_api_with_multiple_output_items() {
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_456\",\"created_at\":1712345680}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"summary\":[{\"type\":\"summary_text\",\"text\":\"thinking...\"}]}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1,\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Final answer\"}],\"role\":\"assistant\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":10,\"total_tokens\":15}}}\n\n",
        );

        let result = sse_stream_to_json(sse.as_bytes(), Some("codex/o4-mini"))
            .unwrap()
            .unwrap();
        // Should only include the message text (reasoning items are skipped).
        assert_eq!(result["choices"][0]["message"]["content"], "Final answer");
        assert_eq!(result["usage"]["total_tokens"], 15);
    }

    #[test]
    fn test_chat_stream_no_choices_returns_none() {
        let sse = "data: {\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"gpt-4\"}\n\ndata: [DONE]\n\n";
        let result = sse_stream_to_json(sse.as_bytes(), None);
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn test_sse_stream_to_json_rejects_plain_json() {
        let json = b"{\"id\":\"chatcmpl-abc\",\"object\":\"chat.completion\",\"choices\":[]}";
        assert_eq!(sse_stream_to_json(json, None), Ok(None));
    }

    #[test]
    fn test_sse_stream_to_json_empty() {
        assert_eq!(sse_stream_to_json(b"", None), Ok(None));
    }

    #[test]
    fn test_cached_tokens_folded_into_prompt_tokens_with_details() {
        // 9router sseToJsonHandler.js: cache-capable upstreams exclude cached
        // tokens from input_tokens. Fold cache_read + cache_creation into
        // prompt_tokens and surface prompt_tokens_details.
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_cached\",\"created_at\":1712345678}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_cached\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"total_tokens\":15,\"cache_read_input_tokens\":20,\"cache_creation_input_tokens\":3}}}\n\n",
            "data: [DONE]\n\n",
        );
        let result = sse_stream_to_json(sse.as_bytes(), Some("gpt-4o"))
            .unwrap()
            .unwrap();
        // 10 (input) + 20 (cache_read) + 3 (cache_creation) = 33.
        assert_eq!(result["usage"]["prompt_tokens"], 33);
        assert_eq!(result["usage"]["completion_tokens"], 5);
        // total_tokens keeps the upstream value (9router keeps upstream total).
        assert_eq!(result["usage"]["total_tokens"], 15);
        // prompt_tokens_details surfaces the cache counters.
        assert_eq!(
            result["usage"]["prompt_tokens_details"]["cached_tokens"],
            20
        );
        assert_eq!(
            result["usage"]["prompt_tokens_details"]["cache_creation_tokens"],
            3
        );
    }

    #[test]
    fn test_no_cache_counters_no_details() {
        // No cache counters → no prompt_tokens_details, prompt_tokens unchanged.
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_nocache\",\"created_at\":1712345678}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_nocache\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"total_tokens\":15}}}\n\n",
            "data: [DONE]\n\n",
        );
        let result = sse_stream_to_json(sse.as_bytes(), Some("gpt-4o"))
            .unwrap()
            .unwrap();
        assert_eq!(result["usage"]["prompt_tokens"], 10);
        assert!(result["usage"].get("prompt_tokens_details").is_none());
    }

    #[test]
    fn rejects_extreme_sparse_and_negative_wire_indices() {
        for index in [json!(u64::MAX), json!(1_000_000_000u64), json!(-1)] {
            let sse = chat_sse([json!({
                "choices": [{"index": index, "delta": {"content": "x"}}]
            })]);
            let error = sse_stream_to_json(sse.as_bytes(), None).unwrap_err();
            assert!(matches!(
                error.code,
                "upstream_stream_index_limit" | "upstream_stream_invalid_index"
            ));
        }

        let sse = chat_sse([json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{"index": u64::MAX, "id": "call", "function": {"name": "f"}}]}
            }]
        })]);
        assert_eq!(
            sse_stream_to_json(sse.as_bytes(), None).unwrap_err().code,
            "upstream_stream_index_limit"
        );
    }

    #[test]
    fn rejects_max_plus_one_tool_calls_without_sparse_allocation() {
        let tool_calls = (0..=MAX_STREAM_TOOL_CALLS)
            .map(|index| {
                json!({
                    "index": index,
                    "id": format!("call_{index}"),
                    "type": "function",
                    "function": {"name": "f", "arguments": "{}"}
                })
            })
            .collect::<Vec<_>>();
        let sse = chat_sse([json!({
            "choices": [{"index": 0, "delta": {"tool_calls": tool_calls}}]
        })]);
        let error = sse_stream_to_json(sse.as_bytes(), None).unwrap_err();
        assert_eq!(error.code, "upstream_stream_state_limit");
        assert!(error.message.contains("128 tool calls"));
    }

    #[test]
    fn tool_arguments_accept_exact_limit_and_reject_plus_one() {
        for (size, accepted) in [
            (MAX_STREAM_TOOL_ARGUMENT_BYTES, true),
            (MAX_STREAM_TOOL_ARGUMENT_BYTES + 1, false),
        ] {
            let arguments = "x".repeat(size);
            let sse = chat_sse([json!({
                "choices": [{
                    "index": 0,
                    "delta": {"tool_calls": [{
                        "index": 0,
                        "id": "call_exact",
                        "type": "function",
                        "function": {"name": "write", "arguments": arguments}
                    }]},
                    "finish_reason": "tool_calls"
                }]
            })]);
            let result = sse_stream_to_json(sse.as_bytes(), None);
            if accepted {
                assert_eq!(
                    result.unwrap().unwrap()["choices"][0]["message"]["tool_calls"][0]["function"]
                        ["arguments"]
                        .as_str()
                        .unwrap()
                        .len(),
                    size
                );
            } else {
                assert_eq!(result.unwrap_err().code, "upstream_stream_state_limit");
            }
        }
    }

    #[test]
    fn out_of_order_repeated_indices_preserve_numeric_order_unicode_and_arguments() {
        let sse = chat_sse([
            json!({"choices": [{"index": 3, "delta": {"tool_calls": [{"index": 2, "id": "call_2", "type": "function", "function": {"name": "echo", "arguments": "{\"emoji\":\""}}]}}]}),
            json!({"choices": [{"index": 3, "delta": {"tool_calls": [{"index": 0, "id": "call_0", "type": "function", "function": {"name": "first", "arguments": "{\"n\":0}"}}]}}]}),
            json!({"choices": [{"index": 3, "delta": {"tool_calls": [{"index": 2, "function": {"arguments": "雪❄️\"}"}}]}, "finish_reason": "tool_calls"}]}),
        ]);
        let result = sse_stream_to_json(sse.as_bytes(), Some("golden"))
            .unwrap()
            .unwrap();
        let tools = result["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["id"], "call_0");
        assert_eq!(tools[1]["id"], "call_2");
        assert_eq!(tools[1]["function"]["arguments"], "{\"emoji\":\"雪❄️\"}");
    }

    #[test]
    fn responses_output_index_and_usage_overflow_fail_explicitly() {
        let sparse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_sparse\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":1000000000,\"item\":{\"type\":\"message\",\"content\":[]}}\n\n"
        );
        assert_eq!(
            sse_stream_to_json(sparse.as_bytes(), None)
                .unwrap_err()
                .code,
            "upstream_stream_index_limit"
        );

        let overflow = format!(
            "event: response.created\ndata: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_overflow\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":{},\"cache_read_input_tokens\":1,\"output_tokens\":0}}}}}}\n\n",
            u64::MAX
        );
        assert_eq!(
            sse_stream_to_json(overflow.as_bytes(), None)
                .unwrap_err()
                .code,
            "upstream_stream_arithmetic_overflow"
        );
    }

    #[test]
    fn incomplete_responses_tool_call_is_not_fabricated() {
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_bad_tool\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"name\":\"tool\",\"arguments\":\"{}\"}}\n\n"
        );
        let error = sse_stream_to_json(sse.as_bytes(), None).unwrap_err();
        assert_eq!(error.code, "upstream_stream_invalid_tool_call");
    }

    #[test]
    fn wire_index_and_choice_count_boundaries_are_inclusive() {
        let at_index = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [{"index": 4095, "delta": {"content": "ok"}}]})
        );
        assert!(sse_stream_to_json(at_index.as_bytes(), None)
            .unwrap()
            .is_some());
        let above_index = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [{"index": 4096, "delta": {"content": "bad"}}]})
        );
        assert_eq!(
            sse_stream_to_json(above_index.as_bytes(), None)
                .unwrap_err()
                .code,
            "upstream_stream_index_limit"
        );

        let choices = (0..MAX_STREAM_CHOICES)
            .map(|index| json!({"index": index, "delta": {"content": "x"}}))
            .collect::<Vec<_>>();
        let exact = format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices": choices}));
        assert!(sse_stream_to_json(exact.as_bytes(), None)
            .unwrap()
            .is_some());

        let choices = (0..=MAX_STREAM_CHOICES)
            .map(|index| json!({"index": index, "delta": {"content": "x"}}))
            .collect::<Vec<_>>();
        let oversized = format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices": choices}));
        assert_eq!(
            sse_stream_to_json(oversized.as_bytes(), None)
                .unwrap_err()
                .code,
            "upstream_stream_state_limit"
        );
    }

    fn responses_function_stream(arguments: &str) -> String {
        format!(
            "event: response.created\ndata: {}\n\nevent: response.output_item.done\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
            json!({"type":"response.created","response":{"id":"resp_bounds"}}),
            json!({
                "type":"response.output_item.done",
                "output_index":0,
                "item":{"type":"function_call","call_id":"call_1","name":"run","arguments":arguments}
            }),
            json!({"type":"response.completed","response":{"usage":{}}})
        )
    }

    #[test]
    fn responses_arguments_accept_exact_limit_and_reject_plus_one() {
        let exact = "x".repeat(MAX_STREAM_TOOL_ARGUMENT_BYTES);
        assert!(
            sse_stream_to_json(responses_function_stream(&exact).as_bytes(), None)
                .unwrap()
                .is_some()
        );

        let oversized = "x".repeat(MAX_STREAM_TOOL_ARGUMENT_BYTES + 1);
        assert_eq!(
            sse_stream_to_json(responses_function_stream(&oversized).as_bytes(), None)
                .unwrap_err()
                .code,
            "upstream_stream_state_limit"
        );
    }

    #[test]
    fn responses_parse_and_final_assembly_share_one_budget() {
        let text = "x".repeat(MAX_STREAM_ACCUMULATED_BYTES / 2 + 1);
        let stream = format!(
            "event: response.created\ndata: {}\n\nevent: response.output_item.done\ndata: {}\n\n",
            json!({"type":"response.created","response":{"id":"resp_shared"}}),
            json!({
                "type":"response.output_item.done",
                "output_index":0,
                "item":{"type":"message","content":[{"type":"output_text","text":text}]}
            })
        );
        assert_eq!(
            sse_stream_to_json(stream.as_bytes(), None)
                .unwrap_err()
                .code,
            "upstream_stream_state_limit"
        );
    }

    fn project_search_sse(sse: &str) -> Result<ResponsesSearchOutput, ResponsesSearchError> {
        let mut framer = SseFramer::new();
        let mut accumulator = ForcedSseAccumulator::new();
        let mut ingest_error = None;
        framer
            .feed(sse.as_bytes(), |event| {
                if ingest_error.is_none() {
                    ingest_error = accumulator.ingest(&event).err();
                }
            })
            .unwrap();
        if let Some(error) = ingest_error {
            return Err(ResponsesSearchError {
                code: error.code.to_string(),
                message: error.message,
            });
        }
        accumulator.finish_responses_search()
    }

    #[test]
    fn responses_search_many_items_preserve_order_messages_and_sources() {
        let mut sse = format!(
            "event: response.created\ndata: {}\n\n",
            json!({"type":"response.created","response":{"id":"resp_many"}})
        );
        for index in (0..120).rev() {
            let item = match index % 3 {
                0 => json!({"type":"web_search_call","id":format!("search_{index}")}),
                1 => json!({"type":"reasoning","summary":[{"text":"private reasoning"}]}),
                _ => {
                    let message = index / 3;
                    json!({
                        "type":"message",
                        "content":[
                            {
                                "type":"output_text",
                                "text":format!("Answer {message} café 雪"),
                                "annotations":[{
                                    "type":"url_citation",
                                    "url":format!("https://example.test/{message}"),
                                    "title":format!(" Source   {message} ")
                                }]
                            },
                            {
                                "type":"output_text",
                                "text":" ✅",
                                "annotations":[{
                                    "type":"url_citation",
                                    "url":format!("https://example.test/{message}"),
                                    "title":"duplicate"
                                }]
                            }
                        ]
                    })
                }
            };
            sse.push_str(&format!(
                "event: response.output_item.done\ndata: {}\n\n",
                json!({
                    "type":"response.output_item.done",
                    "output_index":index,
                    "item":item
                })
            ));
        }
        sse.push_str(&format!(
            "event: response.completed\ndata: {}\n\n",
            json!({"type":"response.completed","response":{"id":"resp_many","status":"completed","output":[],"usage":{}}})
        ));

        let result = project_search_sse(&sse).unwrap().text;
        for index in 0..40 {
            let answer = format!("Answer {index} café 雪 ✅");
            let source = format!(
                "{}. Source {}: https://example.test/{}",
                index + 1,
                index,
                index
            );
            assert_eq!(result.matches(&answer).count(), 1);
            assert_eq!(result.matches(&source).count(), 1);
        }
        assert!(result.find("Answer 0").unwrap() < result.find("Answer 39").unwrap());
        assert_eq!(result.matches("\n\nSources:\n").count(), 1);
        assert!(!result.contains("private reasoning"));
        assert!(!result.contains("search_"));
        assert!(!result.contains("event:"));
    }

    #[test]
    fn responses_search_terminal_failures_never_return_partial_answer() {
        let partial = format!(
            "event: response.created\ndata: {}\n\nevent: response.output_item.done\ndata: {}\n\n",
            json!({"type":"response.created","response":{"id":"resp_failed"}}),
            json!({
                "type":"response.output_item.done",
                "output_index":0,
                "item":{"type":"message","content":[{"type":"output_text","text":"partial secret"}]}
            })
        );
        let cases = vec![
            (String::new(), "upstream_response_incomplete"),
            (
                format!(
                    "event: response.failed\ndata: {}\n\n",
                    json!({"type":"response.failed","response":{"error":{"code":"failed_code","message":"failed safely"}}})
                ),
                "failed_code",
            ),
            (
                format!(
                    "event: response.incomplete\ndata: {}\n\n",
                    json!({"type":"response.incomplete","response":{"error":{"code":"incomplete_code","message":"incomplete safely"}}})
                ),
                "incomplete_code",
            ),
            (
                format!(
                    "event: error\ndata: {}\n\n",
                    json!({"type":"error","error":{"code":"error_code","message":"errored safely"}})
                ),
                "error_code",
            ),
        ];

        for (terminal, expected_code) in cases {
            let error = project_search_sse(&(partial.clone() + &terminal)).unwrap_err();
            assert_eq!(error.code, expected_code);
            assert!(!error.message.contains("partial secret"));
        }
    }
}
