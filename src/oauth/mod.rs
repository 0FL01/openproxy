//! OAuth 2.0 flows implementation
//!
//! Supports:
//! - PKCE Authorization Code Flow (claude, codex, gitlab)
//! - Device Code Flow (github, kimi-coding, kilocode, codebuddy)

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use url::form_urlencoded;

pub const TOKEN_EXPIRY_BUFFER_MS: u64 = 5 * 60 * 1000;
pub mod antigravity_onboarding;
pub mod background_refresh;
pub mod kilocode;
pub mod pending;
pub mod providers;
pub mod secret;
#[cfg(test)]
pub mod tests;
pub mod zed_auth;

pub enum OAuthFlowKind {
    AuthorizationCodePkce,
    DeviceCode,
    ImportToken,
}

pub use providers::OAuthProviderConfig;

pub mod pkce {
    use super::*;

    pub fn generate_code_verifier() -> String {
        generate_code_verifier_with_len(32)
    }

    pub fn generate_code_verifier_with_len(bytes: usize) -> String {
        let mut random_bytes = vec![0u8; bytes];
        rand::thread_rng().fill_bytes(&mut random_bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes)
    }

    pub fn generate_code_challenge(verifier: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let hash = hasher.finalize();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
    }

    pub fn generate_verifier_and_challenge() -> (String, String) {
        let verifier = generate_code_verifier();
        let challenge = generate_code_challenge(&verifier);
        (verifier, challenge)
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub interval: u64,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct OAuthError {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
}

pub struct RefreshRequest {
    pub refresh_token: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
}

pub mod device_code {
    use super::*;

    pub async fn start_device_flow(
        _provider_config: &OAuthProviderConfig,
        client_id: &str,
    ) -> Result<DeviceCodeResponse, OAuthError> {
        let client = reqwest::Client::new();
        let params = [
            ("client_id", client_id),
            ("scope", &_provider_config.scopes.join(" ")),
        ];
        let response = client
            .post(_provider_config.authorize_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| OAuthError {
                error: "request_failed".to_string(),
                error_description: Some(e.to_string()),
            })?;

        if !response.status().is_success() {
            let error: OAuthError = response.json().await.unwrap_or(OAuthError {
                error: "unknown_error".to_string(),
                error_description: None,
            });
            return Err(error);
        }

        response.json().await.map_err(|e| OAuthError {
            error: "parse_error".to_string(),
            error_description: Some(e.to_string()),
        })
    }

    pub async fn poll_for_token(
        provider_config: &OAuthProviderConfig,
        device_code: &str,
        _user_code: &str,
        interval_secs: u64,
    ) -> Result<TokenResponse, OAuthError> {
        let client = reqwest::Client::new();
        let mut current_interval = interval_secs;

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(current_interval)).await;

            let params = [
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                (
                    "client_id",
                    provider_config
                        .get_param("client_id")
                        .unwrap_or("openproxy"),
                ),
                ("device_code", device_code),
            ];
            let response = client
                .post(provider_config.token_url)
                .form(&params)
                .send()
                .await
                .map_err(|e| OAuthError {
                    error: "request_failed".to_string(),
                    error_description: Some(e.to_string()),
                })?;

            let body: serde_json::Value = response.json().await.unwrap_or_default();
            let error = body.get("error").and_then(|e| e.as_str());

            match error {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    current_interval = (current_interval * 2).min(60);
                    continue;
                }
                Some("access_denied") => {
                    return Err(OAuthError {
                        error: "access_denied".to_string(),
                        error_description: Some(
                            "User denied the authorization request".to_string(),
                        ),
                    });
                }
                Some("expired_token") => {
                    return Err(OAuthError {
                        error: "expired_token".to_string(),
                        error_description: Some("The device code has expired".to_string()),
                    });
                }
                _ => {
                    if body.get("access_token").is_some() {
                        let token_response: TokenResponse =
                            serde_json::from_value(body).map_err(|e| OAuthError {
                                error: "parse_error".to_string(),
                                error_description: Some(e.to_string()),
                            })?;
                        return Ok(token_response);
                    }
                    continue;
                }
            }
        }
    }

    pub async fn exchange_code_for_token(
        provider_config: &OAuthProviderConfig,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
        client_id: &str,
    ) -> Result<TokenResponse, OAuthError> {
        let client = reqwest::Client::new();
        let params = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", code_verifier),
        ];

        let response = client
            .post(provider_config.token_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| OAuthError {
                error: "request_failed".to_string(),
                error_description: Some(e.to_string()),
            })?;

        if !response.status().is_success() {
            let error: OAuthError = response.json().await.unwrap_or(OAuthError {
                error: "token_exchange_failed".to_string(),
                error_description: None,
            });
            return Err(error);
        }

        response.json().await.map_err(|e| OAuthError {
            error: "parse_error".to_string(),
            error_description: Some(e.to_string()),
        })
    }

    /// GitHub Copilot special: exchange OAuth token for Copilot token
    pub async fn exchange_github_copilot_token(
        oauth_token: &str,
    ) -> Result<TokenResponse, OAuthError> {
        let client = reqwest::Client::new();
        let response = client
            .post("https://github.com/copilot_internal/v1/token")
            .header("Authorization", format!("Bearer {}", oauth_token))
            .send()
            .await
            .map_err(|e| OAuthError {
                error: "request_failed".to_string(),
                error_description: Some(e.to_string()),
            })?;

        if !response.status().is_success() {
            let error: OAuthError = response.json().await.unwrap_or(OAuthError {
                error: "copilot_token_exchange_failed".to_string(),
                error_description: None,
            });
            return Err(error);
        }

        response.json().await.map_err(|e| OAuthError {
            error: "parse_error".to_string(),
            error_description: Some(e.to_string()),
        })
    }

    pub async fn kilocode_start_device_flow(
        provider_config: &OAuthProviderConfig,
    ) -> Result<DeviceCodeResponse, OAuthError> {
        super::kilocode::kilocode_start_device_flow(provider_config).await
    }

    pub async fn kilocode_poll_for_token(
        provider_config: &OAuthProviderConfig,
        device_code: &str,
    ) -> Result<TokenResponse, OAuthError> {
        super::kilocode::kilocode_poll_for_token(provider_config, device_code).await
    }
}

pub mod token_refresh;

pub fn needs_refresh(expires_at: &Option<String>) -> bool {
    token_refresh::needs_refresh(expires_at)
}

pub fn expires_at_from_seconds(expires_in: i64) -> String {
    let expires = chrono::Utc::now() + chrono::Duration::seconds(expires_in);
    expires.to_rfc3339()
}

// GitLab PAT (Personal Access Token) support
pub mod gitlab_pat {
    use crate::oauth::TokenResponse;

    pub fn create_token_response(pat: &str) -> TokenResponse {
        TokenResponse {
            access_token: pat.to_string(),
            refresh_token: None,
            expires_in: None,
            id_token: None,
            token_type: Some("Bearer".to_string()),
            scope: Some("api read_user".to_string()),
        }
    }

    pub fn is_valid_pat(pat: &str) -> bool {
        !pat.is_empty() && pat.len() >= 20
    }
}
