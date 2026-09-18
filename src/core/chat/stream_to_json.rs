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

use crate::core::translator::limits::{
    checked_add_u64, checked_append, checked_retain, wire_index, StreamLimitError,
    MAX_STREAM_ACCUMULATED_BYTES, MAX_STREAM_CHOICES, MAX_STREAM_TOOL_ARGUMENT_BYTES,
    MAX_STREAM_TOOL_CALLS,
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
struct ResponsesStreamSummary {
    response_id: String,
    created: Option<i64>,
    status: String,
    output: BTreeMap<u64, (Value, usize)>,
    usage: Value,
    retained_bytes: usize,
}

/// Parse a Responses API SSE stream (pairs of `event:` / `data:` lines) into
/// a summary struct.
fn parse_responses_api_stream(
    sse: &str,
) -> Result<Option<ResponsesStreamSummary>, StreamLimitError> {
    let mut summary = ResponsesStreamSummary {
        response_id: String::new(),
        created: None,
        status: "in_progress".to_string(),
        output: BTreeMap::new(),
        usage: json!({"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}),
        retained_bytes: 0,
    };

    for frame in sse.split("\n\n") {
        let frame = frame.trim();
        if frame.is_empty() {
            continue;
        }

        let mut event_name = None::<String>;
        let mut data_str = String::new();

        for line in frame.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("event:") {
                event_name = Some(rest.trim().to_string());
            } else if let Some(rest) = trimmed.strip_prefix("data:") {
                if !data_str.is_empty() {
                    data_str.push('\n');
                }
                data_str.push_str(rest.trim_start());
            }
        }

        let Some(event) = event_name else {
            continue;
        };

        if data_str == "[DONE]" {
            continue;
        }

        let Ok(parsed) = serde_json::from_str::<Value>(&data_str) else {
            continue;
        };

        match event.as_str() {
            "response.created" => {
                if let Some(id_val) = parsed.pointer("/response/id").and_then(|v| v.as_str()) {
                    replace_bounded_plain_string(
                        &mut summary.response_id,
                        id_val,
                        &mut summary.retained_bytes,
                        "response id",
                    )?;
                }
                if let Some(t) = parsed
                    .pointer("/response/created_at")
                    .and_then(|v| v.as_i64())
                {
                    summary.created = Some(t);
                }
            }
            "response.output_item.done" => {
                if let Some(item) = parsed.get("item") {
                    let idx = wire_index(parsed.get("output_index"), "output_index")?;
                    if !summary.output.contains_key(&idx)
                        && summary.output.len() >= MAX_STREAM_TOOL_CALLS
                    {
                        return Err(StreamLimitError::too_many(
                            "response output items",
                            MAX_STREAM_TOOL_CALLS,
                        ));
                    }
                    let old_charge = summary.output.get(&idx).map_or(0, |(_, bytes)| *bytes);
                    let charge = data_str.len();
                    let next = summary
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
                    summary.output.insert(idx, (item.clone(), charge));
                    summary.retained_bytes = next;
                }
            }
            "response.completed" => {
                summary.status = "completed".to_string();
                if let Some(usage) = parsed.pointer("/response/usage") {
                    let mut map = serde_json::Map::new();
                    // Keep the cache counters so the aggregation below can fold
                    // them into prompt_tokens + prompt_tokens_details (P1-F6).
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
                    summary.usage = Value::Object(map);
                }
            }
            "response.failed" => {
                summary.status = "failed".to_string();
            }
            _ => {}
        }
    }

    if summary.response_id.is_empty() {
        return Ok(None);
    }

    Ok(Some(summary))
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
    let summary = match parse_responses_api_stream(sse)? {
        Some(summary) => summary,
        None => return Ok(None),
    };

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
}
