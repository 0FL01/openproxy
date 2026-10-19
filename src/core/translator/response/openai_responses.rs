//! OpenAI Responses API response translator (both directions).

use serde_json::Value;

pub(crate) fn format_error_event(sequence_number: u64, code: &str, message: &str) -> String {
    // OpenCode recognises a late-stream refusal through the nested
    // `error.code`; the official Responses contract uses the flat fields.
    // Retain the canonical flat fields and add the nested object only for
    // the refusal code. Every other error stays byte-identical.
    let mut data = serde_json::json!({
        "type": "error",
        "sequence_number": sequence_number,
        "code": code,
        "message": message,
        "param": null,
    });
    if code == "invalid_prompt" {
        data["error"] = serde_json::json!({
            "type": "invalid_request_error",
            "code": code,
            "message": message,
            "param": null,
        });
    }
    format!("event: error\ndata: {data}\n\n")
}

/// Increment the running `seq` counter in the SSE state map and return the new value.
fn next_seq(state: &mut serde_json::Map<String, Value>) -> u64 {
    let current = state.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
    let Some(s) = current.checked_add(1) else {
        state.insert(
            "_streamArithmeticFailure".to_string(),
            Value::String("response sequence".to_string()),
        );
        return current;
    };
    state.insert("seq".to_string(), Value::Number(s.into()));
    s
}

fn take_arithmetic_failure(
    state: &mut serde_json::Map<String, Value>,
) -> Option<crate::core::translator::limits::StreamLimitError> {
    state
        .remove("_streamArithmeticFailure")
        .and_then(|value| value.as_str().map(str::to_string))
        .map(|kind| crate::core::translator::limits::StreamLimitError::arithmetic(&kind))
}

/// Look up a Responses item key (item.id / data.item_id) in the
/// item_id → chat tool_calls index map, returning the mapped index.
fn resp_tool_index(state: &serde_json::Map<String, Value>, key: Option<&str>) -> Option<u64> {
    let key = key.filter(|k| !k.is_empty())?;
    state
        .get("respToolChatIndex")
        .and_then(Value::as_object)
        .and_then(|map| map.get(key))
        .and_then(Value::as_u64)
}

/// Record an item key → chat tool_calls index mapping.
fn set_resp_tool_index(state: &mut serde_json::Map<String, Value>, key: &str, idx: u64) {
    if key.is_empty() {
        return;
    }
    let map = state
        .entry("respToolChatIndex".to_string())
        .or_insert_with(|| Value::Object(Default::default()));
    if let Some(obj) = map.as_object_mut() {
        obj.insert(key.to_string(), Value::Number(idx.into()));
    }
}

/// Mark a chat tool_calls index as having received argument deltas.
fn mark_resp_tool_args(state: &mut serde_json::Map<String, Value>, idx: u64) {
    let arr = state
        .entry("respToolArgsEmitted".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(list) = arr.as_array_mut() {
        if !list.iter().any(|v| v.as_u64() == Some(idx)) {
            list.push(Value::Number(idx.into()));
        }
    }
}

/// Check whether a chat tool_calls index already received argument deltas.
fn resp_tool_args_emitted(state: &serde_json::Map<String, Value>, idx: u64) -> bool {
    state
        .get("respToolArgsEmitted")
        .and_then(Value::as_array)
        .is_some_and(|list| list.iter().any(|v| v.as_u64() == Some(idx)))
}

fn track_responses_accumulation(
    state: &mut crate::core::translator::registry::ResponseTransformState,
    event: &Value,
) -> Result<(), crate::core::translator::limits::StreamLimitError> {
    use crate::core::translator::limits::StreamLimitError;

    let event_type = event
        .get("type")
        .or_else(|| event.get("event"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let data = event.get("data").unwrap_or(event);
    match event_type {
        "response.output_item.added" => {
            let item = data.get("item").unwrap_or(&Value::Null);
            if !matches!(
                item.get("type").and_then(Value::as_str),
                Some("function_call" | "custom_tool_call")
            ) {
                return Ok(());
            }
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| StreamLimitError {
                    code: "upstream_stream_invalid_tool_call",
                    message: "Responses tool call is missing call_id".to_string(),
                })?;
            if !item
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| !name.is_empty())
            {
                return Err(StreamLimitError {
                    code: "upstream_stream_invalid_tool_call",
                    message: "Responses tool call is missing a name".to_string(),
                });
            }
            let key = item
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| data.get("item_id").and_then(Value::as_str))
                .filter(|value| !value.is_empty())
                .ok_or_else(|| StreamLimitError {
                    code: "upstream_stream_invalid_tool_call",
                    message: "Responses tool call is missing item id".to_string(),
                })?;
            let index = resp_tool_index(&state.responses.state, Some(key)).unwrap_or_else(|| {
                state
                    .responses
                    .state
                    .get("toolCallIndex")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            });
            state.accumulation.track_tool(6, 0, index)?;
            state
                .accumulation
                .track_retained(call_id.len(), "tool metadata")?;
            state
                .accumulation
                .track_retained(key.len(), "tool metadata")?;
            if let Some(name) = item.get("name").and_then(Value::as_str) {
                state
                    .accumulation
                    .track_retained(name.len(), "tool metadata")?;
            }
        }
        "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
            let key = data
                .get("item_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| StreamLimitError {
                    code: "upstream_stream_invalid_tool_call",
                    message: "Responses tool arguments are missing item_id".to_string(),
                })?;
            if resp_tool_index(&state.responses.state, Some(key)).is_none() {
                return Err(StreamLimitError {
                    code: "upstream_stream_invalid_tool_call",
                    message: format!("Responses tool arguments reference unknown item {key}"),
                });
            }
        }
        "response.output_item.done" => {
            let item = data.get("item").unwrap_or(&Value::Null);
            if matches!(
                item.get("type").and_then(Value::as_str),
                Some("function_call" | "custom_tool_call")
            ) {
                let key = item
                    .get("id")
                    .and_then(Value::as_str)
                    .or_else(|| data.get("item_id").and_then(Value::as_str));
                if resp_tool_index(&state.responses.state, key).is_none() {
                    return Err(StreamLimitError {
                        code: "upstream_stream_invalid_tool_call",
                        message: "Responses tool completion references an unknown item".to_string(),
                    });
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Emit an SSE event into `events`, stamping `data.sequence_number` with the
/// next value from `state`.
fn emit(
    events: &mut Vec<Value>,
    state: &mut serde_json::Map<String, Value>,
    event_type: &str,
    data: Value,
) {
    let mut d = data;
    if let Some(obj) = d.as_object_mut() {
        obj.insert(
            "sequence_number".to_string(),
            Value::Number(next_seq(state).into()),
        );
    }
    events.push(serde_json::json!({"event": event_type, "data": d}));
}

/// Whether a tool name is a custom (freeform) tool, per translator-only
/// `_customToolNames` metadata threaded through streaming state (JS
/// `isCustomTool` in open-sse/translator/response/openai-responses.js:261).
fn is_custom_tool(state: &serde_json::Map<String, Value>, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let in_list = |key: &str| {
        state.get(key).and_then(|v| match v {
            Value::String(s) => Some(s.split(',').any(|n| n.trim() == name)),
            Value::Array(a) => Some(
                a.iter()
                    .filter_map(|n| n.as_str())
                    .any(|n| n.trim() == name),
            ),
            _ => None,
        })
    };
    in_list("customToolNames").unwrap_or(false)
}

/// Unwrap the Chat JSON wrapper Codex puts around custom-tool input:
/// `{"input":"..."}` → the freeform program. Falls back to the raw text for
/// incomplete fragments (JS `extractCustomToolInput`, same file:265-272).
fn extract_custom_tool_input(arguments_text: &str) -> String {
    if arguments_text.is_empty() {
        return String::new();
    }
    match serde_json::from_str::<Value>(arguments_text) {
        Ok(Value::Object(map)) => match map.get("input") {
            Some(Value::String(s)) => s.clone(),
            Some(_) => arguments_text.to_string(),
            None => arguments_text.to_string(),
        },
        _ => arguments_text.to_string(),
    }
}

/// C34: point-mutation helpers for Responses accumulators.
///
/// The previous implementation cloned whole `func*`/`msg*` maps on every
/// delta and rebuilt growing text via `format!("{old}{delta}")`, which copies
/// the retained prefix on each fragment (quadratic). These helpers mutate a
/// single map entry / string buffer in place so repeated small deltas append
/// in amortized linear time behind the existing C31 bounds.
fn state_obj_mut<'a>(
    state: &'a mut serde_json::Map<String, Value>,
    key: &str,
) -> &'a mut serde_json::Map<String, Value> {
    let slot = state
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Default::default()));
    if !slot.is_object() {
        *slot = Value::Object(Default::default());
    }
    slot.as_object_mut().expect("ensured object")
}

fn append_indexed_text(
    state: &mut serde_json::Map<String, Value>,
    map_key: &str,
    idx_str: &str,
    delta: &str,
) {
    if delta.is_empty() {
        return;
    }
    let map = state_obj_mut(state, map_key);
    let slot = map
        .entry(idx_str.to_string())
        .or_insert_with(|| Value::String(String::new()));
    match slot {
        Value::String(buf) => buf.push_str(delta),
        _ => *slot = Value::String(delta.to_string()),
    }
}

fn indexed_text(state: &serde_json::Map<String, Value>, map_key: &str, idx_str: &str) -> String {
    state
        .get(map_key)
        .and_then(Value::as_object)
        .and_then(|map| map.get(idx_str))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn indexed_flag(state: &serde_json::Map<String, Value>, map_key: &str, idx_str: &str) -> bool {
    state
        .get(map_key)
        .and_then(Value::as_object)
        .is_some_and(|map| map.contains_key(idx_str))
}

fn set_indexed_flag(state: &mut serde_json::Map<String, Value>, map_key: &str, idx_str: &str) {
    state_obj_mut(state, map_key).insert(idx_str.to_string(), Value::Bool(true));
}

fn take_indexed_text(
    state: &mut serde_json::Map<String, Value>,
    map_key: &str,
    idx_str: &str,
    fallback: &str,
) -> String {
    let text = state
        .get(map_key)
        .and_then(Value::as_object)
        .and_then(|map| map.get(idx_str))
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string();
    if let Some(Value::Object(map)) = state.get_mut(map_key) {
        map.remove(idx_str);
    }
    text
}

fn start_reasoning(state: &mut serde_json::Map<String, Value>, events: &mut Vec<Value>, idx: u64) {
    if state.get("reasoningId").is_none() || state["reasoningId"].is_null() {
        let reasoning_id = format!("rs_{}_{}", state["responseId"].as_str().unwrap_or(""), idx);
        state.insert(
            "reasoningId".to_string(),
            Value::String(reasoning_id.clone()),
        );
        state.insert("reasoningIndex".to_string(), Value::Number(idx.into()));
        emit(
            events,
            state,
            "response.output_item.added",
            serde_json::json!({
                "type": "response.output_item.added",
                "output_index": idx,
                "item": {"id": reasoning_id, "type": "reasoning", "summary": []}
            }),
        );
        emit(
            events,
            state,
            "response.reasoning_summary_part.added",
            serde_json::json!({
                "type": "response.reasoning_summary_part.added",
                "item_id": reasoning_id,
                "output_index": idx,
                "summary_index": 0,
                "part": {"type": "summary_text", "text": ""}
            }),
        );
        state.insert("reasoningPartAdded".to_string(), Value::Bool(true));
    }
}

fn emit_reasoning_delta(
    state: &mut serde_json::Map<String, Value>,
    events: &mut Vec<Value>,
    text: &str,
) {
    if text.is_empty() {
        return;
    }
    match state.get_mut("reasoningBuf") {
        Some(Value::String(buf)) => buf.push_str(text),
        _ => {
            state.insert("reasoningBuf".to_string(), Value::String(text.to_string()));
        }
    }
    let reasoning_id = state["reasoningId"].as_str().unwrap_or("").to_string();
    let reasoning_idx = state
        .get("reasoningIndex")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    emit(
        events,
        state,
        "response.reasoning_summary_text.delta",
        serde_json::json!({
            "type": "response.reasoning_summary_text.delta",
            "item_id": reasoning_id,
            "output_index": reasoning_idx,
            "summary_index": 0,
            "delta": text
        }),
    );
}

fn close_reasoning(state: &mut serde_json::Map<String, Value>, events: &mut Vec<Value>) {
    if state.get("reasoningId").and_then(|v| v.as_str()).is_some()
        && state.get("reasoningDone").and_then(|v| v.as_bool()) == Some(false)
    {
        state.insert("reasoningDone".to_string(), Value::Bool(true));
        let reasoning_id = state["reasoningId"].as_str().unwrap_or("").to_string();
        let reasoning_idx = state
            .get("reasoningIndex")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let reasoning_buf = state["reasoningBuf"].as_str().unwrap_or("").to_string();
        // C34: the full buffer is required for the done payloads below; drop
        // it right after so idle state does not retain completed reasoning.
        state.insert("reasoningBuf".to_string(), Value::String(String::new()));
        emit(
            events,
            state,
            "response.reasoning_summary_text.done",
            serde_json::json!({
                "type": "response.reasoning_summary_text.done",
                "item_id": reasoning_id,
                "output_index": reasoning_idx,
                "summary_index": 0,
                "text": reasoning_buf
            }),
        );
        emit(
            events,
            state,
            "response.reasoning_summary_part.done",
            serde_json::json!({
                "type": "response.reasoning_summary_part.done",
                "item_id": reasoning_id,
                "output_index": reasoning_idx,
                "summary_index": 0,
                "part": {"type": "summary_text", "text": reasoning_buf}
            }),
        );
        emit(
            events,
            state,
            "response.output_item.done",
            serde_json::json!({
                "type": "response.output_item.done",
                "output_index": reasoning_idx,
                "item": {
                    "id": reasoning_id,
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": reasoning_buf}]
                }
            }),
        );
    }
}

fn close_message(
    state: &mut serde_json::Map<String, Value>,
    events: &mut Vec<Value>,
    idx_key: &str,
) {
    if indexed_flag(state, "msgItemDone", idx_key) || !indexed_flag(state, "msgItemAdded", idx_key)
    {
        return;
    }
    set_indexed_flag(state, "msgItemDone", idx_key);
    let msg_key = format!("msgId_{}", idx_key);
    let msg_id = state
        .get(&msg_key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // C34: the retained text is required for the done payloads below; take it
    // out of the map so completed message state does not stay resident.
    let full_text = take_indexed_text(state, "msgTextBuf", idx_key, "");
    state.remove(&msg_key);
    let Ok(idx_num) = idx_key.parse::<u64>() else {
        return;
    };
    emit(
        events,
        state,
        "response.output_text.done",
        serde_json::json!({
            "type": "response.output_text.done",
            "item_id": msg_id,
            "output_index": idx_num,
            "content_index": 0,
            "text": full_text,
            "logprobs": []
        }),
    );
    emit(
        events,
        state,
        "response.content_part.done",
        serde_json::json!({
            "type": "response.content_part.done",
            "item_id": msg_id,
            "output_index": idx_num,
            "content_index": 0,
            "part": {"type": "output_text", "annotations": [], "logprobs": [], "text": full_text}
        }),
    );
    emit(
        events,
        state,
        "response.output_item.done",
        serde_json::json!({
            "type": "response.output_item.done",
            "output_index": idx_num,
            "item": {
                "id": msg_id,
                "type": "message",
                "content": [{"type": "output_text", "annotations": [], "logprobs": [], "text": full_text}],
                "role": "assistant"
            }
        }),
    );
}

fn close_tool_call(
    state: &mut serde_json::Map<String, Value>,
    events: &mut Vec<Value>,
    idx_key: &str,
) {
    let call_id = indexed_text(state, "funcCallIds", idx_key);
    if call_id.is_empty() {
        return;
    }
    if indexed_flag(state, "funcItemDone", idx_key) {
        return;
    }
    set_indexed_flag(state, "funcItemDone", idx_key);
    set_indexed_flag(state, "funcArgsDone", idx_key);
    // C34: full arguments are required once for the done payloads below; take
    // them out so completed tool state does not stay resident.
    let args = take_indexed_text(state, "funcArgsBuf", idx_key, "{}");
    let name = indexed_text(state, "funcNames", idx_key);
    let custom = is_custom_tool(state, &name);
    let idx_num: u64 = idx_key.parse().unwrap_or(0);
    if custom {
        let input = extract_custom_tool_input(&args);
        emit(
            events,
            state,
            "response.custom_tool_call_input.delta",
            serde_json::json!({
                "type": "response.custom_tool_call_input.delta",
                "item_id": format!("ctc_{}", call_id),
                "output_index": idx_num,
                "delta": input
            }),
        );
        emit(
            events,
            state,
            "response.custom_tool_call_input.done",
            serde_json::json!({
                "type": "response.custom_tool_call_input.done",
                "item_id": format!("ctc_{}", call_id),
                "output_index": idx_num,
                "input": input
            }),
        );
    } else {
        emit(
            events,
            state,
            "response.function_call_arguments.done",
            serde_json::json!({
                "type": "response.function_call_arguments.done",
                "item_id": format!("fc_{}", call_id),
                "output_index": idx_num,
                "arguments": args
            }),
        );
    }
    emit(
        events,
        state,
        "response.output_item.done",
        serde_json::json!({
            "type": "response.output_item.done",
            "output_index": idx_num,
            "item": if custom {
                serde_json::json!({
                    "id": format!("ctc_{}", call_id),
                    "type": "custom_tool_call",
                    "input": extract_custom_tool_input(&args),
                    "call_id": call_id,
                    "name": name,
                    "status": "completed"
                })
            } else {
                serde_json::json!({
                    "id": format!("fc_{}", call_id),
                    "type": "function_call",
                    "arguments": args,
                    "call_id": call_id,
                    "name": name,
                    "status": "completed"
                })
            }
        }),
    );
}

fn have_chat_usage(state: &serde_json::Map<String, Value>) -> bool {
    state.get("usage").is_some_and(|v| v.is_object())
}

/// Map original xAI Chat counters without inventing missing usage. xAI may
/// report completion_tokens exclusive of reasoning; only a checked equality
/// against the original total establishes when reasoning must be included.
pub(crate) fn xai_chat_usage_to_responses(usage: &Value) -> Option<Value> {
    let input = usage.get("prompt_tokens").and_then(Value::as_u64);
    let completion = usage.get("completion_tokens").and_then(Value::as_u64);
    let reasoning = usage
        .pointer("/completion_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64);
    let total = usage.get("total_tokens").and_then(Value::as_u64);
    let output = match (input, completion, reasoning, total) {
        (Some(p), Some(c), Some(r), Some(t)) if p.checked_add(c) != Some(t) => p
            .checked_add(c)
            .and_then(|sum| sum.checked_add(r))
            .filter(|sum| *sum == t)
            .and_then(|_| c.checked_add(r))
            .or(completion),
        _ => completion,
    };
    let mut mapped = serde_json::Map::new();
    for (key, value) in [
        ("input_tokens", input),
        ("output_tokens", output),
        ("total_tokens", total),
    ] {
        if let Some(value) = value {
            mapped.insert(key.to_string(), Value::from(value));
        }
    }
    if let Some(cached) = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .filter(|cached| input.is_none_or(|input| *cached <= input))
    {
        mapped.insert(
            "input_tokens_details".to_string(),
            serde_json::json!({"cached_tokens": cached}),
        );
    }
    if let Some(reasoning) = reasoning.filter(|r| output.is_none_or(|output| *r <= output)) {
        mapped.insert(
            "output_tokens_details".to_string(),
            serde_json::json!({"reasoning_tokens": reasoning}),
        );
    }
    (!mapped.is_empty()).then_some(Value::Object(mapped))
}

fn is_xai_chat(state: &serde_json::Map<String, Value>) -> bool {
    state.get("_xaiChat").and_then(Value::as_bool) == Some(true)
}

/// All observed choices must have a recognized generation finish before the
/// transport may arm its EOF budget.
pub(crate) fn xai_chat_terminal_ready(state: &ResponseTransformState) -> bool {
    state.xai_chat
        && state.failure.is_none()
        && state
            .responses
            .state
            .get("_xaiChoices")
            .and_then(Value::as_object)
            .is_some_and(|choices| {
                !choices.is_empty() && choices.values().all(|v| v.as_bool() == Some(true))
            })
}

fn observe_xai_chat(
    state: &mut ResponseTransformState,
    chunk: &Value,
) -> Result<(), crate::core::translator::limits::StreamLimitError> {
    use crate::core::translator::limits::{wire_index, StreamLimitError};

    if chunk.get("error").is_some_and(|error| !error.is_null())
        || matches!(
            chunk.get("type").and_then(Value::as_str),
            Some("error" | "response.failed" | "response.incomplete")
        )
        || chunk.get("object").and_then(Value::as_str) == Some("error")
    {
        return Err(StreamLimitError {
            code: "upstream_stream_failed",
            message: "Upstream xAI Chat stream reported a protocol error".to_string(),
        });
    }
    if chunk
        .get("choices")
        .is_some_and(|choices| !choices.is_null() && !choices.is_array())
    {
        return Err(StreamLimitError {
            code: "upstream_stream_invalid_terminal",
            message: "Upstream xAI Chat stream has invalid choices".to_string(),
        });
    }
    let choices = chunk.get("choices").and_then(Value::as_array);
    let mut finish_frame = false;
    if let Some(choices) = choices {
        for choice in choices {
            let index = wire_index(choice.get("index"), "choices[].index")?;
            // The shared accounting has already enforced the choice bound.
            let key = index.to_string();
            if choice.get("error").is_some_and(|error| !error.is_null()) {
                return Err(StreamLimitError {
                    code: "upstream_stream_failed",
                    message: "Upstream xAI Chat choice reported a protocol error".to_string(),
                });
            }
            let finish = choice.get("finish_reason").filter(|v| !v.is_null());
            if finish.is_some_and(|reason| {
                !matches!(
                    reason.as_str(),
                    Some("stop" | "length" | "tool_calls" | "function_call" | "content_filter")
                )
            }) {
                return Err(StreamLimitError {
                    code: "upstream_stream_invalid_terminal",
                    message: "Upstream xAI Chat choice has an invalid finish_reason".to_string(),
                });
            }
            let finished = finish.is_some();
            if choice
                .get("delta")
                .is_some_and(|delta| !delta.is_null() && !delta.is_object())
            {
                return Err(StreamLimitError {
                    code: "upstream_stream_invalid_terminal",
                    message: "Upstream xAI Chat choice has an invalid delta".to_string(),
                });
            }
            let observed = state_obj_mut(&mut state.responses.state, "_xaiChoices");
            if observed.get(&key).and_then(Value::as_bool) == Some(true)
                && choice
                    .get("delta")
                    .and_then(Value::as_object)
                    .is_some_and(|delta| {
                        delta.values().any(|value| match value {
                            Value::Null => false,
                            Value::String(text) => !text.is_empty(),
                            Value::Array(items) => !items.is_empty(),
                            Value::Object(object) => !object.is_empty(),
                            _ => true,
                        })
                    })
            {
                return Err(StreamLimitError {
                    code: "upstream_stream_invalid_terminal",
                    message: "Upstream xAI Chat choice continued after its finish_reason"
                        .to_string(),
                });
            }
            observed
                .entry(key)
                .and_modify(|value| {
                    if finished {
                        *value = Value::Bool(true);
                    }
                })
                .or_insert(Value::Bool(finished));
            finish_frame |= finished;
        }
    }

    let ready = xai_chat_terminal_ready(state);
    if !ready {
        // A newly observed unfinished choice revokes any provisional snapshot.
        state.responses.state.remove("usage");
    } else if (finish_frame || choices.is_none_or(Vec::is_empty))
        && let Some(usage) = chunk.get("usage").and_then(xai_chat_usage_to_responses)
    {
        // Snapshots replace each other; running usage never enters this slot.
        state.responses.state.insert("usage".to_string(), usage);
    }
    Ok(())
}

fn buffer_chat_usage(state: &mut serde_json::Map<String, Value>, chunk: &Value) {
    let Some(usage) = chunk.get("usage").filter(|v| v.is_object()) else {
        return;
    };
    state.insert(
        "usage".to_string(),
        serde_json::json!({
            "input_tokens": usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
            "input_tokens_details": {
                "cached_tokens": usage.pointer("/prompt_tokens_details/cached_tokens").and_then(Value::as_u64).unwrap_or(0)
            },
            "output_tokens": usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
            "output_tokens_details": {
                "reasoning_tokens": usage.pointer("/completion_tokens_details/reasoning_tokens").and_then(Value::as_u64).unwrap_or(0)
            },
            "total_tokens": usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(0)
        }),
    );
}

fn send_completed(state: &mut serde_json::Map<String, Value>, events: &mut Vec<Value>) {
    if state.get("completedSent").and_then(|v| v.as_bool()) != Some(true) {
        state.insert("completedSent".to_string(), Value::Bool(true));
        let mut response = serde_json::json!({
            "id": state.get("responseId").and_then(|v| v.as_str()).unwrap_or(""),
            "object": "response",
            "created_at": state.get("created").and_then(|v| v.as_i64()).unwrap_or(0),
            "status": "completed",
            "background": false,
            "error": null,
            "incomplete_details": null,
            "service_tier": null,
        });
        // Never synthesize usage: omit the key when upstream sent none
        // (compat.rs terminal does the same).
        if let Some(usage) = state.get("usage").cloned().filter(|v| v.is_object()) {
            response["usage"] = usage;
        }
        emit(
            events,
            state,
            "response.completed",
            serde_json::json!({
                "type": "response.completed",
                "response": response
            }),
        );
    }
}

/// Tool-call + finish_reason tail of `chat_to_responses_response`, shared by
/// the normal path and the <think>-consumed early return (JS 102-116).
fn emit_tool_calls_and_finish(
    chunk: &Value,
    state: &mut serde_json::Map<String, Value>,
    events: &mut Vec<Value>,
    idx: u64,
    idx_str: &str,
    delta: &Value,
) -> Vec<Value> {
    let _ = (idx, idx_str);
    if let Some(tool_calls) = delta
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .filter(|tc| !tc.is_empty())
    {
        emit_tool_calls_block(state, events, tool_calls);
    }
    if chunk
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("finish_reason"))
        .and_then(Value::as_str)
        .is_some_and(|reason| !reason.is_empty())
    {
        emit_finish_block(state, events);
    }
    std::mem::take(events)
}

/// Tool-call arm of `chat_to_responses_response` (JS 102-108): close any open
/// message, then emit/accumulate per tool call. Custom tools wait for both
/// call id AND function name before announcing, and stream no argument deltas
/// (input is emitted once at close after unwrapping the Chat JSON wrapper).
fn emit_tool_calls_block(
    state: &mut serde_json::Map<String, Value>,
    events: &mut Vec<Value>,
    tool_calls: &[Value],
) {
    // C34: mutate one map entry per tool index in place. The previous version
    // cloned all four `func*` maps per chunk (O(items) per delta) and rebuilt
    // argument strings via format! (O(prefix) per delta).
    for tc in tool_calls.iter() {
        let Some(tc_idx) = tc.get("index").and_then(|v| v.as_u64()) else {
            continue;
        };
        let tc_idx_str = tc_idx.to_string();
        let new_call_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let func_name = tc
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if !func_name.is_empty() {
            state_obj_mut(state, "funcNames")
                .insert(tc_idx_str.clone(), Value::String(func_name.to_string()));
        }
        if !new_call_id.is_empty() {
            state_obj_mut(state, "funcCallIds")
                .insert(tc_idx_str.clone(), Value::String(new_call_id.to_string()));
        }

        // Wait for both id and name before deciding custom vs function;
        // otherwise a split-chunk call can be irreversibly announced wrong.
        let call_id = indexed_text(state, "funcCallIds", &tc_idx_str);
        let name = indexed_text(state, "funcNames", &tc_idx_str);
        if !indexed_flag(state, "funcItemAdded", &tc_idx_str)
            && !call_id.is_empty()
            && !name.is_empty()
        {
            set_indexed_flag(state, "funcItemAdded", &tc_idx_str);
            let custom = is_custom_tool(state, &name);
            // Close any open text message first (JS 104: closeMessage).
            close_message(state, events, &tc_idx_str);
            emit(
                events,
                state,
                "response.output_item.added",
                serde_json::json!({
                    "type": "response.output_item.added",
                    "output_index": tc_idx,
                    "item": if custom {
                        serde_json::json!({
                            "id": format!("ctc_{}", call_id),
                            "type": "custom_tool_call",
                            "input": "",
                            "call_id": call_id,
                            "name": name
                        })
                    } else {
                        serde_json::json!({
                            "id": format!("fc_{}", call_id),
                            "type": "function_call",
                            "arguments": "",
                            "call_id": call_id,
                            "name": name
                        })
                    },
                }),
            );
        }

        if let Some(args) = tc
            .get("function")
            .and_then(|f| f.get("arguments"))
            .and_then(|v| v.as_str())
        {
            if !args.is_empty() {
                let ref_call_id = indexed_text(state, "funcCallIds", &tc_idx_str);
                let ref_call_id = if ref_call_id.is_empty() {
                    call_id.clone()
                } else {
                    ref_call_id
                };
                // Custom input is emitted once at close, after the Chat JSON
                // wrapper can be parsed. Streaming raw fragments would expose
                // {"input":"..."} instead of the freeform program.
                let current_name = indexed_text(state, "funcNames", &tc_idx_str);
                let is_custom_now = is_custom_tool(state, &current_name);
                if indexed_flag(state, "funcItemAdded", &tc_idx_str)
                    && !ref_call_id.is_empty()
                    && !is_custom_now
                {
                    emit(
                        events,
                        state,
                        "response.function_call_arguments.delta",
                        serde_json::json!({
                            "type": "response.function_call_arguments.delta",
                            "item_id": format!("fc_{}", ref_call_id),
                            "output_index": tc_idx,
                            "delta": args
                        }),
                    );
                }
                append_indexed_text(state, "funcArgsBuf", &tc_idx_str, args);
            }
        }
    }
}

/// finish_reason arm of `chat_to_responses_response` (JS 110-116): close
/// every open message, reasoning, and tool call, then send completed.
/// The terminal `response.completed` is deferred until upstream usage arrives
/// (`choices:[] + usage` tail) or the stream ends, so a split usage tail is
/// not orphaned behind an early completed with no usage.
fn emit_finish_block(state: &mut serde_json::Map<String, Value>, events: &mut Vec<Value>) {
    let msg_keys: Vec<String> = state
        .get("msgItemAdded")
        .and_then(|v| v.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    for k in &msg_keys {
        close_message(state, events, k);
    }

    close_reasoning(state, events);
    let func_keys: Vec<String> = state
        .get("funcCallIds")
        .and_then(|v| v.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    for k in &func_keys {
        close_tool_call(state, events, k);
    }
    if !is_xai_chat(state) && have_chat_usage(state) {
        send_completed(state, events);
    } else {
        state.insert("needCompleted".to_string(), Value::Bool(true));
    }
}

pub fn chat_to_responses_response(
    chunk: &Value,
    state: &mut serde_json::Map<String, Value>,
) -> Vec<Value> {
    if !is_xai_chat(state) {
        buffer_chat_usage(state, chunk);
    }
    if chunk
        .get("choices")
        .and_then(|v| v.as_array())
        .is_none_or(|a| a.is_empty())
    {
        // Standard OpenAI split usage tail: `choices:[] + usage` arrives after
        // the `finish_reason` chunk. Flush the deferred terminal once.
        if !is_xai_chat(state)
            && state.get("needCompleted").and_then(|v| v.as_bool()) == Some(true)
            && state.get("completedSent").and_then(|v| v.as_bool()) != Some(true)
            && state.get("started").and_then(|v| v.as_bool()) == Some(true)
        {
            let mut events = Vec::new();
            send_completed(state, &mut events);
            return events;
        }
        return vec![];
    }

    let mut events: Vec<Value> = Vec::new();

    if !state.contains_key("started") {
        state.insert("started".to_string(), Value::Bool(true));
        let resp_id = chunk
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| format!("resp_{}", s))
            .unwrap_or_else(|| format!("resp_{}", chrono::Utc::now().timestamp_millis()));
        state.insert("responseId".to_string(), Value::String(resp_id.clone()));
        state.insert(
            "created".to_string(),
            Value::Number(chrono::Utc::now().timestamp().into()),
        );
        state.insert("seq".to_string(), Value::Number(0.into()));
        state.insert("msgItemAdded".to_string(), serde_json::json!({}));
        state.insert("msgContentAdded".to_string(), serde_json::json!({}));
        state.insert("msgTextBuf".to_string(), serde_json::json!({}));
        state.insert("msgItemDone".to_string(), serde_json::json!({}));
        state.insert("funcNames".to_string(), serde_json::json!({}));
        state.insert("funcCallIds".to_string(), serde_json::json!({}));
        state.insert("funcArgsBuf".to_string(), serde_json::json!({}));
        state.insert("funcItemDone".to_string(), serde_json::json!({}));
        state.insert("funcArgsDone".to_string(), serde_json::json!({}));
        state.insert("reasoningId".to_string(), Value::Null);
        state.insert("reasoningBuf".to_string(), Value::String(String::new()));
        state.insert("reasoningDone".to_string(), Value::Bool(false));
        state.insert("reasoningPartAdded".to_string(), Value::Bool(false));
        state.insert("inThinking".to_string(), Value::Bool(false));
        state.insert("completedSent".to_string(), Value::Bool(false));
        state.insert(
            "model".to_string(),
            Value::String(
                chunk
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
        );

        let seq1 = next_seq(state);
        let seq2 = next_seq(state);

        events.push(serde_json::json!({
            "event": "response.created",
            "data": {
                "type": "response.created",
                "sequence_number": seq1,
                "response": {
                    "id": resp_id.clone(),
                    "object": "response",
                    "created_at": chrono::Utc::now().timestamp(),
                    "model": state.get("model").and_then(Value::as_str).unwrap_or(""),
                    "status": "in_progress",
                    "background": false,
                    "error": null,
                    "service_tier": null,
                    "output": []
                }
            }
        }));

        events.push(serde_json::json!({
            "event": "response.in_progress",
            "data": {
                "type": "response.in_progress",
                "sequence_number": seq2,
                "response": {
                    "id": resp_id,
                    "object": "response",
                    "created_at": chrono::Utc::now().timestamp(),
                    "status": "in_progress"
                }
            }
        }));
    }

    let choice = &chunk["choices"][0];
    let Some(idx) = choice.get("index").and_then(|v| v.as_u64()) else {
        return events;
    };
    let idx_str = idx.to_string();
    let delta = choice
        .get("delta")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    // Handle reasoning across vendor shapes (JS concerns/reasoning.js
    // extractReasoningText): reasoning_content (GLM/Qwen/DeepSeek/Kimi) →
    // reasoning (compat layers) → reasoning_details[] (MiniMax
    // reasoning_split=true: [{text|content}]).
    let reasoning_text = delta
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            delta
                .get("reasoning")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            delta.get("reasoning_details").and_then(|d| {
                if let Some(arr) = d.as_array() {
                    let joined = arr
                        .iter()
                        .map(|e| match e {
                            Value::String(s) => s.clone(),
                            _ => e
                                .get("text")
                                .or_else(|| e.get("content"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        })
                        .collect::<String>();
                    if joined.is_empty() {
                        None
                    } else {
                        Some(joined)
                    }
                } else {
                    None
                }
            })
        })
        .unwrap_or_default();
    if !reasoning_text.is_empty() {
        start_reasoning(state, &mut events, idx);
        emit_reasoning_delta(state, &mut events, &reasoning_text);
    }

    // Handle text content, including the <think> state machine (JS
    // openai-responses.js:76-100): <think> routes content into reasoning,
    // </think> splits buffered thinking from resumed text.
    if let Some(raw_content) = delta.get("content").and_then(|v| v.as_str()) {
        if !raw_content.is_empty() {
            let mut content = raw_content.to_string();
            let in_thinking = state
                .get("inThinking")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut return_after_thinking = false;

            if content.contains("<think>") {
                state.insert("inThinking".to_string(), Value::Bool(true));
                content = content.replacen("<think>", "", 1);
                start_reasoning(state, &mut events, idx);
            }

            if content.contains("</think>") {
                let parts: Vec<&str> = content.splitn(2, "</think>").collect();
                let think_part = parts.first().copied().unwrap_or("");
                let text_part = parts.get(1).copied().unwrap_or("");
                if !think_part.is_empty() {
                    emit_reasoning_delta(state, &mut events, think_part);
                }
                close_reasoning(state, &mut events);
                state.insert("inThinking".to_string(), Value::Bool(false));
                content = text_part.to_string();
            } else if in_thinking
                || state
                    .get("inThinking")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            {
                if !content.is_empty() {
                    emit_reasoning_delta(state, &mut events, &content);
                }
                return_after_thinking = true;
            }

            if return_after_thinking {
                if is_xai_chat(state) {
                    return emit_tool_calls_and_finish(
                        chunk,
                        state,
                        &mut events,
                        idx,
                        &idx_str,
                        &delta,
                    );
                }
                return events;
            }
            if content.is_empty() {
                // All consumed by the think machine — fall through to
                // tool_calls / finish_reason handling below.
                return emit_tool_calls_and_finish(
                    chunk,
                    state,
                    &mut events,
                    idx,
                    &idx_str,
                    &delta,
                );
            }
            let content: &str = &content;
            // C34: point-mutate message maps instead of cloning all three per
            // content delta.
            if !indexed_flag(state, "msgItemAdded", &idx_str) {
                set_indexed_flag(state, "msgItemAdded", &idx_str);
                let msg_id = format!("msg_{}_{}", state["responseId"].as_str().unwrap_or(""), idx);
                state.insert(format!("msgId_{}", idx), Value::String(msg_id.clone()));

                emit(
                    &mut events,
                    state,
                    "response.output_item.added",
                    serde_json::json!({
                        "type": "response.output_item.added",
                        "output_index": idx,
                        "item": {"id": msg_id, "type": "message", "content": [], "role": "assistant"}
                    }),
                );

                emit(
                    &mut events,
                    state,
                    "response.content_part.added",
                    serde_json::json!({
                        "type": "response.content_part.added",
                        "item_id": msg_id,
                        "output_index": idx,
                        "content_index": 0,
                        "part": {"type": "output_text", "annotations": [], "logprobs": [], "text": ""}
                    }),
                );
                set_indexed_flag(state, "msgContentAdded", &idx_str);
            }

            let msg_id = state
                .get(&format!("msgId_{}", idx))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            emit(
                &mut events,
                state,
                "response.output_text.delta",
                serde_json::json!({
                    "type": "response.output_text.delta",
                    "item_id": msg_id,
                    "output_index": idx,
                    "content_index": 0,
                    "delta": content,
                    "logprobs": []
                }),
            );

            append_indexed_text(state, "msgTextBuf", &idx_str, content);
        }
    }

    // Handle tool_calls (skip empty arrays — 9router parity: empty array is
    // truthy; require a real call to avoid premature message close).
    if let Some(tool_calls) = delta
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .filter(|tc| !tc.is_empty())
    {
        emit_tool_calls_block(state, &mut events, tool_calls);
    }

    // Handle finish_reason — JS parity (openai-responses.js:111
    // `if (choice.finish_reason)`): explicit null is falsy, so only a
    // non-null string closes the message / sends response.completed.
    if choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        emit_finish_block(state, &mut events);
    }

    events
}

pub fn responses_to_chat_response(
    chunk: &Value,
    state: &mut serde_json::Map<String, Value>,
) -> Vec<Value> {
    if chunk.is_null() {
        if state.get("finishReasonSent").and_then(|v| v.as_bool()) == Some(true)
            || state.get("started").and_then(|v| v.as_bool()) != Some(true)
        {
            return vec![];
        }

        let finish_reason = if state
            .get("toolCallIndex")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            > 0
            || state.get("currentToolCallId").is_some_and(|v| !v.is_null())
        {
            "tool_calls"
        } else {
            "stop"
        };

        state.insert("finishReasonSent".to_string(), Value::Bool(true));
        state.insert(
            "finishReason".to_string(),
            Value::String(finish_reason.to_string()),
        );

        let mut final_chunk = serde_json::json!({
            "id": state.get("chatId").and_then(|v| v.as_str()).unwrap_or("unknown"),
            "object": "chat.completion.chunk",
            "created": state.get("created").and_then(|v| v.as_i64()).unwrap_or(0),
            "model": state.get("model").and_then(|v| v.as_str()).unwrap_or("unknown"),
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": finish_reason
            }]
        });

        if let Some(usage) = state.get("usage") {
            if usage.is_object() {
                final_chunk["usage"] = usage.clone();
            }
        }
        return vec![final_chunk];
    }

    let event_type = chunk
        .get("type")
        .or_else(|| chunk.get("event"))
        .and_then(|v| v.as_str());
    let data = chunk.get("data").unwrap_or(chunk);

    if state.get("started").and_then(|v| v.as_bool()) != Some(true) {
        state.insert("started".to_string(), Value::Bool(true));
        state.insert(
            "chatId".to_string(),
            Value::String(format!(
                "chatcmpl-{}",
                chrono::Utc::now().timestamp_millis()
            )),
        );
        state.insert(
            "created".to_string(),
            Value::Number(chrono::Utc::now().timestamp().into()),
        );
        state.insert("toolCallIndex".to_string(), Value::Number(0.into()));
        state.insert("currentToolCallId".to_string(), Value::Null);
        // JS parity (openai-responses.js:449-455): item_id → chat
        // tool_calls index plus the set of indices that already received
        // argument deltas. Lazily created as plain JSON (object / array)
        // since the state map only holds serde Values.
        state.insert(
            "respToolChatIndex".to_string(),
            Value::Object(Default::default()),
        );
        state.insert("respToolArgsEmitted".to_string(), Value::Array(Vec::new()));
    }

    let chat_id = state
        .get("chatId")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let created = state.get("created").and_then(|v| v.as_i64()).unwrap_or(0);
    let model = state
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    match event_type {
        Some("response.output_text.delta") => {
            let delta = data.get("delta").and_then(|v| v.as_str()).unwrap_or("");
            if delta.is_empty() {
                return vec![];
            }
            vec![serde_json::json!({
                "id": chat_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {"content": delta},
                    "finish_reason": null
                }]
            })]
        }
        Some("response.output_text.done") => vec![],
        Some("response.output_item.added") => {
            let item_type = data
                .get("item")
                .and_then(|i| i.get("type"))
                .and_then(|v| v.as_str());
            if item_type == Some("function_call") || item_type == Some("custom_tool_call") {
                let item = &data["item"];
                let Some(call_id) = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                else {
                    return vec![];
                };
                state.insert(
                    "currentToolCallId".to_string(),
                    Value::String(call_id.clone()),
                );

                // JS parity (openai-responses.js:479-490): index is assigned
                // here (not on done) keyed by the server item id so parallel
                // calls stay separate; duplicate added reuses the mapping.
                // JS parity (openai-responses.js:~483): key is
                // item.id || data.item_id || state.currentToolCallId.
                let fallback_call_id = state
                    .get("currentToolCallId")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let item_id = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        data.get("item_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .or(fallback_call_id);
                let item_id = item_id.as_deref();
                let tool_idx = match resp_tool_index(state, item_id) {
                    Some(idx) => idx,
                    None => {
                        let idx = state
                            .get("toolCallIndex")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let Some(next) = idx.checked_add(1) else {
                            state.insert(
                                "_streamArithmeticFailure".to_string(),
                                Value::String("tool index".to_string()),
                            );
                            return vec![];
                        };
                        state.insert("toolCallIndex".to_string(), Value::Number(next.into()));
                        if let Some(key) = item_id {
                            set_resp_tool_index(state, key, idx);
                        }
                        idx
                    }
                };
                vec![serde_json::json!({
                    "id": chat_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "delta": {
                            "tool_calls": [{
                                "index": tool_idx,
                                "id": call_id,
                                "type": "function",
                                "function": {
                                    "name": item.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                                    "arguments": ""
                                }
                            }]
                        },
                        "finish_reason": null
                    }]
                })]
            } else {
                vec![]
            }
        }
        Some("response.function_call_arguments.delta")
        | Some("response.custom_tool_call_input.delta") => {
            let delta = data.get("delta").and_then(|v| v.as_str()).unwrap_or("");
            if delta.is_empty() {
                return vec![];
            }
            // JS parity (openai-responses.js:505-519): route by item_id so
            // interleaved parallel fragments stay on their own call.
            let item_id = data.get("item_id").and_then(|v| v.as_str());
            let Some(tool_idx) = resp_tool_index(state, item_id) else {
                return vec![];
            };
            mark_resp_tool_args(state, tool_idx);
            vec![serde_json::json!({
                "id": chat_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": tool_idx,
                            "function": {"arguments": delta}
                        }]
                    },
                    "finish_reason": null
                }]
            })]
        }
        Some("response.output_item.done") => {
            let item_type = data
                .get("item")
                .and_then(|i| i.get("type"))
                .and_then(|v| v.as_str());
            if item_type == Some("function_call") || item_type == Some("custom_tool_call") {
                // JS parity (openai-responses.js:521-539): index was assigned
                // at added-time; nothing to advance. Some upstreams send
                // complete arguments only here (no deltas) — emit them once.
                let key = data
                    .get("item")
                    .and_then(|i| i.get("id"))
                    .and_then(|v| v.as_str())
                    .or_else(|| data.get("item_id").and_then(|v| v.as_str()));
                let Some(tool_idx) = resp_tool_index(state, key) else {
                    return vec![];
                };
                let full_args = data
                    .get("item")
                    .and_then(|i| i.get("arguments"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !full_args.is_empty() && !resp_tool_args_emitted(state, tool_idx) {
                    mark_resp_tool_args(state, tool_idx);
                    return vec![serde_json::json!({
                        "id": chat_id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "tool_calls": [{
                                    "index": tool_idx,
                                    "function": {"arguments": full_args}
                                }]
                            },
                            "finish_reason": null
                        }]
                    })];
                }
            }
            vec![]
        }
        Some("response.completed") => {
            if let Some(response) = data.get("response") {
                if let Some(usage) = response.get("usage") {
                    let input_tokens = usage
                        .get("input_tokens")
                        .or_else(|| usage.get("prompt_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let output_tokens = usage
                        .get("output_tokens")
                        .or_else(|| usage.get("completion_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let cache_read = usage
                        .get("input_tokens_details")
                        .and_then(|d| d.get("cached_tokens"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);

                    let mut usage_obj = serde_json::json!({
                        "prompt_tokens": input_tokens,
                        "completion_tokens": output_tokens,
                        "total_tokens": input_tokens + output_tokens
                    });
                    if cache_read > 0 {
                        usage_obj["prompt_tokens_details"] =
                            serde_json::json!({"cached_tokens": cache_read});
                    }
                    state.insert("usage".to_string(), usage_obj);
                }
            }

            if state.get("finishReasonSent").and_then(|v| v.as_bool()) != Some(true) {
                let finish_reason = if state
                    .get("toolCallIndex")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    > 0
                    || state.get("currentToolCallId").is_some_and(|v| !v.is_null())
                {
                    "tool_calls"
                } else {
                    "stop"
                };
                state.insert("finishReasonSent".to_string(), Value::Bool(true));
                state.insert(
                    "finishReason".to_string(),
                    Value::String(finish_reason.to_string()),
                );

                let mut final_chunk = serde_json::json!({
                    "id": chat_id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": finish_reason
                    }]
                });

                if let Some(usage) = state.get("usage") {
                    if usage.is_object() {
                        final_chunk["usage"] = usage.clone();
                    }
                }
                return vec![final_chunk];
            }
            vec![]
        }
        // response.created carries the model name assigned by the backend.
        // Capture it so subsequent chunks emit a meaningful model field instead of "unknown".
        Some("response.created") => {
            if let Some(response) = data.get("response") {
                if let Some(id) = response.get("id").and_then(Value::as_str) {
                    state.insert("chatId".to_string(), Value::String(id.to_string()));
                }
                if let Some(created) = response.get("created_at").and_then(Value::as_i64) {
                    state.insert("created".to_string(), Value::from(created));
                }
                if let Some(model_name) = response.get("model").and_then(|v| v.as_str()) {
                    if !model_name.is_empty() {
                        state.insert("model".to_string(), Value::String(model_name.to_string()));
                    }
                }
            }
            vec![]
        }
        _ => vec![],
    }
}

use crate::core::translator::registry::ResponseTransformState;

/// Registry-compatible streaming wrapper: Responses API -> OpenAI chat completion chunks.
///
/// Handles two input formats:
///   1. Bare JSON: `{"type":"response.completed","response":{...}}`
///   2. SSE-framed: `event: response.completed\ndata: {"type":"response.completed",...}\n\n`
///
/// Registry adapter: OpenAI chat SSE/JSON → Responses API SSE events.
/// Signature matches `registry::ResponseTransformFn`.
pub fn chat_to_responses_streaming(
    chunk: &[u8],
    state: &mut crate::core::translator::registry::ResponseTransformState,
) -> Vec<String> {
    if state.xai_chat {
        if state.failure.is_some()
            || chunk == b"[DONE]"
            || state
                .responses
                .state
                .get("completedSent")
                .and_then(Value::as_bool)
                == Some(true)
        {
            // xAI's usage can follow its Chat sentinel. Only transport EOF
            // constructs our terminal and downstream sentinel.
            return Vec::new();
        }
        state
            .responses
            .state
            .insert("_xaiChat".to_string(), Value::Bool(true));
    }
    if chunk == b"[DONE]" {
        // Flush a deferred terminal before the stream end so a finish chunk
        // without usage still yields exactly one `response.completed`.
        let pending = state
            .responses
            .state
            .get("needCompleted")
            .and_then(|v| v.as_bool())
            == Some(true)
            && state
                .responses
                .state
                .get("completedSent")
                .and_then(|v| v.as_bool())
                != Some(true)
            && state
                .responses
                .state
                .get("started")
                .and_then(|v| v.as_bool())
                == Some(true);
        if pending {
            let mut events = Vec::new();
            send_completed(&mut state.responses.state, &mut events);
            let mut out = format_responses_events(events);
            out.push("data: [DONE]\n\n".to_string());
            return out;
        }
        return vec!["data: [DONE]\n\n".to_string()];
    }
    let Ok(value) = serde_json::from_slice::<Value>(chunk) else {
        if state.xai_chat {
            return state.fail(crate::core::translator::limits::StreamLimitError {
                code: "upstream_stream_invalid_json",
                message: "Upstream xAI Chat stream contains invalid JSON".to_string(),
            });
        }
        return Vec::new();
    };
    if let Err(error) =
        crate::core::translator::registry::track_openai_accumulation(state, &value, 4, true)
    {
        return state.fail(error);
    }
    if state.xai_chat
        && let Err(error) = observe_xai_chat(state, &value)
    {
        return state.fail(error);
    }
    let events = chat_to_responses_response(&value, &mut state.responses.state);
    if let Some(error) = take_arithmetic_failure(&mut state.responses.state) {
        return state.fail(error);
    }
    format_responses_events(events)
}

/// EOF synthesis for `OpenAi -> OpenAiResponses/Codex`: emit the deferred
/// terminal when upstream closed cleanly without `[DONE]` or a usage tail.
/// xAI requires validated generation-end evidence and keeps all terminals here,
/// after the real transport EOF. Other providers retain the split-tail policy.
/// No-op on failure or duplicate calls.
pub fn chat_to_responses_finish(
    state: &mut crate::core::translator::registry::ResponseTransformState,
) -> Vec<String> {
    if state.xai_chat {
        if state.failure.is_some()
            || state
                .responses
                .state
                .get("completedSent")
                .and_then(Value::as_bool)
                == Some(true)
        {
            return Vec::new();
        }
        if !xai_chat_terminal_ready(state) {
            return state.fail(crate::core::translator::limits::StreamLimitError {
                code: "upstream_stream_truncated",
                message: "Upstream xAI Chat stream ended before every observed choice finished"
                    .to_string(),
            });
        }
        let mut events = Vec::new();
        send_completed(&mut state.responses.state, &mut events);
        if let Some(error) = take_arithmetic_failure(&mut state.responses.state) {
            return state.fail(error);
        }
        let mut output = format_responses_events(events);
        output.push("data: [DONE]\n\n".to_string());
        return output;
    }
    let pending = state
        .responses
        .state
        .get("needCompleted")
        .and_then(|v| v.as_bool())
        == Some(true)
        && state
            .responses
            .state
            .get("completedSent")
            .and_then(|v| v.as_bool())
            != Some(true)
        && state
            .responses
            .state
            .get("started")
            .and_then(|v| v.as_bool())
            == Some(true);
    if !pending {
        return Vec::new();
    }
    let mut events = Vec::new();
    send_completed(&mut state.responses.state, &mut events);
    format_responses_events(events)
}

fn format_responses_events(events: Vec<Value>) -> Vec<String> {
    events
        .into_iter()
        .map(|event| {
            let event_type = event
                .get("event")
                .and_then(Value::as_str)
                .or_else(|| event.get("type").and_then(Value::as_str))
                .unwrap_or("message")
                .to_string();
            let data = event.get("data").cloned().unwrap_or(event);
            format!(
                "event: {event_type}\ndata: {}\n\n",
                serde_json::to_string(&data).unwrap_or_default()
            )
        })
        .collect()
}

/// Signature matches `registry::ResponseTransformFn`.
pub fn responses_to_chat_streaming(
    chunk: &[u8],
    state: &mut ResponseTransformState,
) -> Vec<String> {
    if state.responses.completed_sent || chunk == b"[DONE]" {
        return Vec::new();
    }
    let Ok(val) = serde_json::from_slice::<Value>(chunk) else {
        return state.fail(crate::core::translator::limits::StreamLimitError {
            code: "upstream_stream_invalid_json",
            message: "Responses stream contains invalid JSON".to_string(),
        });
    };
    let event_type = val
        .get("type")
        .or_else(|| val.get("event"))
        .and_then(Value::as_str);
    let data = val.get("data").unwrap_or(&val);
    if matches!(
        event_type,
        Some("error" | "response.failed" | "response.incomplete")
    ) {
        let error = data
            .get("error")
            .or_else(|| data.pointer("/response/error"))
            .unwrap_or(data);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| {
                data.pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str)
            })
            .unwrap_or("Upstream Responses generation failed or was incomplete");
        return state.fail(crate::core::translator::limits::StreamLimitError {
            code: "upstream_stream_failed",
            message: message.to_string(),
        });
    }
    let completed = event_type == Some("response.completed");
    if completed && !data.get("response").is_some_and(Value::is_object) {
        return state.fail(crate::core::translator::limits::StreamLimitError {
            code: "upstream_stream_invalid_terminal",
            message: "Responses completion is missing its response".to_string(),
        });
    }
    if let Err(error) = track_responses_accumulation(state, &val) {
        return state.fail(error);
    }
    let inner = &mut state.responses.state;
    let results = responses_to_chat_response(&val, inner);
    if let Some(error) = take_arithmetic_failure(inner) {
        return state.fail(error);
    }
    state.responses.completed_sent = completed;
    results
        .into_iter()
        .map(|value| {
            format!(
                "data: {}\n\n",
                serde_json::to_string(&value).unwrap_or_default()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::translator::registry::{global_registry, Format, ResponseTransformState};
    use serde_json::json;

    fn xai_state() -> ResponseTransformState {
        ResponseTransformState {
            xai_chat: true,
            ..Default::default()
        }
    }

    fn xai_feed(state: &mut ResponseTransformState, value: Value) -> Vec<String> {
        chat_to_responses_streaming(&serde_json::to_vec(&value).unwrap(), state)
    }

    fn xai_choice(delta: Value, finish: Value) -> Value {
        json!({
            "id": "xai-chat",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]
        })
    }

    fn xai_completed(output: &[String]) -> Value {
        let frame = output
            .iter()
            .find(|frame| frame.starts_with("event: response.completed\n"))
            .expect("completed event");
        let data = frame.split_once("\ndata: ").unwrap().1.trim();
        serde_json::from_str::<Value>(data).unwrap()["response"].clone()
    }

    fn assert_xai_no_terminal(output: &[String]) {
        assert!(
            !output
                .iter()
                .any(|frame| frame.contains("response.completed") || frame.contains("[DONE]")),
            "terminal must wait for actual EOF: {output:?}"
        );
    }

    #[test]
    fn xai_chat_usage_exclusive_and_inclusive_reasoning() {
        for completion in [9, 103] {
            let usage = json!({
                "prompt_tokens": 32,
                "completion_tokens": completion,
                "total_tokens": 135,
                "prompt_tokens_details": {"cached_tokens": 6},
                "completion_tokens_details": {"reasoning_tokens": 94},
                "unrelated": "not copied"
            });
            assert_eq!(
                xai_chat_usage_to_responses(&usage),
                Some(json!({
                    "input_tokens": 32,
                    "output_tokens": 103,
                    "total_tokens": 135,
                    "input_tokens_details": {"cached_tokens": 6},
                    "output_tokens_details": {"reasoning_tokens": 94}
                }))
            );
        }
    }

    #[test]
    fn xai_chat_usage_omits_unknown_invalid_and_empty_fields() {
        for invalid in [
            Value::Null,
            json!(-1),
            json!(1.5),
            json!(1.0),
            json!("9"),
            json!(true),
            serde_json::from_str("18446744073709551616").unwrap(),
        ] {
            let usage = json!({
                "prompt_tokens": invalid,
                "completion_tokens": invalid,
                "total_tokens": invalid,
                "prompt_tokens_details": {"cached_tokens": invalid},
                "completion_tokens_details": {"reasoning_tokens": invalid}
            });
            assert_eq!(xai_chat_usage_to_responses(&usage), None, "{usage}");
        }
        for empty in [
            Value::Null,
            json!([]),
            json!({}),
            json!({"estimated": true}),
            json!({"prompt_tokens_details": {}, "completion_tokens_details": null}),
        ] {
            assert_eq!(xai_chat_usage_to_responses(&empty), None, "{empty}");
        }
        assert_eq!(
            xai_chat_usage_to_responses(&json!({"completion_tokens": 9})),
            Some(json!({"output_tokens": 9}))
        );
        assert_eq!(
            xai_chat_usage_to_responses(&json!({
                "prompt_tokens_details": {"cached_tokens": 6},
                "completion_tokens_details": {"reasoning_tokens": 4}
            })),
            Some(json!({
                "input_tokens_details": {"cached_tokens": 6},
                "output_tokens_details": {"reasoning_tokens": 4}
            }))
        );
    }

    #[test]
    fn xai_chat_usage_checked_arithmetic_and_contradictory_details() {
        for (usage, expected) in [
            (
                json!({"prompt_tokens": 32, "completion_tokens": 9, "total_tokens": 999,
                    "prompt_tokens_details": {"cached_tokens": 33},
                    "completion_tokens_details": {"reasoning_tokens": 94}}),
                json!({"input_tokens": 32, "output_tokens": 9, "total_tokens": 999}),
            ),
            (
                json!({"prompt_tokens": 32, "completion_tokens": 9,
                    "completion_tokens_details": {"reasoning_tokens": 4}}),
                json!({"input_tokens": 32, "output_tokens": 9,
                    "output_tokens_details": {"reasoning_tokens": 4}}),
            ),
            (
                json!({"prompt_tokens": u64::MAX, "completion_tokens": 1, "total_tokens": 0,
                    "completion_tokens_details": {"reasoning_tokens": 1}}),
                json!({"input_tokens": u64::MAX, "output_tokens": 1, "total_tokens": 0,
                    "output_tokens_details": {"reasoning_tokens": 1}}),
            ),
            (
                json!({"prompt_tokens": 0, "completion_tokens": u64::MAX, "total_tokens": 1,
                    "completion_tokens_details": {"reasoning_tokens": 2}}),
                json!({"input_tokens": 0, "output_tokens": u64::MAX, "total_tokens": 1,
                    "output_tokens_details": {"reasoning_tokens": 2}}),
            ),
            (
                json!({"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0,
                    "prompt_tokens_details": {"cached_tokens": 0},
                    "completion_tokens_details": {"reasoning_tokens": 0}}),
                json!({"input_tokens": 0, "output_tokens": 0, "total_tokens": 0,
                    "input_tokens_details": {"cached_tokens": 0},
                    "output_tokens_details": {"reasoning_tokens": 0}}),
            ),
        ] {
            assert_eq!(
                xai_chat_usage_to_responses(&usage),
                Some(expected),
                "{usage}"
            );
        }
    }

    #[test]
    fn xai_chat_stream_waits_for_eof_and_latest_authoritative_usage() {
        let mut state = xai_state();
        let mut running = xai_choice(json!({"content": "hi"}), Value::Null);
        running["usage"] = json!({"completion_tokens": 1});
        let output = xai_feed(&mut state, running);
        assert!(output.join("").contains("response.output_text.delta"));
        assert_xai_no_terminal(&output);
        assert!(!xai_chat_terminal_ready(&state));
        assert!(!state.responses.state.contains_key("usage"));
        // Provisional usage-only frames must not arm the EOF budget either.
        assert_xai_no_terminal(&xai_feed(
            &mut state,
            json!({"choices": [], "usage": {"completion_tokens": 2}}),
        ));
        assert!(!xai_chat_terminal_ready(&state));
        assert!(!state.responses.state.contains_key("usage"));

        let mut finish = xai_choice(json!({}), json!("stop"));
        finish["usage"] = json!({"completion_tokens": 9});
        let output = xai_feed(&mut state, finish);
        assert!(output.join("").contains("response.output_item.done"));
        assert_xai_no_terminal(&output);
        assert!(xai_chat_terminal_ready(&state));

        for empty in [
            Value::Null,
            json!({}),
            json!({"choices": []}),
            json!({"usage": null}),
            json!({"usage": {}}),
        ] {
            assert_xai_no_terminal(&xai_feed(&mut state, empty));
            assert_eq!(state.responses.state["usage"], json!({"output_tokens": 9}));
        }
        assert!(chat_to_responses_streaming(b"[DONE]", &mut state).is_empty());
        let tail = json!({"choices": [], "usage": {
            "prompt_tokens": 32, "completion_tokens": 9, "total_tokens": 135,
            "prompt_tokens_details": {"cached_tokens": 6},
            "completion_tokens_details": {"reasoning_tokens": 94}
        }});
        assert_xai_no_terminal(&xai_feed(&mut state, tail));
        // A later final snapshot replaces, never sums with, the earlier one.
        assert_xai_no_terminal(&xai_feed(
            &mut state,
            json!({"choices": [], "usage": {"completion_tokens": 0}}),
        ));
        assert!(chat_to_responses_streaming(b"[DONE]", &mut state).is_empty());
        let eof = chat_to_responses_finish(&mut state);
        assert_eq!(xai_completed(&eof)["usage"], json!({"output_tokens": 0}));
        assert_eq!(
            eof.iter()
                .filter(|s| s.contains("response.completed"))
                .count(),
            1
        );
        assert_eq!(eof.last().unwrap(), "data: [DONE]\n\n");
        assert!(chat_to_responses_finish(&mut state).is_empty());
        assert!(chat_to_responses_streaming(b"[DONE]", &mut state).is_empty());
    }

    #[test]
    fn xai_chat_stream_without_final_usage_omits_running_snapshot() {
        for final_usage in [
            None,
            Some(Value::Null),
            Some(json!({})),
            Some(json!({"completion_tokens": -1})),
        ] {
            let mut state = xai_state();
            let mut running = xai_choice(json!({"content": "hi"}), Value::Null);
            running["usage"] =
                json!({"prompt_tokens": 32, "completion_tokens": 1, "total_tokens": 33});
            xai_feed(&mut state, running);
            let mut finish = xai_choice(json!({}), json!("stop"));
            if let Some(usage) = final_usage {
                finish["usage"] = usage;
            }
            assert_xai_no_terminal(&xai_feed(&mut state, finish));
            assert!(chat_to_responses_streaming(b"[DONE]", &mut state).is_empty());
            let eof = chat_to_responses_finish(&mut state);
            assert!(xai_completed(&eof).get("usage").is_none(), "{eof:?}");
        }
    }

    #[test]
    fn xai_chat_stream_finish_and_usage_tail_map_inclusive_output() {
        for on_finish in [true, false] {
            let mut state = xai_state();
            let usage = json!({"prompt_tokens": 32, "completion_tokens": 9, "total_tokens": 135,
                "completion_tokens_details": {"reasoning_tokens": 94}});
            let mut finish = xai_choice(json!({"content": "hi"}), json!("stop"));
            if on_finish {
                finish["usage"] = usage.clone();
            }
            assert_xai_no_terminal(&xai_feed(&mut state, finish));
            if !on_finish {
                assert_xai_no_terminal(&xai_feed(
                    &mut state,
                    json!({"choices": [], "usage": usage}),
                ));
            }
            assert_eq!(
                xai_completed(&chat_to_responses_finish(&mut state))["usage"]["output_tokens"],
                103
            );
        }
    }

    #[test]
    fn xai_chat_stream_tool_deltas_and_item_done_are_not_deferred() {
        let mut state = xai_state();
        let tool = xai_choice(
            json!({"tool_calls": [{"index": 0, "id": "call_1",
            "function": {"name": "lookup", "arguments": "{}"}}]}),
            Value::Null,
        );
        let output = xai_feed(&mut state, tool);
        assert!(output
            .join("")
            .contains("response.function_call_arguments.delta"));
        assert!(!xai_chat_terminal_ready(&state));
        let output = xai_feed(&mut state, xai_choice(json!({}), json!("tool_calls")));
        assert!(output
            .join("")
            .contains("response.function_call_arguments.done"));
        assert!(output.join("").contains("response.output_item.done"));
        assert_xai_no_terminal(&output);
        assert!(xai_chat_terminal_ready(&state));
        assert_eq!(chat_to_responses_finish(&mut state).len(), 2);
    }

    #[test]
    fn xai_chat_stream_finishing_inside_think_still_closes_items_immediately() {
        let mut state = xai_state();
        let output = xai_feed(
            &mut state,
            xai_choice(json!({"content": "<think>reasoning"}), json!("stop")),
        );
        assert!(output
            .join("")
            .contains("response.reasoning_summary_text.done"));
        assert!(output.join("").contains("response.output_item.done"));
        assert_xai_no_terminal(&output);
        assert!(xai_chat_terminal_ready(&state));
        assert_eq!(chat_to_responses_finish(&mut state).len(), 2);
    }

    #[test]
    fn xai_chat_stream_rejects_truncation_and_invalid_finishes() {
        for finish in [
            Value::Null,
            json!(""),
            json!("unknown"),
            json!(1),
            json!(false),
        ] {
            let mut state = xai_state();
            xai_feed(
                &mut state,
                xai_choice(json!({"content": "partial"}), finish),
            );
            assert!(!xai_chat_terminal_ready(&state));
            assert!(chat_to_responses_streaming(b"[DONE]", &mut state).is_empty());
            assert!(chat_to_responses_finish(&mut state).is_empty());
            assert!(state.failure.is_some());
        }
        for incomplete in [
            json!({"choices": []}),
            json!({"usage": {"completion_tokens": 4}}),
            Value::Null,
        ] {
            let mut state = xai_state();
            xai_feed(&mut state, incomplete);
            assert!(chat_to_responses_finish(&mut state).is_empty());
            assert_eq!(state.failure.unwrap().code, "upstream_stream_truncated");
        }
        let mut state = xai_state();
        xai_feed(&mut state, json!({"choices": [{"finish_reason": "stop"}]}));
        assert_eq!(state.failure.unwrap().code, "upstream_stream_invalid_index");

        let mut state = xai_state();
        xai_feed(
            &mut state,
            xai_choice(json!({"content": "partial"}), Value::Null),
        );
        assert!(chat_to_responses_finish(&mut state).is_empty());
        assert_eq!(state.failure.unwrap().code, "upstream_stream_truncated");

        for malformed in [
            json!({"choices": {"finish_reason": "stop"}}),
            xai_choice(json!("invalid delta"), json!("stop")),
        ] {
            let mut state = xai_state();
            xai_feed(&mut state, malformed);
            assert_eq!(
                state.failure.unwrap().code,
                "upstream_stream_invalid_terminal"
            );
        }
    }

    #[test]
    fn xai_chat_stream_rejects_protocol_errors_even_after_finish() {
        for error in [
            json!({"error": {"message": "failed"}}),
            json!({"error": "failed"}),
            json!({"type": "error", "message": "failed"}),
            json!({"type": "response.failed"}),
            json!({"object": "error"}),
            json!({"choices": [{"index": 0, "error": {"message": "failed"}}]}),
        ] {
            let mut state = xai_state();
            xai_feed(
                &mut state,
                xai_choice(json!({"content": "hi"}), json!("stop")),
            );
            assert!(xai_chat_terminal_ready(&state));
            assert!(xai_feed(&mut state, error).is_empty());
            assert!(!xai_chat_terminal_ready(&state));
            assert!(chat_to_responses_finish(&mut state).is_empty());
            assert_eq!(state.failure.unwrap().code, "upstream_stream_failed");
        }
        let mut state = xai_state();
        xai_feed(&mut state, xai_choice(json!({}), json!("stop")));
        assert!(chat_to_responses_streaming(b"{", &mut state).is_empty());
        assert!(chat_to_responses_finish(&mut state).is_empty());
        assert_eq!(state.failure.unwrap().code, "upstream_stream_invalid_json");
    }

    #[test]
    fn xai_chat_stream_finish_gate_tracks_all_observed_choices() {
        let mut state = xai_state();
        xai_feed(
            &mut state,
            json!({"choices": [
                {"index": 0, "delta": {"content": "one"}},
                {"index": 1, "delta": {"content": "two"}}
            ]}),
        );
        xai_feed(&mut state, xai_choice(json!({}), json!("stop")));
        assert!(!xai_chat_terminal_ready(&state));
        assert!(chat_to_responses_finish(&mut state).is_empty());
        assert_eq!(state.failure.unwrap().code, "upstream_stream_truncated");

        let mut state = xai_state();
        xai_feed(
            &mut state,
            json!({"choices": [
            {"index": 0, "finish_reason": "stop"},
            {"index": 1, "finish_reason": null}
        ], "usage": {"completion_tokens": 4}}),
        );
        assert!(!xai_chat_terminal_ready(&state));
        assert!(!state.responses.state.contains_key("usage"));
        xai_feed(
            &mut state,
            json!({"choices": [{"index": 1, "finish_reason": "length"}]}),
        );
        assert!(xai_chat_terminal_ready(&state));
        assert!(xai_completed(&chat_to_responses_finish(&mut state))
            .get("usage")
            .is_none());

        let mut state = xai_state();
        let mut first = xai_choice(json!({}), json!("stop"));
        first["usage"] = json!({"completion_tokens": 4});
        xai_feed(&mut state, first);
        assert!(xai_chat_terminal_ready(&state));
        assert!(state.responses.state.contains_key("usage"));
        xai_feed(&mut state, json!({"choices": [{"index": 5, "delta": {}}]}));
        assert!(!xai_chat_terminal_ready(&state));
        assert!(!state.responses.state.contains_key("usage"));
        xai_feed(
            &mut state,
            json!({"choices": [{"index": 5, "finish_reason": "stop"}]}),
        );
        assert!(xai_chat_terminal_ready(&state));
        assert!(xai_completed(&chat_to_responses_finish(&mut state))
            .get("usage")
            .is_none());
    }

    #[test]
    fn xai_chat_stream_choice_tracking_keeps_existing_bounds() {
        use crate::core::translator::limits::{MAX_STREAM_CHOICES, MAX_STREAM_WIRE_INDEX};

        let mut state = xai_state();
        xai_feed(
            &mut state,
            json!({"choices": [{
                "index": MAX_STREAM_WIRE_INDEX + 1, "finish_reason": "stop"
            }]}),
        );
        assert_eq!(state.failure.unwrap().code, "upstream_stream_index_limit");

        let mut state = xai_state();
        for index in 0..MAX_STREAM_CHOICES {
            xai_feed(
                &mut state,
                json!({"choices": [{"index": index, "delta": {}}]}),
            );
        }
        assert!(state.failure.is_none());
        xai_feed(
            &mut state,
            json!({"choices": [{"index": MAX_STREAM_CHOICES, "delta": {}}]}),
        );
        assert_eq!(state.failure.unwrap().code, "upstream_stream_state_limit");
        assert_eq!(
            state.responses.state["_xaiChoices"]
                .as_object()
                .unwrap()
                .len(),
            MAX_STREAM_CHOICES
        );
    }

    #[test]
    fn xai_chat_policy_is_explicit_and_default_behavior_is_unchanged() {
        let mut state = ResponseTransformState::default();
        assert!(!state.xai_chat);
        state.generic.insert("provider".to_string(), json!("xai"));
        state
            .responses
            .state
            .insert("xai_chat".to_string(), json!(true));
        let mut finish = xai_choice(json!({"content": "hi"}), json!("stop"));
        finish["model"] = json!("grok-unknown");
        finish["usage"] = json!({"prompt_tokens": 32, "completion_tokens": 9, "total_tokens": 135,
            "completion_tokens_details": {"reasoning_tokens": 94}});
        let output = xai_feed(&mut state, finish);
        assert_eq!(xai_completed(&output)["usage"]["output_tokens"], 9);
        assert!(!xai_chat_terminal_ready(&state));
        assert_eq!(
            chat_to_responses_streaming(b"[DONE]", &mut state),
            vec!["data: [DONE]\n\n"]
        );
        assert!(chat_to_responses_finish(&mut state).is_empty());
    }

    #[test]
    fn chat_to_responses_streaming_buffers_split_multi_frame_chunks() {
        let mut state = ResponseTransformState::default();
        let stream = concat!(
            "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"READY\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"glm-5.3-flash\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":16,\"completion_tokens\":25,\"total_tokens\":41,\"prompt_tokens_details\":{\"cached_tokens\":3},\"completion_tokens_details\":{\"reasoning_tokens\":21}}}\n\n",
            "data: [DONE]\n\n"
        );
        let split = stream.find("READY").unwrap() + 2;

        assert!(global_registry()
            .translate_response(
                Format::OpenAi,
                Format::OpenAiResponses,
                &stream.as_bytes()[..split],
                &mut state,
            )
            .unwrap()
            .is_empty());
        let output = global_registry()
            .translate_response(
                Format::OpenAi,
                Format::OpenAiResponses,
                &stream.as_bytes()[split..],
                &mut state,
            )
            .unwrap()
            .join("");

        let text = output.find("response.output_text.delta").unwrap();
        let completed = output.find("response.completed").unwrap();
        assert!(text < completed);
        assert!(output.contains("\"delta\":\"READY\""));
        assert!(output.contains("\"input_tokens\":16"));
        assert!(output.contains("\"reasoning_tokens\":21"));
        assert!(output.contains("\"incomplete_details\":null"));
        assert!(output.contains("data: [DONE]"));
    }

    #[test]
    fn split_usage_tail_completes_once_with_real_usage() {
        // OpenAI split tail: `finish_reason` chunk without usage, then a
        // `choices:[] + usage` chunk. The terminal must carry real usage
        // exactly once, with no fake-zero completed before it.
        let mut state = ResponseTransformState::default();
        let open = json!({
            "id": "chatcmpl-1",
            "model": "gpt-6-luna",
            "choices": [{"index": 0, "delta": {"content": "hi"}, "finish_reason": null}]
        });
        let open_events = chat_to_responses_response(&open, &mut state.responses.state);
        assert!(!open_events
            .iter()
            .any(|e| e.get("event").and_then(|v| v.as_str()) == Some("response.completed")));
        let finish = json!({
            "id": "chatcmpl-1",
            "model": "gpt-6-luna",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        });
        let finish_events = chat_to_responses_response(&finish, &mut state.responses.state);
        assert!(
            !finish_events
                .iter()
                .any(|e| e.get("event").and_then(|v| v.as_str()) == Some("response.completed")),
            "split tail must defer completed until usage: {finish_events:?}"
        );
        let tail = json!({
            "id": "chatcmpl-1",
            "model": "gpt-6-luna",
            "choices": [],
            "usage": {"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13}
        });
        let tail_events = chat_to_responses_response(&tail, &mut state.responses.state);
        let completed: Vec<&Value> = tail_events
            .iter()
            .filter(|e| e.get("event").and_then(|v| v.as_str()) == Some("response.completed"))
            .collect();
        assert_eq!(
            completed.len(),
            1,
            "usage tail completes once: {tail_events:?}"
        );
        let response = &completed[0]["data"]["response"];
        assert_eq!(response["usage"]["input_tokens"], json!(10));
        assert_eq!(response["usage"]["output_tokens"], json!(3));
        assert_eq!(response["usage"]["total_tokens"], json!(13));
        // A following [DONE] must not duplicate the terminal.
        let done_out = chat_to_responses_streaming(b"[DONE]", &mut state);
        assert_eq!(
            done_out
                .iter()
                .filter(|s| s.contains("response.completed"))
                .count(),
            0,
            "completed must stay single: {done_out:?}"
        );
        assert!(done_out.iter().any(|s| s.contains("[DONE]")));
    }

    #[test]
    fn think_close_chunk_with_null_finish_does_not_complete_response() {
        // Regression: a `</think>` chunk (all content consumed by the think
        // machine) takes the early-return path through
        // `emit_tool_calls_and_finish`. Its finish_reason is null mid-stream;
        // treating key presence as terminal emitted response.completed in
        // the middle of the answer (seen in production stream traces).
        let mut state = ResponseTransformState::default();

        // Open a reasoning item via <think>.
        let open = json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "delta": {"content": "<think>pondering"},
                "finish_reason": null
            }]
        });
        let events = chat_to_responses_response(&open, &mut state.responses.state);
        assert!(
            events
                .iter()
                .any(|e| e.get("event").and_then(|v| v.as_str())
                    == Some("response.output_item.added"))
        );

        // Close thinking: content fully consumed → early-return path with
        // finish_reason null. MUST NOT emit response.completed.
        let close = json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "delta": {"content": "</think>"},
                "finish_reason": null
            }]
        });
        let events = chat_to_responses_response(&close, &mut state.responses.state);
        assert!(
            !events
                .iter()
                .any(|e| e.get("event").and_then(|v| v.as_str()) == Some("response.completed")),
            "null finish_reason must not complete the response: {events:?}"
        );

        // Missing finish_reason key entirely: also not terminal.
        let no_key = json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "delta": {"content": "more"},
            }]
        });
        let events = chat_to_responses_response(&no_key, &mut state.responses.state);
        assert!(
            !events
                .iter()
                .any(|e| e.get("event").and_then(|v| v.as_str()) == Some("response.completed")),
            "missing finish_reason must not complete the response: {events:?}"
        );

        // A real terminal chunk closes text; the terminal is deferred until
        // usage arrives or the stream ends (split usage tail must not be
        // orphaned behind an early completed).
        let finish = json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        });
        let events = chat_to_responses_response(&finish, &mut state.responses.state);
        assert!(
            events
                .iter()
                .any(|e| e.get("event").and_then(|v| v.as_str())
                    == Some("response.output_item.done")),
            "finish must close the message: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| e.get("event").and_then(|v| v.as_str()) == Some("response.completed")),
            "finish without usage defers completed until [DONE]/EOF: {events:?}"
        );
        let flushed = chat_to_responses_finish(&mut state);
        let completed = flushed
            .iter()
            .filter(|s| s.contains("response.completed"))
            .count();
        assert_eq!(completed, 1, "exactly one deferred completion: {flushed:?}");
        assert!(
            !flushed.join("").contains("\"usage\""),
            "no usage must not be fabricated: {flushed:?}"
        );
    }

    #[test]
    fn empty_tool_calls_array_does_not_trigger_tool_call_processing() {
        // Regression test for openproxy-mfs3.4 (9router v0.5.55 parity).
        // Some providers attach an empty tool_calls: [] to every SSE chunk.
        // Without the guard, this would enter the tool_calls block and could
        // cause the message to close prematurely on the first content token.
        let mut state = ResponseTransformState::default();

        // First chunk: content + empty tool_calls (chat completions format input
        // to chat_to_responses_response, which converts to Responses API format).
        let chunk1 = json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "content": "Hello",
                    "tool_calls": []
                },
                "finish_reason": null
            }]
        });
        let events = chat_to_responses_response(&chunk1, &mut state.responses.state);

        // The empty tool_calls should NOT produce any function_call/tool_use events.
        // (response.output_item events for message content are expected.)
        let has_function_call = events.iter().any(|e| {
            let s = serde_json::to_string(e).unwrap_or_default();
            s.contains("function_call")
                || s.contains("tool_use")
                || s.contains("function_call_output")
        });
        assert!(
            !has_function_call,
            "empty tool_calls array should not produce function_call/tool_use events, got: {:?}",
            events
        );

        // Content "Hello" should still be present in the output.
        let has_content = events.iter().any(|e| {
            let s = serde_json::to_string(e).unwrap_or_default();
            s.contains("Hello")
        });
        assert!(
            has_content,
            "content should still be present, got: {:?}",
            events
        );
    }

    #[test]
    fn non_empty_tool_calls_still_processed() {
        let mut state = ResponseTransformState::default();

        let start = json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "glob",
                            "arguments": ""
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let fragment = |arguments: &str| {
            json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": {"arguments": arguments}
                        }]
                    },
                    "finish_reason": null
                }]
            })
        };
        let finish = json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }]
        });

        let mut events = chat_to_responses_response(&start, &mut state.responses.state);
        events.extend(chat_to_responses_response(
            &fragment("{\"pattern\":\""),
            &mut state.responses.state,
        ));
        events.extend(chat_to_responses_response(
            &fragment("**/*.rs\"}"),
            &mut state.responses.state,
        ));
        events.extend(chat_to_responses_response(
            &finish,
            &mut state.responses.state,
        ));

        let added_count = events
            .iter()
            .filter(|event| {
                event["event"] == "response.output_item.added"
                    && event["data"]["item"]["type"] == "function_call"
            })
            .count();
        assert_eq!(added_count, 1);

        let deltas: Vec<&str> = events
            .iter()
            .filter(|event| event["event"] == "response.function_call_arguments.delta")
            .filter_map(|event| event["data"]["delta"].as_str())
            .collect();
        assert_eq!(deltas, vec!["{\"pattern\":\"", "**/*.rs\"}"]);

        let arguments = "{\"pattern\":\"**/*.rs\"}";
        let arguments_done = events
            .iter()
            .find(|event| event["event"] == "response.function_call_arguments.done")
            .expect("function arguments done event");
        assert_eq!(arguments_done["data"]["arguments"], arguments);

        let item_done = events
            .iter()
            .find(|event| {
                event["event"] == "response.output_item.done"
                    && event["data"]["item"]["type"] == "function_call"
            })
            .expect("function item done event");
        assert_eq!(item_done["data"]["item"]["arguments"], arguments);
        assert_eq!(item_done["data"]["item"]["name"], "glob");
        assert_eq!(item_done["data"]["item"]["status"], "completed");
        assert!(
            !events
                .iter()
                .any(|event| event["event"] == "response.completed"),
            "tool-call finish without usage defers completed until [DONE]/EOF"
        );
        let flushed = chat_to_responses_finish(&mut state);
        assert!(
            flushed.iter().any(|s| s.contains("response.completed")),
            "deferred tool-call finish should complete on EOF"
        );
    }

    #[test]
    fn parallel_tool_calls_keep_separate_indices() {
        // JS parity (openai-responses.js:449-539): item_id → index map keeps
        // parallel calls separate when upstream emits all addeds before
        // any delta; done-with-args emits only when no deltas were seen.
        let mut state = serde_json::Map::new();
        let added = |id: &str, call_id: &str, name: &str| {
            json!({
                "type": "response.output_item.added",
                "item": {"id": id, "type": "function_call", "call_id": call_id, "name": name}
            })
        };
        let c1 = responses_to_chat_response(&added("item_a", "call_a", "fa"), &mut state);
        let c2 = responses_to_chat_response(&added("item_b", "call_b", "fb"), &mut state);
        let idx = |chunks: &[Value]| {
            chunks[0]["choices"][0]["delta"]["tool_calls"][0]["index"]
                .as_u64()
                .unwrap()
        };
        assert_eq!(idx(&c1), 0);
        assert_eq!(idx(&c2), 1);

        // Deltas routed by item_id — interleaved fragments stay separate.
        let d1 = responses_to_chat_response(
            &json!({"type": "response.function_call_arguments.delta", "item_id": "item_b", "delta": "{\"x\":1}"}),
            &mut state,
        );
        assert_eq!(idx(&d1), 1);

        // Done with full args but deltas already emitted → nothing.
        let done_b = responses_to_chat_response(
            &json!({"type": "response.output_item.done", "item": {"id": "item_b", "type": "function_call", "arguments": "{\"x\":1}"}}),
            &mut state,
        );
        assert!(done_b.is_empty());

        // Done with full args and no deltas → emits once on its own index.
        let mut state2 = serde_json::Map::new();
        let _ = responses_to_chat_response(&added("item_c", "call_c", "fc"), &mut state2);
        let done_c = responses_to_chat_response(
            &json!({"type": "response.output_item.done", "item": {"id": "item_c", "type": "function_call", "arguments": "{\"y\":2}"}}),
            &mut state2,
        );
        assert_eq!(done_c.len(), 1);
        assert_eq!(idx(&done_c), 0);
        assert!(
            done_c[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap()
                .contains("\"y\"")
        );
    }

    #[test]
    fn empty_finish_reason_does_not_close_message() {
        // JS parity (openai-responses.js:111 `if (choice.finish_reason)`):
        // "" is falsy and must not close the message / emit completed.
        let mut state = serde_json::Map::new();
        let chunk = json!({
            "choices": [{
                "index": 0,
                "delta": { "content": "Hi" },
                "finish_reason": ""
            }]
        });
        let events = chat_to_responses_response(&chunk, &mut state);
        let sse = serde_json::to_string(&events).unwrap_or_default();
        assert!(
            !sse.contains("response.completed"),
            "empty finish_reason must not emit completed, got: {sse}"
        );
    }

    #[test]
    fn added_time_key_falls_back_to_current_tool_call_id() {
        // JS parity (openai-responses.js:~483): the added-time map key is
        // item.id || data.item_id || state.currentToolCallId. An added event
        // with no id/item_id still keys on the call_id, so a later duplicate
        // added reuses the same index instead of advancing.
        let mut state = serde_json::Map::new();
        let added_no_id = json!({
            "type": "response.output_item.added",
            "item": {"type": "function_call", "call_id": "call_x", "name": "fx"}
        });
        let c1 = responses_to_chat_response(&added_no_id, &mut state);
        let c2 = responses_to_chat_response(&added_no_id, &mut state);
        let idx = |chunks: &[Value]| {
            chunks[0]["choices"][0]["delta"]["tool_calls"][0]["index"]
                .as_u64()
                .unwrap()
        };
        assert_eq!(idx(&c1), 0);
        assert_eq!(
            idx(&c2),
            0,
            "duplicate added must reuse the call_id-keyed index"
        );
    }

    #[test]
    fn reasoning_vendor_shapes_fall_back() {
        // JS concerns/reasoning.js extractReasoningText: reasoning_content →
        // reasoning → reasoning_details[].
        for (delta, expect) in [
            (
                serde_json::json!({"reasoning": "compat-text"}),
                "compat-text",
            ),
            (
                serde_json::json!({"reasoning_details": [{"text": "a"}, {"content": "b"}]}),
                "ab",
            ),
        ] {
            let mut state = ResponseTransformState::default();
            let chunk = serde_json::json!({
                "id": "x", "choices": [{"index": 0, "delta": delta, "finish_reason": null}]
            });
            let events = chat_to_responses_response(&chunk, &mut state.responses.state);
            let found = events.iter().any(|e| {
                serde_json::to_string(e)
                    .unwrap_or_default()
                    .contains(expect)
            });
            assert!(found, "expected {expect} in {events:?}");
        }
    }

    #[test]
    fn think_tags_route_into_reasoning() {
        // JS 76-100 <think> state machine: <think> content → reasoning
        // deltas, </think> closes reasoning and resumes text.
        let mut state = ResponseTransformState::default();
        let chunk = serde_json::json!({
            "id": "x",
            "choices": [{"index": 0,
                "delta": {"content": "<think>hmm</think>hi"},
                "finish_reason": null}]
        });
        let events = chat_to_responses_response(&chunk, &mut state.responses.state);
        let s = serde_json::to_string(&events).unwrap_or_default();
        assert!(
            s.contains("reasoning_summary_text.delta"),
            "think → reasoning: {s}"
        );
        assert!(
            s.contains("reasoning_summary_text.done"),
            "think closed: {s}"
        );
        assert!(s.contains("output_text.delta"), "resumed text: {s}");
    }

    #[test]
    fn custom_tool_calls_emit_custom_events() {
        // JS 261-366: custom tools announce custom_tool_call and close with
        // unwrapped input ({"input":"..."} → freeform).
        let mut state = ResponseTransformState::default();
        state
            .responses
            .state
            .insert("customToolNames".to_string(), serde_json::json!("exec"));
        let chunk = serde_json::json!({
            "id": "x",
            "choices": [{"index": 0,
                "delta": {"tool_calls": [{
                    "index": 0, "id": "call_1", "type": "function",
                    "function": {"name": "exec", "arguments": "{\"input\":\"ls\"}"}
                }]},
                "finish_reason": null}]
        });
        let events = chat_to_responses_response(&chunk, &mut state.responses.state);
        let s = serde_json::to_string(&events).unwrap_or_default();
        assert!(s.contains("custom_tool_call"), "custom announce: {s}");
        assert!(
            !s.contains("function_call_arguments.delta"),
            "no streamed JSON fragments for custom: {s}"
        );
        // Close path: finish_reason flushes custom input unwrapped.
        let done = serde_json::json!({
            "id": "x", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
        });
        let events2 = chat_to_responses_response(&done, &mut state.responses.state);
        let s2 = serde_json::to_string(&events2).unwrap_or_default();
        assert!(
            s2.contains("custom_tool_call_input.done"),
            "custom close: {s2}"
        );
        assert!(s2.contains("\"input\":\"ls\""), "unwrapped input: {s2}");
    }

    #[test]
    fn refusal_error_event_keeps_flat_shape_and_adds_nested_error() {
        let frame = format_error_event(
            503,
            "invalid_prompt",
            "Claude refused to respond (stop_reason=refusal).",
        );
        assert!(frame.starts_with("event: error\ndata: "));
        let payload: Value =
            serde_json::from_str(frame.strip_prefix("event: error\ndata: ").unwrap().trim())
                .expect("error payload JSON");
        assert_eq!(payload["type"], "error");
        assert_eq!(payload["sequence_number"], 503);
        assert_eq!(payload["code"], "invalid_prompt");
        assert_eq!(
            payload["message"],
            "Claude refused to respond (stop_reason=refusal)."
        );
        assert!(payload.get("param").is_some_and(Value::is_null));
        assert_eq!(payload["error"]["type"], "invalid_request_error");
        assert_eq!(payload["error"]["code"], "invalid_prompt");
        assert_eq!(
            payload["error"]["message"],
            "Claude refused to respond (stop_reason=refusal)."
        );
    }

    #[test]
    fn ordinary_error_event_shape_is_unchanged() {
        let frame = format_error_event(5, "upstream_stream_error", "Upstream stream error");
        assert_eq!(frame, "event: error\ndata: {\"code\":\"upstream_stream_error\",\"message\":\"Upstream stream error\",\"param\":null,\"sequence_number\":5,\"type\":\"error\"}\n\n");
    }
}
