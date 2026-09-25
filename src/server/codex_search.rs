use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::Duration;

use regex::{Captures, Regex};
use reqwest::StatusCode;
use serde_json::Value;

use crate::core::account_fallback::GenerationAttemptBudget;
use crate::core::executor::{
    diagnostic_body_limit, read_reqwest_body, CodexSearchExecutionRequest, CodexSearchExecutor,
};
use crate::core::proxy::resolve_proxy_target;
use crate::core::translator::limits::MAX_STREAM_ACCUMULATED_BYTES;
use crate::oauth::token_refresh::{
    connection_credential_generation, CONNECTION_REFRESH_COORDINATOR,
};
use crate::server::application_logs::{error_kind, RequestLogContext};
use crate::server::request_logger::new_request_id;
use crate::server::state::AppState;
use crate::types::{ApiKey, AppDb, ProviderConnection, ProviderNode};

const MAX_SEARCH_RESULTS: usize = 512;
const MAX_TRANSIENT_RETRIES: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexSearchOutput {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexSearchError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone)]
struct IndexedResult {
    ref_id: Option<String>,
    url: Option<String>,
    title: Option<String>,
    snippet: Option<String>,
}

pub(crate) async fn run_codex_standalone_search(
    state: &AppState,
    api_key: &ApiKey,
    query: &str,
    response_length: &str,
) -> Result<CodexSearchOutput, CodexSearchError> {
    let initial = state.db.snapshot();
    let eligible = active_codex_connections(&initial, &HashSet::new())
        .len()
        .max(1);
    let attempt_budget = GenerationAttemptBudget::new(
        eligible
            .saturating_mul(MAX_TRANSIENT_RETRIES + 1)
            .saturating_add(1),
    );
    let request_id = format!("openproxy_{}", new_request_id());
    let log_context = RequestLogContext::new(state.db.clone(), api_key, "/v1/mcp#codex_web_search");
    let mut excluded = HashSet::new();
    let mut last_error = None;
    let mut auth_recovery_used = false;
    let mut reloaded = false;

    'accounts: loop {
        let snapshot = state.db.snapshot();
        let Some(connection) = active_codex_connections(&snapshot, &excluded)
            .into_iter()
            .next()
        else {
            if !reloaded {
                reloaded = true;
                if state.db.reload_snapshot().await.is_ok() {
                    continue;
                }
            }
            return Err(last_error.unwrap_or_else(|| CodexSearchError {
                code: "codex_search_credentials_unavailable".to_string(),
                message: "No active Codex account with usable credentials is configured"
                    .to_string(),
            }));
        };
        let connection =
            crate::oauth::token_refresh::codex_connection_for_request(state.db.clone(), connection)
                .await;
        let proxy = resolve_proxy_target(&snapshot, &connection, &snapshot.settings);
        let executor =
            CodexSearchExecutor::new(state.client_pool.clone(), codex_provider_node(&snapshot));
        let mut transient_retries = 0usize;

        loop {
            if !attempt_budget.try_acquire() {
                return Err(last_error.unwrap_or_else(|| CodexSearchError {
                    code: "codex_search_attempt_budget_exhausted".to_string(),
                    message: format!(
                        "Codex standalone search attempt budget exhausted ({}/{})",
                        attempt_budget.used(),
                        attempt_budget.max()
                    ),
                }));
            }
            let attempt_log = log_context
                .start_attempt("codex", "standalone-web-search")
                .await;
            let response = executor
                .execute(CodexSearchExecutionRequest {
                    request_id: &request_id,
                    query,
                    response_length,
                    credentials: &connection,
                    proxy: proxy.as_ref(),
                })
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    if let Some(attempt_log) = attempt_log {
                        attempt_log
                            .finish("error", None, None, Some(error_kind::UPSTREAM_FAILURE))
                            .await;
                    }
                    last_error = Some(CodexSearchError {
                        code: "codex_search_transport_error".to_string(),
                        message: format!("Codex standalone search transport failed: {error:?}"),
                    });
                    excluded.insert(connection.id.clone());
                    continue 'accounts;
                }
            };
            let status = response.status();
            let body_limit = if status.is_success() {
                MAX_STREAM_ACCUMULATED_BYTES
            } else {
                diagnostic_body_limit()
            };
            let body = match read_reqwest_body(response, body_limit).await {
                Ok(body) => body,
                Err(error) => {
                    if let Some(attempt_log) = attempt_log {
                        attempt_log
                            .finish(
                                "error",
                                Some(status.as_u16()),
                                None,
                                Some(error_kind::LOCAL_FAILURE),
                            )
                            .await;
                    }
                    return Err(CodexSearchError {
                        code: "codex_search_response_limit".to_string(),
                        message: error.to_string(),
                    });
                }
            };

            if status.is_success() {
                let normalized = normalize_search_response(&body);
                match normalized {
                    Ok(output) => {
                        if let Some(attempt_log) = attempt_log {
                            attempt_log
                                .finish("success", Some(status.as_u16()), None, None)
                                .await;
                        }
                        return Ok(output);
                    }
                    Err(error) => {
                        if let Some(attempt_log) = attempt_log {
                            attempt_log
                                .finish(
                                    "error",
                                    Some(status.as_u16()),
                                    None,
                                    Some(error_kind::LOCAL_FAILURE),
                                )
                                .await;
                        }
                        return Err(error);
                    }
                }
            }

            let message = upstream_error_message(status, &body);
            let refreshable_auth =
                matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN);
            let error_kind_value = if refreshable_auth {
                error_kind::AUTH_FAILURE
            } else if status == StatusCode::TOO_MANY_REQUESTS {
                error_kind::RATE_LIMITED
            } else if matches!(status.as_u16(), 400 | 413 | 422) {
                error_kind::INVALID_REQUEST
            } else {
                error_kind::UPSTREAM_FAILURE
            };
            if let Some(attempt_log) = attempt_log {
                attempt_log
                    .finish("error", Some(status.as_u16()), None, Some(error_kind_value))
                    .await;
            }
            last_error = Some(CodexSearchError {
                code: search_error_code(status).to_string(),
                message,
            });

            if matches!(status.as_u16(), 400 | 413 | 422) {
                return Err(last_error.expect("standalone search error recorded"));
            }
            if refreshable_auth && connection.refresh_token.is_some() && !auth_recovery_used {
                auth_recovery_used = true;
                let observed_generation = connection_credential_generation(&connection);
                if CONNECTION_REFRESH_COORDINATOR
                    .refresh_connection(
                        state.db.clone(),
                        &connection.provider,
                        &connection.id,
                        observed_generation,
                    )
                    .await
                    .is_ok()
                {
                    continue 'accounts;
                }
            }
            if matches!(
                status,
                StatusCode::BAD_GATEWAY
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
            ) && transient_retries < MAX_TRANSIENT_RETRIES
            {
                transient_retries += 1;
                tokio::time::sleep(Duration::from_millis(500 * transient_retries as u64)).await;
                continue;
            }
            excluded.insert(connection.id.clone());
            continue 'accounts;
        }
    }
}

fn active_codex_connections(
    snapshot: &AppDb,
    excluded: &HashSet<String>,
) -> Vec<ProviderConnection> {
    let mut connections = snapshot
        .provider_connections
        .iter()
        .filter(|connection| {
            connection.provider == "codex"
                && connection.is_active()
                && has_credentials(connection)
                && !excluded.contains(&connection.id)
        })
        .cloned()
        .collect::<Vec<_>>();
    connections.sort_by(|left, right| {
        (left.priority.unwrap_or(u32::MAX), left.id.as_str())
            .cmp(&(right.priority.unwrap_or(u32::MAX), right.id.as_str()))
    });
    connections
}

fn has_credentials(connection: &ProviderConnection) -> bool {
    connection
        .api_key
        .as_deref()
        .or(connection.access_token.as_deref())
        .is_some_and(|token| !token.trim().is_empty())
}

fn codex_provider_node(snapshot: &AppDb) -> Option<ProviderNode> {
    snapshot
        .provider_nodes
        .iter()
        .find(|node| {
            node.id == "codex"
                || node.prefix.as_deref() == Some("codex")
                || node.prefix.as_deref() == Some("cx")
        })
        .cloned()
}

fn normalize_search_response(body: &[u8]) -> Result<CodexSearchOutput, CodexSearchError> {
    let value = serde_json::from_slice::<Value>(body).map_err(|error| CodexSearchError {
        code: "codex_search_invalid_response".to_string(),
        message: format!("Codex standalone search returned invalid JSON: {error}"),
    })?;
    let object = value.as_object().ok_or_else(|| CodexSearchError {
        code: "codex_search_invalid_response".to_string(),
        message: "Codex standalone search returned a non-object response".to_string(),
    })?;
    let raw_results = object
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if raw_results.len() > MAX_SEARCH_RESULTS {
        return Err(CodexSearchError {
            code: "codex_search_result_limit".to_string(),
            message: format!(
                "Codex standalone search returned more than {MAX_SEARCH_RESULTS} results"
            ),
        });
    }
    let results = raw_results
        .iter()
        .filter_map(normalize_result)
        .collect::<Vec<_>>();
    let output = object
        .get("output")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|output| !output.is_empty());
    let text = match output {
        Some(output) => format_endpoint_output(output, &results)?,
        None if !results.is_empty() => format_structured_results(&results)?,
        None => {
            return Err(CodexSearchError {
                code: "codex_search_empty_response".to_string(),
                message: "Codex standalone search returned no output or indexed results"
                    .to_string(),
            })
        }
    };
    Ok(CodexSearchOutput { text })
}

fn normalize_result(value: &Value) -> Option<IndexedResult> {
    let object = value.as_object()?;
    let string = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let result = IndexedResult {
        ref_id: string("ref_id"),
        url: string("url"),
        title: string("title"),
        snippet: string("snippet"),
    };
    (result.ref_id.is_some()
        || result.url.is_some()
        || result.title.is_some()
        || result.snippet.is_some())
    .then_some(result)
}

fn format_endpoint_output(
    output: &str,
    results: &[IndexedResult],
) -> Result<String, CodexSearchError> {
    let mut text = replace_citation_markers(output, results);
    let sources = indexed_sources(results);
    if !sources.is_empty() && !text.contains("Sources:") {
        append_bounded(&mut text, "\n\nSources:")?;
        for (index, (title, url)) in sources.iter().enumerate() {
            append_bounded(&mut text, "\n")?;
            append_bounded(&mut text, &(index + 1).to_string())?;
            append_bounded(&mut text, ". ")?;
            if !title.is_empty() {
                append_bounded(&mut text, title)?;
                append_bounded(&mut text, ": ")?;
            }
            append_bounded(&mut text, url)?;
        }
    }
    ensure_output_limit(text)
}

fn format_structured_results(results: &[IndexedResult]) -> Result<String, CodexSearchError> {
    let mut text = String::new();
    for (index, result) in results.iter().enumerate() {
        if index > 0 {
            append_bounded(&mut text, "\n\n")?;
        }
        append_bounded(&mut text, &(index + 1).to_string())?;
        append_bounded(&mut text, ". ")?;
        append_bounded(
            &mut text,
            result
                .title
                .as_deref()
                .or(result.url.as_deref())
                .unwrap_or("Indexed result"),
        )?;
        if let Some(url) = result.url.as_deref() {
            append_bounded(&mut text, "\n   URL: ")?;
            append_bounded(&mut text, url)?;
        }
        if let Some(snippet) = result.snippet.as_deref() {
            append_bounded(&mut text, "\n   ")?;
            append_bounded(&mut text, snippet)?;
        }
    }
    ensure_output_limit(text)
}

fn replace_citation_markers(output: &str, results: &[IndexedResult]) -> String {
    let references = results
        .iter()
        .enumerate()
        .filter_map(|(index, result)| result.ref_id.as_deref().map(|ref_id| (ref_id, index + 1)))
        .collect::<HashMap<_, _>>();
    let text = private_citation_pattern()
        .replace_all(output, |captures: &Captures<'_>| {
            let inner = captures
                .get(1)
                .map(|value| value.as_str().trim())
                .unwrap_or_default();
            if let Some((_, label)) = inner.split_once('†') {
                let label = label.trim();
                return (!label.is_empty())
                    .then(|| format!("[{label}]"))
                    .unwrap_or_default();
            }
            references
                .get(inner)
                .map(|number| format!("[{number}]"))
                .unwrap_or_else(|| format!("[{inner}]"))
        })
        .into_owned();
    let mut text = raw_turn_citation_pattern()
        .replace_all(&text, |captures: &Captures<'_>| {
            captures
                .get(1)
                .map(|value| {
                    value
                        .as_str()
                        .split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(|ref_id| {
                            references
                                .get(ref_id)
                                .map(|number| format!("[{number}]"))
                                .unwrap_or_else(|| format!("[{ref_id}]"))
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        })
        .into_owned();
    for (index, result) in results.iter().enumerate() {
        let Some(ref_id) = result.ref_id.as_deref() else {
            continue;
        };
        let label = format!("[{}]", index + 1);
        text = text.replace(&format!("[{ref_id}]"), &label);
    }
    text
}

fn private_citation_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?is)\u{e200}cite\u{e202}([^\u{e000}-\u{e2ff}]+)\u{e201}")
            .expect("valid Codex private citation pattern")
    })
}

fn raw_turn_citation_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\[(turn\d+[a-z0-9_,\s]*)\]").expect("valid Codex raw citation pattern")
    })
}

fn indexed_sources(results: &[IndexedResult]) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    for result in results {
        let Some(url) = result
            .url
            .as_deref()
            .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
        else {
            continue;
        };
        if !seen.insert(url.to_string()) {
            continue;
        }
        let title = result
            .title
            .as_deref()
            .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        sources.push((title, url.to_string()));
    }
    sources
}

fn append_bounded(target: &mut String, value: &str) -> Result<(), CodexSearchError> {
    let next = target
        .len()
        .checked_add(value.len())
        .filter(|next| *next <= MAX_STREAM_ACCUMULATED_BYTES)
        .ok_or_else(output_limit_error)?;
    target.reserve(next - target.len());
    target.push_str(value);
    Ok(())
}

fn ensure_output_limit(text: String) -> Result<String, CodexSearchError> {
    if text.len() > MAX_STREAM_ACCUMULATED_BYTES {
        Err(output_limit_error())
    } else {
        Ok(text)
    }
}

fn output_limit_error() -> CodexSearchError {
    CodexSearchError {
        code: "codex_search_output_limit".to_string(),
        message: "Codex standalone search output exceeds the 16 MiB limit".to_string(),
    }
}

fn upstream_error_message(status: StatusCode, body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| format!("Codex standalone search returned HTTP {}", status.as_u16()))
}

fn search_error_code(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "codex_search_auth_failed",
        StatusCode::TOO_MANY_REQUESTS => "codex_search_rate_limited",
        StatusCode::BAD_REQUEST
        | StatusCode::PAYLOAD_TOO_LARGE
        | StatusCode::UNPROCESSABLE_ENTITY => "codex_search_invalid_request",
        _ => "codex_search_upstream_failed",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{normalize_search_response, MAX_SEARCH_RESULTS};

    #[test]
    fn indexed_output_preserves_order_unicode_and_citations() {
        let results = (0..40)
            .map(|index| {
                json!({
                    "ref_id": format!("turn0search{index}"),
                    "title": format!("Source {index} 雪"),
                    "url": format!("https://sources.example/{index}"),
                    "snippet": format!("Snippet {index} café")
                })
            })
            .collect::<Vec<_>>();
        let body = json!({
            "output": "First \u{e200}cite\u{e202}turn0search0, turn0search1\u{e201}; last [turn0search39] 😀; label \u{e200}cite\u{e202}40†Official docs\u{e201}",
            "results": results
        });
        let output = normalize_search_response(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert!(output
            .text
            .contains("First [1] [2]; last [40] 😀; label [Official docs]"));
        assert!(!output
            .text
            .chars()
            .any(|value| ('\u{e000}'..='\u{e2ff}').contains(&value)));
        assert!(output
            .text
            .contains("1. Source 0 雪: https://sources.example/0"));
        assert!(output
            .text
            .contains("40. Source 39 雪: https://sources.example/39"));
        assert!(
            output.text.find("sources.example/0").unwrap()
                < output.text.find("sources.example/39").unwrap()
        );
    }

    #[test]
    fn structured_results_are_used_when_output_is_missing() {
        let body = json!({
            "results": [{
                "title": "Rust",
                "url": "https://www.rust-lang.org/",
                "snippet": "A language empowering everyone."
            }]
        });
        let output = normalize_search_response(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert_eq!(
            output.text,
            "1. Rust\n   URL: https://www.rust-lang.org/\n   A language empowering everyone."
        );
    }

    #[test]
    fn invalid_empty_and_oversized_result_sets_are_errors() {
        assert!(normalize_search_response(b"not-json").is_err());
        assert!(normalize_search_response(br#"{"results":[]}"#).is_err());
        let body = json!({"results": vec![json!({"title":"x"}); MAX_SEARCH_RESULTS + 1]});
        let error = normalize_search_response(&serde_json::to_vec(&body).unwrap()).unwrap_err();
        assert_eq!(error.code, "codex_search_result_limit");
    }
}
