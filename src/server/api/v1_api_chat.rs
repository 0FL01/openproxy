use std::collections::BTreeMap;
use std::convert::Infallible;

use async_stream::stream;
use axum::body::{to_bytes, Body};
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};

use crate::core::stream_framing::{TextStreamFramer, TextStreamMode};
use crate::server::state::AppState;

use super::chat;
use super::cors::{cors_preflight_response, with_cors_json};

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/v1/api/chat",
        post(handle_ollama_chat).options(cors_options),
    )
}

async fn cors_options() -> Response {
    cors_preflight_response()
}

async fn handle_ollama_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Json(mut request_body) = match body {
        Ok(body) => body,
        Err(error) if error.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return with_cors_json(
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({ "error": super::LLM_BODY_TOO_LARGE_MESSAGE }),
            )
        }
        Err(_) => {
            return with_cors_json(
                StatusCode::BAD_REQUEST,
                json!({ "error": "Invalid JSON body" }),
            )
        }
    };

    let model = request_body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("llama3.2")
        .to_string();

    // Ollama defaults to streaming when the client omits the field entirely.
    if let Some(fields) = request_body.as_object_mut() {
        fields
            .entry("stream".to_string())
            .or_insert(Value::Bool(true));
    }

    let stream = request_body
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let upstream = chat::chat_completions(State(state), headers, Ok(Json(request_body))).await;
    if stream {
        convert_openai_sse_to_ollama(upstream, &model).await
    } else {
        convert_openai_json_to_ollama(upstream, &model).await
    }
}

async fn convert_openai_json_to_ollama(response: Response, fallback_model: &str) -> Response {
    let status = response.status();
    let body_bytes = match to_bytes(response.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return with_cors_json(
                StatusCode::BAD_GATEWAY,
                json!({ "error": "Failed to read upstream response" }),
            )
        }
    };

    let parsed = match serde_json::from_slice::<Value>(&body_bytes) {
        Ok(parsed) => parsed,
        Err(_) => return raw_json_response(status, body_bytes),
    };

    if !status.is_success() || parsed.get("error").is_some() || parsed.get("done").is_some() {
        return with_cors_json(status, parsed);
    }

    let Some(choice) = parsed
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return with_cors_json(status, parsed);
    };

    let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
    let mut ollama_message = Map::new();
    ollama_message.insert("role".to_string(), Value::String("assistant".to_string()));
    ollama_message.insert(
        "content".to_string(),
        Value::String(message_content(&message).unwrap_or_default()),
    );

    if let Some(reasoning) = message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        ollama_message.insert("thinking".to_string(), Value::String(reasoning.to_string()));
    }

    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        let converted = openai_tool_calls_to_ollama(tool_calls);
        if !converted.is_empty() {
            ollama_message.insert("tool_calls".to_string(), Value::Array(converted));
        }
    }

    let mut ollama = json!({
        "model": parsed
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(fallback_model),
        "created_at": chrono::Utc::now().to_rfc3339(),
        "message": Value::Object(ollama_message),
        "done": true,
        "done_reason": finish_reason_for_ollama(choice),
    });

    if let Some(usage) = parsed.get("usage") {
        ollama["prompt_eval_count"] = Value::from(
            usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
        ollama["eval_count"] = Value::from(
            usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
    }

    with_cors_json(status, ollama)
}

async fn convert_openai_sse_to_ollama(response: Response, model: &str) -> Response {
    let status = response.status();
    if !status.is_success() {
        return with_cors_nonstream_response(response).await;
    }

    let mut headers = response.headers().clone();
    let framing_mode = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.contains("text/event-stream") || value.contains("text/plain"))
        .map_or(TextStreamMode::Lines, |_| TextStreamMode::Sse);
    let mut body_stream = response.into_body().into_data_stream();
    let mut state = OllamaSseTransformState::new(framing_mode);
    let model = model.to_string();

    let stream = stream! {
        while let Some(next) = body_stream.next().await {
            let Ok(chunk) = next else {
                break;
            };

            let batch = state.transform(&chunk, &model);
            for event in batch.events {
                yield Ok::<_, Infallible>(event);
            }
            if let Some(error) = batch.error {
                yield Ok::<_, Infallible>(ndjson_line(json!({
                    "error": error.message,
                    "error_code": error.code,
                    "done": true
                })));
                return;
            }
        }

        let batch = state.finish_input(&model);
        for event in batch.events {
            yield Ok::<_, Infallible>(event);
        }
        if let Some(error) = batch.error {
            yield Ok::<_, Infallible>(ndjson_line(json!({
                "error": error.message,
                "error_code": error.code,
                "done": true
            })));
        }
    };

    let mut proxied = Response::new(Body::from_stream(stream));
    *proxied.status_mut() = status;
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    *proxied.headers_mut() = headers;
    proxied
}

struct OllamaSseTransformState {
    records: TextStreamFramer,
    pending_tool_calls: BTreeMap<u64, PendingToolCall>,
    retained_tool_bytes: usize,
    emitted_done: bool,
}

#[derive(Default)]
struct OllamaTransformBatch {
    events: Vec<Bytes>,
    error: Option<crate::core::translator::limits::StreamLimitError>,
}

#[derive(Default)]
struct PendingToolCall {
    name: String,
    arguments: String,
}

impl OllamaSseTransformState {
    fn new(mode: TextStreamMode) -> Self {
        Self {
            records: TextStreamFramer::new(mode),
            pending_tool_calls: BTreeMap::new(),
            retained_tool_bytes: 0,
            emitted_done: false,
        }
    }

    fn transform(&mut self, chunk: &[u8], model: &str) -> OllamaTransformBatch {
        let mut records = std::mem::replace(
            &mut self.records,
            TextStreamFramer::new(TextStreamMode::Sse),
        );
        let mut output = Vec::new();
        let mut transform_error = None;
        let frame_result = records.feed(chunk, |frame| {
            if transform_error.is_some() {
                return;
            }
            let Some(payload) = frame.payload() else {
                return;
            };
            match self.transform_payload(payload, model) {
                Ok(events) => output.extend(events),
                Err(error) => transform_error = Some(error),
            }
        });
        self.records = records;
        let error = transform_error.or_else(|| {
            frame_result
                .err()
                .map(|error| crate::core::translator::limits::StreamLimitError {
                    code: error.code(),
                    message: error.to_string(),
                })
        });
        OllamaTransformBatch {
            events: output,
            error,
        }
    }

    fn finish_input(&mut self, model: &str) -> OllamaTransformBatch {
        let mode = self.records.mode();
        let mut records = std::mem::replace(&mut self.records, TextStreamFramer::new(mode));
        let mut output = Vec::new();
        let mut transform_error = None;
        let frame_result = records.finish(|frame| {
            let Some(payload) = frame.payload() else {
                return;
            };
            match self.transform_payload(payload, model) {
                Ok(events) => output.extend(events),
                Err(error) => transform_error = Some(error),
            }
        });
        self.records = records;
        let error = transform_error.or_else(|| {
            frame_result
                .err()
                .map(|error| crate::core::translator::limits::StreamLimitError {
                    code: error.code(),
                    message: error.to_string(),
                })
        });
        if error.is_none() {
            if let Some(final_event) = self.finish(model) {
                output.push(final_event);
            }
        }
        OllamaTransformBatch {
            events: output,
            error,
        }
    }

    fn transform_payload(
        &mut self,
        data: &str,
        model: &str,
    ) -> Result<Vec<Bytes>, crate::core::translator::limits::StreamLimitError> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == "[DONE]" {
            return Ok(self.finish(model).into_iter().collect());
        }

        let Ok(parsed) = serde_json::from_str::<Value>(data) else {
            return Ok(Vec::new());
        };
        let Some(choice) = parsed
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return Ok(Vec::new());
        };

        let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));
        let finish_reason = choice.get("finish_reason").and_then(Value::as_str);
        let usage = parsed.get("usage");
        let mut output = Vec::new();

        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            self.absorb_tool_calls(tool_calls)?;
        }

        let content = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let thinking = delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());

        if content.is_some() || thinking.is_some() {
            let mut message = Map::new();
            message.insert("role".to_string(), Value::String("assistant".to_string()));
            message.insert(
                "content".to_string(),
                Value::String(content.unwrap_or_default().to_string()),
            );
            if let Some(thinking) = thinking {
                message.insert("thinking".to_string(), Value::String(thinking.to_string()));
            }

            output.push(ndjson_line(json!({
                "model": model,
                "message": Value::Object(message),
                "done": false,
            })));
        }

        if let Some(reason) = finish_reason {
            if self.emitted_done {
                return Ok(output);
            }

            let message = if self.pending_tool_calls.is_empty() {
                json!({
                    "role": "assistant",
                    "content": "",
                })
            } else {
                json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": self.pending_tool_calls_to_value()?,
                })
            };

            let mut done_chunk = json!({
                "model": model,
                "message": message,
                "done": true,
                "done_reason": if self.pending_tool_calls.is_empty() {
                    Value::String(reason.to_string())
                } else {
                    Value::String("tool_calls".to_string())
                },
            });
            if let Some(usage) = usage {
                done_chunk["prompt_eval_count"] = Value::from(
                    usage
                        .get("prompt_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                done_chunk["eval_count"] = Value::from(
                    usage
                        .get("completion_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
            }
            self.pending_tool_calls.clear();
            self.retained_tool_bytes = 0;
            self.emitted_done = true;
            output.push(ndjson_line(done_chunk));
        }

        Ok(output)
    }

    fn finish(&mut self, model: &str) -> Option<Bytes> {
        if self.emitted_done {
            return None;
        }

        self.pending_tool_calls.clear();
        self.retained_tool_bytes = 0;
        self.emitted_done = true;
        Some(ndjson_line(json!({
            "model": model,
            "message": {
                "role": "assistant",
                "content": "",
            },
            "done": true,
        })))
    }

    fn absorb_tool_calls(
        &mut self,
        tool_calls: &[Value],
    ) -> Result<(), crate::core::translator::limits::StreamLimitError> {
        for tool_call in tool_calls {
            let index = crate::core::translator::limits::wire_index(
                tool_call.get("index"),
                "tool_calls[].index",
            )?;
            if !self.pending_tool_calls.contains_key(&index)
                && self.pending_tool_calls.len()
                    >= crate::core::translator::limits::MAX_STREAM_TOOL_CALLS
            {
                return Err(crate::core::translator::limits::StreamLimitError::too_many(
                    "tool calls",
                    crate::core::translator::limits::MAX_STREAM_TOOL_CALLS,
                ));
            }
            let pending = self.pending_tool_calls.entry(index).or_default();

            if let Some(name) = tool_call
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
            {
                crate::core::translator::limits::checked_append(
                    &mut pending.name,
                    name,
                    crate::core::translator::limits::MAX_STREAM_TOOL_ARGUMENT_BYTES,
                    &mut self.retained_tool_bytes,
                    "tool name",
                )?;
            }
            if let Some(arguments) = tool_call
                .get("function")
                .and_then(|function| function.get("arguments"))
                .and_then(Value::as_str)
            {
                crate::core::translator::limits::checked_append(
                    &mut pending.arguments,
                    arguments,
                    crate::core::translator::limits::MAX_STREAM_TOOL_ARGUMENT_BYTES,
                    &mut self.retained_tool_bytes,
                    "tool arguments",
                )?;
            }
        }
        Ok(())
    }

    fn pending_tool_calls_to_value(
        &self,
    ) -> Result<Vec<Value>, crate::core::translator::limits::StreamLimitError> {
        self.pending_tool_calls
            .values()
            .map(|pending| {
                if pending.name.is_empty() {
                    return Err(crate::core::translator::limits::StreamLimitError {
                        code: "upstream_stream_invalid_tool_call",
                        message: "Ollama compatibility tool call ended without a name".to_string(),
                    });
                }
                let arguments =
                    serde_json::from_str::<Value>(&pending.arguments).map_err(|_| {
                        crate::core::translator::limits::StreamLimitError {
                            code: "upstream_stream_invalid_tool_call",
                            message: "Ollama compatibility tool arguments are incomplete"
                                .to_string(),
                        }
                    })?;
                Ok(json!({
                    "function": {
                        "name": pending.name,
                        "arguments": arguments,
                    }
                }))
            })
            .collect()
    }
}

fn message_content(message: &Value) -> Option<String> {
    match message.get("content") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let text = parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| part.as_str())
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some(text)
        }
        _ => None,
    }
}

fn finish_reason_for_ollama(choice: &Value) -> String {
    let has_tool_calls = choice
        .get("message")
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
        .is_some_and(|tool_calls| !tool_calls.is_empty());
    if has_tool_calls {
        return "tool_calls".to_string();
    }

    choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop")
        .to_string()
}

fn openai_tool_calls_to_ollama(tool_calls: &[Value]) -> Vec<Value> {
    tool_calls
        .iter()
        .map(|tool_call| {
            let name = tool_call
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = tool_call
                .get("function")
                .and_then(|function| function.get("arguments"))
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));
            let arguments = if let Some(raw) = arguments.as_str() {
                serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::Object(Map::new()))
            } else {
                arguments
            };

            json!({
                "function": {
                    "name": name,
                    "arguments": arguments,
                }
            })
        })
        .collect()
}

fn ndjson_line(payload: Value) -> Bytes {
    Bytes::from(format!("{}\n", serde_json::to_string(&payload).unwrap()))
}

async fn with_cors_nonstream_response(response: Response) -> Response {
    let status = response.status();
    let headers = response.headers().clone();
    let body_bytes = match to_bytes(response.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return with_cors_json(
                StatusCode::BAD_GATEWAY,
                json!({ "error": "Failed to read upstream response" }),
            )
        }
    };

    let mut proxied = Response::new(Body::from(body_bytes));
    *proxied.status_mut() = status;
    for (name, value) in &headers {
        proxied.headers_mut().insert(name, value.clone());
    }
    proxied.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    proxied
}

fn raw_json_response(status: StatusCode, body_bytes: Bytes) -> Response {
    let mut response = Response::new(Body::from(body_bytes));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_tool_calls_convert_to_ollama_shape() {
        let converted = openai_tool_calls_to_ollama(&[json!({
            "function": {
                "name": "search",
                "arguments": "{\"query\":\"rust\"}"
            }
        })]);

        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["function"]["name"], "search");
        assert_eq!(converted[0]["function"]["arguments"]["query"], "rust");
    }

    #[test]
    fn streaming_transformer_emits_content_then_single_done() {
        let mut state = OllamaSseTransformState::new(TextStreamMode::Lines);
        let output = state.transform(
            br#"data: {"choices":[{"delta":{"content":"hello"},"finish_reason":null}]}
data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2}}
data: [DONE]
"#,
            "llama3.2",
        ).events;

        assert_eq!(output.len(), 2);
        let first = String::from_utf8(output[0].to_vec()).unwrap();
        assert!(first.contains("\"content\":\"hello\""));
        assert!(first.contains("\"done\":false"));

        let second = String::from_utf8(output[1].to_vec()).unwrap();
        assert!(second.contains("\"done\":true"));
        assert!(second.contains("\"eval_count\":2"));
    }

    #[test]
    fn streaming_transformer_accumulates_tool_calls() {
        let mut state = OllamaSseTransformState::new(TextStreamMode::Lines);
        let output = state.transform(
            br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"sea","arguments":"{\"q\""}}]},"finish_reason":null}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"rch","arguments":":\"rust\"}"}}]},"finish_reason":"tool_calls"}]}
"#,
            "llama3.2",
        ).events;

        assert_eq!(output.len(), 1);
        let done = String::from_utf8(output[0].to_vec()).unwrap();
        assert!(done.contains("\"done_reason\":\"tool_calls\""));
        assert!(done.contains("\"name\":\"search\""));
        assert!(done.contains("\"q\":\"rust\""));
    }

    #[test]
    fn streaming_transformer_distinguishes_ndjson_and_multiline_sse() {
        let ndjson = concat!(
            "{\"choices\":[{\"delta\":{\"content\":\"nd-one\"},\"finish_reason\":null}]}\n",
            "{\"choices\":[{\"delta\":{\"content\":\"nd-two\"},\"finish_reason\":null}]}\n"
        );
        for split in 0..=ndjson.len() {
            let mut state = OllamaSseTransformState::new(TextStreamMode::Lines);
            let mut output = state.transform(&ndjson.as_bytes()[..split], "llama").events;
            output.extend(state.transform(&ndjson.as_bytes()[split..], "llama").events);
            let text = output
                .iter()
                .map(|bytes| String::from_utf8_lossy(bytes))
                .collect::<String>();
            assert!(text.contains("nd-one"), "split {split}: {text}");
            assert!(text.contains("nd-two"), "split {split}: {text}");
        }

        let sse = b": keepalive\r\nevent: chunk\r\ndata: {\"choices\":[{\"delta\":\r\ndata: {\"content\":\"sse-multiline\"},\"finish_reason\":null}]}\r\n\r\n";
        let mut state = OllamaSseTransformState::new(TextStreamMode::Sse);
        let mut output = Vec::new();
        for byte in sse {
            output.extend(state.transform(std::slice::from_ref(byte), "llama").events);
        }
        assert_eq!(output.len(), 1);
        assert!(String::from_utf8_lossy(&output[0]).contains("sse-multiline"));
    }

    #[test]
    fn streaming_transformer_emits_valid_record_before_terminal_overflow() {
        let valid = b"data: {\"choices\":[{\"delta\":{\"content\":\"before-limit\"},\"finish_reason\":null}]}\n\n";
        let mut input = valid.to_vec();
        input.extend(std::iter::repeat_n(b'x', 257));
        let mut state = OllamaSseTransformState::new(TextStreamMode::Sse);
        state.records = TextStreamFramer::with_max_frame_bytes(TextStreamMode::Sse, 256);
        let batch = state.transform(&input, "llama");
        assert!(batch
            .events
            .iter()
            .any(|event| String::from_utf8_lossy(event).contains("before-limit")));
        assert_eq!(
            batch.error.as_ref().map(|error| error.code),
            Some("upstream_sse_frame_too_large")
        );
    }
}
