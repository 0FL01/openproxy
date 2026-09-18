//! CommandCode to OpenAI response translator.
//!
//! Converts CommandCode NDJSON AI SDK v5 stream events to OpenAI SSE chunks.
//! Matches the registry's ResponseTransformFn signature:
//!   fn(chunk: &[u8], state: &mut ResponseTransformState) -> Vec<String>

use crate::core::translator::limits::StreamLimitError;
use crate::core::translator::registry::{CommandCodeResponseState, ResponseTransformState};
use serde_json::Value;

fn map_finish_reason(reason: &str) -> &str {
    match reason {
        "stop" => "stop",
        "length" => "length",
        "tool-calls" | "tool_use" => "tool_calls",
        "content-filter" => "content_filter",
        "error" => "stop",
        _ => "stop",
    }
}

fn ensure_state_initialized(state: &mut CommandCodeResponseState, model_hint: Option<&str>) {
    if state.response_id.is_some() {
        return;
    }
    state.response_id = Some(format!(
        "chatcmpl-{}",
        chrono::Utc::now().timestamp_millis()
    ));
    state.created = Some(chrono::Utc::now().timestamp());
    state.model = Some(model_hint.unwrap_or("commandcode").to_string());
    state.chunk_index = 0;
    state.tool_index = 0;
    state.tool_index_by_id = serde_json::Map::new();
}

fn make_chunk_line(
    state: &CommandCodeResponseState,
    delta: Value,
    finish_reason: Option<&str>,
) -> Result<String, StreamLimitError> {
    let response_id = state.response_id.as_deref().unwrap_or("unknown");
    let created = state.created.unwrap_or(0);
    let model = state.model.as_deref().unwrap_or("commandcode");

    let mut obj = serde_json::json!({
        "id": response_id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason
        }]
    });

    // Attach usage on the final chunk if available
    if finish_reason.is_some() {
        if let Some(usage) = &state.usage {
            let input_tokens = usage
                .get("inputTokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let output_tokens = usage
                .get("outputTokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let total = match usage.get("totalTokens").and_then(Value::as_u64) {
                Some(total) => total,
                None => input_tokens
                    .checked_add(output_tokens)
                    .ok_or_else(|| StreamLimitError::arithmetic("total token"))?,
            };
            obj["usage"] = serde_json::json!({
                "prompt_tokens": input_tokens,
                "completion_tokens": output_tokens,
                "total_tokens": total
            });
        }
    }

    // Wire Responses API event-based SSE framing.
    // The first chunk gets event: response.created, the final
    // chunk gets event: response.done; intermediate content
    // chunks carry no event line (bare data: lines).
    let event_prefix = if state.chunk_index == 0 && finish_reason.is_none() {
        "event: response.created\n"
    } else if finish_reason.is_some() {
        "event: response.done\n"
    } else {
        ""
    };

    let json_str = serde_json::to_string(&obj).unwrap_or_default();
    Ok(format!("{event_prefix}data: {}\n\n", json_str))
}

fn process_event(
    event: &Value,
    state: &mut CommandCodeResponseState,
) -> Result<Vec<String>, StreamLimitError> {
    let event_type = event.get("type").and_then(|v| v.as_str());
    if event_type.is_none() {
        return Ok(vec![]);
    }
    let event_type = event_type.unwrap();

    // Seed model from first event if not yet set
    if state.model.is_none() {
        if let Some(m) = event.get("model").and_then(|v| v.as_str()) {
            state.model = Some(m.to_string());
        }
    }

    let chunk_index = state.chunk_index;
    let tool_index = state.tool_index;

    let mut out = Vec::new();

    match event_type {
        "text-delta" => {
            let text = event
                .get("text")
                .or_else(|| event.get("delta"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if text.is_empty() {
                return Ok(vec![]);
            }
            let delta = if chunk_index == 0 {
                serde_json::json!({"role": "assistant", "content": text})
            } else {
                serde_json::json!({"content": text})
            };
            out.push(make_chunk_line(state, delta, None)?);
            state.chunk_index = state
                .chunk_index
                .checked_add(1)
                .ok_or_else(|| StreamLimitError::arithmetic("chunk index"))?;
        }
        "reasoning-delta" => {
            let text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text.is_empty() {
                return Ok(vec![]);
            }
            let delta = if chunk_index == 0 {
                serde_json::json!({"role": "assistant", "reasoning_content": text})
            } else {
                serde_json::json!({"reasoning_content": text})
            };
            out.push(make_chunk_line(state, delta, None)?);
            state.chunk_index = state
                .chunk_index
                .checked_add(1)
                .ok_or_else(|| StreamLimitError::arithmetic("chunk index"))?;
        }
        "tool-input-start" => {
            let id = event
                .get("id")
                .or_else(|| event.get("toolCallId"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let idx = if let Some(existing) = state.tool_index_by_id.get(id) {
                let Some(index) = existing.as_u64() else {
                    return Ok(vec![]);
                };
                index
            } else {
                state
                    .tool_index_by_id
                    .insert(id.to_string(), Value::Number(tool_index.into()));
                let idx = tool_index;
                state.tool_index = state
                    .tool_index
                    .checked_add(1)
                    .ok_or_else(|| StreamLimitError::arithmetic("tool index"))?;
                idx
            };
            let delta = if chunk_index == 0 {
                serde_json::json!({
                    "role": "assistant",
                    "tool_calls": [{
                        "index": idx,
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": event.get("toolName").and_then(|v| v.as_str()).unwrap_or(""),
                            "arguments": ""
                        }
                    }]
                })
            } else {
                serde_json::json!({
                    "tool_calls": [{
                        "index": idx,
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": event.get("toolName").and_then(|v| v.as_str()).unwrap_or(""),
                            "arguments": ""
                        }
                    }]
                })
            };
            out.push(make_chunk_line(state, delta, None)?);
            state.chunk_index = state
                .chunk_index
                .checked_add(1)
                .ok_or_else(|| StreamLimitError::arithmetic("chunk index"))?;
        }
        "tool-input-delta" => {
            let id = event
                .get("id")
                .or_else(|| event.get("toolCallId"))
                .and_then(|v| v.as_str());
            if id.is_none() {
                return Ok(vec![]);
            }
            let id = id.unwrap();
            if let Some(idx_val) = state.tool_index_by_id.get(id) {
                let Some(idx) = idx_val.as_u64() else {
                    return Ok(vec![]);
                };
                let delta = serde_json::json!({
                    "tool_calls": [{
                        "index": idx,
                        "function": {
                            "arguments": event.get("delta").or_else(|| event.get("inputTextDelta")).and_then(|v| v.as_str()).unwrap_or("")
                        }
                    }]
                });
                out.push(make_chunk_line(state, delta, None)?);
            }
        }
        "tool-call" => {
            let id = event.get("toolCallId").and_then(|v| v.as_str());
            if id.is_none() {
                return Ok(vec![]);
            }
            let id = id.unwrap();
            if state.tool_index_by_id.get(id).is_some() {
                return Ok(vec![]);
            }
            state
                .tool_index_by_id
                .insert(id.to_string(), Value::Number(tool_index.into()));
            state.tool_index = state
                .tool_index
                .checked_add(1)
                .ok_or_else(|| StreamLimitError::arithmetic("tool index"))?;

            let args_str = if let Some(s) = event.get("input").and_then(|v| v.as_str()) {
                s.to_string()
            } else {
                serde_json::to_string(
                    &event
                        .get("input")
                        .cloned()
                        .unwrap_or(Value::Object(serde_json::Map::new())),
                )
                .unwrap_or_else(|_| "{}".to_string())
            };
            let delta = if chunk_index == 0 {
                serde_json::json!({
                    "role": "assistant",
                    "tool_calls": [{
                        "index": tool_index,
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": event.get("toolName").and_then(|v| v.as_str()).unwrap_or(""),
                            "arguments": args_str
                        }
                    }]
                })
            } else {
                serde_json::json!({
                    "tool_calls": [{
                        "index": tool_index,
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": event.get("toolName").and_then(|v| v.as_str()).unwrap_or(""),
                            "arguments": args_str
                        }
                    }]
                })
            };
            out.push(make_chunk_line(state, delta, None)?);
            state.chunk_index = state
                .chunk_index
                .checked_add(1)
                .ok_or_else(|| StreamLimitError::arithmetic("chunk index"))?;
        }
        "finish-step" => {
            if let Some(reason) = event.get("finishReason").and_then(|v| v.as_str()) {
                state.finish_reason = Some(map_finish_reason(reason).to_string());
            }
            if let Some(usage) = event.get("usage") {
                state.usage = Some(usage.clone());
            }
        }
        "finish" => {
            let finish_reason = event
                .get("finishReason")
                .and_then(|v| v.as_str())
                .map(map_finish_reason)
                .or(state.finish_reason.as_deref())
                .unwrap_or("stop");

            let mut final_chunk = serde_json::json!({
                "id": state.response_id.as_deref().unwrap_or("unknown"),
                "object": "chat.completion.chunk",
                "created": state.created.unwrap_or(0),
                "model": state.model.as_deref().unwrap_or("commandcode"),
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": finish_reason
                }]
            });

            let total_usage = event.get("totalUsage").or(state.usage.as_ref());
            if let Some(usage) = total_usage {
                let input_tokens = usage
                    .get("inputTokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let output_tokens = usage
                    .get("outputTokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let total = match usage.get("totalTokens").and_then(Value::as_u64) {
                    Some(total) => total,
                    None => input_tokens
                        .checked_add(output_tokens)
                        .ok_or_else(|| StreamLimitError::arithmetic("total token"))?,
                };
                final_chunk["usage"] = serde_json::json!({
                    "prompt_tokens": input_tokens,
                    "completion_tokens": output_tokens,
                    "total_tokens": total
                });
            }
            let json_str = serde_json::to_string(&final_chunk).unwrap_or_default();
            out.push(format!("data: {}\n\n", json_str));
        }
        "error" => {
            let err_val = event.get("error").or_else(|| event.get("message"));
            let err_str = err_val
                .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "unknown".to_string()))
                .unwrap_or_else(|| "unknown".to_string());
            // Text delta with error content
            let delta =
                serde_json::json!({"content": format!("\n\n[CommandCode error: {}]", err_str)});
            out.push(make_chunk_line(state, delta, None)?);
            // Final stop chunk
            out.push(make_chunk_line(state, serde_json::json!({}), Some("stop"))?);
            state.finish_reason = Some("stop".to_string());
        }
        _ => {}
    }

    Ok(out)
}

/// CommandCode response transform matching the registry's ResponseTransformFn.
/// Input: raw bytes (NDJSON lines like `{"type":"text-delta","text":"hello"}`)
/// Output: OpenAI SSE lines (`data: {"id":"...","object":"chat.completion.chunk",...}\n\n`)
pub fn commandcode_to_openai_response(
    chunk: &[u8],
    state: &mut ResponseTransformState,
) -> Vec<String> {
    let raw = String::from_utf8_lossy(chunk);
    let line = raw.trim();

    // Skip empty lines and [DONE] markers
    if line.is_empty() || line == "[DONE]" {
        return vec![];
    }

    // If the data is already an OpenAI chunk (passthrough from an upstream transform), forward it
    if let Ok(v) = serde_json::from_str::<Value>(line) {
        if v.get("object").and_then(|o| o.as_str()) == Some("chat.completion.chunk") {
            return vec![format!("data: {}\n\n", line)];
        }
    }

    // Parse the NDJSON event
    let event: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let event_type = event.get("type").and_then(|v| v.as_str());
    if event_type.is_none() {
        return vec![];
    }

    if matches!(event_type, Some("tool-input-start" | "tool-call")) {
        let Some(id) = event
            .get("id")
            .or_else(|| event.get("toolCallId"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            return state.fail(crate::core::translator::limits::StreamLimitError {
                code: "upstream_stream_invalid_tool_call",
                message: "CommandCode tool call is missing an id".to_string(),
            });
        };
        let Some(name) = event
            .get("toolName")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        else {
            return state.fail(crate::core::translator::limits::StreamLimitError {
                code: "upstream_stream_invalid_tool_call",
                message: "CommandCode tool call is missing a name".to_string(),
            });
        };
        let is_new = !state.commandcode.tool_index_by_id.contains_key(id);
        if is_new {
            let index = state.commandcode.tool_index;
            if let Err(error) = state.accumulation.track_tool(8, 0, index) {
                return state.fail(error);
            }
            if let Err(error) = state.accumulation.track_retained(id.len(), "tool metadata") {
                return state.fail(error);
            }
            if let Err(error) = state
                .accumulation
                .track_retained(name.len(), "tool metadata")
            {
                return state.fail(error);
            }
        }
        if event_type == Some("tool-call") && is_new {
            let Some(input) = event.get("input") else {
                return state.fail(StreamLimitError {
                    code: "upstream_stream_invalid_tool_call",
                    message: "CommandCode tool call is missing input".to_string(),
                });
            };
            let input_bytes = match input.as_str() {
                Some(input) => input.len(),
                None => match super::kiro_events::serialized_len(input) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return state.fail(StreamLimitError {
                            code: "upstream_stream_invalid_tool_call",
                            message: "CommandCode tool input could not be serialized".to_string(),
                        })
                    }
                },
            };
            let index = state.commandcode.tool_index;
            if let Err(error) = state
                .accumulation
                .track_tool_arguments(8, 0, index, input_bytes)
            {
                return state.fail(error);
            }
        }
    }

    if event_type == Some("tool-input-delta") {
        let Some(id) = event
            .get("id")
            .or_else(|| event.get("toolCallId"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return state.fail(StreamLimitError {
                code: "upstream_stream_invalid_tool_call",
                message: "CommandCode tool input delta is missing an id".to_string(),
            });
        };
        let Some(index) = state
            .commandcode
            .tool_index_by_id
            .get(id)
            .and_then(Value::as_u64)
        else {
            return state.fail(StreamLimitError {
                code: "upstream_stream_invalid_tool_call",
                message: "CommandCode tool input delta references an unknown tool".to_string(),
            });
        };
        let arguments = event
            .get("delta")
            .or_else(|| event.get("inputTextDelta"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if let Err(error) = state
            .accumulation
            .track_tool_arguments(8, 0, index, arguments.len())
        {
            return state.fail(error);
        }
    }

    // Initialize state on first meaningful event
    let cc_state = &mut state.commandcode;
    ensure_state_initialized(cc_state, event.get("model").and_then(|v| v.as_str()));

    match process_event(&event, cc_state) {
        Ok(output) => output,
        Err(error) => state.fail(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::translator::limits::MAX_STREAM_TOOL_ARGUMENT_BYTES;
    use serde_json::json;

    fn translate(event: Value, state: &mut ResponseTransformState) -> Vec<String> {
        commandcode_to_openai_response(&serde_json::to_vec(&event).unwrap(), state)
    }

    #[test]
    fn tool_input_deltas_are_cumulatively_bounded() {
        let mut state = ResponseTransformState::default();
        assert!(!translate(
            json!({"type":"tool-input-start","id":"call_1","toolName":"run"}),
            &mut state,
        )
        .is_empty());
        assert!(!translate(
            json!({"type":"tool-input-delta","id":"call_1","delta":"x".repeat(MAX_STREAM_TOOL_ARGUMENT_BYTES)}),
            &mut state,
        )
        .is_empty());
        assert!(translate(
            json!({"type":"tool-input-delta","id":"call_1","delta":"y"}),
            &mut state,
        )
        .is_empty());
        assert_eq!(state.failure.unwrap().code, "upstream_stream_state_limit");
    }

    #[test]
    fn complete_tool_input_is_bounded_before_output() {
        let mut state = ResponseTransformState::default();
        let output = translate(
            json!({
                "type":"tool-call",
                "toolCallId":"call_1",
                "toolName":"run",
                "input":"x".repeat(MAX_STREAM_TOOL_ARGUMENT_BYTES + 1)
            }),
            &mut state,
        );
        assert!(output.is_empty());
        assert_eq!(state.failure.unwrap().code, "upstream_stream_state_limit");
    }
}
