use serde_json::Value;
use std::collections::HashMap;

use crate::core::translator::limits::StreamLimitError;

pub fn gemini_to_openai_response(
    chunk: &Value,
    state: &mut HashMap<String, Value>,
) -> Result<Vec<Value>, StreamLimitError> {
    let mut results = Vec::new();

    let response = chunk.get("response").unwrap_or(chunk);
    let Some(candidates) = response.get("candidates").and_then(|v| v.as_array()) else {
        return Ok(results);
    };
    let Some(candidate) = candidates.first() else {
        return Ok(results);
    };

    if !state.contains_key("messageId") {
        let msg_id = response
            .get("responseId")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let model = response
            .get("modelVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("gemini")
            .to_string();
        state.insert("messageId".to_string(), Value::String(msg_id.clone()));
        state.insert("model".to_string(), Value::String(model.clone()));
        state.insert("functionIndex".to_string(), Value::Number(0.into()));

        results.push(serde_json::json!({
            "id": format!("chatcmpl-{}", msg_id),
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant" },
                "finish_reason": null
            }]
        }));
    }

    let msg_id = state
        .get("messageId")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let model = state
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("gemini")
        .to_string();
    let mut func_idx = state
        .get("functionIndex")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if let Some(content) = candidate.get("content") {
        if let Some(parts) = content.get("parts").and_then(|v| v.as_array()) {
            for part in parts {
                let is_thought = part
                    .get("thought")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let has_thought_sig = part
                    .get("thoughtSignature")
                    .or_else(|| part.get("thought_signature"))
                    .is_some();

                if is_thought || has_thought_sig {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            let delta_key = if is_thought {
                                "reasoning_content"
                            } else {
                                "content"
                            };
                            let mut delta = serde_json::Map::new();
                            delta.insert(delta_key.to_string(), Value::String(text.to_string()));
                            results.push(serde_json::json!({
                                "id": format!("chatcmpl-{}", msg_id),
                                "object": "chat.completion.chunk",
                                "created": chrono::Utc::now().timestamp(),
                                "model": model,
                                "choices": [{
                                    "index": 0,
                                    "delta": delta,
                                    "finish_reason": null
                                }]
                            }));
                        }
                    }

                    if let Some(func_call) = part.get("functionCall") {
                        let raw_name = function_name(func_call)?;
                        let fc_args = func_call.get("args").cloned().unwrap_or(Value::Null);
                        let serialized_args =
                            serde_json::to_string(&fc_args).map_err(|_| StreamLimitError {
                                code: "upstream_stream_invalid_tool_call",
                                message: "Gemini function arguments could not be serialized"
                                    .to_string(),
                            })?;
                        let tool_call_id = format!(
                            "{}-{}-{}",
                            raw_name,
                            chrono::Utc::now().timestamp_millis(),
                            func_idx
                        );
                        let tool_call = serde_json::json!({
                            "id": tool_call_id,
                            "index": func_idx,
                            "type": "function",
                            "function": {
                                "name": raw_name,
                                "arguments": serialized_args
                            }
                        });
                        func_idx = func_idx
                            .checked_add(1)
                            .ok_or_else(|| StreamLimitError::arithmetic("tool index"))?;
                        results.push(serde_json::json!({
                            "id": format!("chatcmpl-{}", msg_id),
                            "object": "chat.completion.chunk",
                            "created": chrono::Utc::now().timestamp(),
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": { "tool_calls": [tool_call] },
                                "finish_reason": null
                            }]
                        }));
                    }
                    continue;
                }

                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        results.push(serde_json::json!({
                            "id": format!("chatcmpl-{}", msg_id),
                            "object": "chat.completion.chunk",
                            "created": chrono::Utc::now().timestamp(),
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "delta": { "content": text },
                                "finish_reason": null
                            }]
                        }));
                    }
                }

                if let Some(func_call) = part.get("functionCall") {
                    let raw_name = function_name(func_call)?;
                    let fc_args = func_call.get("args").cloned().unwrap_or(Value::Null);
                    let serialized_args =
                        serde_json::to_string(&fc_args).map_err(|_| StreamLimitError {
                            code: "upstream_stream_invalid_tool_call",
                            message: "Gemini function arguments could not be serialized"
                                .to_string(),
                        })?;
                    let tool_call_id = format!(
                        "{}-{}-{}",
                        raw_name,
                        chrono::Utc::now().timestamp_millis(),
                        func_idx
                    );
                    let tool_call = serde_json::json!({
                        "id": tool_call_id,
                        "index": func_idx,
                        "type": "function",
                        "function": {
                            "name": raw_name,
                            "arguments": serialized_args
                        }
                    });
                    func_idx = func_idx
                        .checked_add(1)
                        .ok_or_else(|| StreamLimitError::arithmetic("tool index"))?;
                    results.push(serde_json::json!({
                        "id": format!("chatcmpl-{}", msg_id),
                        "object": "chat.completion.chunk",
                        "created": chrono::Utc::now().timestamp(),
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": { "tool_calls": [tool_call] },
                            "finish_reason": null
                        }]
                    }));
                }
            }
        }
    }

    state.insert("functionIndex".to_string(), Value::Number(func_idx.into()));

    if state
        .get("emitted_done")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Ok(results);
    }

    if let Some(finish_reason) = candidate.get("finishReason").and_then(|v| v.as_str()) {
        state.insert("emitted_done".to_string(), Value::Bool(true));

        let mut fr = finish_reason.to_lowercase();
        if fr == "stop" && func_idx > 0 {
            fr = "tool_calls".to_string();
        }
        let mut final_chunk = serde_json::json!({
            "id": format!("chatcmpl-{}", msg_id),
            "object": "chat.completion.chunk",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": fr
            }]
        });

        if let Some(usage_meta) = response
            .get("usageMetadata")
            .or_else(|| chunk.get("usageMetadata"))
        {
            if let Some(usage_obj) = usage_meta.as_object() {
                let prompt_tokens = usage_obj
                    .get("promptTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let thoughts_tokens = usage_obj
                    .get("thoughtsTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let mut candidates_tokens = usage_obj
                    .get("candidatesTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let total_tokens = usage_obj
                    .get("totalTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                if candidates_tokens == 0 && total_tokens > 0 {
                    candidates_tokens = total_tokens
                        .saturating_sub(prompt_tokens)
                        .saturating_sub(thoughts_tokens);
                }
                let completion_tokens = candidates_tokens
                    .checked_add(thoughts_tokens)
                    .ok_or_else(|| StreamLimitError::arithmetic("completion token"))?;

                let mut usage = serde_json::json!({
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": total_tokens
                });

                if let Some(cached) = usage_obj
                    .get("cachedContentTokenCount")
                    .and_then(|v| v.as_u64())
                {
                    if cached > 0 {
                        usage["prompt_tokens_details"] =
                            serde_json::json!({ "cached_tokens": cached });
                    }
                }
                if thoughts_tokens > 0 {
                    usage["completion_tokens_details"] =
                        serde_json::json!({ "reasoning_tokens": thoughts_tokens });
                }
                final_chunk["usage"] = usage;
            }
        }

        results.push(final_chunk);
    }

    Ok(results)
}

fn function_name(function_call: &Value) -> Result<&str, StreamLimitError> {
    function_call
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| StreamLimitError {
            code: "upstream_stream_invalid_tool_call",
            message: "Gemini function call is missing a name".to_string(),
        })
}

/// Registry-compatible wrapper: parses raw bytes, calls the typed
/// `gemini_to_openai_response`, and serialises results back to SSE lines.
///
/// Signature matches `registry::ResponseTransformFn`.
pub fn gemini_to_openai_streaming(
    chunk: &[u8],
    state: &mut crate::core::translator::registry::ResponseTransformState,
) -> Vec<String> {
    let val: Value = match serde_json::from_slice(chunk) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let mut next_index = state
        .gemini
        .gemini_state
        .get("functionIndex")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if let Err(error) = state.accumulation.track_choice(7, 0) {
        return state.fail(error);
    }
    let response = val.get("response").unwrap_or(&val);
    if let Some(parts) = response
        .pointer("/candidates/0/content/parts")
        .and_then(Value::as_array)
    {
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if let Err(error) = state.accumulation.track_retained(text.len(), "Gemini text") {
                    return state.fail(error);
                }
            }
            if let Some(function_call) = part.get("functionCall") {
                let name = match function_name(function_call) {
                    Ok(name) => name,
                    Err(error) => return state.fail(error),
                };
                if let Err(error) = state.accumulation.track_tool(7, 0, next_index) {
                    return state.fail(error);
                }
                let arguments = function_call.get("args").unwrap_or(&Value::Null);
                let argument_bytes = match super::serialized_len(arguments) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return state.fail(StreamLimitError {
                            code: "upstream_stream_invalid_tool_call",
                            message: "Gemini function arguments could not be serialized"
                                .to_string(),
                        })
                    }
                };
                if let Err(error) =
                    state
                        .accumulation
                        .track_tool_arguments(7, 0, next_index, argument_bytes)
                {
                    return state.fail(error);
                }
                if let Err(error) = state
                    .accumulation
                    .track_retained(name.len(), "tool metadata")
                {
                    return state.fail(error);
                }
                next_index = match next_index.checked_add(1) {
                    Some(index) => index,
                    None => {
                        return state.fail(
                            crate::core::translator::limits::StreamLimitError::arithmetic(
                                "tool index",
                            ),
                        )
                    }
                };
            }
        }
    }
    let inner = &mut state.gemini.gemini_state;
    let results = match gemini_to_openai_response(&val, inner) {
        Ok(results) => results,
        Err(error) => return state.fail(error),
    };
    results
        .into_iter()
        .map(|v| {
            format!(
                "data: {}\n\n",
                serde_json::to_string(&v).unwrap_or_default()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::translator::limits::{
        MAX_STREAM_ACCUMULATED_BYTES, MAX_STREAM_TOOL_ARGUMENT_BYTES,
    };
    use crate::core::translator::registry::ResponseTransformState;
    use serde_json::json;

    fn function_chunk(arguments: String) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "response": {
                "responseId": "gemini-c31",
                "modelVersion": "gemini-test",
                "candidates": [{"content": {"parts": [{
                    "functionCall": {"name": "run", "args": arguments}
                }]}}]
            }
        }))
        .unwrap()
    }

    #[test]
    fn registry_path_accounts_serialized_arguments_before_output() {
        let mut state = ResponseTransformState::default();
        let exact = "x".repeat(MAX_STREAM_TOOL_ARGUMENT_BYTES - 2);
        assert!(!gemini_to_openai_streaming(&function_chunk(exact), &mut state).is_empty());
        assert!(state.failure.is_none());

        let mut state = ResponseTransformState::default();
        let oversized = "x".repeat(MAX_STREAM_TOOL_ARGUMENT_BYTES - 1);
        assert!(gemini_to_openai_streaming(&function_chunk(oversized), &mut state).is_empty());
        assert_eq!(state.failure.unwrap().code, "upstream_stream_state_limit");
    }

    #[test]
    fn registry_path_accounts_text_before_output() {
        let chunk = serde_json::to_vec(&json!({
            "response": {"candidates": [{"content": {"parts": [{
                "text": "x".repeat(MAX_STREAM_ACCUMULATED_BYTES + 1)
            }]}}]}
        }))
        .unwrap();
        let mut state = ResponseTransformState::default();
        assert!(gemini_to_openai_streaming(&chunk, &mut state).is_empty());
        assert_eq!(state.failure.unwrap().code, "upstream_stream_state_limit");
    }
}
