use serde_json::Value;
use std::collections::HashMap;

use crate::core::translator::limits::StreamLimitError;

pub fn kiro_to_openai_response(
    chunk: &Value,
    state: &mut HashMap<String, Value>,
) -> Result<Option<Value>, StreamLimitError> {
    if chunk.get("object").and_then(|v| v.as_str()) == Some("chat.completion.chunk")
        && chunk.get("choices").is_some()
    {
        return Ok(Some(chunk.clone()));
    }

    if !state.contains_key("responseId") {
        state.insert(
            "responseId".to_string(),
            Value::String(format!(
                "chatcmpl-{}",
                chrono::Utc::now().timestamp_millis()
            )),
        );
        state.insert(
            "created".to_string(),
            Value::Number(chrono::Utc::now().timestamp().into()),
        );
        state.insert("chunkIndex".to_string(), Value::Number(0usize.into()));
        state.insert("toolIndex".to_string(), Value::Number(0usize.into()));
    }

    let response_id = state
        .get("responseId")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let created = state.get("created").and_then(|v| v.as_i64()).unwrap_or(0);
    let chunk_idx = state
        .get("chunkIndex")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let model = state
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("kiro")
        .to_string();

    let event_type = chunk
        .get("_eventType")
        .or_else(|| chunk.get("event"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if event_type == "assistantResponseEvent" || chunk.get("assistantResponseEvent").is_some() {
        let content = chunk
            .get("assistantResponseEvent")
            .and_then(|v| v.get("content"))
            .or_else(|| chunk.get("content"))
            .and_then(|v| v.as_str());
        let Some(content) = content else {
            return Ok(None);
        };

        let mut delta = serde_json::Map::new();
        if chunk_idx == 0 {
            delta.insert("role".to_string(), Value::String("assistant".to_string()));
        }
        delta.insert("content".to_string(), Value::String(content.to_string()));

        state.insert(
            "chunkIndex".to_string(),
            Value::Number(
                chunk_idx
                    .checked_add(1)
                    .ok_or_else(|| StreamLimitError::arithmetic("chunk index"))?
                    .into(),
            ),
        );
        return Ok(Some(serde_json::json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": null
            }]
        })));
    }

    if event_type == "reasoningContentEvent" || chunk.get("reasoningContentEvent").is_some() {
        let reasoning = chunk.get("reasoningContentEvent").unwrap_or(chunk);
        let content = reasoning
            .get("text")
            .or_else(|| reasoning.get("content"))
            .and_then(|v| v.as_str())
            .or_else(|| chunk.get("content").and_then(|v| v.as_str()));
        let Some(content) = content else {
            return Ok(None);
        };

        let mut delta = serde_json::Map::new();
        if chunk_idx == 0 {
            delta.insert("role".to_string(), Value::String("assistant".to_string()));
        }
        delta.insert(
            "reasoning_content".to_string(),
            Value::String(content.to_string()),
        );

        state.insert(
            "chunkIndex".to_string(),
            Value::Number(
                chunk_idx
                    .checked_add(1)
                    .ok_or_else(|| StreamLimitError::arithmetic("chunk index"))?
                    .into(),
            ),
        );
        return Ok(Some(serde_json::json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": null
            }]
        })));
    }

    if event_type == "toolUseEvent" || chunk.get("toolUseEvent").is_some() {
        let tool_use = chunk.get("toolUseEvent").unwrap_or(chunk);
        let tool_call_id = tool_use
            .get("toolUseId")
            .and_then(|v| v.as_str())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| StreamLimitError {
                code: "upstream_stream_invalid_tool_call",
                message: "Kiro tool use is missing an id".to_string(),
            })?
            .to_string();
        let tool_name = tool_use
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| StreamLimitError {
                code: "upstream_stream_invalid_tool_call",
                message: "Kiro tool use is missing a name".to_string(),
            })?
            .to_string();
        let tool_input = tool_use
            .get("input")
            .cloned()
            .ok_or_else(|| StreamLimitError {
                code: "upstream_stream_invalid_tool_call",
                message: "Kiro tool use is missing input".to_string(),
            })?;
        let arguments = serde_json::to_string(&tool_input).map_err(|_| StreamLimitError {
            code: "upstream_stream_invalid_tool_call",
            message: "Kiro tool input could not be serialized".to_string(),
        })?;
        let tool_index = state.get("toolIndex").and_then(Value::as_u64).unwrap_or(0);

        let mut delta = serde_json::Map::new();
        if chunk_idx == 0 {
            delta.insert("role".to_string(), Value::String("assistant".to_string()));
        }
        delta.insert(
            "tool_calls".to_string(),
            serde_json::json!([{
                "index": tool_index,
                "id": tool_call_id,
                "type": "function",
                "function": {
                    "name": tool_name,
                    "arguments": arguments
                }
            }]),
        );

        state.insert(
            "chunkIndex".to_string(),
            Value::Number(
                chunk_idx
                    .checked_add(1)
                    .ok_or_else(|| StreamLimitError::arithmetic("chunk index"))?
                    .into(),
            ),
        );
        state.insert(
            "toolIndex".to_string(),
            Value::Number(
                tool_index
                    .checked_add(1)
                    .ok_or_else(|| StreamLimitError::arithmetic("tool index"))?
                    .into(),
            ),
        );
        return Ok(Some(serde_json::json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": null
            }]
        })));
    }

    if event_type == "messageStopEvent"
        || event_type == "done"
        || chunk.get("messageStopEvent").is_some()
    {
        state.insert(
            "finishReason".to_string(),
            Value::String("stop".to_string()),
        );
        let mut final_chunk = serde_json::json!({
            "id": response_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        });
        if let Some(usage) = state.get("usage") {
            final_chunk["usage"] = usage.clone();
        }
        return Ok(Some(final_chunk));
    }

    if event_type == "usageEvent" || chunk.get("usageEvent").is_some() {
        let usage = chunk.get("usageEvent").unwrap_or(chunk);
        if let Some(usage_obj) = usage.as_object() {
            let input_tokens = usage_obj
                .get("inputTokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let output_tokens = usage_obj
                .get("outputTokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            state.insert(
                "usage".to_string(),
                serde_json::json!({
                    "prompt_tokens": input_tokens,
                    "completion_tokens": output_tokens,
                    "total_tokens": input_tokens.checked_add(output_tokens)
                        .ok_or_else(|| StreamLimitError::arithmetic("total token"))?
                }),
            );
        }
        return Ok(None);
    }

    Ok(None)
}

use crate::core::translator::registry::ResponseTransformState;

/// Registry-compatible streaming wrapper.
/// Signature matches `registry::ResponseTransformFn`.
///
/// Kiro upstream returns AWS EventStream v1 binary. We buffer bytes across
/// chunks, decode complete frames via `EventStreamDecoder`, and assemble
/// OpenAI `chat.completion.chunk` SSE via `KiroSseAssembler`.
/// Falls back to the legacy JSON path for callers that already hand us
/// decoded JSON events (e.g. tests, or a provider that skipped EventStream).
pub fn kiro_to_openai_streaming(chunk: &[u8], state: &mut ResponseTransformState) -> Vec<String> {
    if state.kiro.stream_failed {
        return Vec::new();
    }

    // Legacy JSON path: if the chunk parses as JSON with an _eventType or
    // chat.completion.chunk, use kiro_to_openai_response.
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(chunk) {
        if val.get("_eventType").is_some()
            || val.get("event").is_some()
            || val.get("object").and_then(|v| v.as_str()) == Some("chat.completion.chunk")
        {
            if val
                .get("_eventType")
                .or_else(|| val.get("event"))
                .and_then(Value::as_str)
                == Some("toolUseEvent")
                || val.get("toolUseEvent").is_some()
            {
                let tool = val.get("toolUseEvent").unwrap_or(&val);
                let Some(id) = tool
                    .get("toolUseId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                else {
                    return state.fail(crate::core::translator::limits::StreamLimitError {
                        code: "upstream_stream_invalid_tool_call",
                        message: "Kiro tool use is missing an id".to_string(),
                    });
                };
                let Some(name) = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                else {
                    return state.fail(StreamLimitError {
                        code: "upstream_stream_invalid_tool_call",
                        message: "Kiro tool use is missing a name".to_string(),
                    });
                };
                let Some(input) = tool.get("input") else {
                    return state.fail(StreamLimitError {
                        code: "upstream_stream_invalid_tool_call",
                        message: "Kiro tool use is missing input".to_string(),
                    });
                };
                let index = state
                    .kiro
                    .state
                    .get("toolIndex")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if let Err(error) = state.accumulation.track_tool(9, 0, index) {
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
                let argument_bytes = match super::kiro_events::serialized_len(input) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return state.fail(StreamLimitError {
                            code: "upstream_stream_invalid_tool_call",
                            message: "Kiro tool input could not be serialized".to_string(),
                        })
                    }
                };
                if let Err(error) =
                    state
                        .accumulation
                        .track_tool_arguments(9, 0, index, argument_bytes)
                {
                    return state.fail(error);
                }
            }
            let inner = &mut state.kiro.state;
            return match kiro_to_openai_response(&val, inner) {
                Ok(Some(v)) => vec![format!(
                    "data: {}\n\n",
                    serde_json::to_string(&v).unwrap_or_default()
                )],
                Ok(None) => vec![],
                Err(error) => state.fail(error),
            };
        }
    }

    // Binary EventStream path.
    let buffer = &mut state.kiro.event_buffer;
    buffer.extend_from_slice(chunk);

    let events = match crate::core::executor::EventStreamDecoder::decode_chunk(buffer) {
        Ok(events) => events,
        Err(error) => {
            buffer.clear();
            state.kiro.stream_failed = true;
            return vec![
                format!(
                    "data: {}\n\n",
                    serde_json::json!({
                        "error": {
                            "message": format!("Invalid Kiro EventStream frame: {error:?}"),
                            "type": "upstream_error",
                            "code": "kiro_eventstream_decode_error"
                        }
                    })
                ),
                "data: [DONE]\n\n".to_string(),
            ];
        }
    };
    if events.is_empty() {
        return vec![];
    }

    // Buffer any trailing partial frame for the next chunk.
    let consumed = crate::core::executor::consumed_eventstream_bytes(buffer);
    if consumed < buffer.len() {
        let rest = buffer.split_off(consumed);
        buffer.clear();
        buffer.extend_from_slice(&rest);
    } else {
        buffer.clear();
    }

    // Lazily create the assembler on first event.
    if state.kiro.assembler.is_none() {
        let model = state
            .kiro
            .state
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("kiro")
            .to_string();
        state.kiro.assembler = Some(super::kiro_events::KiroSseAssembler::new(&model));
    }

    let mut out = Vec::new();
    for event in &events {
        if event.message_type == "error" || event.message_type == "exception" {
            // Emit an SSE error chunk mirroring 9router's upstream_error.
            out.push(format!(
                "data: {}\n\n",
                serde_json::json!({
                    "error": {
                        "message": event.payload
                            .as_ref()
                            .and_then(|p| p.get("message"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("Kiro upstream sent an EventStream error"),
                        "type": "upstream_error",
                        "code": "kiro_upstream_eventstream_error"
                    }
                })
            ));
            out.push("data: [DONE]\n\n".to_string());
            state.kiro.stream_failed = true;
            continue;
        }
        // Delegate to the assembler (per-event).
        if let Some(assembler) = state.kiro.assembler.as_mut() {
            match assembler.process_event(event) {
                Ok(chunks) => {
                    for c in chunks {
                        out.push(format!(
                            "data: {}\n\n",
                            serde_json::to_string(&c).unwrap_or_default()
                        ));
                    }
                }
                Err(msg) => {
                    out.push(format!(
                        "data: {}\n\n",
                        serde_json::json!({
                            "error": { "message": msg, "type": "upstream_error", "code": "kiro_event_parse_error" }
                        })
                    ));
                    out.push("data: [DONE]\n\n".to_string());
                    state.kiro.stream_failed = true;
                    break;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::translator::limits::{
        MAX_STREAM_ACCUMULATED_BYTES, MAX_STREAM_TOOL_ARGUMENT_BYTES,
    };
    use serde_json::json;

    fn legacy_tool(name: &str, input: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "_eventType": "toolUseEvent",
            "toolUseEvent": {"toolUseId": "tool_1", "name": name, "input": input}
        }))
        .unwrap()
    }

    #[test]
    fn legacy_json_path_bounds_arguments_and_name_state() {
        let mut state = ResponseTransformState::default();
        let oversized = legacy_tool(
            "run",
            Value::String("x".repeat(MAX_STREAM_TOOL_ARGUMENT_BYTES - 1)),
        );
        assert!(kiro_to_openai_streaming(&oversized, &mut state).is_empty());
        assert_eq!(state.failure.unwrap().code, "upstream_stream_state_limit");

        let mut state = ResponseTransformState::default();
        state
            .accumulation
            .track_retained(MAX_STREAM_ACCUMULATED_BYTES - 1, "test prefill")
            .unwrap();
        assert!(kiro_to_openai_streaming(&legacy_tool("xx", json!({})), &mut state,).is_empty());
        assert_eq!(state.failure.unwrap().code, "upstream_stream_state_limit");
    }

    #[test]
    fn legacy_json_path_reports_counter_overflow() {
        let mut state = ResponseTransformState::default();
        state
            .kiro
            .state
            .insert("responseId".to_string(), Value::String("r".to_string()));
        state
            .kiro
            .state
            .insert("chunkIndex".to_string(), Value::from(u64::MAX));
        let chunk = serde_json::to_vec(&json!({
            "_eventType": "assistantResponseEvent",
            "content": "x"
        }))
        .unwrap();
        assert!(kiro_to_openai_streaming(&chunk, &mut state).is_empty());
        assert_eq!(
            state.failure.unwrap().code,
            "upstream_stream_arithmetic_overflow"
        );
    }
}
