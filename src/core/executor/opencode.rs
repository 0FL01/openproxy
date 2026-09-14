use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use uuid::Uuid;

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::core::translator::registry::Format;
use crate::core::utils::session_manager::resolve_session_identity;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1";
const GO_BASE_URL: &str = "https://opencode.ai/zen/go/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenCodeTier {
    Zen,
    Go,
}

impl OpenCodeTier {
    pub fn from_provider(provider: &str) -> Option<Self> {
        match provider {
            "opencode" | "opencode-zen" => Some(Self::Zen),
            "opencode-go" => Some(Self::Go),
            _ => None,
        }
    }

    pub(crate) fn base_url(self) -> &'static str {
        match self {
            Self::Zen => ZEN_BASE_URL,
            Self::Go => GO_BASE_URL,
        }
    }

    fn pool_key(self) -> &'static str {
        match self {
            Self::Zen => "opencode-zen",
            Self::Go => "opencode-go",
        }
    }
}

#[derive(Clone)]
pub struct OpenCodeExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum OpenCodeExecutorError {
    RequestFailed(String),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
}

impl From<reqwest::Error> for OpenCodeExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for OpenCodeExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<hyper_util::client::legacy::Error> for OpenCodeExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<std::io::Error> for OpenCodeExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<serde_json::Error> for OpenCodeExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

pub struct OpenCodeExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
    pub raw_headers: BTreeMap<String, String>,
    pub tier: OpenCodeTier,
    pub format: Format,
    pub family: Option<String>,
}

pub struct OpenCodeExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl OpenCodeExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, OpenCodeExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn build_url(
        tier: OpenCodeTier,
        format: Format,
        model: &str,
        stream: bool,
    ) -> Result<String, OpenCodeExecutorError> {
        let base = tier.base_url();
        match format {
            Format::OpenAi => Ok(format!("{base}/chat/completions")),
            Format::OpenAiResponses => Ok(format!("{base}/responses")),
            Format::Claude => Ok(format!("{base}/messages")),
            Format::Gemini => {
                let action = if stream {
                    "streamGenerateContent?alt=sse"
                } else {
                    "generateContent"
                };
                Ok(format!("{base}/models/{model}:{action}"))
            }
            other => Err(OpenCodeExecutorError::RequestFailed(format!(
                "Unsupported OpenCode format: {}",
                other.as_str()
            ))),
        }
    }

    fn build_headers(
        request: &OpenCodeExecutionRequest,
    ) -> Result<HeaderMap, OpenCodeExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let key = if request.credentials.auth_type == "none" {
            None
        } else {
            request
                .credentials
                .api_key
                .as_deref()
                .or(request.credentials.access_token.as_deref())
        };
        if request.tier == OpenCodeTier::Go && key.is_none() {
            return Err(OpenCodeExecutorError::RequestFailed(
                "OpenCode Go requires an API key".to_string(),
            ));
        }
        if let Some(key) = key {
            let value = HeaderValue::from_str(key)?;
            if request.format == Format::Claude
                || (request.tier == OpenCodeTier::Zen && request.format == Format::OpenAiResponses)
            {
                headers.insert("x-api-key", value);
            } else {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {key}"))?,
                );
            }
        }
        if request.format == Format::Claude {
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        }

        let raw: HashMap<String, String> = request
            .raw_headers
            .iter()
            .map(|(key, value)| (key.to_ascii_lowercase(), value.clone()))
            .collect();
        let resolved_session = resolve_session_identity(
            Some(&raw),
            Some(&request.body),
            Some(&request.credentials.id),
            "opencode",
        )
        .session_id;
        let supplied_session = raw
            .get("x-opencode-session")
            .cloned()
            .unwrap_or(resolved_session);
        let session = if request
            .family
            .as_deref()
            .is_some_and(|family| family.starts_with("muse"))
            && Uuid::parse_str(&supplied_session).is_err()
        {
            Uuid::new_v5(&Uuid::NAMESPACE_URL, supplied_session.as_bytes()).to_string()
        } else {
            supplied_session
        };

        let downstream_ua = raw.get("user-agent").map(String::as_str).unwrap_or("");
        let ua = if downstream_ua.to_ascii_lowercase().starts_with("opencode") {
            downstream_ua
        } else {
            "opencode"
        };
        insert_header(&mut headers, "user-agent", ua)?;
        insert_header(
            &mut headers,
            "x-opencode-client",
            raw.get("x-opencode-client")
                .map(String::as_str)
                .unwrap_or("desktop"),
        )?;
        insert_header(
            &mut headers,
            "x-opencode-project",
            raw.get("x-opencode-project")
                .map(String::as_str)
                .unwrap_or("global"),
        )?;
        insert_header(&mut headers, "x-opencode-session", &session)?;
        insert_header(
            &mut headers,
            "x-opencode-request",
            raw.get("x-opencode-request")
                .map(String::as_str)
                .unwrap_or_else(|| ""),
        )?;
        if headers
            .get("x-opencode-request")
            .is_some_and(|value| value.is_empty())
        {
            headers.insert(
                "x-opencode-request",
                HeaderValue::from_str(&Uuid::new_v4().to_string())?,
            );
        }
        for name in ["x-session-id", "x-session-affinity", "x-title"] {
            if let Some(value) = raw.get(name) {
                insert_header(&mut headers, name, value)?;
            }
        }

        if request.stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        }
        Ok(headers)
    }

    fn normalize_body(request: &mut OpenCodeExecutionRequest) {
        if request.format == Format::OpenAi {
            normalize_developer_role(&mut request.body);
        }
        let Some(body) = request.body.as_object_mut() else {
            return;
        };
        body.remove("client_metadata");
        body.remove("client_meta_data");

        if request.format == Format::OpenAiResponses {
            let max = body
                .remove("max_tokens")
                .or_else(|| body.remove("max_completion_tokens"));
            if let Some(max) = max {
                body.insert("max_output_tokens".to_string(), max);
            }
            if request
                .family
                .as_deref()
                .is_some_and(|family| family.starts_with("muse"))
            {
                if let Some(value) = body.get_mut("max_output_tokens") {
                    if value.as_u64().is_some_and(|limit| limit < 512) {
                        *value = Value::from(512);
                    }
                }
            }
        }

        if request.tier == OpenCodeTier::Go && body.get("reasoning").is_some_and(Value::is_boolean)
        {
            body.remove("reasoning");
        }
    }

    pub async fn execute_request(
        &self,
        mut request: OpenCodeExecutionRequest,
    ) -> Result<OpenCodeExecutorResponse, OpenCodeExecutorError> {
        let _ = &self.provider_node;
        Self::normalize_body(&mut request);
        let url = Self::build_url(request.tier, request.format, &request.model, request.stream)?;
        let headers = Self::build_headers(&request)?;
        let client = self
            .pool
            .get(request.tier.pool_key(), request.proxy.as_ref())?;
        let response = client
            .post(&url)
            .headers(headers.clone())
            .json(&request.body)
            .send()
            .await?;

        Ok(OpenCodeExecutorResponse {
            response: UpstreamResponse::Reqwest(response),
            url,
            headers,
            transformed_body: request.body,
            transport: TransportKind::Reqwest,
        })
    }
}

fn insert_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), OpenCodeExecutorError> {
    headers.insert(HeaderName::from_static(name), HeaderValue::from_str(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(tier: OpenCodeTier, format: Format) -> OpenCodeExecutionRequest {
        OpenCodeExecutionRequest {
            model: "model".to_string(),
            body: serde_json::json!({"model":"model","stream":true}),
            stream: true,
            credentials: ProviderConnection {
                id: "connection".to_string(),
                api_key: Some("key".to_string()),
                ..Default::default()
            },
            proxy: None,
            raw_headers: BTreeMap::new(),
            tier,
            format,
            family: None,
        }
    }

    #[test]
    fn routes_by_tier_and_format() {
        assert_eq!(
            crate::core::executor::provider_config_base_url("opencode-go").as_deref(),
            Some(GO_BASE_URL)
        );
        assert_eq!(
            crate::core::executor::provider_config_base_url("opencode-zen").as_deref(),
            Some(ZEN_BASE_URL)
        );
        assert_eq!(
            OpenCodeExecutor::build_url(OpenCodeTier::Go, Format::OpenAiResponses, "muse", true)
                .unwrap(),
            "https://opencode.ai/zen/go/v1/responses"
        );
        assert_eq!(
            OpenCodeExecutor::build_url(OpenCodeTier::Go, Format::OpenAi, "deepseek", true)
                .unwrap(),
            "https://opencode.ai/zen/go/v1/chat/completions"
        );
        assert_eq!(
            OpenCodeExecutor::build_url(OpenCodeTier::Zen, Format::Claude, "claude", true).unwrap(),
            "https://opencode.ai/zen/v1/messages"
        );
        assert_eq!(
            OpenCodeExecutor::build_url(OpenCodeTier::Zen, Format::Gemini, "gemini", true).unwrap(),
            "https://opencode.ai/zen/v1/models/gemini:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn applies_auth_matrix_and_identity_defaults() {
        let zen = request(OpenCodeTier::Zen, Format::OpenAiResponses);
        let headers = OpenCodeExecutor::build_headers(&zen).unwrap();
        assert_eq!(headers.get("x-api-key").unwrap(), "key");
        assert!(!headers.contains_key(AUTHORIZATION));
        assert!(
            Uuid::parse_str(headers.get("x-opencode-request").unwrap().to_str().unwrap()).is_ok()
        );

        let go = request(OpenCodeTier::Go, Format::OpenAiResponses);
        let headers = OpenCodeExecutor::build_headers(&go).unwrap();
        assert_eq!(headers.get(AUTHORIZATION).unwrap(), "Bearer key");
        assert!(!headers.contains_key("x-api-key"));

        let claude = request(OpenCodeTier::Go, Format::Claude);
        let headers = OpenCodeExecutor::build_headers(&claude).unwrap();
        assert_eq!(headers.get("x-api-key").unwrap(), "key");
        assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
    }

    #[test]
    fn anonymous_zen_sends_no_synthetic_credential() {
        let mut zen = request(OpenCodeTier::Zen, Format::OpenAiResponses);
        zen.credentials.auth_type = "none".to_string();
        zen.credentials.api_key = None;
        zen.credentials.access_token = Some("public".to_string());
        let headers = OpenCodeExecutor::build_headers(&zen).unwrap();
        assert!(!headers.contains_key(AUTHORIZATION));
        assert!(!headers.contains_key("x-api-key"));
    }

    #[test]
    fn muse_floor_preserves_responses_tools_and_reasoning() {
        let mut req = request(OpenCodeTier::Go, Format::OpenAiResponses);
        req.family = Some("muse".to_string());
        req.body = serde_json::json!({
            "max_output_tokens": 16,
            "reasoning": {"effort":"high"},
            "tools": [{"type":"function","name":"tool","parameters":{"type":"object"}}],
            "client_metadata": {"x": true}
        });
        OpenCodeExecutor::normalize_body(&mut req);
        assert_eq!(req.body["max_output_tokens"], 512);
        assert_eq!(req.body["reasoning"]["effort"], "high");
        assert_eq!(req.body["tools"][0]["name"], "tool");
        assert!(req.body.get("client_metadata").is_none());

        let mut without_limit = request(OpenCodeTier::Go, Format::OpenAiResponses);
        without_limit.family = Some("muse".to_string());
        OpenCodeExecutor::normalize_body(&mut without_limit);
        assert!(without_limit.body.get("max_output_tokens").is_none());
    }

    #[test]
    fn muse_session_is_stable_uuid() {
        let mut request = request(OpenCodeTier::Go, Format::OpenAiResponses);
        request.family = Some("muse".to_string());
        request.raw_headers.insert(
            "x-opencode-session".to_string(),
            "conversation-1".to_string(),
        );
        let first = OpenCodeExecutor::build_headers(&request).unwrap();
        let second = OpenCodeExecutor::build_headers(&request).unwrap();
        let first = first.get("x-opencode-session").unwrap().to_str().unwrap();
        let second = second.get("x-opencode-session").unwrap().to_str().unwrap();
        assert_eq!(first, second);
        assert!(Uuid::parse_str(first).is_ok());
    }
}
