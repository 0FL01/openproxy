//! Durable metadata-only logs for authenticated provider attempts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{routing, Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::db::sqlite::repo::request_repo::{self, NewRequestDetail};
use crate::db::Db;
use crate::server::state::AppState;
use crate::types::TokenUsage;

#[derive(Clone)]
pub struct RequestLogContext {
    db: Arc<Db>,
    route: String,
}

impl RequestLogContext {
    pub fn new(db: Arc<Db>, route: &str) -> Self {
        Self {
            db,
            route: route.to_string(),
        }
    }

    pub async fn start_attempt(&self, provider: &str, model: &str) -> Option<AttemptLog> {
        let id = uuid::Uuid::new_v4().to_string();
        let timestamp = Utc::now().to_rfc3339();
        let data = json!({
            "route": self.route,
            "statusCode": Value::Null,
            "durationMs": 0,
            "inputTokens": Value::Null,
            "outputTokens": Value::Null,
        });
        let sqlite = self.db.sqlite.clone();
        let record_id = id.clone();
        let record_timestamp = timestamp.clone();
        let record_provider = provider.to_string();
        let record_model = model.to_string();
        let record_data = data.clone();
        let inserted = tokio::task::spawn_blocking(move || {
            sqlite.with_conn(|conn| {
                request_repo::insert(
                    conn,
                    &NewRequestDetail {
                        id: &record_id,
                        timestamp: &record_timestamp,
                        provider: Some(&record_provider),
                        model: Some(&record_model),
                        connection_id: None,
                        status: "pending",
                        api_key_id: None,
                        api_key_name: None,
                        correlation_id: None,
                        data: &record_data,
                    },
                )
            })
        })
        .await;

        match inserted {
            Ok(Ok(())) => Some(AttemptLog {
                db: self.db.clone(),
                id,
                started: Instant::now(),
                data,
                finished: Arc::new(AtomicBool::new(false)),
            }),
            Ok(Err(error)) => {
                tracing::warn!(target: "openproxy::logs", %error, "failed to start request log");
                None
            }
            Err(error) => {
                tracing::warn!(target: "openproxy::logs", %error, "request log task failed");
                None
            }
        }
    }
}

pub struct AttemptLog {
    db: Arc<Db>,
    id: String,
    started: Instant,
    data: Value,
    finished: Arc<AtomicBool>,
}

impl AttemptLog {
    pub async fn finish(
        self,
        status: &'static str,
        status_code: Option<u16>,
        tokens: Option<&TokenUsage>,
    ) {
        if self.finished.load(Ordering::Acquire) {
            return;
        }
        let data = self.finished_data(status_code, tokens);
        persist_finish(self.db.clone(), self.id.clone(), status, data).await;
        self.finished.store(true, Ordering::Release);
    }

    fn finished_data(&self, status_code: Option<u16>, tokens: Option<&TokenUsage>) -> Value {
        let mut data = self.data.as_object().cloned().unwrap_or_else(Map::new);
        data.insert("statusCode".into(), json!(status_code));
        data.insert(
            "durationMs".into(),
            json!(self.started.elapsed().as_millis()),
        );
        data.insert(
            "inputTokens".into(),
            json!(tokens.and_then(|tokens| tokens.prompt_tokens.or(tokens.input_tokens))),
        );
        data.insert(
            "outputTokens".into(),
            json!(tokens.and_then(|tokens| tokens.completion_tokens.or(tokens.output_tokens))),
        );
        Value::Object(data)
    }
}

impl Drop for AttemptLog {
    fn drop(&mut self) {
        if self.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let data = self.finished_data(None, None);
        let db = self.db.clone();
        let id = self.id.clone();
        runtime.spawn(async move {
            persist_finish(db, id, "interrupted", data).await;
        });
    }
}

async fn persist_finish(db: Arc<Db>, id: String, status: &'static str, data: Value) {
    let sqlite = db.sqlite.clone();
    let result = tokio::task::spawn_blocking(move || {
        sqlite.with_conn(|conn| request_repo::finish(conn, &id, status, &data))
    })
    .await;
    match result {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            tracing::warn!(target: "openproxy::logs", "request log disappeared before finish");
        }
        Ok(Err(error)) => {
            tracing::warn!(target: "openproxy::logs", %error, "failed to finish request log");
        }
        Err(error) => {
            tracing::warn!(target: "openproxy::logs", %error, "request log finish task failed");
        }
    }
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/request-logs", routing::get(get_request_logs))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestLogsQuery {
    page: Option<usize>,
    page_size: Option<usize>,
    provider: Option<String>,
    model: Option<String>,
    status: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestLogsPayload {
    requests: Vec<RequestLogRecord>,
    pagination: RequestLogsPagination,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestLogsPagination {
    page: usize,
    page_size: usize,
    total_items: usize,
    total_pages: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestLogRecord {
    request_id: String,
    timestamp: String,
    route: String,
    provider: String,
    model: String,
    status: String,
    status_code: Option<u16>,
    duration_ms: u64,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

async fn get_request_logs(
    State(state): State<AppState>,
    Query(query): Query<RequestLogsQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) =
        crate::server::api::require_dashboard_or_management_api_key(&headers, &state)
    {
        return response;
    }

    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);
    if page == 0 || !(1..=100).contains(&page_size) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "page must be >= 1 and pageSize must be between 1 and 100" })),
        )
            .into_response();
    }

    let clean = |value: Option<String>| {
        value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let provider = clean(query.provider);
    let model = clean(query.model);
    let status = clean(query.status);
    let start_date = query.start_date.as_deref().and_then(parse_timestamp);
    let end_date = query.end_date.as_deref().and_then(parse_timestamp);
    let offset = (page - 1) * page_size;
    let sqlite = state.db.sqlite.clone();
    let result = tokio::task::spawn_blocking(move || {
        sqlite.with_conn(|conn| {
            let filter = request_repo::RequestDetailFilter {
                provider: provider.as_deref(),
                model: model.as_deref(),
                status: status.as_deref(),
                start_date: start_date.as_deref(),
                end_date: end_date.as_deref(),
                ..Default::default()
            };
            let total = request_repo::count(conn, &filter)?;
            let rows = request_repo::list(conn, &filter, page_size, offset)?;
            Ok((total, rows))
        })
    })
    .await;

    let (total_items, rows) = match result {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            tracing::error!(target: "openproxy::logs", %error, "failed to query request logs");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(error) => {
            tracing::error!(target: "openproxy::logs", %error, "request log query task failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let total_pages = total_items.div_ceil(page_size);
    Json(RequestLogsPayload {
        requests: rows.into_iter().map(request_log_from_row).collect(),
        pagination: RequestLogsPagination {
            page,
            page_size,
            total_items,
            total_pages,
        },
    })
    .into_response()
}

fn parse_timestamp(value: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc).to_rfc3339())
}

fn request_log_from_row(row: request_repo::RequestDetailRow) -> RequestLogRecord {
    RequestLogRecord {
        request_id: row.id,
        timestamp: row.timestamp,
        route: row
            .data
            .get("route")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        provider: row.provider.unwrap_or_default(),
        model: row.model.unwrap_or_default(),
        status: row.status.unwrap_or_else(|| "interrupted".to_string()),
        status_code: row
            .data
            .get("statusCode")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok()),
        duration_ms: row
            .data
            .get("durationMs")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        input_tokens: row.data.get("inputTokens").and_then(Value::as_u64),
        output_tokens: row.data.get("outputTokens").and_then(Value::as_u64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_log_exposes_only_metadata() {
        let record = request_log_from_row(request_repo::RequestDetailRow {
            id: "request-1".into(),
            timestamp: "2026-09-15T12:00:00Z".into(),
            provider: Some("openai".into()),
            model: Some("gpt-5".into()),
            connection_id: Some("secret-connection".into()),
            status: Some("success".into()),
            api_key_id: Some("secret-key".into()),
            api_key_name: Some("private".into()),
            correlation_id: Some("internal".into()),
            data: json!({
                "route": "work",
                "statusCode": 200,
                "durationMs": 42,
                "inputTokens": 10,
                "outputTokens": 20,
                "request": "secret prompt",
                "response": "secret response"
            }),
        });

        let value = serde_json::to_value(record).unwrap();
        assert_eq!(value["requestId"], "request-1");
        assert_eq!(value["route"], "work");
        assert_eq!(value["inputTokens"], 10);
        let serialized = value.to_string();
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains("private"));
        assert!(!serialized.contains("internal"));
    }
}
