//! Durable metadata-only logs for authenticated provider attempts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use serde_json::{json, Map, Value};

use crate::db::sqlite::repo::request_repo::{self, NewRequestDetail};
use crate::db::Db;
use crate::types::{ApiKey, TokenUsage};

const MAX_ERROR_CHARS: usize = 4_000;

#[derive(Clone)]
pub struct RequestLogContext {
    db: Arc<Db>,
    correlation_id: String,
    api_key_id: String,
    api_key_name: String,
    endpoint: Option<String>,
    requested_model: String,
}

impl RequestLogContext {
    pub fn new(
        db: Arc<Db>,
        api_key: &ApiKey,
        endpoint: Option<&str>,
        requested_model: &str,
    ) -> Self {
        Self {
            db,
            correlation_id: uuid::Uuid::new_v4().to_string(),
            api_key_id: api_key.id.clone(),
            api_key_name: api_key.name.clone(),
            endpoint: endpoint.map(str::to_string),
            requested_model: requested_model.to_string(),
        }
    }

    pub async fn start_attempt(
        &self,
        provider: &str,
        model: &str,
        connection_id: &str,
    ) -> Option<AttemptLog> {
        let id = uuid::Uuid::new_v4().to_string();
        let timestamp = Utc::now().to_rfc3339();
        let data = json!({
            "method": "POST",
            "endpoint": self.endpoint,
            "requestedModel": self.requested_model,
            "statusCode": Value::Null,
            "durationMs": 0,
            "tokens": Value::Null,
            "cost": 0.0,
            "error": Value::Null,
        });
        let sqlite = self.db.sqlite.clone();
        let record_id = id.clone();
        let record_timestamp = timestamp.clone();
        let record_provider = provider.to_string();
        let record_model = model.to_string();
        let record_connection = connection_id.to_string();
        let api_key_id = self.api_key_id.clone();
        let api_key_name = self.api_key_name.clone();
        let correlation_id = self.correlation_id.clone();
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
                        connection_id: Some(&record_connection),
                        status: "pending",
                        api_key_id: Some(&api_key_id),
                        api_key_name: Some(&api_key_name),
                        correlation_id: Some(&correlation_id),
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
                provider: provider.to_string(),
                model: model.to_string(),
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
    provider: String,
    model: String,
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
        error: Option<&str>,
    ) {
        if self.finished.load(Ordering::Acquire) {
            return;
        }
        let data = self.finished_data(status_code, tokens, error);
        persist_finish(self.db.clone(), self.id.clone(), status, data).await;
        self.finished.store(true, Ordering::Release);
    }

    fn finished_data(
        &self,
        status_code: Option<u16>,
        tokens: Option<&TokenUsage>,
        error: Option<&str>,
    ) -> Value {
        let mut data = self.data.as_object().cloned().unwrap_or_else(Map::new);
        data.insert("statusCode".into(), json!(status_code));
        data.insert(
            "durationMs".into(),
            json!(self.started.elapsed().as_millis()),
        );
        data.insert("tokens".into(), json!(tokens));
        data.insert(
            "cost".into(),
            json!(request_cost(&self.db, &self.provider, &self.model, tokens)),
        );
        data.insert(
            "error".into(),
            error
                .map(|value| Value::String(value.chars().take(MAX_ERROR_CHARS).collect()))
                .unwrap_or(Value::Null),
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
        let data = self.finished_data(None, None, Some("Request interrupted before completion"));
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

fn request_cost(db: &Db, provider: &str, model: &str, tokens: Option<&TokenUsage>) -> f64 {
    let Some(tokens) = tokens else {
        return 0.0;
    };
    let input = tokens.prompt_tokens.or(tokens.input_tokens).unwrap_or(0);
    let output = tokens
        .completion_tokens
        .or(tokens.output_tokens)
        .unwrap_or(0);
    let cache_creation = tokens.cache_creation_input_tokens.unwrap_or(0);
    let cache_read = tokens.cache_read_input_tokens.unwrap_or(0);
    let snapshot = db.snapshot();
    let pricing = if snapshot.pricing.is_empty() {
        crate::core::usage::Pricing::default()
    } else {
        crate::core::usage::Pricing::from_db(&snapshot.pricing)
    };
    pricing.calculate_cost(provider, model, input, output, cache_creation, cache_read)
}
