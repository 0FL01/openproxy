use serde_json::{json, Value};
use std::collections::HashMap;

use crate::core::translator::limits::{wire_index, StreamLimitError};

pub fn ollama_to_openai_response(
    chunk: &Value,
    state: &mut HashMap<String, Value>,
) -> Result<Option<Value>, StreamLimitError> {
    if !state.contains_key("id") {
        let id = format!("chatcmpl-{}", chrono::Utc::now().timestamp_millis());
        let model = chunk
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("ollama")
            .to_string();
        state.insert("id".to_string(), Value::String(id));
        state.insert("model".to_string(), Value::String(model));
        state.insert(
            "created".to_string(),
            Value::Number(chrono::Utc::now().timestamp().into()),
        );
    }

    let id = state
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let model = state
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("ollama")
        .to_string();
    let created = state.get("created").and_then(|v| v.as_i64()).unwrap_or(0);

    if chunk.get("done").and_then(|v| v.as_bool()).unwrap_or(false) {
        let prompt_tokens = chunk
            .get("prompt_eval_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let completion_tokens = chunk.get("eval_count").and_then(Value::as_u64).unwrap_or(0);
        let total_tokens = prompt_tokens
            .checked_add(completion_tokens)
            .ok_or_else(|| StreamLimitError::arithmetic("total token"))?;
        let usage = json!({
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": total_tokens
        });

        let mut finish_reason = "stop";
        if chunk.get("done_reason").and_then(|v| v.as_str()) == Some("tool_calls")
            || state
                .get("hadToolCalls")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        {
            finish_reason = "tool_calls";
        }

        // Extract tool_calls from the final chunk's message, if present.
        // Ollama may emit tool_calls only in the done=true chunk.
        let mut delta = serde_json::Map::new();
        if let Some(message) = chunk.get("message") {
            if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                if !tool_calls.is_empty() {
                    state.insert("hadToolCalls".to_string(), Value::Bool(true));
                    let converted = convert_tool_calls(tool_calls)?;
                    delta.insert("tool_calls".to_string(), Value::Array(converted));
                }
            }
        }

        return Ok(Some(serde_json::json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason
            }],
            "usage": usage
        })));
    }

    let Some(message) = chunk.get("message") else {
        return Ok(None);
    };

    let content = message
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let thinking = message
        .get("thinking")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_calls = message.get("tool_calls").and_then(|v| v.as_array());

    if content.is_empty() && thinking.is_empty() && tool_calls.is_none() {
        return Ok(None);
    }

    let mut delta = serde_json::Map::new();
    if !content.is_empty() {
        delta.insert("content".to_string(), Value::String(content.to_string()));
    }
    if !thinking.is_empty() {
        delta.insert(
            "reasoning_content".to_string(),
            Value::String(thinking.to_string()),
        );
    }
    if let Some(tool_calls_arr) = tool_calls {
        state.insert("hadToolCalls".to_string(), Value::Bool(true));
        let converted = convert_tool_calls(tool_calls_arr)?;
        delta.insert("tool_calls".to_string(), Value::Array(converted));
    }

    Ok(Some(serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": null
        }]
    })))
}

fn tool_fields(tool: &Value) -> Result<(u64, &str, &str, &Value), StreamLimitError> {
    let index = wire_index(
        tool.get("index")
            .or_else(|| tool.pointer("/function/index")),
        "tool_calls[].index",
    )?;
    let id = tool
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| StreamLimitError {
            code: "upstream_stream_invalid_tool_call",
            message: "Ollama tool call is missing an id".to_string(),
        })?;
    let name = tool
        .pointer("/function/name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| StreamLimitError {
            code: "upstream_stream_invalid_tool_call",
            message: "Ollama tool call is missing a function name".to_string(),
        })?;
    let arguments = tool
        .pointer("/function/arguments")
        .ok_or_else(|| StreamLimitError {
            code: "upstream_stream_invalid_tool_call",
            message: "Ollama tool call is missing arguments".to_string(),
        })?;
    Ok((index, id, name, arguments))
}

fn argument_string(arguments: &Value) -> Result<String, StreamLimitError> {
    match arguments.as_str() {
        Some(arguments) => Ok(arguments.to_string()),
        None => serde_json::to_string(arguments).map_err(|_| StreamLimitError {
            code: "upstream_stream_invalid_tool_call",
            message: "Ollama tool arguments could not be serialized".to_string(),
        }),
    }
}

fn convert_tool_calls(tool_calls: &[Value]) -> Result<Vec<Value>, StreamLimitError> {
    let mut converted = Vec::new();
    converted
        .try_reserve(tool_calls.len())
        .map_err(|_| StreamLimitError::capacity("tool calls"))?;
    for tool in tool_calls {
        let (index, id, name, arguments) = tool_fields(tool)?;
        let arguments = argument_string(arguments)?;
        converted.push(json!({
            "index": index,
            "id": id,
            "type": "function",
            "function": { "name": name, "arguments": arguments }
        }));
    }
    Ok(converted)
}

use crate::core::translator::registry::ResponseTransformState;

/// Registry-compatible streaming wrapper.
/// Signature matches `registry::ResponseTransformFn`.
pub fn ollama_to_openai_streaming(chunk: &[u8], state: &mut ResponseTransformState) -> Vec<String> {
    let val: serde_json::Value = match serde_json::from_slice(chunk) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    if let Some(tool_calls) = val.pointer("/message/tool_calls").and_then(Value::as_array) {
        for tool in tool_calls {
            let (index, id, name, arguments) = match tool_fields(tool) {
                Ok(fields) => fields,
                Err(error) => return state.fail(error),
            };
            if let Err(error) = state.accumulation.track_tool(10, 0, index) {
                return state.fail(error);
            }
            let argument_bytes = match arguments.as_str() {
                Some(arguments) => arguments.len(),
                None => match super::serialized_len(arguments) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return state.fail(StreamLimitError {
                            code: "upstream_stream_invalid_tool_call",
                            message: "Ollama tool arguments could not be serialized".to_string(),
                        })
                    }
                },
            };
            if let Err(error) =
                state
                    .accumulation
                    .track_tool_arguments(10, 0, index, argument_bytes)
            {
                return state.fail(error);
            }
            let metadata_bytes = match id.len().checked_add(name.len()) {
                Some(bytes) => bytes,
                None => return state.fail(StreamLimitError::arithmetic("tool metadata")),
            };
            if let Err(error) = state
                .accumulation
                .track_retained(metadata_bytes, "tool metadata")
            {
                return state.fail(error);
            }
        }
    }
    let inner = &mut state.ollama.state;
    match ollama_to_openai_response(&val, inner) {
        Ok(Some(v)) => vec![format!(
            "data: {}\n\n",
            serde_json::to_string(&v).unwrap_or_default()
        )],
        Ok(None) => vec![],
        Err(error) => state.fail(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::translator::limits::{
        MAX_STREAM_TOOL_ARGUMENT_BYTES, MAX_STREAM_TOOL_CALLS, MAX_STREAM_WIRE_INDEX,
    };

    fn tool(index: u64, arguments: Value) -> Value {
        json!({
            "model": "qwen",
            "message": {"tool_calls": [{
                "index": index,
                "id": format!("call_{index}"),
                "function": {"name": "run", "arguments": arguments}
            }]}
        })
    }

    #[test]
    fn registry_path_bounds_indices_arguments_and_identity() {
        let mut state = ResponseTransformState::default();
        assert!(!ollama_to_openai_streaming(
            &serde_json::to_vec(&tool(MAX_STREAM_WIRE_INDEX, json!({"x": 1}))).unwrap(),
            &mut state,
        )
        .is_empty());

        for bad in [MAX_STREAM_WIRE_INDEX + 1, 1_000_000_000, u64::MAX] {
            let mut state = ResponseTransformState::default();
            assert!(ollama_to_openai_streaming(
                &serde_json::to_vec(&tool(bad, json!({}))).unwrap(),
                &mut state,
            )
            .is_empty());
            assert!(state.failure.is_some());
        }

        let mut state = ResponseTransformState::default();
        let oversized = tool(
            0,
            Value::String("x".repeat(MAX_STREAM_TOOL_ARGUMENT_BYTES + 1)),
        );
        assert!(
            ollama_to_openai_streaming(&serde_json::to_vec(&oversized).unwrap(), &mut state)
                .is_empty()
        );
        assert_eq!(state.failure.unwrap().code, "upstream_stream_state_limit");

        let mut missing = tool(0, json!({}));
        missing["message"]["tool_calls"][0]
            .as_object_mut()
            .unwrap()
            .remove("id");
        let mut state = ResponseTransformState::default();
        assert!(
            ollama_to_openai_streaming(&serde_json::to_vec(&missing).unwrap(), &mut state)
                .is_empty()
        );
        assert_eq!(
            state.failure.unwrap().code,
            "upstream_stream_invalid_tool_call"
        );
    }

    #[test]
    fn registry_path_rejects_tool_call_129() {
        let mut state = ResponseTransformState::default();
        for index in 0..=MAX_STREAM_TOOL_CALLS {
            let output = ollama_to_openai_streaming(
                &serde_json::to_vec(&tool(index as u64, json!({}))).unwrap(),
                &mut state,
            );
            if index < MAX_STREAM_TOOL_CALLS {
                assert!(!output.is_empty());
            } else {
                assert!(output.is_empty());
            }
        }
        assert_eq!(state.failure.unwrap().code, "upstream_stream_state_limit");
    }
}
