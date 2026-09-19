use std::time::Duration;

use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::core::translator::limits::MAX_STREAM_ACCUMULATED_BYTES;
use crate::server::auth::require_api_key_with_reload;
use crate::server::codex_search::run_codex_standalone_search;
use crate::server::state::AppState;
use crate::types::ApiKey;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_BODY_LIMIT_BYTES: usize = 64 * 1024;
const MCP_QUERY_LIMIT_BYTES: usize = 8_000;
const MCP_SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/mcp", post(handle_mcp))
        .route_layer(middleware::from_fn_with_state(state, authenticate_mcp))
        .layer(DefaultBodyLimit::max(MCP_BODY_LIMIT_BYTES))
}

async fn authenticate_mcp(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    if request.headers().contains_key("origin") {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Browser Origin is not allowed for /v1/mcp"})),
        )
            .into_response();
    }
    let api_key = match require_api_key_with_reload(request.headers(), &state.db).await {
        Ok(api_key) => api_key,
        Err(error) => return super::auth_error_response(error),
    };
    request.extensions_mut().insert(api_key);
    next.run(request).await
}

async fn handle_mcp(
    State(state): State<AppState>,
    Extension(api_key): Extension<ApiKey>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Json(request) = match body {
        Ok(body) => body,
        Err(error) if error.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return transport_error(StatusCode::PAYLOAD_TOO_LARGE, "MCP request exceeds 64 KiB")
        }
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json_rpc_error(Value::Null, -32700, "Parse error")),
            )
                .into_response()
        }
    };
    let Some(object) = request.as_object() else {
        return Json(json_rpc_error(Value::Null, -32600, "Invalid Request")).into_response();
    };
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Json(json_rpc_error(id, -32600, "Invalid Request")).into_response();
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Json(json_rpc_error(id, -32600, "Invalid Request")).into_response();
    };

    if method != "initialize"
        && headers
            .get("mcp-protocol-version")
            .and_then(|value| value.to_str().ok())
            != Some(MCP_PROTOCOL_VERSION)
    {
        return transport_error(
            StatusCode::BAD_REQUEST,
            "MCP-Protocol-Version must be 2025-11-25",
        );
    }

    match method {
        "initialize" => initialize(id, object.get("params")),
        "notifications/initialized" | "notifications/cancelled" => {
            StatusCode::ACCEPTED.into_response()
        }
        "ping" => Json(json_rpc_result(id, json!({}))).into_response(),
        "tools/list" => Json(json_rpc_result(
            id,
            json!({
                "tools": [{
                    "name": "search",
                    "description": "Search the live web through the Codex standalone indexed-search service.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string", "minLength": 1, "maxLength": MCP_QUERY_LIMIT_BYTES},
                            "response_length": {
                                "type": "string",
                                "enum": ["short", "medium", "long"],
                                "default": "medium"
                            }
                        },
                        "required": ["query"],
                        "additionalProperties": false
                    }
                }]
            }),
        ))
        .into_response(),
        "tools/call" => tools_call(&state, &api_key, id, object.get("params")).await,
        _ => Json(json_rpc_error(id, -32601, "Method not found")).into_response(),
    }
}

fn initialize(id: Value, params: Option<&Value>) -> Response {
    if params
        .and_then(|value| value.get("protocolVersion"))
        .and_then(Value::as_str)
        != Some(MCP_PROTOCOL_VERSION)
    {
        return Json(json_rpc_error(
            id,
            -32602,
            "Only MCP protocol 2025-11-25 is supported",
        ))
        .into_response();
    }
    Json(json_rpc_result(
        id,
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {
                "name": "openproxy-codex-web",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    ))
    .into_response()
}

async fn tools_call(
    state: &AppState,
    api_key: &ApiKey,
    id: Value,
    params: Option<&Value>,
) -> Response {
    let Some(params) = params.and_then(Value::as_object) else {
        return Json(json_rpc_error(id, -32602, "Invalid tools/call params")).into_response();
    };
    if params.get("name").and_then(Value::as_str) != Some("search") {
        return Json(json_rpc_error(id, -32602, "Unknown tool")).into_response();
    }
    let Some(arguments) = params.get("arguments").and_then(Value::as_object) else {
        return Json(json_rpc_error(id, -32602, "Invalid search arguments")).into_response();
    };
    if arguments
        .keys()
        .any(|key| !matches!(key.as_str(), "query" | "response_length"))
    {
        return Json(json_rpc_error(id, -32602, "Unknown search argument")).into_response();
    }
    let Some(query) = arguments
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty() && query.len() <= MCP_QUERY_LIMIT_BYTES)
    else {
        return Json(json_rpc_error(
            id,
            -32602,
            "query must contain 1 to 8000 bytes",
        ))
        .into_response();
    };
    let response_length = arguments
        .get("response_length")
        .and_then(Value::as_str)
        .unwrap_or("medium");
    if !matches!(response_length, "short" | "medium" | "long") {
        return Json(json_rpc_error(
            id,
            -32602,
            "response_length must be short, medium, or long",
        ))
        .into_response();
    }

    let Some(_permit) = state.llm_admission.acquire_generation().await else {
        return super::admission::admission_rejection();
    };
    let result = match tokio::time::timeout(
        MCP_SEARCH_TIMEOUT,
        run_codex_standalone_search(state, api_key, query, response_length),
    )
    .await
    {
        Ok(Ok(output)) => json!({"content": [{"type": "text", "text": output.text}]}),
        Ok(Err(error)) => tool_error(&error.code, &error.message),
        Err(_) => tool_error(
            "codex_search_timeout",
            "Codex standalone search exceeded the 15 second server deadline",
        ),
    };
    let mut response = json_rpc_result(id, result);
    if serde_json::to_vec(&response)
        .map_or(true, |bytes| bytes.len() > MAX_STREAM_ACCUMULATED_BYTES)
    {
        response = json_rpc_result(
            response.get("id").cloned().unwrap_or(Value::Null),
            tool_error(
                "mcp_result_too_large",
                "Codex web search result exceeds the 16 MiB MCP limit",
            ),
        );
    }
    Json(response).into_response()
}

fn tool_error(code: &str, message: &str) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": format!("Codex web search failed [{code}]: {message}")
        }],
        "isError": true
    })
}

fn json_rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn json_rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

fn transport_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}
