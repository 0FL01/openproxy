use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::core::account_fallback::{BACKOFF_BASE_MS, BACKOFF_MAX_MS, MAX_BACKOFF_LEVEL};
use crate::types::Combo;

pub mod capabilities;

const LONG_COOLDOWN: Duration = Duration::from_secs(120);
const TRANSIENT_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboAttemptError {
    pub status: u16,
    pub message: String,
    pub retry_after: Option<DateTime<Utc>>,
    pub upstream_body: Option<Vec<u8>>,
}

impl ComboAttemptError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
            upstream_body: None,
        }
    }
}

pub fn parse_retry_after_from_body(body: &[u8]) -> Option<DateTime<Utc>> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let retry_after = value
        .get("error")
        .and_then(|error| error.get("retryAfter"))
        .or_else(|| value.get("retryAfter"))?;

    if let Some(value) = retry_after.as_str() {
        if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
            return Some(timestamp.with_timezone(&Utc));
        }
        if let Ok(seconds) = value.parse::<i64>() {
            return Utc::now().checked_add_signed(chrono::Duration::seconds(seconds));
        }
        return None;
    }

    retry_after
        .as_i64()
        .or_else(|| retry_after.as_f64().map(|seconds| seconds as i64))
        .and_then(|seconds| Utc::now().checked_add_signed(chrono::Duration::seconds(seconds)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboExecutionError {
    pub status: u16,
    pub message: String,
    pub earliest_retry_after: Option<DateTime<Utc>>,
    pub upstream_body: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackDecision {
    pub should_fallback: bool,
    pub cooldown: Duration,
    pub new_backoff_level: Option<u32>,
}

pub fn get_quota_cooldown(backoff_level: u32) -> Duration {
    let level = backoff_level.saturating_sub(1);
    let cooldown_ms = BACKOFF_BASE_MS.saturating_mul(2u64.saturating_pow(level));
    Duration::from_millis(cooldown_ms.min(BACKOFF_MAX_MS))
}

pub fn check_fallback_error(status: u16, error_text: &str, backoff_level: u32) -> FallbackDecision {
    use crate::core::config::error_config::{classify_error, ErrorClassification};

    match classify_error(Some(error_text), Some(status)) {
        ErrorClassification::Backoff => {
            let new_level = (backoff_level + 1).min(MAX_BACKOFF_LEVEL);
            FallbackDecision {
                should_fallback: true,
                cooldown: get_quota_cooldown(new_level),
                new_backoff_level: Some(new_level),
            }
        }
        ErrorClassification::Cooldown(duration) => FallbackDecision {
            should_fallback: true,
            cooldown: duration,
            new_backoff_level: None,
        },
        ErrorClassification::NoMatch => FallbackDecision {
            should_fallback: true,
            cooldown: TRANSIENT_COOLDOWN,
            new_backoff_level: None,
        },
        ErrorClassification::Permanent => FallbackDecision {
            should_fallback: true,
            cooldown: LONG_COOLDOWN,
            new_backoff_level: None,
        },
    }
}

pub fn get_combo_models_from_data(model_str: &str, combos: &[Combo]) -> Option<Vec<String>> {
    if model_str.contains('/') {
        return None;
    }

    combos
        .iter()
        .find(|combo| combo.name == model_str && !combo.models.is_empty())
        .map(|combo| combo.models.clone())
}

pub fn get_disabled_members_for_combo(combo_name: &str, combos: &[Combo]) -> Vec<String> {
    combos
        .iter()
        .find(|combo| combo.name == combo_name)
        .map(|combo| combo.disabled_models.clone())
        .unwrap_or_default()
}

/// Try explicitly configured combo members in their declared order.
pub async fn execute_combo<T, F, Fut>(
    models: &[String],
    disabled_members: &[String],
    mut handle_single_model: F,
) -> Result<T, ComboExecutionError>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<T, ComboAttemptError>>,
{
    let active: Vec<&String> = models
        .iter()
        .filter(|model| !disabled_members.contains(model))
        .collect();

    if active.is_empty() {
        return Err(ComboExecutionError {
            status: 400,
            message: "All combo members are disabled".into(),
            earliest_retry_after: None,
            upstream_body: None,
        });
    }

    let mut first_error = None;
    let mut last_error = None;
    let mut earliest_retry_after = None;

    for model in active {
        match handle_single_model(model).await {
            Ok(result) => return Ok(result),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error.clone());
                }
                if let Some(retry_after) = error.retry_after {
                    earliest_retry_after = match earliest_retry_after {
                        Some(current) if current <= retry_after => Some(current),
                        _ => Some(retry_after),
                    };
                }
                last_error = Some(error);
            }
        }
    }

    let first_status = first_error.as_ref().map(|error| error.status);
    let message = last_error
        .as_ref()
        .map(|error| error.message.clone())
        .or_else(|| first_error.as_ref().map(|error| error.message.clone()))
        .unwrap_or_else(|| "All combo models unavailable".into());
    let status = if message.to_lowercase().contains("no credentials") {
        503
    } else {
        match first_status.unwrap_or(503) {
            0 => 503,
            status => status,
        }
    };
    let upstream_body = last_error
        .as_ref()
        .and_then(|error| error.upstream_body.clone())
        .or_else(|| {
            first_error
                .as_ref()
                .and_then(|error| error.upstream_body.clone())
        });

    Err(ComboExecutionError {
        status,
        message,
        earliest_retry_after,
        upstream_body,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[tokio::test]
    async fn explicit_fallback_keeps_order_and_stops_after_success() {
        let models = vec!["first".into(), "second".into(), "third".into()];
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let seen = attempts.clone();

        let result = execute_combo(&models, &[], move |model| {
            let model = model.to_string();
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(model.clone());
                if model == "second" {
                    Ok(model)
                } else {
                    Err(ComboAttemptError::new(503, "unavailable"))
                }
            }
        })
        .await;

        assert_eq!(result, Ok("second".to_string()));
        assert_eq!(*attempts.lock().unwrap(), ["first", "second"]);
    }
}
