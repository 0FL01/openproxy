use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};

use crate::core::config::app_constants::{
    CODEX_CLIENT_VERSION, CODEX_ORIGINATOR, CODEX_USER_AGENT,
};
use crate::core::usage::quota_fetcher::codex_account_id;
use crate::types::ProviderConnection;

/// Headers common to Codex inference, model discovery, and standalone search.
/// Each caller adds its own response format and session headers.
pub(crate) fn build_codex_headers(
    token: &str,
    credentials: &ProviderConnection,
) -> Result<HeaderMap, reqwest::header::InvalidHeaderValue> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))?,
    );
    headers.insert("originator", HeaderValue::from_static(CODEX_ORIGINATOR));
    headers.insert("Version", HeaderValue::from_static(CODEX_CLIENT_VERSION));
    headers.insert(USER_AGENT, HeaderValue::from_static(CODEX_USER_AGENT));
    if let Some(account_id) = codex_account_id(&credentials.provider_specific_data) {
        headers.insert("chatgpt-account-id", HeaderValue::from_str(&account_id)?);
    }
    Ok(headers)
}
