use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use chrono::Utc;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Map, Value};

use crate::core::stream_framing::{FrameError, SseEvent, SseFramer};
use crate::core::translator::registry::Format;
use crate::core::translator::response_transform::{
    AnthropicToOpenAiTransformer, StreamingTransformer,
};
use crate::server::state::AppState;

use super::chat;

pub async fn cors_options() -> Response {
    cors_preflight_response("POST, OPTIONS")
}

pub async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let model = body
        .as_ref()
        .ok()
        .and_then(|b| b.get("model").and_then(|m| m.as_str()));
    let request_id = crate::server::request_logger::new_request_id();
    let _log = crate::server::request_logger::RequestLog::start(
        "POST",
        "/v1/messages",
        model,
        Some(request_id.clone()),
    );
    let response = forward_compat(state, headers, body, CompatMode::Messages).await;
    let response = crate::server::request_logger::attach_request_id(response, &request_id);
    _log.watch(response)
}

pub async fn responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let model = body
        .as_ref()
        .ok()
        .and_then(|b| b.get("model").and_then(|m| m.as_str()));
    let request_id = crate::server::request_logger::new_request_id();
    let _log = crate::server::request_logger::RequestLog::start(
        "POST",
        "/v1/responses",
        model,
        Some(request_id.clone()),
    );
    let response = forward_compat(
        state,
        headers,
        body,
        CompatMode::Responses { compact: false },
    )
    .await;
    let response = crate::server::request_logger::attach_request_id(response, &request_id);
    _log.watch(response)
}

pub async fn responses_compact(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let model = body
        .as_ref()
        .ok()
        .and_then(|b| b.get("model").and_then(|m| m.as_str()));
    let request_id = crate::server::request_logger::new_request_id();
    let _log = crate::server::request_logger::RequestLog::start(
        "POST",
        "/v1/responses/compact",
        model,
        Some(request_id.clone()),
    );
    let response = forward_compat(
        state,
        headers,
        body,
        CompatMode::Responses { compact: true },
    )
    .await;
    let response = crate::server::request_logger::attach_request_id(response, &request_id);
    _log.watch(response)
}

pub async fn count_tokens(
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(error) => return invalid_json_response(error, &headers, "/v1/messages/count_tokens"),
    };

    let total_chars = count_request_chars(&body);
    let input_tokens = total_chars.div_ceil(4) as u64;

    with_cors_response(Json(json!({ "input_tokens": input_tokens })).into_response())
}

#[derive(Clone, Copy)]
enum CompatMode {
    Messages,
    Responses { compact: bool },
}

async fn forward_compat(
    state: AppState,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
    mode: CompatMode,
) -> Response {
    let endpoint = match mode {
        CompatMode::Messages => "/v1/messages",
        CompatMode::Responses { compact: false } => "/v1/responses",
        CompatMode::Responses { compact: true } => "/v1/responses/compact",
    };
    let Json(body) = match body {
        Ok(body) => body,
        Err(error) => return invalid_json_response(error, &headers, endpoint),
    };

    let normalized = normalize_body(body, mode);
    let stream_request = normalized
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let response =
        chat::chat_completions_for_endpoint(state, headers, Ok(Json(normalized)), Some(endpoint))
            .await;

    let native_format = response
        .extensions()
        .get::<chat::RoutedResponseFormats>()
        .copied();
    let expected_format = match mode {
        CompatMode::Messages => Format::Claude,
        CompatMode::Responses { .. } => Format::OpenAiResponses,
    };
    let sanitize_injected_search = response
        .extensions()
        .get::<chat::CodexWebSearchInjected>()
        .is_some();
    if should_bypass_compat_conversion(
        stream_request,
        native_format,
        expected_format,
        sanitize_injected_search,
    ) {
        return with_cors_response(response);
    }

    match mode {
        CompatMode::Responses { .. } => {
            with_cors_response(convert_to_responses_api(response, stream_request).await)
        }
        CompatMode::Messages => with_cors_response(convert_to_messages_api(response).await),
    }
}

fn should_bypass_compat_conversion(
    stream_request: bool,
    routed: Option<chat::RoutedResponseFormats>,
    expected_format: Format,
    sanitize_injected_search: bool,
) -> bool {
    stream_request
        && !sanitize_injected_search
        && routed.is_some_and(|formats| {
            formats.native_passthrough
                && formats.client == expected_format
                && formats.upstream == expected_format
        })
}

// ---------------------------------------------------------------------------
// Responses API SSE format conversion
// ---------------------------------------------------------------------------

/// Convert an OpenAI chat-completion response (streaming or non-streaming) to
/// the OpenAI Responses API format.
///
/// `stream_request` indicates whether the client requested SSE (streaming)
/// or a single JSON response (non-streaming). When the upstream returns
/// Responses API SSE events for a non-streaming request, we collect them
/// and reconstruct the final JSON.
async fn convert_to_responses_api(response: Response, stream_request: bool) -> Response {
    let sanitize_injected_search = response
        .extensions()
        .get::<chat::CodexWebSearchInjected>()
        .is_some();
    let status = response.status();
    if !status.is_success() {
        return response;
    }

    let is_sse = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream") || ct.contains("text/plain"));

    if !is_sse {
        // Non-streaming — collect the whole body and convert the JSON.
        // If the upstream returned a Claude-format body (type: message),
        // convert it to OpenAI format first (matching 9router's translateNonStreamingResponse).
        let (parts, body) = response.into_parts();
        let body_bytes = match body.collect().await {
            Ok(col) => col.to_bytes(),
            Err(_) => return Response::from_parts(parts, Body::empty()),
        };
        let Ok(mut body_value) = serde_json::from_slice::<Value>(&body_bytes) else {
            return (status, body_bytes).into_response();
        };

        if body_value.get("object").and_then(Value::as_str) == Some("response") {
            if sanitize_injected_search {
                remove_injected_web_search_items(&mut body_value);
            }
            return Json(body_value).into_response();
        }

        // Detect Claude-format body: {"type":"message","content":[...],"stop_reason":"end_turn"}
        let chat_completion = if body_value.get("type").and_then(Value::as_str) == Some("message") {
            claude_body_to_chat_completion(&body_value)
        } else {
            body_value
        };

        let responses_json = chat_completion_to_responses_json(&chat_completion);
        return Json(responses_json).into_response();
    }

    if stream_request {
        return stream_to_responses_api(response, sanitize_injected_search);
    }

    // Streaming or pseudo-streaming — wrap the body through the SSE converter.
    // The upstream may return OpenAI SSE (chat.completion.chunk) or
    // Anthropic SSE (message_start / content_block_* / message_delta).
    // We detect Claude format by looking for "type":"message_start" in the data.
    //
    // NOTE: Some upstreams (esp. opencode-go) return a non-streaming
    //       chat.completion JSON body with Content-Type: text/event-stream.
    //       We detect this case and handle it as non-streaming by trying
    //       to parse the full body as a single chat.completion JSON first.
    let (parts, body) = response.into_parts();
    let body_bytes = match body.collect().await {
        Ok(col) => col.to_bytes(),
        Err(_) => return Response::from_parts(parts, Body::empty()),
    };

    // Validate the complete collected SSE body before accepting any event.
    // This prevents a valid first event from masking an oversized or invalid
    // tail when no response.* marker is present.
    let bare_json = serde_json::from_slice::<Value>(&body_bytes).ok();
    let inspection = if bare_json.is_none() {
        match inspect_collected_responses_sse(&body_bytes) {
            Ok(inspection) => Some(inspection),
            Err(error) => return precommit_framing_error_response(error),
        }
    } else {
        None
    };
    let pseudo_streaming_json = bare_json.clone().or_else(|| {
        inspection
            .as_ref()
            .and_then(|value| value.first_json.clone())
    });
    if let Some(body_value) = pseudo_streaming_json {
        let is_pseudo_streaming = body_value
            .get("object")
            .and_then(|o| o.as_str())
            .is_some_and(|o| o == "chat.completion")
            && body_value
                .get("choices")
                .and_then(|a| a.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("message"))
                .is_some();

        if is_pseudo_streaming {
            // Convert directly as non-streaming JSON (skip SSE wrapping)
            let chat_completion =
                if body_value.get("type").and_then(Value::as_str) == Some("message") {
                    claude_body_to_chat_completion(&body_value)
                } else {
                    body_value
                };
            let responses_json = chat_completion_to_responses_json(&chat_completion);
            return Json(responses_json).into_response();
        }
    }

    // A non-streaming client may still receive forced SSE from an upstream.
    // Prefer the final native Responses object when present; otherwise reuse
    // the incremental converter over the already-collected body.
    if let Some(mut responses_json) = inspection.and_then(|value| value.completed_response) {
        if sanitize_injected_search {
            remove_injected_web_search_items(&mut responses_json);
        }
        return Json(responses_json).into_response();
    }
    stream_to_responses_api(
        Response::from_parts(parts, Body::from(body_bytes)),
        sanitize_injected_search,
    )
}

fn stream_to_responses_api(response: Response, sanitize_injected_search: bool) -> Response {
    let status = response.status();
    let (parts, body) = response.into_parts();
    let mut upstream = body.into_data_stream();
    let converted = async_stream::stream! {
        let mut framer = SseFramer::new();
        let mut conv_state = ResponsesSseState::new();
        let mut claude_xform = AnthropicToOpenAiTransformer::new();
        let mut native_responses_stream = false;

        while let Some(next) = upstream.next().await {
            let chunk = match next {
                Ok(chunk) => chunk,
                Err(_) => return,
            };
            let mut converted_frames = Vec::new();
            let mut conversion_error = None;
            let frame_result = framer.feed(&chunk, |event| {
                if conversion_error.is_some() {
                    return;
                }
                match convert_responses_sse_event(
                    &event,
                    &mut conv_state,
                    &mut claude_xform,
                    &mut native_responses_stream,
                    sanitize_injected_search,
                ) {
                    Ok(outputs) => converted_frames.extend(outputs),
                    Err(error) => conversion_error = Some(error),
                }
            });
            for output in converted_frames {
                yield Ok::<Bytes, std::io::Error>(output);
            }
            if let Some(error) = conversion_error {
                yield Ok::<Bytes, std::io::Error>(Bytes::from(format_sse_event(
                    "error",
                    &json!({"type": "error", "error": {"message": error.message, "code": error.code}}),
                )));
                return;
            }
            if let Err(error) = frame_result {
                yield Ok::<Bytes, std::io::Error>(compat_framing_error_event(&error));
                return;
            }
        }

        let mut converted_frames = Vec::new();
        let mut conversion_error = None;
        let frame_result = framer.finish(|event| {
            if conversion_error.is_some() {
                return;
            }
            match convert_responses_sse_event(
                &event,
                &mut conv_state,
                &mut claude_xform,
                &mut native_responses_stream,
                sanitize_injected_search,
            ) {
                Ok(outputs) => converted_frames.extend(outputs),
                Err(error) => conversion_error = Some(error),
            }
        });
        for output in converted_frames {
            yield Ok::<Bytes, std::io::Error>(output);
        }
        if let Some(error) = conversion_error {
            yield Ok::<Bytes, std::io::Error>(Bytes::from(format_sse_event(
                "error",
                &json!({"type": "error", "error": {"message": error.message, "code": error.code}}),
            )));
            return;
        }
        if let Err(error) = frame_result {
            yield Ok::<Bytes, std::io::Error>(compat_framing_error_event(&error));
            return;
        }

        if !native_responses_stream {
            let flushed = match conv_state.flush_frames() {
                Ok(flushed) => flushed,
                Err(error) => {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(format_sse_event(
                        "error",
                        &json!({"type": "error", "error": {"message": error.message, "code": error.code}}),
                    )));
                    return;
                }
            };
            for event_bytes in flushed {
                yield Ok::<Bytes, std::io::Error>(Bytes::from(event_bytes));
            }
            yield Ok::<Bytes, std::io::Error>(Bytes::from_static(b"data: [DONE]\n\n"));
        }
    };

    let mut response = Response::new(Body::from_stream(converted));
    *response.status_mut() = status;
    for (name, value) in &parts.headers {
        if name != header::CONTENT_LENGTH
            && name != header::CONTENT_TYPE
            && name != header::TRANSFER_ENCODING
        {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response.headers_mut().insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response.headers_mut().insert(
        header::HeaderName::from_static("connection"),
        HeaderValue::from_static("keep-alive"),
    );
    response
}

fn convert_responses_sse_event(
    event: &SseEvent<'_>,
    conv_state: &mut ResponsesSseState,
    claude_xform: &mut AnthropicToOpenAiTransformer,
    native_responses_stream: &mut bool,
    sanitize_injected_search: bool,
) -> Result<Vec<Bytes>, crate::core::translator::limits::StreamLimitError> {
    let frame = event.raw().trim();
    if frame.is_empty() {
        return Ok(Vec::new());
    }
    let Some(json_str) = event
        .data()
        .or_else(|| frame.starts_with('{').then_some(frame))
    else {
        return Ok(Vec::new());
    };
    if json_str == "[DONE]" {
        return Ok(Vec::new());
    }
    let Ok(mut value) = serde_json::from_str::<Value>(json_str) else {
        return Ok(Vec::new());
    };
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    if event_type.starts_with("response.") {
        *native_responses_stream = true;
        if sanitize_injected_search && should_drop_injected_web_search_event(&value) {
            return Ok(Vec::new());
        }
        if sanitize_injected_search && event_type == "response.completed" {
            remove_injected_web_search_items(&mut value);
            return Ok(vec![Bytes::from(format_sse_event(&event_type, &value))]);
        }
        return Ok(vec![Bytes::from(format!("{frame}\n\n"))]);
    }

    if value.get("object").and_then(Value::as_str) == Some("chat.completion") {
        *native_responses_stream = true;
        let response = chat_completion_to_responses_json(&value);
        return Ok(vec![Bytes::from(format_sse_event(
            "response.completed",
            &json!({"type": "response.completed", "response": response}),
        ))]);
    }

    let is_claude_event = matches!(
        event_type.as_str(),
        "message_start"
            | "message_delta"
            | "message_stop"
            | "ping"
            | "content_block_start"
            | "content_block_delta"
            | "content_block_stop"
    );
    if is_claude_event {
        if event_type == "ping" {
            return Ok(Vec::new());
        }
        let mut output = Vec::new();
        for chunk in claude_xform
            .transform_chunk(&Bytes::from(format!("data: {json_str}\n\n")))
            .into_iter()
            .filter_map(|line| line.strip_prefix("data: ").map(str::to_string))
            .filter(|data| data != "[DONE]")
            .filter_map(|data| serde_json::from_str::<Value>(&data).ok())
        {
            output.extend(
                openai_chunk_to_responses(conv_state, &chunk)?
                    .into_iter()
                    .map(Bytes::from),
            );
        }
        return Ok(output);
    }

    Ok(openai_chunk_to_responses(conv_state, &value)?
        .into_iter()
        .map(Bytes::from)
        .collect())
}

fn should_drop_injected_web_search_event(value: &Value) -> bool {
    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    event_type.starts_with("response.web_search_call.")
        || (matches!(
            event_type,
            "response.output_item.added" | "response.output_item.done"
        ) && value
            .get("item")
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            == Some("web_search_call"))
}

fn remove_injected_web_search_items(response: &mut Value) {
    let output = if response.get("response").is_some() {
        response
            .get_mut("response")
            .and_then(|response| response.get_mut("output"))
    } else {
        response.get_mut("output")
    };
    if let Some(output) = output.and_then(Value::as_array_mut) {
        output.retain(|item| item.get("type").and_then(Value::as_str) != Some("web_search_call"));
    }
}

/// Extract the final Responses API JSON from a series of Responses API SSE events.
/// Finds the `response.completed` event and returns its `response` payload.
struct CollectedSseInspection {
    first_json: Option<Value>,
    completed_response: Option<Value>,
}

fn inspect_collected_responses_sse(body: &[u8]) -> Result<CollectedSseInspection, FrameError> {
    let mut inspection = CollectedSseInspection {
        first_json: None,
        completed_response: None,
    };
    let mut framer = SseFramer::new();
    let mut inspect = |event: SseEvent<'_>| {
        let Some(json_str) = event.data() else {
            return;
        };
        if let Ok(v) = serde_json::from_str::<Value>(json_str) {
            if inspection.first_json.is_none() {
                inspection.first_json = Some(v.clone());
            }
            if v.get("type").and_then(|t| t.as_str()) == Some("response.completed") {
                if let Some(response) = v.get("response").cloned() {
                    inspection.completed_response = Some(response);
                }
            }
        }
    };
    framer.feed(body, &mut inspect)?;
    framer.finish(inspect)?;
    Ok(inspection)
}

fn compat_framing_error_event(error: &FrameError) -> Bytes {
    Bytes::from(format_sse_event(
        "error",
        &json!({"type": "error", "error": {"message": error.to_string(), "code": error.code()}}),
    ))
}

fn precommit_framing_error_response(error: FrameError) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        [(header::CONTENT_TYPE, "application/json")],
        json!({
            "error": {
                "message": error.to_string(),
                "type": "upstream_error",
                "code": error.code(),
            }
        })
        .to_string(),
    )
        .into_response()
}

/// Convert a completed chat-completion JSON object to a Responses API JSON object.
/// Used for non-streaming responses.
fn chat_completion_to_responses_json(source: &Value) -> Value {
    let response_id = source
        .get("id")
        .and_then(|v| v.as_str())
        .map(|id| {
            if let Some(stripped) = id.strip_prefix("chatcmpl-") {
                format!("resp_{}", stripped)
            } else {
                format!("resp_{}", id)
            }
        })
        .unwrap_or_else(|| format!("resp_{:x}", Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    let created = source
        .get("created")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| Utc::now().timestamp());
    let model = source.get("model").and_then(|v| v.as_str()).unwrap_or("");

    let mut output = Vec::new();
    let mut usage = None;

    if let Some(choices) = source.get("choices").and_then(|v| v.as_array()) {
        for (idx, choice) in choices.iter().enumerate() {
            let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());
            if let Some(message) = choice.get("message") {
                let role = message
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("assistant");
                let content = message
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let tool_calls = message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();

                // Build content parts array
                let mut content_parts = Vec::new();

                // Reasoning content (non-standard field like DeepSeek)
                // Emit as a SEPARATE reasoning item at output level, not as a
                // content part inside the message — matching Responses API spec
                // where `reasoning` is a top-level output item.
                if let Some(reasoning) = message.get("reasoning_content").and_then(|v| v.as_str()) {
                    if !reasoning.is_empty() {
                        output.push(json!({
                            "id": format!("item_{}_reasoning", idx),
                            "type": "reasoning",
                            "role": role,
                            "content": [{
                                "id": format!("item_{}_reasoning_summary", idx),
                                "type": "summary_text",
                                "text": reasoning,
                            }],
                        }));
                    }
                }

                if !content.is_empty() || tool_calls.is_empty() {
                    content_parts.push(json!({
                        "id": format!("item_{}_text", idx),
                        "type": "output_text",
                        "text": content,
                        "annotations": [],
                    }));

                    output.push(json!({
                        "id": format!("item_{}", idx),
                        "type": "message",
                        "role": role,
                        "status": if finish_reason.is_some() { "completed" } else { "in_progress" },
                        "content": content_parts,
                    }));
                }

                for (tool_idx, tool_call) in tool_calls.iter().enumerate() {
                    let function = tool_call.get("function").unwrap_or(tool_call);
                    let call_id = tool_call
                        .get("id")
                        .or_else(|| tool_call.get("call_id"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    output.push(json!({
                        "id": format!("fc_{}_{}", idx, tool_idx),
                        "type": "function_call",
                        "status": "completed",
                        "call_id": call_id,
                        "name": function.get("name").and_then(Value::as_str).unwrap_or(""),
                        "arguments": function.get("arguments").and_then(Value::as_str).unwrap_or("{}"),
                    }));
                }
            }
        }
    }

    if let Some(u) = source.get("usage") {
        let mut mapped = json!({});
        if let Some(m) = mapped.as_object_mut() {
            if let Some(v) = u.get("prompt_tokens").and_then(Value::as_u64) {
                m.insert("input_tokens".into(), json!(v));
            }
            if let Some(v) = u.get("completion_tokens").and_then(Value::as_u64) {
                m.insert("output_tokens".into(), json!(v));
            }
            if let Some(v) = u.get("total_tokens").and_then(Value::as_u64) {
                m.insert("total_tokens".into(), json!(v));
            }
        }
        usage = Some(mapped);
    }

    let mut resp = json!({
        "id": response_id,
        "object": "response",
        "created_at": created,
        "status": "completed",
        "model": model,
        "output": output,
    });

    if let Some(u) = usage {
        resp.as_object_mut().unwrap().insert("usage".to_string(), u);
    }

    resp
}

/// State machine that tracks the SSE conversion from chat.completion.chunk
/// events to Responses API events.
struct ResponsesSseState {
    started: bool,
    response_id: String,
    created: i64,
    model: String,
    seq: u64,
    // Per-choice-index tracking
    msg_item_added: BTreeMap<usize, bool>,
    msg_content_added: BTreeMap<usize, bool>,
    msg_text_buf: BTreeMap<usize, String>,
    msg_item_done: BTreeMap<usize, bool>,
    added_item_id_map: BTreeMap<usize, String>,
    added_content_part_id_map: BTreeMap<usize, String>,
    // Reasoning tracking
    reasoning_id: Option<String>,
    reasoning_buf: Option<String>,
    reasoning_done: bool,
    reasoning_item_added: bool,
    reasoning_content_added: bool,
    // Global state
    completed_sent: bool,
    choice_indices: BTreeSet<usize>,
    retained_bytes: usize,
}

impl ResponsesSseState {
    fn new() -> Self {
        Self {
            started: false,
            response_id: String::new(),
            created: 0,
            model: String::new(),
            seq: 0,
            msg_item_added: BTreeMap::new(),
            msg_content_added: BTreeMap::new(),
            msg_text_buf: BTreeMap::new(),
            msg_item_done: BTreeMap::new(),
            added_item_id_map: BTreeMap::new(),
            added_content_part_id_map: BTreeMap::new(),
            reasoning_id: None,
            reasoning_buf: None,
            reasoning_done: false,
            reasoning_item_added: false,
            reasoning_content_added: false,
            completed_sent: false,
            choice_indices: BTreeSet::new(),
            retained_bytes: 0,
        }
    }

    fn choice_index(
        &mut self,
        choice: &Value,
    ) -> Result<usize, crate::core::translator::limits::StreamLimitError> {
        use crate::core::translator::limits::{wire_index, StreamLimitError, MAX_STREAM_CHOICES};

        let index = wire_index(choice.get("index"), "choices[].index")?;
        let index = usize::try_from(index)
            .map_err(|_| StreamLimitError::arithmetic("choice index conversion"))?;
        if !self.choice_indices.contains(&index) && self.choice_indices.len() >= MAX_STREAM_CHOICES
        {
            return Err(StreamLimitError::too_many(
                "response choices",
                MAX_STREAM_CHOICES,
            ));
        }
        self.choice_indices.insert(index);
        Ok(index)
    }
}

fn advance_response_seq(
    seq: &mut u64,
) -> Result<(), crate::core::translator::limits::StreamLimitError> {
    *seq = seq.checked_add(1).ok_or_else(|| {
        crate::core::translator::limits::StreamLimitError::arithmetic("response sequence")
    })?;
    Ok(())
}

/// Flush any incomplete event state and emit `response.completed` if it
/// hasn't been sent yet.  Returns SSE event frames that should be yielded
/// to the client.  Matching 9router's `responsesTransformer.js` `flush()`.
impl ResponsesSseState {
    fn flush_frames(
        &mut self,
    ) -> Result<Vec<Vec<u8>>, crate::core::translator::limits::StreamLimitError> {
        let mut frames: Vec<Vec<u8>> = Vec::new();
        if self.completed_sent {
            return Ok(frames); // already flushed
        }

        // Close any still-open message items (no finish_reason arrived)
        let indices: Vec<usize> = self
            .msg_item_added
            .keys()
            .copied()
            .filter(|idx| !self.msg_item_done.contains_key(idx) || !self.msg_item_done[idx])
            .collect();
        for &idx in &indices {
            let text = self
                .msg_text_buf
                .get(&idx)
                .map(|s| s.as_str())
                .unwrap_or("");
            let part_id = self
                .added_content_part_id_map
                .get(&idx)
                .map(|s| s.as_str())
                .unwrap_or("");
            let item_id = self
                .added_item_id_map
                .get(&idx)
                .map(|s| s.as_str())
                .unwrap_or("");

            advance_response_seq(&mut self.seq)?;
            frames.push(format_sse_event(
                "response.output_text.done",
                &json!({
                    "type": "response.output_text.done",
                    "part_index": idx,
                    "text": text,
                }),
            ));

            advance_response_seq(&mut self.seq)?;
            frames.push(format_sse_event(
                "response.content_part.done",
                &json!({
                    "type": "response.content_part.done",
                    "part_index": idx,
                    "part": {
                        "id": part_id,
                        "type": "output_text",
                        "text": text,
                        "annotations": [],
                    },
                }),
            ));

            advance_response_seq(&mut self.seq)?;
            frames.push(format_sse_event(
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "part_index": idx,
                    "item": {
                        "id": item_id,
                        "type": "message",
                        "role": "assistant",
                        "content": [{
                            "id": part_id,
                            "type": "output_text",
                            "text": text,
                            "annotations": [],
                        }],
                    },
                }),
            ));

            self.msg_item_done.insert(idx, true);
        }

        // Close reasoning if still open
        if !self.reasoning_done && self.reasoning_item_added {
            let reasoning_text = self.reasoning_buf.as_deref().unwrap_or("");
            self.reasoning_done = true;

            advance_response_seq(&mut self.seq)?;
            frames.push(format_sse_event(
                "response.reasoning_summary_text.done",
                &json!({
                    "type": "response.reasoning_summary_text.done",
                    "text": reasoning_text,
                }),
            ));

            advance_response_seq(&mut self.seq)?;
            frames.push(format_sse_event(
                "response.content_part.done",
                &json!({
                    "type": "response.content_part.done",
                    "part": {
                        "type": "summary_text",
                    },
                }),
            ));
        }

        // Emit response.completed (matching 9router's sendCompleted())
        let mut output: Vec<Value> = Vec::new();
        for &idx in self.msg_item_done.keys() {
            let text = self
                .msg_text_buf
                .get(&idx)
                .map(|s| s.as_str())
                .unwrap_or("");
            let part_id = self
                .added_content_part_id_map
                .get(&idx)
                .map(|s| s.as_str())
                .unwrap_or("");
            let item_id = self
                .added_item_id_map
                .get(&idx)
                .map(|s| s.as_str())
                .unwrap_or("");
            output.push(json!({
                "id": item_id,
                "type": "message",
                "role": "assistant",
                "content": [{
                    "id": part_id,
                    "type": "output_text",
                    "text": text,
                    "annotations": [],
                }],
            }));
        }
        if self.reasoning_item_added {
            let reasoning_text = self.reasoning_buf.as_deref().unwrap_or("");
            let reasoning_id = self.reasoning_id.as_deref().unwrap_or("");
            output.insert(
                0,
                json!({
                    "id": reasoning_id,
                    "type": "reasoning",
                    "role": "assistant",
                    "content": [{
                        "id": format!("{}_summary", reasoning_id),
                        "type": "summary_text",
                        "text": reasoning_text,
                    }],
                }),
            );
        }

        let mut skeleton = response_skeleton(self);
        let obj = skeleton
            .as_object_mut()
            .expect("response_skeleton returns object");
        obj.insert("status".to_string(), json!("completed"));
        obj.insert("background".to_string(), json!(false));
        obj.insert("error".into(), Value::Null);
        if !output.is_empty() {
            obj.insert("output".to_string(), json!(output));
        }

        self.completed_sent = true;
        advance_response_seq(&mut self.seq)?;
        frames.push(format_sse_event(
            "response.completed",
            &json!({
                "type": "response.completed",
                "response": skeleton,
            }),
        ));

        Ok(frames)
    }
}

static RESP_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

fn generate_response_id() -> String {
    let n = RESP_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("resp_{:x}", n)
}

fn response_skeleton(state: &ResponsesSseState) -> Value {
    let mut resp = json!({
        "id": state.response_id,
        "object": "response",
        "created_at": state.created,
        "status": "in_progress",
    });
    if !state.model.is_empty() {
        resp.as_object_mut()
            .expect("json! macro always produces object")
            .insert("model".to_string(), json!(state.model));
    }
    resp
}

/// Format a single SSE frame for a Responses API event.
fn format_sse_event(event: &str, data: &Value) -> Vec<u8> {
    let json_str = serde_json::to_string(data).unwrap_or_default();
    format!("event: {event}\ndata: {json_str}\n\n").into_bytes()
}

/// Convert a single chat.completion.chunk JSON to zero or more Responses API
/// SSE event frames (as raw bytes).
fn openai_chunk_to_responses(
    state: &mut ResponsesSseState,
    chunk: &Value,
) -> Result<Vec<Vec<u8>>, crate::core::translator::limits::StreamLimitError> {
    let mut frames: Vec<Vec<u8>> = Vec::new();

    // ── Initialisation ────────────────────────────────────────────────
    if !state.started {
        state.started = true;
        state.response_id = chunk
            .get("id")
            .and_then(|v| v.as_str())
            .map(|id| {
                if let Some(stripped) = id.strip_prefix("chatcmpl-") {
                    format!("resp_{}", stripped)
                } else {
                    generate_response_id()
                }
            })
            .unwrap_or_else(generate_response_id);
        state.created = chunk
            .get("created")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| Utc::now().timestamp());
        state.model = chunk
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        advance_response_seq(&mut state.seq)?;
        frames.push(format_sse_event(
            "response.created",
            &json!({
                "type": "response.created",
                "response": response_skeleton(state),
            }),
        ));

        advance_response_seq(&mut state.seq)?;
        frames.push(format_sse_event(
            "response.in_progress",
            &json!({
                "type": "response.in_progress",
                "response": response_skeleton(state),
            }),
        ));
    }

    // ── Process choices ──────────────────────────────────────────────
    let Some(choices) = chunk.get("choices").and_then(|v| v.as_array()) else {
        return Ok(frames);
    };

    for choice in choices {
        let index = state.choice_index(choice)?;
        let delta = choice
            .get("delta")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let delta = Value::Object(delta);
        let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());

        // ── Reasoning content ────────────────────────────────────────
        if let Some(reasoning) = delta
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .filter(|r| !r.is_empty())
        {
            if !state.reasoning_item_added {
                state.reasoning_item_added = true;
                let rid = generate_response_id();
                state.reasoning_id = Some(rid.clone());
                advance_response_seq(&mut state.seq)?;
                frames.push(format_sse_event(
                    "response.output_item.added",
                    &json!({
                        "type": "response.output_item.added",
                        "part_index": index,
                        "item": {
                            "id": rid,
                            "type": "reasoning",
                            "status": "in_progress",
                            "role": "assistant",
                        },
                    }),
                ));
            }

            if !state.reasoning_content_added {
                state.reasoning_content_added = true;
                advance_response_seq(&mut state.seq)?;
                frames.push(format_sse_event(
                    "response.reasoning_summary_part.added",
                    &json!({
                        "type": "response.reasoning_summary_part.added",
                        "part_index": index,
                        "part": {
                            "id": format!("reasoning_part_{}", state.seq),
                            "type": "summary_text",
                        },
                    }),
                ));
            }

            advance_response_seq(&mut state.seq)?;
            crate::core::translator::limits::checked_append(
                state.reasoning_buf.get_or_insert_with(String::new),
                reasoning,
                crate::core::translator::limits::MAX_STREAM_ACCUMULATED_BYTES,
                &mut state.retained_bytes,
                "reasoning",
            )?;
            frames.push(format_sse_event(
                "response.reasoning_summary_text.delta",
                &json!({
                    "type": "response.reasoning_summary_text.delta",
                    "delta": reasoning,
                }),
            ));
        }

        // ── Regular text content ─────────────────────────────────────
        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
            if !state.msg_item_added.contains_key(&index) || !state.msg_item_added[&index] {
                state.msg_item_added.insert(index, true);
                let item_id = generate_response_id();
                state.added_item_id_map.insert(index, item_id.clone());
                advance_response_seq(&mut state.seq)?;
                frames.push(format_sse_event(
                    "response.output_item.added",
                    &json!({
                        "type": "response.output_item.added",
                        "part_index": index,
                        "item": {
                            "id": item_id,
                            "type": "message",
                            "status": "in_progress",
                            "role": "assistant",
                            "content": [],
                        },
                    }),
                ));
            }

            if !state.msg_content_added.contains_key(&index) || !state.msg_content_added[&index] {
                state.msg_content_added.insert(index, true);
                let part_id = generate_response_id();
                state
                    .added_content_part_id_map
                    .insert(index, part_id.clone());
                advance_response_seq(&mut state.seq)?;
                frames.push(format_sse_event(
                    "response.content_part.added",
                    &json!({
                        "type": "response.content_part.added",
                        "part_index": index,
                        "part": {
                            "id": part_id,
                            "type": "output_text",
                            "text": "",
                            "annotations": [],
                        },
                    }),
                ));
            }

            let buf = state.msg_text_buf.entry(index).or_default();
            crate::core::translator::limits::checked_append(
                buf,
                content,
                crate::core::translator::limits::MAX_STREAM_ACCUMULATED_BYTES,
                &mut state.retained_bytes,
                "response text",
            )?;
            // Only emit delta events for non-empty content to avoid
            // flooding the client with empty frames (many providers
            // send empty content chunks during streaming).
            if !content.is_empty() {
                advance_response_seq(&mut state.seq)?;
                frames.push(format_sse_event(
                    "response.output_text.delta",
                    &json!({
                        "type": "response.output_text.delta",
                        "delta": content,
                    }),
                ));
            }
        }

        // ── Finish reason ────────────────────────────────────────────
        if let Some(_reason) = finish_reason {
            // Close reasoning if we actually emitted reasoning events
            if !state.reasoning_done && state.reasoning_item_added {
                let reasoning_text = state.reasoning_buf.as_deref().unwrap_or("");
                state.reasoning_done = true;
                advance_response_seq(&mut state.seq)?;
                frames.push(format_sse_event(
                    "response.reasoning_summary_text.done",
                    &json!({
                        "type": "response.reasoning_summary_text.done",
                        "part_index": index,
                        "text": reasoning_text,
                    }),
                ));
                advance_response_seq(&mut state.seq)?;
                frames.push(format_sse_event(
                    "response.content_part.done",
                    &json!({
                        "type": "response.content_part.done",
                        "part_index": index,
                        "part": {
                            "id": state
                                .added_content_part_id_map
                                .get(&index)
                                .map(|s| s.as_str())
                                .unwrap_or(""),
                            "type": "summary_text",
                        },
                    }),
                ));
            }

            // Close message text
            if !state.msg_item_done.contains_key(&index) || !state.msg_item_done[&index] {
                let text = state
                    .msg_text_buf
                    .get(&index)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let part_id = state
                    .added_content_part_id_map
                    .get(&index)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let item_id = state
                    .added_item_id_map
                    .get(&index)
                    .map(|s| s.as_str())
                    .unwrap_or("");

                advance_response_seq(&mut state.seq)?;
                frames.push(format_sse_event(
                    "response.output_text.done",
                    &json!({
                        "type": "response.output_text.done",
                        "part_index": index,
                        "text": text,
                    }),
                ));

                advance_response_seq(&mut state.seq)?;
                frames.push(format_sse_event(
                    "response.content_part.done",
                    &json!({
                        "type": "response.content_part.done",
                        "part_index": index,
                        "part": {
                            "id": part_id,
                            "type": "output_text",
                            "text": text,
                            "annotations": [],
                        },
                    }),
                ));

                advance_response_seq(&mut state.seq)?;
                frames.push(format_sse_event(
                    "response.output_item.done",
                    &json!({
                        "type": "response.output_item.done",
                        "part_index": index,
                        "item": {
                            "id": item_id,
                            "type": "message",
                            "role": "assistant",
                            "content": [
                                {
                                    "id": part_id,
                                    "type": "output_text",
                                    "text": text,
                                    "annotations": [],
                                }
                            ],
                        },
                    }),
                ));

                state.msg_item_done.insert(index, true);
            }
        }
    }

    // ── Usage + completed (last chunk) ──────────────────────────────
    let usage_in_chunk = chunk.get("usage").filter(|v| !v.is_null());
    let has_finish_reason =
        chunk
            .get("choices")
            .and_then(|v| v.as_array())
            .is_some_and(|choices| {
                choices
                    .iter()
                    .any(|c| c.get("finish_reason").and_then(|v| v.as_str()).is_some())
            });

    if usage_in_chunk.is_some() || (has_finish_reason && !state.completed_sent) {
        state.completed_sent = true;

        // Build final output array with all choices
        let mut output = Vec::new();
        for &idx in state.msg_item_done.keys() {
            let text = state
                .msg_text_buf
                .get(&idx)
                .map(|s| s.as_str())
                .unwrap_or("");
            let part_id = state
                .added_content_part_id_map
                .get(&idx)
                .map(|s| s.as_str())
                .unwrap_or("");
            let item_id = state
                .added_item_id_map
                .get(&idx)
                .map(|s| s.as_str())
                .unwrap_or("");
            output.push(json!({
                "id": item_id,
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "id": part_id,
                        "type": "output_text",
                        "text": text,
                        "annotations": [],
                    }
                ],
            }));
        }

        // If we had reasoning output, include it too
        if state.reasoning_item_added {
            let reasoning_text = state.reasoning_buf.as_deref().unwrap_or("");
            let reasoning_id = state.reasoning_id.as_deref().unwrap_or("");
            output.insert(
                0,
                json!({
                    "id": reasoning_id,
                    "type": "reasoning",
                    "role": "assistant",
                    "content": [
                        {
                            "id": format!("{}_summary", reasoning_id),
                            "type": "summary_text",
                            "text": reasoning_text,
                        }
                    ],
                }),
            );
        }

        let mut skeleton = response_skeleton(state);
        let obj = skeleton
            .as_object_mut()
            .expect("response_skeleton returns object");
        obj.insert("status".to_string(), json!("completed"));
        obj.insert("background".to_string(), json!(false));
        obj.insert("error".into(), Value::Null);
        if !output.is_empty() {
            obj.insert("output".to_string(), json!(output));
        }
        // Include usage from the last chunk (fix: 9router omit bug — sendCompleted()
        // in the JS version omits usage, breaking downstream consumers).
        // Responses API spec requires usage in response.completed with field names
        // `input_tokens` / `output_tokens`, not `prompt_tokens` / `completion_tokens`.
        if let Some(usage) = chunk.get("usage").filter(|v| !v.is_null()) {
            let mut mapped = serde_json::Map::new();
            if let Some(v) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                mapped.insert("input_tokens".into(), json!(v));
            }
            if let Some(v) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                mapped.insert("output_tokens".into(), json!(v));
            }
            if let Some(v) = usage.get("total_tokens").and_then(|v| v.as_u64()) {
                mapped.insert("total_tokens".into(), json!(v));
            }
            if !mapped.is_empty() {
                obj.insert("usage".into(), json!(mapped));
            }
        }

        advance_response_seq(&mut state.seq)?;
        frames.push(format_sse_event(
            "response.completed",
            &json!({
                "type": "response.completed",
                "response": skeleton,
            }),
        ));
    }

    Ok(frames)
}

// ══════════════════════════════════════════════════════════════════════════
// Anthropic Messages API format converter
// ══════════════════════════════════════════════════════════════════════════

/// Convert an OpenAI chat-completion response (streaming or non-streaming) to
/// the Anthropic Messages API format.
async fn convert_to_messages_api(response: Response) -> Response {
    let status = response.status();
    if !status.is_success() {
        return response;
    }

    let is_sse = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream") || ct.contains("text/plain"));

    if !is_sse {
        let (parts, body) = response.into_parts();
        let body_bytes = match body.collect().await {
            Ok(col) => col.to_bytes(),
            Err(_) => return Response::from_parts(parts, Body::empty()),
        };
        let Ok(chat_completion) = serde_json::from_slice::<Value>(&body_bytes) else {
            return Response::from_parts(parts, Body::from(body_bytes));
        };
        let messages_json = chat_completion_to_messages_json(&chat_completion);
        return Json(messages_json).into_response();
    }

    let (parts, body) = response.into_parts();
    let data_stream = body.into_data_stream();
    let converted = async_stream::stream! {
        let mut framer = SseFramer::new();
        let mut conv_state = MessagesSseState::new();

        let mut stream = data_stream;
        loop {
            let next = stream.next().await;
            match next {
                Some(Ok(chunk)) => {
                    let mut events = Vec::new();
                    let mut conversion_error = None;
                    let frame_result = framer.feed(&chunk, |event| {
                        if conversion_error.is_some() {
                            return;
                        }
                        let Some(json_str) = event.data() else {
                            return;
                        };
                        if json_str == "[DONE]" {
                            return;
                        }
                        if let Ok(chunk_value) = serde_json::from_str::<Value>(json_str) {
                            match openai_chunk_to_messages(&mut conv_state, &chunk_value) {
                                Ok(converted) => events.extend(converted),
                                Err(error) => conversion_error = Some(error),
                            }
                        }
                    });
                    for event_bytes in events {
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(event_bytes));
                    }
                    if let Some(error) = conversion_error {
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(format_messages_sse_event(
                            "error",
                            &json!({"type": "error", "error": {"message": error.message, "code": error.code}}),
                        )));
                        return;
                    }
                    if let Err(error) = frame_result {
                        yield Ok::<Bytes, std::io::Error>(compat_framing_error_event(&error));
                        return;
                    }
                }
                Some(Err(_)) | None => break,
            }
        }

        let mut tail_events = Vec::new();
        let mut conversion_error = None;
        let frame_result = framer.finish(|event| {
            let Some(json_str) = event.data() else {
                return;
            };
            if json_str == "[DONE]" {
                return;
            }
            if let Ok(chunk_value) = serde_json::from_str::<Value>(json_str) {
                match openai_chunk_to_messages(&mut conv_state, &chunk_value) {
                    Ok(converted) => tail_events.extend(converted),
                    Err(error) => conversion_error = Some(error),
                }
            }
        });
        for event_bytes in tail_events {
            yield Ok::<Bytes, std::io::Error>(Bytes::from(event_bytes));
        }
        if let Some(error) = conversion_error {
            yield Ok::<Bytes, std::io::Error>(Bytes::from(format_messages_sse_event(
                "error",
                &json!({"type": "error", "error": {"message": error.message, "code": error.code}}),
            )));
            return;
        }
        if let Err(error) = frame_result {
            yield Ok::<Bytes, std::io::Error>(compat_framing_error_event(&error));
            return;
        }

        // Send message_stop if not already sent
        if !conv_state.stop_sent {
            if let Err(error) = conv_state.validate_tool_identity() {
                yield Ok::<Bytes, std::io::Error>(Bytes::from(format_messages_sse_event(
                    "error",
                    &json!({"type": "error", "error": {"message": error.message, "code": error.code}}),
                )));
                return;
            }
            conv_state.stop_sent = true;
            yield Ok::<Bytes, std::io::Error>(Bytes::from("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
        }
    };

    let body = Body::from_stream(converted);
    let mut resp = Response::new(body);
    *resp.status_mut() = status;

    for (name, value) in &parts.headers {
        if name.as_str() != "content-length"
            && name.as_str() != "content-type"
            && name.as_str() != "transfer-encoding"
        {
            resp.headers_mut().insert(name, value.clone());
        }
    }
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp.headers_mut().insert(
        header::HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    resp.headers_mut().insert(
        header::HeaderName::from_static("connection"),
        HeaderValue::from_static("keep-alive"),
    );

    resp
}

/// Map OpenAI finish_reason to Anthropic stop_reason.
fn stop_reason_map(finish_reason: Option<&str>) -> Value {
    match finish_reason {
        Some("stop") => json!("end_turn"),
        Some("length") => json!("max_tokens"),
        Some("tool_calls") => json!("tool_use"),
        Some(other) => json!(other),
        None => Value::Null,
    }
}

/// Convert OpenAI usage to Anthropic usage format.
fn usage_to_anthropic(usage: Option<&Value>) -> Option<Value> {
    let usage = usage?;
    let input_tokens = usage
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Some(json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
    }))
}

/// Convert a completed chat-completion JSON object to Anthropic Messages API JSON.
fn chat_completion_to_messages_json(source: &Value) -> Value {
    let msg_id = source
        .get("id")
        .and_then(|v| v.as_str())
        .map(|id| {
            if let Some(stripped) = id.strip_prefix("chatcmpl-") {
                format!("msg_{}", stripped)
            } else {
                format!("msg_{}", id)
            }
        })
        .unwrap_or_else(|| format!("msg_{:x}", Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    let model = source.get("model").and_then(|v| v.as_str()).unwrap_or("");

    let mut content = Vec::new();
    let mut stop_reason = Value::Null;
    let mut usage = None;

    if let Some(choices) = source.get("choices").and_then(|v| v.as_array()) {
        if let Some(first_choice) = choices.first() {
            stop_reason =
                stop_reason_map(first_choice.get("finish_reason").and_then(|v| v.as_str()));

            if let Some(message) = first_choice.get("message") {
                // reasoning_content → thinking block
                if let Some(reasoning) = message.get("reasoning_content").and_then(|v| v.as_str()) {
                    if !reasoning.is_empty() {
                        content.push(json!({
                            "type": "thinking",
                            "thinking": reasoning,
                            "signature": null,
                        }));
                    }
                }

                let text = message
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !text.is_empty() {
                    content.push(json!({
                        "type": "text",
                        "text": text,
                    }));
                }

                // Convert OpenAI tool_calls to Anthropic tool_use blocks
                if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tool_calls {
                        let tc_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let args_str = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}");
                        let input: Value =
                            serde_json::from_str(args_str).unwrap_or(Value::Object(Map::new()));

                        content.push(json!({
                            "type": "tool_use",
                            "id": tc_id,
                            "name": name,
                            "input": input,
                        }));
                    }
                }
            }
        }
    }

    if content.is_empty() {
        content.push(json!({
            "type": "text",
            "text": "",
        }));
    }

    if let Some(u) = source.get("usage") {
        usage = usage_to_anthropic(Some(u));
    }

    json!({
        "id": msg_id,
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage,
    })
}

/// State machine for streaming chat.completion.chunk → Anthropic Messages API SSE.
struct MessagesSseState {
    started: bool,
    msg_id: String,
    model: String,

    // Thinking block state
    thinking_started: bool,
    thinking_stopped: bool,

    // Text block state
    text_started: bool,
    text_stopped: bool,

    // Block index counter
    block_idx: u64,

    // Whether final events were sent
    stop_sent: bool,

    // Wire tool index -> emitted Anthropic block index. A map preserves sparse
    // ordering without allocating through an attacker-controlled index.
    toolcalls: BTreeMap<u64, u64>,
    seen_tool_indices: BTreeSet<u64>,
    pending_tool_args: BTreeMap<u64, String>,
    tool_argument_bytes: BTreeMap<u64, usize>,
    choice_indices: BTreeSet<u64>,
    retained_bytes: usize,
}

impl MessagesSseState {
    fn new() -> Self {
        Self {
            started: false,
            msg_id: String::new(),
            model: String::new(),
            thinking_started: false,
            thinking_stopped: false,
            text_started: false,
            text_stopped: false,
            block_idx: 0,
            stop_sent: false,
            toolcalls: BTreeMap::new(),
            seen_tool_indices: BTreeSet::new(),
            pending_tool_args: BTreeMap::new(),
            tool_argument_bytes: BTreeMap::new(),
            choice_indices: BTreeSet::new(),
            retained_bytes: 0,
        }
    }

    /// True if any tool call is currently active (started but not finished).
    fn has_active_toolcalls(&self) -> bool {
        !self.toolcalls.is_empty()
    }

    fn validate_tool_identity(
        &self,
    ) -> Result<(), crate::core::translator::limits::StreamLimitError> {
        for index in &self.seen_tool_indices {
            if !self.toolcalls.contains_key(index) {
                return Err(crate::core::translator::limits::StreamLimitError {
                    code: "upstream_stream_invalid_tool_call",
                    message: format!(
                        "OpenAI tool call index {index} finished without a non-empty id and function name"
                    ),
                });
            }
        }
        Ok(())
    }
}

/// Format an Anthropic Messages API SSE event frame.
fn format_messages_sse_event(event: &str, data: &Value) -> Vec<u8> {
    let json_str = serde_json::to_string(data).unwrap_or_default();
    format!("event: {event}\ndata: {json_str}\n\n").into_bytes()
}

/// Convert a single chat.completion.chunk to Anthropic Messages API SSE events.
fn openai_chunk_to_messages(
    state: &mut MessagesSseState,
    chunk: &Value,
) -> Result<Vec<Vec<u8>>, crate::core::translator::limits::StreamLimitError> {
    let mut frames: Vec<Vec<u8>> = Vec::new();

    // ── Initialize on first chunk ──────────────────────────────────────────
    if !state.started {
        state.started = true;
        state.msg_id = chunk
            .get("id")
            .and_then(|v| v.as_str())
            .map(|id| {
                if let Some(stripped) = id.strip_prefix("chatcmpl-") {
                    format!("msg_{}", stripped)
                } else {
                    format!("msg_{}", id)
                }
            })
            .unwrap_or_else(|| format!("msg_{:x}", Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        state.model = chunk
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // emit message_start
        frames.push(format_messages_sse_event(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": state.msg_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": state.model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                },
            }),
        ));
    }

    let Some(choices) = chunk.get("choices").and_then(|v| v.as_array()) else {
        return Ok(frames);
    };

    for choice in choices {
        let choice_index =
            crate::core::translator::limits::wire_index(choice.get("index"), "choices[].index")?;
        if !state.choice_indices.contains(&choice_index)
            && state.choice_indices.len() >= crate::core::translator::limits::MAX_STREAM_CHOICES
        {
            return Err(crate::core::translator::limits::StreamLimitError::too_many(
                "response choices",
                crate::core::translator::limits::MAX_STREAM_CHOICES,
            ));
        }
        state.choice_indices.insert(choice_index);
        let delta = choice
            .get("delta")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let delta = Value::Object(delta);
        let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());

        // ── Reasoning / thinking content ──────────────────────────────────────
        if let Some(reasoning) = delta
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .filter(|r| !r.is_empty())
        {
            if !state.thinking_started {
                state.thinking_started = true;
                let idx = state.block_idx;
                state.block_idx = state.block_idx.checked_add(1).ok_or_else(|| {
                    crate::core::translator::limits::StreamLimitError::arithmetic(
                        "content block index",
                    )
                })?;
                frames.push(format_messages_sse_event(
                    "content_block_start",
                    &json!({
                        "type": "content_block_start",
                        "index": idx,
                        "content_block": {
                            "type": "thinking",
                            "thinking": "",
                            "signature": null,
                        },
                    }),
                ));
            }

            let thinking_idx = if state.thinking_started && !state.text_started {
                0
            } else {
                0
            };
            frames.push(format_messages_sse_event(
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": thinking_idx,
                    "delta": {
                        "type": "thinking_delta",
                        "thinking": reasoning,
                    },
                }),
            ));
        }

        // ── Text content ──────────────────────────────────────────────────────
        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
            // Skip empty content deltas when tool calls are being streamed
            // (DeepSeek sends content:"" alongside finish_reason:"tool_calls")
            let skip_empty = state.has_active_toolcalls() && content.is_empty();
            if !skip_empty {
                // Close thinking block if we're transitioning to text
                if state.thinking_started && !state.thinking_stopped {
                    state.thinking_stopped = true;
                    frames.push(format_messages_sse_event(
                        "content_block_stop",
                        &json!({
                            "type": "content_block_stop",
                            "index": 0,
                        }),
                    ));
                }

                if !state.text_started {
                    state.text_started = true;
                    let idx = state.block_idx;
                    state.block_idx = state.block_idx.checked_add(1).ok_or_else(|| {
                        crate::core::translator::limits::StreamLimitError::arithmetic(
                            "content block index",
                        )
                    })?;
                    frames.push(format_messages_sse_event(
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": idx,
                            "content_block": {
                                "type": "text",
                                "text": "",
                            },
                        }),
                    ));
                }

                if !content.is_empty() {
                    let text_idx = if state.thinking_started { 1 } else { 0 };
                    frames.push(format_messages_sse_event(
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": text_idx,
                            "delta": {
                                "type": "text_delta",
                                "text": content,
                            },
                        }),
                    ));
                }
            }
        }

        // ── Tool calls ──────────────────────────────────────────────────────
        // OpenAI streams tool_calls in the delta. Convert to Anthropic tool_use
        // content blocks with input_json_delta for streaming arguments.
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            // Close thinking block if open (use a simple flag to avoid double-close)
            if state.thinking_started && !state.thinking_stopped {
                state.thinking_stopped = true;
                frames.push(format_messages_sse_event(
                    "content_block_stop",
                    &json!({"type": "content_block_stop", "index": 0}),
                ));
            }

            for tc in tool_calls {
                let tcidx = crate::core::translator::limits::wire_index(
                    tc.get("index"),
                    "tool_calls[].index",
                )?;
                if !state.seen_tool_indices.contains(&tcidx)
                    && state.seen_tool_indices.len()
                        >= crate::core::translator::limits::MAX_STREAM_TOOL_CALLS
                {
                    return Err(crate::core::translator::limits::StreamLimitError::too_many(
                        "tool calls",
                        crate::core::translator::limits::MAX_STREAM_TOOL_CALLS,
                    ));
                }
                state.seen_tool_indices.insert(tcidx);

                // First chunk: has id + function.name → emit content_block_start
                let id = tc.get("id").and_then(Value::as_str);
                let name = tc.pointer("/function/name").and_then(Value::as_str);
                if id.is_some() || name.is_some() {
                    let id = id.filter(|id| !id.is_empty()).ok_or_else(|| {
                        crate::core::translator::limits::StreamLimitError {
                            code: "upstream_stream_invalid_tool_call",
                            message: "OpenAI tool call declaration is missing an id".to_string(),
                        }
                    })?;
                    let name = name.filter(|name| !name.is_empty()).ok_or_else(|| {
                        crate::core::translator::limits::StreamLimitError {
                            code: "upstream_stream_invalid_tool_call",
                            message: "OpenAI tool call declaration is missing a function name"
                                .to_string(),
                        }
                    })?;
                    if !state.toolcalls.contains_key(&tcidx) {
                        let idx = state.block_idx;
                        state.block_idx = state.block_idx.checked_add(1).ok_or_else(|| {
                            crate::core::translator::limits::StreamLimitError::arithmetic(
                                "content block index",
                            )
                        })?;
                        state.toolcalls.insert(tcidx, idx);

                        frames.push(format_messages_sse_event(
                            "content_block_start",
                            &json!({
                                "type": "content_block_start",
                                "index": idx,
                                "content_block": {
                                    "type": "tool_use",
                                    "id": id,
                                    "name": name,
                                },
                            }),
                        ));
                        if let Some(pending) = state.pending_tool_args.remove(&tcidx) {
                            state.retained_bytes = state
                                .retained_bytes
                                .checked_sub(pending.len())
                                .ok_or_else(|| {
                                    crate::core::translator::limits::StreamLimitError::arithmetic(
                                        "retained state",
                                    )
                                })?;
                            if !pending.is_empty() {
                                frames.push(format_messages_sse_event(
                                    "content_block_delta",
                                    &json!({
                                        "type": "content_block_delta",
                                        "index": idx,
                                        "delta": {
                                            "type": "input_json_delta",
                                            "partial_json": pending,
                                        },
                                    }),
                                ));
                            }
                        }
                    }
                }

                // Subsequent chunks: arguments delta → emit content_block_delta (input_json_delta)
                if let Some(args) = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                {
                    if !args.is_empty() {
                        let current = state.tool_argument_bytes.get(&tcidx).copied().unwrap_or(0);
                        let next = current.checked_add(args.len()).ok_or_else(|| {
                            crate::core::translator::limits::StreamLimitError::arithmetic(
                                "tool arguments",
                            )
                        })?;
                        if next > crate::core::translator::limits::MAX_STREAM_TOOL_ARGUMENT_BYTES {
                            return Err(crate::core::translator::limits::StreamLimitError::bytes(
                                "tool arguments",
                                crate::core::translator::limits::MAX_STREAM_TOOL_ARGUMENT_BYTES,
                            ));
                        }
                        state.tool_argument_bytes.insert(tcidx, next);
                    }
                    if let Some(&tcidx_start) =
                        state.toolcalls.get(&tcidx).filter(|_| !args.is_empty())
                    {
                        frames.push(format_messages_sse_event(
                            "content_block_delta",
                            &json!({
                                "type": "content_block_delta",
                                "index": tcidx_start,
                                "delta": {
                                    "type": "input_json_delta",
                                    "partial_json": args,
                                },
                            }),
                        ));
                    } else if !args.is_empty() {
                        let pending = state.pending_tool_args.entry(tcidx).or_default();
                        crate::core::translator::limits::checked_append(
                            pending,
                            args,
                            crate::core::translator::limits::MAX_STREAM_TOOL_ARGUMENT_BYTES,
                            &mut state.retained_bytes,
                            "tool arguments",
                        )?;
                    }
                }
            }
        }

        // ── Finish reason → close blocks + message_delta + message_stop ──────
        if finish_reason.is_some() && !state.stop_sent {
            state.validate_tool_identity()?;
            // Close tool call blocks
            for &tcidx_start in state.toolcalls.values() {
                frames.push(format_messages_sse_event(
                    "content_block_stop",
                    &json!({"type": "content_block_stop", "index": tcidx_start}),
                ));
            }
            state.toolcalls.clear();

            // Close text block if open
            if state.text_started && !state.text_stopped {
                state.text_stopped = true;
                let text_idx = if state.thinking_started { 1 } else { 0 };
                frames.push(format_messages_sse_event(
                    "content_block_stop",
                    &json!({
                        "type": "content_block_stop",
                        "index": text_idx,
                    }),
                ));
            }

            // Close thinking block if still open
            if state.thinking_started && !state.thinking_stopped {
                state.thinking_stopped = true;
                frames.push(format_messages_sse_event(
                    "content_block_stop",
                    &json!({
                        "type": "content_block_stop",
                        "index": 0,
                    }),
                ));
            }

            let stop_reason = stop_reason_map(finish_reason);
            let usage = chunk
                .get("usage")
                .and_then(|u| usage_to_anthropic(Some(u)))
                .unwrap_or(json!({ "input_tokens": 0, "output_tokens": 0 }));

            frames.push(format_messages_sse_event(
                "message_delta",
                &json!({
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": stop_reason,
                        "stop_sequence": null,
                    },
                    "usage": usage,
                }),
            ));

            state.stop_sent = true;
            frames.push(format_messages_sse_event(
                "message_stop",
                &json!({
                    "type": "message_stop",
                }),
            ));
        }
    }

    Ok(frames)
}

// ---------------------------------------------------------------------------
// Body normalisation (unchanged below this line)
// ---------------------------------------------------------------------------

fn normalize_body(mut body: Value, mode: CompatMode) -> Value {
    let Some(fields) = body.as_object_mut() else {
        return body;
    };

    match mode {
        // Keep native Messages input intact. The chat planner now chooses the
        // minimal path from source/target protocol capability; when the target
        // is not Claude, the registered Claude translator performs the one
        // required format conversion. Pre-converting here duplicated prompt
        // mutation and discarded client cache anchors before planning.
        CompatMode::Messages => {}
        CompatMode::Responses { compact } => {
            if compact {
                fields.insert("_compact".to_string(), Value::Bool(true));
            }
        }
    }

    body
}

fn count_request_chars(body: &Value) -> usize {
    let mut total = 0;

    for field in ["messages", "input", "instructions", "system"] {
        if let Some(value) = body.get(field) {
            total += count_chars(value);
        }
    }

    total
}

fn count_chars(value: &Value) -> usize {
    match value {
        Value::String(text) => text.chars().count(),
        Value::Array(items) => items.iter().map(count_chars).sum(),
        Value::Object(fields) => {
            if let Some(content) = fields.get("content") {
                return count_chars(content);
            }

            if let Some(text) = fields.get("text") {
                return count_chars(text);
            }

            0
        }
        _ => 0,
    }
}

fn invalid_json_response(
    error: JsonRejection,
    headers: &HeaderMap,
    endpoint: &'static str,
) -> Response {
    let status = error.status();
    let rejection = match &error {
        JsonRejection::JsonDataError(_) => "json_data",
        JsonRejection::JsonSyntaxError(_) => "json_syntax",
        JsonRejection::MissingJsonContentType(_) => "content_type",
        JsonRejection::BytesRejection(_) if status == StatusCode::PAYLOAD_TOO_LARGE => {
            "body_too_large"
        }
        JsonRejection::BytesRejection(_) => "body_read",
        _ => "unknown",
    };
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("missing");
    tracing::warn!(
        endpoint,
        rejection,
        status = status.as_u16(),
        content_length,
        content_type,
        "request JSON rejected"
    );

    let message = match status {
        StatusCode::PAYLOAD_TOO_LARGE => super::LLM_BODY_TOO_LARGE_MESSAGE,
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "Content-Type must be application/json",
        _ => "Invalid JSON body",
    };
    with_cors_response((status, Json(json!({ "error": message }))).into_response())
}

fn with_cors_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, OPTIONS"),
    );
    response
}

fn cors_preflight_response(methods: &str) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_str(methods).unwrap_or(HeaderValue::from_static("POST, OPTIONS")),
    );
    response
}

/// Convert a Claude-format non-streaming response body to OpenAI chat.completion format.
/// Port of 9router's translateNonStreamingResponse Claude branch.
fn claude_body_to_chat_completion(body: &Value) -> Value {
    // body: { "type":"message", "content":[{"type":"text","text":"..."}],
    //         "stop_reason":"end_turn", "usage":{...}, "id":"msg_...", "model":"..." }
    let mut text_content = String::new();
    let mut thinking_content = String::new();
    let mut tool_calls = Vec::new();

    if let Some(content) = body.get("content").and_then(Value::as_array) {
        for block in content {
            let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
            match block_type {
                "text" => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        text_content.push_str(text);
                    }
                }
                "thinking" => {
                    if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                        thinking_content.push_str(t);
                    }
                }
                "tool_use" => {
                    let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    let args = block.get("input").cloned().unwrap_or(json!({}));
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string())
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let mut message = json!({"role": "assistant"});
    if !text_content.is_empty() {
        message["content"] = json!(text_content);
    }
    if !thinking_content.is_empty() {
        message["reasoning_content"] = json!(thinking_content);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }
    if !text_content.is_empty() || tool_calls.is_empty() {
        message["content"] = message.get("content").cloned().unwrap_or(json!(""));
    }

    let stop_reason = body
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop");
    let finish_reason = match stop_reason {
        "end_turn" => "stop",
        "tool_use" => "tool_calls",
        other => other,
    };

    let id = body
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("msg_unknown");
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let created = chrono::Utc::now().timestamp();

    let mut result = json!({
        "id": format!("chatcmpl-{}", id),
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason
        }]
    });

    if let Some(usage) = body.get("usage") {
        let input_tokens = usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output_tokens = usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        result["usage"] = json!({
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_stream_bypass_never_skips_injected_search_sanitization() {
        let routed = Some(chat::RoutedResponseFormats {
            client: Format::OpenAiResponses,
            upstream: Format::OpenAiResponses,
            native_passthrough: true,
        });
        assert!(should_bypass_compat_conversion(
            true,
            routed,
            Format::OpenAiResponses,
            false,
        ));
        assert!(!should_bypass_compat_conversion(
            true,
            routed,
            Format::OpenAiResponses,
            true,
        ));
    }

    #[test]
    fn responses_input_is_left_for_format_registry() {
        let body = json!({
            "model": "openai/gpt-4o-mini",
            "instructions": "Be terse",
            "input": [
                { "role": "user", "content": [{ "type": "input_text", "text": "Hello" }] }
            ],
            "max_output_tokens": 64
        });

        let normalized = normalize_body(body, CompatMode::Responses { compact: true });

        assert_eq!(normalized["_compact"], true);
        assert_eq!(normalized["instructions"], "Be terse");
        assert_eq!(normalized["max_output_tokens"], 64);
        assert_eq!(normalized["input"][0]["role"], "user");
        assert!(normalized.get("messages").is_none());
    }

    #[test]
    fn messages_route_preserves_native_body_for_protocol_planner() {
        let body = json!({
            "model": "openai/gpt-4o-mini",
            "system": [{
                "type": "text",
                "text": "Stay concise",
                "cache_control": {"type": "ephemeral", "ttl": "client-owned"}
            }],
            "messages": [{ "role": "user", "content": "Ping" }],
            "prompt_cache_key": "client-key"
        });

        let normalized = normalize_body(body, CompatMode::Messages);
        let messages = normalized["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(normalized["system"][0]["text"], "Stay concise");
        assert_eq!(
            normalized["system"][0]["cache_control"]["ttl"],
            "client-owned"
        );
        assert_eq!(normalized["prompt_cache_key"], "client-key");
    }

    #[test]
    fn null_finish_reason_does_not_close_message() {
        // JS parity (openai-responses.js:111 `if (choice.finish_reason)`):
        // explicit null must not close the message.
        let mut state = serde_json::Map::new();
        let chunk = json!({
            "choices": [{
                "index": 0,
                "delta": { "content": "Hi" },
                "finish_reason": null
            }]
        });
        let events =
            crate::core::translator::response::openai_responses::chat_to_responses_response(
                &chunk, &mut state,
            );
        let sse = serde_json::to_string(&events).unwrap_or_default();
        assert!(
            !sse.contains("response.completed"),
            "null finish_reason must not emit completed, got: {sse}"
        );
    }

    #[test]
    fn token_counter_counts_nested_text_parts() {
        let request = json!({
            "messages": [
                { "role": "user", "content": "abcd" },
                { "role": "assistant", "content": [{ "type": "text", "text": "efghij" }] }
            ]
        });

        assert_eq!(count_request_chars(&request), 10);
    }

    // ── Responses API SSE conversion tests ──────────────────────────

    #[test]
    fn openai_chunk_to_responses_starts_with_created_event() {
        let mut state = ResponsesSseState::new();
        let chunk = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion.chunk",
            "created": 1712345678,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hello" },
                "finish_reason": null
            }]
        });

        let frames = openai_chunk_to_responses(&mut state, &chunk);
        let all_frames = frames.unwrap().concat();
        let sse = String::from_utf8_lossy(&all_frames);

        assert!(
            sse.contains("event: response.created\n"),
            "should emit response.created"
        );
        assert!(
            sse.contains("event: response.in_progress\n"),
            "should emit response.in_progress"
        );
        assert!(
            sse.contains("event: response.output_item.added\n"),
            "should emit output_item.added"
        );
        assert!(
            sse.contains("event: response.content_part.added\n"),
            "should emit content_part.added"
        );
        assert!(
            sse.contains("event: response.output_text.delta\n"),
            "should emit output_text.delta"
        );
        assert!(
            sse.contains("\"delta\":\"Hello\""),
            "delta should contain 'Hello'"
        );
    }

    #[test]
    fn openai_chunk_to_responses_completes_on_finish_reason() {
        let mut state = ResponsesSseState::new();
        let chunk1 = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion.chunk",
            "created": 1712345678,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hi" },
                "finish_reason": null
            }]
        });
        let chunk2 = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion.chunk",
            "created": 1712345678,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        });

        let _ = openai_chunk_to_responses(&mut state, &chunk1);
        let frames = openai_chunk_to_responses(&mut state, &chunk2);
        let all_frames = frames.unwrap().concat();
        let sse = String::from_utf8_lossy(&all_frames);

        assert!(
            sse.contains("event: response.output_text.done\n"),
            "should emit output_text.done"
        );
        assert!(
            sse.contains("event: response.content_part.done\n"),
            "should emit content_part.done"
        );
        assert!(
            sse.contains("event: response.output_item.done\n"),
            "should emit output_item.done"
        );
        assert!(
            sse.contains("event: response.completed\n"),
            "should emit completed"
        );
        assert!(
            sse.contains("\"total_tokens\":12"),
            "usage should be included"
        );
    }

    #[test]
    fn openai_chunk_to_responses_handles_reasoning_content() {
        let mut state = ResponsesSseState::new();
        let chunk = json!({
            "id": "chatcmpl-def456",
            "object": "chat.completion.chunk",
            "created": 1712345678,
            "model": "deepseek-chat",
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "reasoning_content": "Let me think..."
                },
                "finish_reason": null
            }]
        });

        let frames = openai_chunk_to_responses(&mut state, &chunk);
        let all_frames = frames.unwrap().concat();
        let sse = String::from_utf8_lossy(&all_frames);

        assert!(
            sse.contains("event: response.reasoning_summary_text.delta\n"),
            "should emit reasoning delta"
        );
        assert!(
            sse.contains("\"delta\":\"Let me think...\""),
            "reasoning delta content"
        );
    }

    #[test]
    fn chat_completion_to_responses_json_converts_non_streaming() {
        let chat = json!({
            "id": "chatcmpl-xyz789",
            "object": "chat.completion",
            "created": 1712345678,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello there!"
                },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 }
        });

        let resp = chat_completion_to_responses_json(&chat);

        assert_eq!(resp["object"], "response");
        assert_eq!(resp["status"], "completed");
        assert_eq!(resp["model"], "gpt-4o");
        assert_eq!(resp["output"][0]["type"], "message");
        assert_eq!(resp["output"][0]["content"][0]["type"], "output_text");
        assert_eq!(resp["output"][0]["content"][0]["text"], "Hello there!");
        assert_eq!(resp["usage"]["total_tokens"], 8);
    }

    #[tokio::test]
    async fn native_responses_json_is_not_converted_as_chat_completion() {
        let native = json!({
            "id": "resp_native",
            "object": "response",
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "MUSE_OK"}]
            }],
            "usage": {"input_tokens": 4, "output_tokens": 2, "total_tokens": 6}
        });
        let upstream = Json(native.clone()).into_response();

        let response = convert_to_responses_api(upstream, false).await;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let converted: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(converted, native);
    }

    #[tokio::test]
    async fn proxy_injected_web_search_is_removed_from_non_streaming_response() {
        let native = json!({
            "id": "resp_native",
            "object": "response",
            "status": "completed",
            "output": [
                {"id": "ws_1", "type": "web_search_call"},
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "grounded",
                        "annotations": [{"type": "url_citation", "url": "https://example.com"}]
                    }]
                }
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2, "total_tokens": 3}
        });
        let mut upstream = Json(native).into_response();
        upstream
            .extensions_mut()
            .insert(chat::CodexWebSearchInjected);

        let response = convert_to_responses_api(upstream, false).await;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let converted: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(converted["output"].as_array().unwrap().len(), 1);
        assert_eq!(converted["output"][0]["type"], "message");
        assert_eq!(
            converted["output"][0]["content"][0]["annotations"][0]["url"],
            "https://example.com"
        );
        assert_eq!(converted["usage"]["total_tokens"], 3);
    }

    #[tokio::test]
    async fn native_responses_sse_does_not_get_a_second_completion() {
        let completed = json!({
            "type": "response.completed",
            "response": {
                "id": "resp_native",
                "object": "response",
                "status": "completed",
                "output": [],
                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
            }
        });
        let body = format!(
            "event: response.completed\ndata: {}\n\n",
            serde_json::to_string(&completed).unwrap()
        );
        let upstream = (
            [(header::CONTENT_TYPE, "text/event-stream")],
            Body::from(body),
        )
            .into_response();

        let response = convert_to_responses_api(upstream, true).await;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let output = String::from_utf8(body.to_vec()).unwrap();

        assert_eq!(output.matches("event: response.completed").count(), 1);
        assert!(!output.contains("data: [DONE]"));
    }

    #[tokio::test]
    async fn native_responses_sse_emits_before_upstream_completion() {
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let stream_release = release.clone();
        let upstream_body = Body::from_stream(async_stream::stream! {
            yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                b"event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
            ));
            stream_release.notified().await;
            yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n",
            ));
        });
        let upstream =
            ([(header::CONTENT_TYPE, "text/event-stream")], upstream_body).into_response();

        let response = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            convert_to_responses_api(upstream, true),
        )
        .await
        .expect("converter must return before the upstream stream completes");
        let mut body = response.into_body().into_data_stream();
        let first = tokio::time::timeout(std::time::Duration::from_millis(100), body.next())
            .await
            .expect("first frame must stream immediately")
            .expect("first frame")
            .expect("valid body frame");
        assert!(String::from_utf8_lossy(&first).contains("response.created"));
        release.notify_one();
        while body.next().await.is_some() {}
    }

    #[tokio::test]
    async fn proxy_injected_web_search_is_hidden_without_losing_response_events() {
        let fixture = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\"}\n\n",
            "event: response.reasoning_summary_text.delta\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"thinking\"}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"ws_1\",\"type\":\"web_search_call\"}}\n\n",
            "event: response.web_search_call.searching\n",
            "data: {\"type\":\"response.web_search_call.searching\",\"item_id\":\"ws_1\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"ws_1\",\"type\":\"web_search_call\",\"action\":{\"type\":\"search\"}}}\n\n",
            "event: response.output_text.annotation.added\n",
            "data: {\"type\":\"response.output_text.annotation.added\",\"annotation\":{\"type\":\"url_citation\",\"url\":\"https://example.com\"}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"grounded\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"object\":\"response\",\"output\":[{\"id\":\"ws_1\",\"type\":\"web_search_call\"},{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"grounded\"}]}],\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n\n",
        );
        let split = fixture.len() / 3;
        let chunks = vec![
            Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&fixture.as_bytes()[..split])),
            Ok(Bytes::copy_from_slice(
                &fixture.as_bytes()[split..split * 2],
            )),
            Ok(Bytes::copy_from_slice(&fixture.as_bytes()[split * 2..])),
        ];
        let mut upstream = (
            [(header::CONTENT_TYPE, "text/event-stream")],
            Body::from_stream(futures_util::stream::iter(chunks)),
        )
            .into_response();
        upstream
            .extensions_mut()
            .insert(chat::CodexWebSearchInjected);

        let response = convert_to_responses_api(upstream, true).await;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let output = String::from_utf8(body.to_vec()).unwrap();

        assert!(!output.contains("web_search_call"));
        assert!(output.contains("response.reasoning_summary_text.delta"));
        assert!(output.contains("response.output_text.annotation.added"));
        assert!(output.contains("https://example.com"));
        assert!(output.contains("grounded"));
        assert!(output.contains("\"total_tokens\":3"));
        assert_eq!(output.matches("event: response.completed").count(), 1);
    }

    #[tokio::test]
    async fn client_native_web_search_events_pass_through() {
        let fixture = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"id\":\"ws_1\",\"type\":\"web_search_call\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"ws_1\",\"type\":\"web_search_call\",\"action\":{\"type\":\"search\"}}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"id\":\"ws_1\",\"type\":\"web_search_call\"}]}}\n\n",
        );
        let upstream = (
            [(header::CONTENT_TYPE, "text/event-stream")],
            Body::from(fixture),
        )
            .into_response();

        let response = convert_to_responses_api(upstream, true).await;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let output = String::from_utf8(body.to_vec()).unwrap();

        assert_eq!(output.matches("web_search_call").count(), 3);
        assert_eq!(output.matches("event: response.completed").count(), 1);
    }

    #[test]
    fn chat_completion_to_responses_json_preserves_tool_calls() {
        let chat = json!({
            "id": "chatcmpl-tool",
            "model": "gpt-5.6-luna",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "test_tool", "arguments": "{}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let resp = chat_completion_to_responses_json(&chat);

        assert_eq!(resp["output"].as_array().unwrap().len(), 1);
        assert_eq!(resp["output"][0]["type"], "function_call");
        assert_eq!(resp["output"][0]["call_id"], "call_1");
        assert_eq!(resp["output"][0]["name"], "test_tool");
        assert_eq!(resp["output"][0]["arguments"], "{}");
    }

    #[test]
    fn chat_completion_to_responses_json_includes_reasoning() {
        let chat = json!({
            "id": "chatcmpl-rst000",
            "object": "chat.completion",
            "created": 1712345678,
            "model": "deepseek-r1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Final answer",
                    "reasoning_content": "Step by step thinking..."
                },
                "finish_reason": "stop"
            }],
            "usage": { "total_tokens": 42 }
        });

        let resp = chat_completion_to_responses_json(&chat);

        // First output item is reasoning (separate top-level item per Responses API spec)
        assert_eq!(resp["output"][0]["type"], "reasoning");
        assert_eq!(
            resp["output"][0]["content"].as_array().unwrap().len(),
            1,
            "reasoning item has one summary_text content part"
        );
        assert_eq!(resp["output"][0]["content"][0]["type"], "summary_text");
        assert_eq!(
            resp["output"][0]["content"][0]["text"],
            "Step by step thinking..."
        );

        // Second output item is the message
        assert_eq!(resp["output"][1]["type"], "message");
        assert_eq!(
            resp["output"][1]["content"].as_array().unwrap().len(),
            1,
            "message item has one output_text content part"
        );
        assert_eq!(resp["output"][1]["content"][0]["type"], "output_text");
        assert_eq!(resp["output"][1]["content"][0]["text"], "Final answer");
    }

    #[test]
    fn messages_preserves_arguments_before_identity_and_rejects_missing_identity() {
        let args_first = json!({"choices":[{"index":0,"delta":{"tool_calls":[{
            "index":7,"function":{"arguments":"{\"city\":\""}
        }]}}]});
        let identity = json!({"choices":[{"index":0,"delta":{"tool_calls":[{
            "index":7,"id":"call_7","function":{"name":"weather","arguments":"雪\"}"}
        }]}}]});
        let finish = json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]});

        let mut state = MessagesSseState::new();
        openai_chunk_to_messages(&mut state, &args_first).unwrap();
        let frames = openai_chunk_to_messages(&mut state, &identity).unwrap();
        let output = String::from_utf8(frames.concat()).unwrap();
        let first = output.find("{\\\"city\\\":\\\"").unwrap();
        let second = output.find("雪\\\"}").unwrap();
        assert!(first < second);
        openai_chunk_to_messages(&mut state, &finish).unwrap();

        let mut state = MessagesSseState::new();
        openai_chunk_to_messages(&mut state, &args_first).unwrap();
        let error = openai_chunk_to_messages(&mut state, &finish).unwrap_err();
        assert_eq!(error.code, "upstream_stream_invalid_tool_call");
    }

    #[test]
    fn responses_completion_output_is_numeric_and_sequence_overflow_is_error() {
        let mut state = ResponsesSseState::new();
        for index in [7usize, 2usize] {
            state.msg_item_done.insert(index, true);
            state.msg_text_buf.insert(index, format!("text_{index}"));
            state
                .added_item_id_map
                .insert(index, format!("item_{index}"));
            state
                .added_content_part_id_map
                .insert(index, format!("part_{index}"));
        }
        let output = String::from_utf8(state.flush_frames().unwrap().concat()).unwrap();
        assert!(output.find("item_2").unwrap() < output.find("item_7").unwrap());

        let mut state = ResponsesSseState::new();
        state.seq = u64::MAX;
        let error = state.flush_frames().unwrap_err();
        assert_eq!(error.code, "upstream_stream_arithmetic_overflow");
    }

    #[test]
    fn collected_responses_validates_unmarked_oversized_tail() {
        let mut body = b"data: {\"choices\":[]}\n\n".to_vec();
        body.extend(std::iter::repeat_n(
            b'x',
            crate::core::stream_framing::DEFAULT_MAX_SSE_FRAME_BYTES + 1,
        ));
        assert!(matches!(
            inspect_collected_responses_sse(&body),
            Err(FrameError::FrameTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn compat_converters_emit_completed_event_before_same_chunk_overflow() {
        let valid = b": comment\nevent: chunk\ndata: {\"id\":\"chatcmpl-c32\",\"model\":\"c32\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"valid-before-limit\"},\"finish_reason\":null}]}\n\n";
        let mut combined = valid.to_vec();
        combined.extend(std::iter::repeat_n(
            b'x',
            crate::core::stream_framing::DEFAULT_MAX_SSE_FRAME_BYTES + 1,
        ));

        for messages in [false, true] {
            let upstream_body =
                Body::from_stream(futures_util::stream::iter([Ok::<Bytes, std::io::Error>(
                    Bytes::from(combined.clone()),
                )]));
            let upstream =
                ([(header::CONTENT_TYPE, "text/event-stream")], upstream_body).into_response();
            let response = if messages {
                convert_to_messages_api(upstream).await
            } else {
                convert_to_responses_api(upstream, true).await
            };
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let output = String::from_utf8(body.to_vec()).unwrap();
            let valid_position = output.find("valid-before-limit").unwrap();
            let error_position = output.find("upstream_sse_frame_too_large").unwrap();
            assert!(valid_position < error_position, "{output}");
            assert_eq!(output.matches("upstream_sse_frame_too_large").count(), 1);
        }
    }
}
