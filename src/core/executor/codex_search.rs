use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use serde_json::json;

use crate::core::proxy::ProxyTarget;
use crate::core::usage::quota_fetcher::codex_account_id;
use crate::types::{ProviderConnection, ProviderNode};

use super::ClientPool;

pub const CODEX_STANDALONE_SEARCH_URL: &str = "https://chatgpt.com/backend-api/codex/alpha/search";
const CODEX_SEARCH_MODEL: &str = "gpt-4o";
const CODEX_SEARCH_USER_AGENT: &str = "codex-cli/0.147.0-alpha.6.5";

#[derive(Debug)]
pub enum CodexSearchExecutorError {
    MissingCredentials,
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    Request(reqwest::Error),
}

impl From<reqwest::header::InvalidHeaderValue> for CodexSearchExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<reqwest::Error> for CodexSearchExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

pub struct CodexSearchExecutionRequest<'a> {
    pub request_id: &'a str,
    pub query: &'a str,
    pub response_length: &'a str,
    pub credentials: &'a ProviderConnection,
    pub proxy: Option<&'a ProxyTarget>,
}

#[derive(Clone)]
pub struct CodexSearchExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

impl CodexSearchExecutor {
    pub fn new(pool: Arc<ClientPool>, provider_node: Option<ProviderNode>) -> Self {
        Self {
            pool,
            provider_node,
        }
    }

    pub async fn execute(
        &self,
        request: CodexSearchExecutionRequest<'_>,
    ) -> Result<reqwest::Response, CodexSearchExecutorError> {
        let token = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or(CodexSearchExecutorError::MissingCredentials)?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(CODEX_SEARCH_USER_AGENT),
        );
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
        if let Some(account_id) = codex_account_id(&request.credentials.provider_specific_data) {
            headers.insert("chatgpt-account-id", HeaderValue::from_str(&account_id)?);
        }

        let body = json!({
            "id": request.request_id,
            "model": CODEX_SEARCH_MODEL,
            "commands": {
                "search_query": [{"q": request.query}],
                "response_length": request.response_length
            }
        });
        let client = self.pool.get("openai", request.proxy)?;
        Ok(client
            .post(self.search_url())
            .headers(headers)
            .json(&body)
            .send()
            .await?)
    }

    fn search_url(&self) -> String {
        self.provider_node
            .as_ref()
            .and_then(|node| node.base_url.as_deref())
            .and_then(standalone_url_from_codex_base)
            .unwrap_or_else(|| CODEX_STANDALONE_SEARCH_URL.to_string())
    }
}

fn standalone_url_from_codex_base(base_url: &str) -> Option<String> {
    let base_url = base_url.trim().trim_end_matches('/');
    let (origin, _) = base_url.split_once("/backend-api/codex/")?;
    if !(origin.starts_with("http://") || origin.starts_with("https://")) {
        return None;
    }
    Some(format!("{origin}/backend-api/codex/alpha/search"))
}

#[cfg(test)]
mod tests {
    use super::standalone_url_from_codex_base;

    #[test]
    fn derives_standalone_url_from_codex_endpoint() {
        assert_eq!(
            standalone_url_from_codex_base("http://127.0.0.1:1234/backend-api/codex/responses")
                .as_deref(),
            Some("http://127.0.0.1:1234/backend-api/codex/alpha/search")
        );
        assert!(standalone_url_from_codex_base("https://example.com/v1/responses").is_none());
    }
}
