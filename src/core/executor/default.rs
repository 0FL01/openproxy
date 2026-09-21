use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming as HyperIncoming;
use hyper::http;
use hyper::http::uri::InvalidUri;
use hyper::{Request as HyperRequest, Response as HyperResponse, Uri};
use once_cell::sync::Lazy;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use tokio::sync::{self, Semaphore};

use crate::core::proxy::ProxyTarget;
use crate::core::translator::helpers::openai_helper::normalize_developer_role;
use crate::core::utils::reasoning_content_injector::inject_reasoning_content;
use crate::types::{ProviderConnection, ProviderNode};

use super::strip_unsupported::strip_unsupported_params;
use super::ClientPool;

static PROVIDER_CONFIGS: Lazy<BTreeMap<&'static str, ProviderConfig>> = Lazy::new(|| {
    BTreeMap::from([
        (
            "openai",
            ProviderConfig::openai("https://api.openai.com/v1/chat/completions"),
        ),
        (
            "openrouter",
            ProviderConfig::openai("https://openrouter.ai/api/v1/chat/completions")
                .with_header("HTTP-Referer", "https://endpoint-proxy.local")
                .with_header("X-Title", "Endpoint Proxy"),
        ),
        (
            "anthropic",
            ProviderConfig::anthropic("https://api.anthropic.com/v1/messages"),
        ),
        (
            "claude",
            ProviderConfig::anthropic("https://api.anthropic.com/v1/messages"),
        ),
        (
            "gemini",
            ProviderConfig::gemini("https://generativelanguage.googleapis.com/v1beta/models"),
        ),
        (
            "glm",
            ProviderConfig::openai("https://api.z.ai/api/coding/paas/v4/chat/completions"),
        ),
        (
            "kimi",
            ProviderConfig::claude_compatible("https://api.kimi.com/coding/v1/messages"),
        ),
        (
            "minimax",
            ProviderConfig::claude_compatible("https://api.minimax.io/anthropic/v1/messages"),
        ),
        (
            "deepseek",
            ProviderConfig::openai("https://api.deepseek.com/chat/completions"),
        ),
        (
            "xai",
            ProviderConfig::openai("https://api.x.ai/v1/chat/completions"),
        ),
        (
            "mistral",
            ProviderConfig::openai("https://api.mistral.ai/v1/chat/completions"),
        ),
        (
            "together",
            ProviderConfig::openai("https://api.together.xyz/v1/chat/completions"),
        ),
        (
            "fireworks",
            ProviderConfig::openai("https://api.fireworks.ai/inference/v1/chat/completions"),
        ),
        (
            "cerebras",
            ProviderConfig::openai("https://api.cerebras.ai/v1/chat/completions"),
        ),
        (
            "cohere",
            ProviderConfig::openai("https://api.cohere.ai/v1/chat/completions"),
        ),
        (
            "commandcode",
            ProviderConfig::openai(
                "https://api.commandcode.ai/provider/v1/chat/completions",
            ),
        ),
        (
            "a6api",
            ProviderConfig::openai("https://api.a6api.com/v1/chat/completions"),
        ),
        (
            "hyperbolic",
            ProviderConfig::openai("https://api.hyperbolic.xyz/v1/chat/completions"),
        ),
        (
            "perplexity",
            ProviderConfig::openai("https://api.perplexity.ai/chat/completions"),
        ),
        (
            "gitlab",
            ProviderConfig::openai("https://gitlab.com/api/v4/chat/completions"),
        ),
        (
            "codebuddy",
            ProviderConfig::openai("https://copilot.tencent.com/v1/chat/completions"),
        ),
        (
            "kilocode",
            ProviderConfig::openai("https://api.kilo.ai/api/openrouter/chat/completions"),
        ),
        (
            "cline",
            ProviderConfig::openai("https://api.cline.bot/api/v1/chat/completions")
                .with_header("HTTP-Referer", "https://cline.bot")
                .with_header("X-Title", "Cline"),
        ),
        (
            "glm-cn",
            ProviderConfig::openai("https://open.bigmodel.cn/api/coding/paas/v4/chat/completions"),
        ),
        (
            "alicode",
            ProviderConfig::openai("https://coding.dashscope.aliyuncs.com/v1/chat/completions"),
        ),
        (
            "alicode-intl",
            ProviderConfig::openai(
                "https://coding-intl.dashscope.aliyuncs.com/v1/chat/completions",
            ),
        ),
        (
            "alims-intl",
            ProviderConfig::openai(
                "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions",
            ),
        ),
        (
            "clinepass",
            ProviderConfig::openai("https://api.cline.bot/api/v1/chat/completions")
                .with_header("HTTP-Referer", "https://cline.bot")
                .with_header("X-Title", "Cline"),
        ),
        (
            "codebuddy-intl",
            ProviderConfig::openai("https://www.codebuddy.ai/v2/chat/completions")
                .with_header("User-Agent", "IDE/2.108.1 CodeBuddy/2.108.1")
                .with_header("X-Product", "SaaS")
                .with_header("X-IDE-Type", "IDE")
                .with_header("X-IDE-Name", "IDE")
                .with_header("x-requested-with", "XMLHttpRequest")
                .with_header("x-codebuddy-request", "1"),
        ),
        (
            "featherless",
            ProviderConfig::openai("https://api.featherless.ai/v1/chat/completions"),
        ),
        (
            "kilo-gateway",
            ProviderConfig::openai("https://api.kilo.ai/api/gateway/chat/completions"),
        ),
        (
            "perplexity-agent",
            // OpenAI Responses API — do NOT normalize to /chat/completions.
            ProviderConfig::openai("https://api.perplexity.ai/v1/responses"),
        ),
        (
            "tokenrouter",
            ProviderConfig::openai("https://api.tokenrouter.com/v1/chat/completions"),
        ),
        (
            "venice",
            // /api/v1 (double path) — do NOT "fix" to /v1.
            ProviderConfig::openai("https://api.venice.ai/api/v1/chat/completions"),
        ),
        (
            "zed",
            // MINIMAL chat-path fix: zed's non-standard auth header
            // ("Authorization: <user_id> <access_token>", no Bearer) and NDJSON
            // wire protocol are a separate executor task (parity A3). This entry
            // clears UnsupportedProvider so a pre-obtained token can route.
            ProviderConfig::openai("https://cloud.zed.dev/completions"),
        ),
        (
            "nvidia",
            ProviderConfig::openai("https://integrate.api.nvidia.com/v1/chat/completions"),
        ),
        (
            "cloudflare-ai",
            ProviderConfig::openai(
                "https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/v1/chat/completions",
            ),
        ),
        (
            "azure",
            ProviderConfig::openai("https://{resource}.openai.azure.com/v1/chat/completions"),
        ),
        (
            "ollama-cloud",
            ProviderConfig::openai("https://ollama.com/v1/chat/completions"),
        ),
        (
            "vertex",
            ProviderConfig::gemini("https://generativelanguage.googleapis.com/v1beta/models"),
        ),
        (
            "vertex-partner",
            ProviderConfig::gemini("https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}"),
        ),
        (
            "antigravity",
            ProviderConfig::gemini("https://cloudcode-pa.googleapis.com/v1internal"),
        ),
        (
            "xiaomi-mimo",
            ProviderConfig::openai("https://api.xiaomimimo.com/v1/chat/completions"),
        ),
        (
            "lm-studio",
            ProviderConfig::openai("http://localhost:1234/v1/chat/completions"),
        ),
        (
            "vllm",
            ProviderConfig::openai("http://localhost:8000/v1/chat/completions"),
        ),
        (
            "inference-net",
            ProviderConfig::openai("https://api.inference.net/v1/chat/completions"),
        ),
        (
            "vercel-ai-gateway",
            ProviderConfig::openai("https://ai-gateway.vercel.sh/v1/chat/completions"),
        ),
        (
            "xiaomi-tokenplan",
            ProviderConfig::openai("https://token-plan-sgp.xiaomimimo.com/v1/chat/completions"),
        ),
        (
            "github-models",
            ProviderConfig::openai("https://models.github.ai/inference/chat/completions"),
        ),
        (
            "hackclub",
            ProviderConfig::openai("https://ai.hackclub.com/proxy/v1/chat/completions"),
        ),
        (
            "ollama",
            ProviderConfig::openai("https://ollama.com/v1/chat/completions"),
        ),
        (
            "modal",
            ProviderConfig::openai("https://api.modal.com/v1/chat/completions"),
        ),
        (
            "llm7",
            ProviderConfig::openai("https://api.llm7.io/v1/chat/completions"),
        ),
        (
            "longcat",
            ProviderConfig::openai("https://api.longcat.chat/openai/v1/chat/completions"),
        ),
        (
            "scaleway",
            ProviderConfig::openai("https://api.scaleway.ai/v1/chat/completions"),
        ),
        (
            "sambanova",
            ProviderConfig::openai("https://api.sambanova.ai/v1/chat/completions"),
        ),
        (
            "nous-research",
            ProviderConfig::openai("https://inference-api.nousresearch.com/v1/chat/completions"),
        ),
        (
            "glhf",
            ProviderConfig::openai("https://glhf.chat/api/openai/v1/chat/completions"),
        ),
        (
            "codebuddy-cn",
            ProviderConfig::openai("https://api.codebuddy.cn/v1/chat/completions"),
        ),
        (
            "mimo-free",
            ProviderConfig::openai("https://mimo.kiro.dev/v1/chat/completions"),
        ),
        (
            "xiaomi-tokenplan",
            ProviderConfig::openai("https://tokenplan.xiaomi.com/v1/chat/completions"),
        ),
    ])
});

// Semaphore for tokenrouter free models to limit concurrent requests to 1
static TOKENROUTER_SEMAPHORE: Lazy<Semaphore> = Lazy::new(|| Semaphore::new(1));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub base_url: String,
    pub format: String,
    pub default_headers: Vec<(String, String)>,
    pub fallback_urls: Vec<String>,
}

impl ProviderConfig {
    fn openai(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            format: "openai".into(),
            default_headers: Vec::new(),
            fallback_urls: Vec::new(),
        }
    }

    fn gemini(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            format: "gemini".into(),
            default_headers: Vec::new(),
            fallback_urls: Vec::new(),
        }
    }

    fn anthropic(base_url: &str) -> Self {
        Self::openai(base_url)
            .with_header("anthropic-version", "2023-06-01")
            .with_header(
                "anthropic-beta",
                "claude-code-20250219,interleaved-thinking-2025-05-14",
            )
    }

    fn claude_compatible(base_url: &str) -> Self {
        Self::anthropic(base_url)
    }

    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.default_headers
            .push((name.to_string(), value.to_string()));
        self
    }

    #[allow(dead_code)]
    fn with_fallback(mut self, url: &str) -> Self {
        self.fallback_urls.push(url.to_string());
        self
    }
}

/// Anthropic beta flags, ported from `selectAnthropicBeta` in
/// `open-sse/providers/shared.js:51-69`. Heavy-agent flags are gated to
/// opus/sonnet — cheaper models don't need them.
const ANTHROPIC_BETA_BASE: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,structured-outputs-2025-12-15,fast-mode-2026-02-01,redact-thinking-2026-02-12,token-efficient-tools-2026-03-28";
const ANTHROPIC_BETA_HEAVY_AGENT: &str = "advanced-tool-use-2025-11-20,effort-2025-11-24";

pub fn select_anthropic_beta(model: &str) -> String {
    if model.starts_with("claude-opus") || model.starts_with("claude-sonnet") {
        format!("{ANTHROPIC_BETA_BASE},{ANTHROPIC_BETA_HEAVY_AGENT}")
    } else {
        ANTHROPIC_BETA_BASE.to_string()
    }
}

pub struct DefaultExecutor {
    provider: String,
    config: ProviderConfig,
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug, Clone)]
pub struct ExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
    /// Lowercase headers from this client request. Only an explicit, non-secret
    /// allowlist is eligible for forwarding by provider adapters.
    pub client_headers: BTreeMap<String, String>,
}

/// Claude client identity and protocol metadata that may be forwarded from the
/// current request. Authentication, cookies, forwarding headers, and arbitrary
/// extension headers are intentionally excluded.
const CLAUDE_REQUEST_HEADER_ALLOWLIST: &[&str] = &[
    "user-agent",
    "anthropic-beta",
    "anthropic-version",
    "anthropic-dangerous-direct-browser-access",
    "x-app",
    "x-stainless-helper-method",
    "x-stainless-retry-count",
    "x-stainless-runtime-version",
    "x-stainless-package-version",
    "x-stainless-runtime",
    "x-stainless-lang",
    "x-stainless-arch",
    "x-stainless-os",
    "x-stainless-timeout",
    "x-claude-code-session-id",
    "package-version",
    "runtime-version",
    "os",
    "arch",
];

pub struct ExecutionResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transport: TransportKind,
}

/// Maximum serialized request body retained by the DefaultExecutor.
///
/// Public generation routes already cap inbound JSON at 32 MiB. A transform
/// may duplicate a large JSON schema into an instruction, so the prepared
/// representation allows bounded expansion without restoring an unbounded
/// serializer buffer.
pub const MAX_PREPARED_UPSTREAM_BODY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedBodyKey {
    provider: String,
    provider_node: Option<ProviderNode>,
    model: String,
}

/// Request-scoped, fully transformed and bounded JSON serialization.
///
/// The bytes may be shared by byte-identical account attempts. URL, headers,
/// credentials, and proxy selection are deliberately rebuilt for every
/// attempt. This object is never stored across incoming requests.
#[derive(Debug)]
pub struct PreparedUpstreamBody {
    key: PreparedBodyKey,
    bytes: Bytes,
}

impl PreparedUpstreamBody {
    #[doc(hidden)]
    pub fn serialized_bytes(&self) -> &Bytes {
        &self.bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Reqwest,
    Hyper,
}

pub enum UpstreamResponse {
    Reqwest(reqwest::Response),
    Hyper(HyperResponse<HyperIncoming>),
}

impl UpstreamResponse {
    pub fn status(&self) -> http::StatusCode {
        match self {
            Self::Reqwest(response) => response.status(),
            Self::Hyper(response) => response.status(),
        }
    }

    pub fn headers(&self) -> &HeaderMap {
        match self {
            Self::Reqwest(response) => response.headers(),
            Self::Hyper(response) => response.headers(),
        }
    }
}

#[derive(Debug)]
pub enum ExecutorError {
    UnsupportedProvider(String),
    MissingCredentials(String),
    MissingProviderSpecificData(String, &'static str),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    InvalidUri(InvalidUri),
    InvalidRequest(http::Error),
    Serialize(serde_json::Error),
    PreparedBodyTooLarge { limit: usize },
    PreparedBodyMismatch,
    HyperClientInit(io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    SemaphoreAcquisitionFailed,
    CredentialRefreshFailed(String),
    MaxRetriesExhausted(String),
    UpstreamStatus(http::StatusCode, String),
}
impl ExecutorError {
    /// Map an executor failure into a provider attempt error preserving the raw
    /// upstream status when available (429 rate limits must surface with
    /// retry-after, not be masked as 500).
    pub fn into_provider_attempt_error(
        self,
    ) -> crate::core::account_fallback::ProviderAttemptError {
        use crate::core::account_fallback::ProviderAttemptError;
        match &self {
            Self::UpstreamStatus(status, message) => ProviderAttemptError {
                status: status.as_u16(),
                message: message.clone(),
                retry_after: None,
                upstream_body: None,
            },
            Self::MissingCredentials(p) => ProviderAttemptError {
                status: 400,
                message: format!("Missing credentials for provider: {p}"),
                retry_after: None,
                upstream_body: None,
            },
            Self::PreparedBodyTooLarge { limit } => ProviderAttemptError {
                status: http::StatusCode::PAYLOAD_TOO_LARGE.as_u16(),
                message: format!("Prepared upstream request exceeds the {limit}-byte limit"),
                retry_after: None,
                upstream_body: None,
            },
            other => ProviderAttemptError {
                status: 500,
                message: format!("Execution failed: {other:?}"),
                retry_after: None,
                upstream_body: None,
            },
        }
    }
}

impl From<reqwest::Error> for ExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for ExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<InvalidUri> for ExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<http::Error> for ExecutorError {
    fn from(error: http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for ExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<io::Error> for ExecutorError {
    fn from(error: io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for ExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

impl From<tokio::sync::AcquireError> for ExecutorError {
    fn from(_: tokio::sync::AcquireError) -> Self {
        Self::SemaphoreAcquisitionFailed
    }
}

/// Resolve a provider's upstream base URL from its executor configuration.
/// Returns `None` for unknown providers.
pub fn provider_config_base_url(provider: &str) -> Option<String> {
    if let Some(tier) = super::opencode::OpenCodeTier::from_provider(provider) {
        return Some(tier.base_url().to_string());
    }
    PROVIDER_CONFIGS
        .get(provider)
        .map(|config| config.base_url.clone())
}

impl DefaultExecutor {
    pub fn new(
        provider: impl Into<String>,
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, ExecutorError> {
        let provider = provider.into();
        let config = if let Some(node) = &provider_node {
            if node.r#type == "openai-compatible" || node.r#type == "anthropic-compatible" {
                ProviderConfig::openai("")
            } else {
                PROVIDER_CONFIGS
                    .get(provider.as_str())
                    .cloned()
                    .ok_or_else(|| ExecutorError::UnsupportedProvider(provider.clone()))?
            }
        } else {
            PROVIDER_CONFIGS
                .get(provider.as_str())
                .cloned()
                .ok_or_else(|| ExecutorError::UnsupportedProvider(provider.clone()))?
        };

        Ok(Self {
            provider,
            config,
            pool,
            provider_node,
        })
    }

    /// Full endpoint URL already (path present); optional query is ignored for matching.
    fn is_already_endpoint(url: &str) -> bool {
        let path = url.split('?').next().unwrap_or(url);
        path.contains("/chat/completions")
            || path.ends_with("/messages")
            || path.contains("/anthropic/v1/messages")
            || path.contains("/responses")
    }

    /// Providers that use Claude-compatible `?beta=true` (9r transport urlSuffix).
    fn provider_wants_claude_beta(provider: &str) -> bool {
        matches!(
            provider,
            "claude" | "anthropic" | "glm" | "kimi" | "kimi-coding" | "minimax"
        )
    }

    /// Ensure Claude multi-endpoint absolute URLs keep `?beta=true` when missing.
    fn ensure_claude_beta_suffix(url: &str, provider: &str) -> String {
        if !Self::provider_wants_claude_beta(provider) {
            return url.to_string();
        }
        let path = url.split('?').next().unwrap_or(url);
        let is_messages = path.ends_with("/messages") || path.contains("/anthropic/v1/messages");
        if !is_messages {
            return url.to_string();
        }
        if url.contains("beta=") {
            return url.to_string();
        }
        if url.contains('?') {
            format!("{url}&beta=true")
        } else {
            format!("{url}?beta=true")
        }
    }

    /// Xiaomi Token Plan: region host + dual OpenAI/Claude path (9router XiaomiTokenplanExecutor).
    fn xiaomi_tokenplan_url(credentials: &ProviderConnection) -> Result<String, ExecutorError> {
        let region =
            compatible_value(credentials.provider_specific_data.get("region")).unwrap_or("sgp");
        let base = match region {
            "cn" => "https://token-plan-cn.xiaomimimo.com/v1",
            "ams" => "https://token-plan-ams.xiaomimimo.com/v1",
            _ => "https://token-plan-sgp.xiaomimimo.com/v1",
        };
        let wants_claude = credentials
            .runtime_transport
            .as_ref()
            .and_then(|rt| rt.base_url.as_deref())
            .map(|u| u.contains("/anthropic/") || u.ends_with("/messages"))
            .unwrap_or(false);
        if wants_claude {
            let host = base.trim_end_matches('/').trim_end_matches("/v1");
            return Ok(format!("{host}/anthropic/v1/messages"));
        }
        Ok(format!("{base}/chat/completions"))
    }

    pub fn build_url(
        &self,
        model: &str,
        stream: bool,
        credentials: &ProviderConnection,
    ) -> Result<String, ExecutorError> {
        // Region-specific providers must win over resolve_transport's default-region URL.
        if self.provider == "xiaomi-tokenplan" || self.provider == "xmtp" {
            return Self::xiaomi_tokenplan_url(credentials);
        }

        // Check runtime_transport base_url override on the connection first.
        // 9router multi-endpoint transports store a full endpoint URL
        // (…/chat/completions or …/messages[?beta=true]). Use as-is when path is present;
        // otherwise append the provider-default path. Claude beta is baked into the
        // multi-endpoint table (or appended here when missing) so already_endpoint
        // never silently drops urlSuffix.
        if let Some(rt) = &credentials.runtime_transport {
            if let Some(rt_base_url) = &rt.base_url {
                let normalized = rt_base_url.trim_end_matches('/');
                let already_endpoint = Self::is_already_endpoint(normalized);
                if already_endpoint {
                    return Ok(Self::ensure_claude_beta_suffix(normalized, &self.provider));
                }
                if let Some(node) = &self.provider_node {
                    if node.r#type == "anthropic-compatible" {
                        return Ok(format!("{}/messages", normalized));
                    }
                }
                if matches!(
                    self.provider.as_str(),
                    "claude"
                        | "anthropic"
                        | "glm"
                        | "kimi"
                        | "kimi-coding"
                        | "minimax"
                        | "xiaomi-mimo"
                        | "mimo"
                ) {
                    let messages = format!("{}/messages", normalized);
                    return Ok(Self::ensure_claude_beta_suffix(&messages, &self.provider));
                }
                return Ok(format!("{}/chat/completions", normalized));
            }
        }

        if let Some(node) = &self.provider_node {
            if node.r#type == "openai-compatible" {
                let base_url = compatible_value(credentials.provider_specific_data.get("baseUrl"))
                    .or_else(|| non_empty_option(node.base_url.as_deref()))
                    .unwrap_or("https://api.openai.com/v1");
                let api_type = compatible_value(credentials.provider_specific_data.get("apiType"))
                    .or_else(|| non_empty_option(node.api_type.as_deref()))
                    .unwrap_or("chat");
                let normalized = base_url.trim_end_matches('/');
                let path = if api_type == "responses" {
                    "/responses"
                } else {
                    "/chat/completions"
                };
                return Ok(format!("{normalized}{path}"));
            }

            if node.r#type == "anthropic-compatible" {
                let base_url = compatible_value(credentials.provider_specific_data.get("baseUrl"))
                    .or_else(|| non_empty_option(node.base_url.as_deref()))
                    .unwrap_or("https://api.anthropic.com/v1");
                return Ok(format!("{}/messages", base_url.trim_end_matches('/')));
            }
        }

        if self.provider == "gemini" {
            let action = if stream {
                "streamGenerateContent?alt=sse"
            } else {
                "generateContent"
            };
            return Ok(format!("{}/{model}:{action}", self.config.base_url));
        }

        if self.config.base_url.contains("{accountId}")
            || self.config.base_url.contains("{project}")
            || self.config.base_url.contains("{location}")
        {
            let mut url = self.config.base_url.clone();
            if url.contains("{accountId}") {
                let account_id = compatible_value(
                    credentials.provider_specific_data.get("accountId"),
                )
                .ok_or(ExecutorError::MissingProviderSpecificData(
                    self.provider.clone(),
                    "accountId",
                ))?;
                url = url.replace("{accountId}", account_id);
            }
            if url.contains("{project}") {
                let project = compatible_value(credentials.provider_specific_data.get("project"))
                    .ok_or(ExecutorError::MissingProviderSpecificData(
                    self.provider.clone(),
                    "project",
                ))?;
                url = url.replace("{project}", project);
            }
            if url.contains("{location}") {
                let location = compatible_value(credentials.provider_specific_data.get("location"))
                    .ok_or(ExecutorError::MissingProviderSpecificData(
                        self.provider.clone(),
                        "location",
                    ))?;
                url = url.replace("{location}", location);
            }
            return Ok(url);
        }

        if matches!(
            self.provider.as_str(),
            "claude" | "kimi" | "minimax" | "kimi-coding"
        ) {
            return Ok(format!("{}?beta=true", self.config.base_url));
        }

        Ok(self.config.base_url.clone())
    }

    pub fn build_headers(
        &self,
        model: &str,
        credentials: &ProviderConnection,
        stream: bool,
    ) -> Result<HeaderMap, ExecutorError> {
        self.build_headers_for_request(model, credentials, stream, &BTreeMap::new())
    }

    pub fn build_headers_for_request(
        &self,
        model: &str,
        credentials: &ProviderConnection,
        stream: bool,
        client_headers: &BTreeMap<String, String>,
    ) -> Result<HeaderMap, ExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        for (name, value) in &self.config.default_headers {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .expect("static header name"),
                HeaderValue::from_str(value)?,
            );
        }

        let glm_claude_transport = self.provider == "glm"
            && credentials
                .runtime_transport
                .as_ref()
                .and_then(|transport| transport.base_url.as_deref())
                .is_some_and(|url| url.contains("/anthropic/") || url.ends_with("/messages"));
        if glm_claude_transport {
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            headers.insert(
                "anthropic-beta",
                HeaderValue::from_static("claude-code-20250219,interleaved-thinking-2025-05-14"),
            );
        }
        let commandcode_messages_transport = self.provider == "commandcode"
            && credentials
                .runtime_transport
                .as_ref()
                .and_then(|transport| transport.base_url.as_deref())
                .is_some_and(|url| url.ends_with("/messages"));
        if commandcode_messages_transport {
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        }

        let is_anthropic_compatible = self
            .provider_node
            .as_ref()
            .is_some_and(|node| node.r#type == "anthropic-compatible");

        if self.provider == "gemini" {
            if let Some(api_key) = credentials.api_key.as_deref() {
                headers.insert("x-goog-api-key", HeaderValue::from_str(api_key)?);
            } else if let Some(access_token) = credentials.access_token.as_deref() {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {access_token}"))?,
                );
            } else {
                return Err(ExecutorError::MissingCredentials(self.provider.clone()));
            }
        } else if self.provider == "anthropic" {
            if let Some(api_key) = credentials.api_key.as_deref() {
                headers.insert("x-api-key", HeaderValue::from_str(api_key)?);
            } else if let Some(access_token) = credentials.access_token.as_deref() {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {access_token}"))?,
                );
            } else {
                return Err(ExecutorError::MissingCredentials(self.provider.clone()));
            }
        } else if matches!(
            self.provider.as_str(),
            "xiaomi-tokenplan" | "xmtp" | "xiaomi-mimo" | "mimo"
        ) && credentials
            .runtime_transport
            .as_ref()
            .and_then(|rt| rt.base_url.as_deref())
            .is_some_and(|u| u.contains("/anthropic/") || u.ends_with("/messages"))
        {
            // Claude native transport: x-api-key (9router xiaomi-tokenplan / xiaomi-mimo)
            let token = credentials
                .api_key
                .as_deref()
                .or(credentials.access_token.as_deref())
                .ok_or_else(|| ExecutorError::MissingCredentials(self.provider.clone()))?;
            headers.insert("x-api-key", HeaderValue::from_str(token)?);
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        } else if is_anthropic_compatible || self.provider.starts_with("anthropic-compatible") {
            // 9router: anthropic-version + dual auth (x-api-key and/or Bearer)
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            if let Some(api_key) = credentials.api_key.as_deref() {
                headers.insert("x-api-key", HeaderValue::from_str(api_key)?);
                // Dual-auth: also send Bearer for third-party gateways (9router)
                if !headers.contains_key(AUTHORIZATION) {
                    headers.insert(
                        AUTHORIZATION,
                        HeaderValue::from_str(&format!("Bearer {api_key}"))?,
                    );
                }
            }
            if let Some(access_token) = credentials.access_token.as_deref() {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {access_token}"))?,
                );
            }
            if !headers.contains_key("x-api-key") && !headers.contains_key(AUTHORIZATION) {
                return Err(ExecutorError::MissingCredentials(self.provider.clone()));
            }
            // Strip first-party Claude Code identity headers for non-Anthropic upstreams
            for h in [
                "x-stainless-package-version",
                "x-stainless-runtime",
                "x-stainless-runtime-version",
                "anthropic-beta",
            ] {
                headers.remove(h);
            }
        } else {
            // Prefer access_token over api_key for Bearer (9router BaseExecutor)
            let token = credentials
                .access_token
                .as_deref()
                .or(credentials.api_key.as_deref())
                .ok_or_else(|| ExecutorError::MissingCredentials(self.provider.clone()))?;

            if matches!(self.provider.as_str(), "glm" | "kimi") {
                headers.insert("x-api-key", HeaderValue::from_str(token)?);
            } else if matches!(self.provider.as_str(), "minimax") {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}"))?,
                );
            } else {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}"))?,
                );
            }

            // Header hooks: kimi / cline / claude overlay (9router default.js)
            if self.provider == "kimi" || self.provider == "kimi-coding" {
                headers.insert(
                    "User-Agent",
                    HeaderValue::from_static("Mozilla/5.0 KimiCoding"),
                );
            }
            if self.provider == "cline" || self.provider == "clinepass" {
                // 9router parity (open-sse/executors/default.js HEADER_HOOKS.clineHeaders
                // + open-sse/shared/clineAuth.js buildClineHeaders): the hook overlays
                // the Cline client headers. Hooks run BEFORE auth in JS, so the
                // generic Bearer Authorization above stands (verbatim token); only
                // the client-identifying headers are overlaid here.
                headers.insert(
                    "User-Agent",
                    HeaderValue::from_str(&format!("OpenProxy/{}", env!("CARGO_PKG_VERSION")))?,
                );
                headers.insert("X-PLATFORM", HeaderValue::from_static(std::env::consts::OS));
                headers.insert("X-PLATFORM-VERSION", HeaderValue::from_static("rust"));
                headers.insert("X-CLIENT-TYPE", HeaderValue::from_static("openproxy"));
                headers.insert(
                    "X-CLIENT-VERSION",
                    HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
                );
                headers.insert(
                    "X-CORE-VERSION",
                    HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
                );
                headers.insert("X-IS-MULTIROOT", HeaderValue::from_static("false"));
            }
            // Per-model Anthropic-Beta flags (9router default.js:167-170 +
            // shared.js selectAnthropicBeta). anthropic-compatible nodes
            // serving a real Claude model sit in front of Anthropic itself,
            // so they need the same flags; the model id gates it so gateways
            // fronting other models are left untouched. Overwrites the
            // static default (which only had 2 flags).
            let is_claude_model = model.starts_with("claude-");
            if self.provider == "claude"
                || (self.provider.starts_with("anthropic-compatible") && is_claude_model)
            {
                if let Ok(val) = HeaderValue::from_str(&select_anthropic_beta(model)) {
                    headers.insert("anthropic-beta", val);
                }
            }
            if self.provider == "kilocode" {
                if let Some(org_id) =
                    compatible_value(credentials.provider_specific_data.get("orgId"))
                {
                    headers.insert("x-kilocode-organizationid", HeaderValue::from_str(org_id)?);
                }
            }
        }

        if matches!(self.provider.as_str(), "claude" | "anthropic") {
            for name in CLAUDE_REQUEST_HEADER_ALLOWLIST {
                if !headers.contains_key(*name) {
                    let Some(value) = client_headers.get(*name) else {
                        continue;
                    };
                    headers.insert(
                        reqwest::header::HeaderName::from_static(name),
                        HeaderValue::from_str(value)?,
                    );
                }
            }
        }

        if self.provider == "commandcode"
            && client_headers.get("x-cmd-zdr").map(String::as_str) == Some("1")
        {
            headers.insert("x-cmd-zdr", HeaderValue::from_static("1"));
        }

        if stream {
            headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        }

        Ok(headers)
    }

    pub fn transform_request(&self, body: &Value, model: &str) -> Value {
        if self.provider == "commandcode" {
            return body.clone();
        }
        let mut body = self.apply_json_schema_fallback(body);

        // Normalize developer→system role (many providers reject role:developer)
        normalize_developer_role(&mut body);

        // Convert OpenAI-format tools to Claude format when the provider
        // uses a Claude-compatible endpoint (minimax, kimi, etc.)
        if matches!(self.provider.as_str(), "minimax" | "kimi" | "kimi-coding") {
            convert_openai_tools_to_claude(&mut body);
        }

        // Inject reasoning_content placeholder for DeepSeek/Kimi providers
        inject_reasoning_content(&self.provider, model, &mut body);

        // Quirk: cerebras/mistral reject Anthropic's client_metadata field
        // (9router default.js dropClientMetadata — top-level delete only)
        if self.provider == "cerebras" || self.provider == "mistral" {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("client_metadata");
            }
        }

        // Strip unsupported request params for providers that don't support them
        strip_unsupported_params(&self.provider, model, &mut body);

        // Z.ai quirk: `stream: true` alone does not stream tool-call arguments —
        // GLM emits them in one silent batch at the end of the stream, which
        // degrades tool calling (zeroclaw-labs/zeroclaw#2901) and trips
        // api.z.ai's 30s idle timeout (vercel/ai#12949). The documented fix is
        // `tool_stream: true` (docs.z.ai/guides/capabilities/stream-tool).
        if matches!(self.provider.as_str(), "glm" | "glm-cn") {
            inject_glm_tool_stream(&mut body);
        }

        body
    }

    /// Fallback json_schema -> json_object for openai-compatible providers
    /// without native Structured Output support.
    ///
    /// When `response_format.type` is `"json_schema"`, this method:
    /// 1. Extracts the JSON schema
    /// 2. Injects schema instructions into the system message
    /// 3. Downgrades `response_format` to `{"type": "json_object"}`
    fn apply_json_schema_fallback(&self, body: &Value) -> Value {
        let is_openai_compatible = self
            .provider_node
            .as_ref()
            .is_some_and(|node| node.r#type == "openai-compatible");

        if !is_openai_compatible {
            return body.clone();
        }

        let response_format = match body.get("response_format") {
            Some(rf) => rf,
            None => return body.clone(),
        };

        if response_format.get("type").and_then(Value::as_str) != Some("json_schema") {
            return body.clone();
        }

        let schema = match response_format
            .get("json_schema")
            .and_then(|s| s.get("schema"))
        {
            Some(s) => s,
            None => return body.clone(),
        };

        let schema_json = serde_json::to_string_pretty(schema).unwrap_or_default();
        let prompt = format!(
            "You must respond with valid JSON that strictly follows this JSON schema:\n```json\n{schema_json}\n```\nRespond ONLY with the JSON object, no other text."
        );

        let mut new_body = body.clone();

        if let Some(messages) = new_body.get_mut("messages").and_then(Value::as_array_mut) {
            let sys_idx = messages
                .iter()
                .position(|m| m.get("role").and_then(Value::as_str) == Some("system"));

            if let Some(idx) = sys_idx {
                let sys = &mut messages[idx];
                if let Some(content) = sys.get_mut("content") {
                    if content.is_string() {
                        let existing = content.as_str().unwrap_or("");
                        *content = Value::String(format!("{existing}\n\n{prompt}"));
                    } else if let Some(arr) = content.as_array_mut() {
                        arr.push(serde_json::json!({
                            "type": "text",
                            "text": format!("\n\n{prompt}")
                        }));
                    }
                }
            } else {
                messages.insert(
                    0,
                    serde_json::json!({
                        "role": "system",
                        "content": prompt
                    }),
                );
            }
        }

        new_body["response_format"] = serde_json::json!({"type": "json_object"});
        new_body
    }

    pub async fn execute(
        &self,
        request: ExecutionRequest,
    ) -> Result<ExecutionResponse, ExecutorError> {
        let prepared = self.prepare_upstream_body(&request.body, &request.model)?;
        self.execute_prepared(
            &request.model,
            request.stream,
            &request.credentials,
            request.proxy.as_ref(),
            &request.client_headers,
            &prepared,
        )
        .await
    }

    /// Apply all provider-required body transforms and serialize exactly once.
    pub fn prepare_upstream_body(
        &self,
        body: &Value,
        model: &str,
    ) -> Result<PreparedUpstreamBody, ExecutorError> {
        let transformed_body = self.transform_request(body, model);
        let bytes = serialize_json_bounded(&transformed_body, MAX_PREPARED_UPSTREAM_BODY_BYTES)?;
        Ok(PreparedUpstreamBody {
            key: self.prepared_body_key(model),
            bytes,
        })
    }

    /// Whether `prepared` was built by an executor with the same transform
    /// configuration and routed model. The caller must additionally guarantee
    /// that the post-planning JSON body has not changed; the chat planner does
    /// so by preparing only after its final request-body mutation.
    pub fn can_reuse_prepared_body(&self, prepared: &PreparedUpstreamBody, model: &str) -> bool {
        prepared.key == self.prepared_body_key(model)
    }

    /// Send an already transformed and bounded body with current account
    /// credentials. Account-dependent URL/header/proxy state is rebuilt while
    /// the immutable request bytes are shared by reference count.
    pub async fn execute_prepared(
        &self,
        model: &str,
        stream: bool,
        credentials: &ProviderConnection,
        proxy: Option<&ProxyTarget>,
        client_headers: &BTreeMap<String, String>,
        prepared: &PreparedUpstreamBody,
    ) -> Result<ExecutionResponse, ExecutorError> {
        if !self.can_reuse_prepared_body(prepared, model) {
            return Err(ExecutorError::PreparedBodyMismatch);
        }
        let headers = self.build_headers_for_request(model, credentials, stream, client_headers)?;
        let url = self.build_url(model, stream, credentials)?;

        // Acquire semaphore for tokenrouter free models to limit concurrent requests to 1
        let _tokenrouter_permit = if self.provider == "tokenrouter"
            && (model == "qwen/qwen3.8-max-free" || model == "moonshotai/kimi-k3-free")
        {
            Some(TOKENROUTER_SEMAPHORE.acquire().await?)
        } else {
            None
        };

        let use_hyper = Self::use_hyper_transport(proxy, &url);
        let upstream = self
            .send_one(&url, &headers, &prepared.bytes, proxy, use_hyper)
            .await?;

        // C13: account selection and the sole 401/403 recovery live in the
        // request-scoped planner. Preserve the raw response for that owner.
        Ok(ExecutionResponse {
            response: upstream,
            url,
            headers,
            transport: if use_hyper {
                TransportKind::Hyper
            } else {
                TransportKind::Reqwest
            },
        })
    }

    /// Send a single request without retries, returning the raw upstream response.
    async fn send_one(
        &self,
        url: &str,
        headers: &HeaderMap,
        body: &Bytes,
        proxy: Option<&ProxyTarget>,
        use_hyper: bool,
    ) -> Result<UpstreamResponse, ExecutorError> {
        if use_hyper {
            let client = self.pool.get_hyper_direct(&self.provider)?;
            let uri: Uri = url.parse()?;
            let mut req = HyperRequest::post(uri).body(Full::new(body.clone()))?;
            *req.headers_mut() = headers.clone();
            client
                .request(req)
                .await
                .map_err(ExecutorError::Hyper)
                .map(UpstreamResponse::Hyper)
        } else {
            let client = self.pool.get(&self.provider, proxy)?;
            client
                .post(url)
                .headers(headers.clone())
                .body(body.clone())
                .send()
                .await
                .map_err(ExecutorError::Request)
                .map(UpstreamResponse::Reqwest)
        }
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    fn prepared_body_key(&self, model: &str) -> PreparedBodyKey {
        PreparedBodyKey {
            provider: self.provider.clone(),
            provider_node: self.provider_node.clone(),
            model: model.to_string(),
        }
    }

    fn use_hyper_transport(proxy: Option<&ProxyTarget>, url: &str) -> bool {
        proxy.is_none()
            && url
                .split('?')
                .next()
                .is_some_and(|path| path.ends_with("/chat/completions"))
    }
}

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedJsonWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(next_len) = self.bytes.len().checked_add(buf.len()) else {
            self.exceeded = true;
            return Err(io::Error::other("prepared JSON body length overflow"));
        };
        if next_len > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("prepared JSON body exceeds limit"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialize_json_bounded(value: &Value, limit: usize) -> Result<Bytes, ExecutorError> {
    let mut writer = BoundedJsonWriter::new(limit);
    if let Err(error) = serde_json::to_writer(&mut writer, value) {
        if writer.exceeded {
            return Err(ExecutorError::PreparedBodyTooLarge { limit });
        }
        return Err(ExecutorError::Serialize(error));
    }
    Ok(Bytes::from(writer.bytes))
}

fn compatible_value(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn non_empty_option(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn bearer_token(credentials: &ProviderConnection) -> Option<&str> {
    non_empty_option(credentials.access_token.as_deref())
        .or_else(|| non_empty_option(credentials.api_key.as_deref()))
}

/// Z.ai coding/PaaS endpoints require `tool_stream: true` in addition to
/// `stream: true` for tool-call arguments to arrive as `delta.tool_calls`
/// fragments (docs.z.ai/guides/capabilities/stream-tool). Without it GLM
/// batches arguments at the end of the stream, causing degenerate tool
/// calling (zeroclaw-labs/zeroclaw#2901) and 30s idle-timeout connection
/// resets on api.z.ai (vercel/ai#12949).
///
/// No-op for non-streaming, tool-less or Claude-shaped bodies (the anthropic
/// transport of glm carries Claude-style tools without a `function` key),
/// and when the client already set the flag itself.
fn inject_glm_tool_stream(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    if obj.contains_key("tool_stream") {
        return;
    }
    if obj.get("stream").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let has_openai_tools = obj
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            !tools.is_empty()
                && tools.first().is_some_and(|tool| {
                    tool.get("type").and_then(Value::as_str) == Some("function")
                        || tool.get("function").is_some()
                })
        });
    if has_openai_tools {
        obj.insert("tool_stream".to_string(), Value::Bool(true));
    }
}

/// Convert OpenAI-format tools to Claude format.
///
/// OpenAI: `{"type":"function", "function": {"name":"x", "description":"d", "parameters":{...}}}`
/// Claude: `{"name":"x", "description":"d", "input_schema": {...}}`
///
/// Also strips `tool_choice` from OpenAI format and converts it.
fn convert_openai_tools_to_claude(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    // Convert tools[]
    if let Some(tools) = obj.get_mut("tools").and_then(Value::as_array_mut) {
        let mut claude_tools = Vec::new();
        for tool in tools.drain(..) {
            let Some(tool_obj) = tool.as_object() else {
                continue;
            };
            let type_ = tool_obj.get("type").and_then(Value::as_str).unwrap_or("");
            if type_ != "function" {
                // Skip non-function tools
                continue;
            }
            let Some(func) = tool_obj.get("function") else {
                continue;
            };
            let name = func
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let description = func
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let input_schema = func
                .get("parameters")
                .cloned()
                .or_else(|| func.get("input_schema").cloned())
                .unwrap_or(serde_json::json!({"type": "object", "properties": {}}));

            claude_tools.push(serde_json::json!({
                "name": name,
                "description": description,
                "input_schema": input_schema,
            }));
        }
        if !claude_tools.is_empty() {
            // Add cache_control to last tool
            if let Some(last) = claude_tools.last_mut() {
                if let Some(last_obj) = last.as_object_mut() {
                    last_obj.insert(
                        "cache_control".to_string(),
                        serde_json::json!({"type": "ephemeral"}),
                    );
                }
            }
            tools.clear();
            tools.extend(claude_tools);
        } else {
            obj.remove("tools");
        }
    }

    // Convert tool_choice
    // OpenAI: {"type": "function", "function": {"name": "..."}} → Claude: {"type": "tool", "name": "..."}
    // OpenAI: "auto" → Claude: {"type": "auto"}
    // OpenAI: "required" → Claude: {"type": "any"}
    // OpenAI: "none" → Claude: {"type": "none"}
    if let Some(tc) = obj.get("tool_choice") {
        let new_tc = match tc {
            Value::String(s) => match s.as_str() {
                "required" => Some(serde_json::json!({"type": "any"})),
                "none" => Some(serde_json::json!({"type": "none"})),
                "auto" => Some(serde_json::json!({"type": "auto"})),
                _ => Some(serde_json::json!({"type": "auto"})),
            },
            Value::Object(m) => {
                if let Some(name) = m
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                {
                    Some(serde_json::json!({"type": "tool", "name": name}))
                } else {
                    Some(serde_json::json!({"type": "auto"}))
                }
            }
            _ => None,
        };
        if let Some(new_tc) = new_tc {
            obj.insert("tool_choice".to_string(), new_tc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a6api_uses_the_canonical_chat_completions_endpoint() {
        assert_eq!(
            provider_config_base_url("a6api").as_deref(),
            Some("https://api.a6api.com/v1/chat/completions")
        );
    }

    #[test]
    fn bounded_serializer_accepts_exact_limit_and_rejects_plus_one() {
        let value = serde_json::json!({"unicode": "Привет 🌍", "unknown": {"x": true}});
        let exact = serde_json::to_vec(&value).unwrap();
        let prepared = serialize_json_bounded(&value, exact.len()).unwrap();
        assert_eq!(prepared.as_ref(), exact.as_slice());

        let error = serialize_json_bounded(&value, exact.len() - 1).unwrap_err();
        assert!(matches!(
            error,
            ExecutorError::PreparedBodyTooLarge { limit } if limit == exact.len() - 1
        ));
    }

    #[test]
    fn drops_client_metadata_for_cerebras() {
        // 9router default.js dropClientMetadata quirk (cerebras.js quirks).
        let executor = DefaultExecutor::new("cerebras", Arc::new(ClientPool::new()), None).unwrap();
        let body = serde_json::json!({
            "client_metadata": { "ideType": 9 },
            "messages": []
        });
        let transformed = executor.transform_request(&body, "llama-3.3-70b");
        assert!(
            !transformed
                .as_object()
                .unwrap()
                .contains_key("client_metadata"),
            "cerebras must drop client_metadata"
        );
        assert_eq!(transformed["messages"], serde_json::json!([]));
    }

    #[test]
    fn drops_client_metadata_for_mistral() {
        // 9router default.js dropClientMetadata quirk (mistral.js quirks).
        let executor = DefaultExecutor::new("mistral", Arc::new(ClientPool::new()), None).unwrap();
        let body = serde_json::json!({
            "client_metadata": { "ideType": 9 },
            "messages": []
        });
        let transformed = executor.transform_request(&body, "mistral-large-latest");
        assert!(
            !transformed
                .as_object()
                .unwrap()
                .contains_key("client_metadata"),
            "mistral must drop client_metadata"
        );
        assert_eq!(transformed["messages"], serde_json::json!([]));
    }

    #[test]
    fn keeps_client_metadata_for_openai() {
        // No dropClientMetadata quirk — the field must survive (JS parity).
        let executor = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let body = serde_json::json!({
            "client_metadata": { "ideType": 9 },
            "messages": []
        });
        let transformed = executor.transform_request(&body, "gpt-4o");
        assert_eq!(
            transformed["client_metadata"],
            serde_json::json!({ "ideType": 9 }),
            "openai must keep client_metadata"
        );
    }

    #[test]
    fn glm_injects_tool_stream_for_streaming_tool_requests() {
        // Z.ai requires tool_stream=true alongside stream=true for streamed
        // tool-call arguments (docs.z.ai/guides/capabilities/stream-tool).
        let executor = DefaultExecutor::new("glm", Arc::new(ClientPool::new()), None).unwrap();
        let body = serde_json::json!({
            "model": "glm-5.3-flash",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {"type": "object", "properties": {}}
                }
            }]
        });
        let transformed = executor.transform_request(&body, "glm-5.3-flash");
        assert_eq!(transformed["tool_stream"], serde_json::json!(true));

        // glm-cn (open.bigmodel.cn) is the same Zhipu API surface.
        let executor = DefaultExecutor::new("glm-cn", Arc::new(ClientPool::new()), None).unwrap();
        let transformed = executor.transform_request(&body, "glm-5.3-flash");
        assert_eq!(transformed["tool_stream"], serde_json::json!(true));
    }

    #[test]
    fn glm_skips_tool_stream_without_tools_or_stream() {
        let executor = DefaultExecutor::new("glm", Arc::new(ClientPool::new()), None).unwrap();
        let tools = serde_json::json!([{
            "type": "function",
            "function": {"name": "get_weather", "parameters": {"type": "object"}}
        }]);

        // Non-streaming request: tool_stream requires stream=true.
        let body = serde_json::json!({
            "model": "glm-5.3-flash",
            "stream": false,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": tools
        });
        let transformed = executor.transform_request(&body, "glm-5.3-flash");
        assert!(transformed.get("tool_stream").is_none());

        // No tools: nothing to stream.
        let body = serde_json::json!({
            "model": "glm-5.3-flash",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        });
        let transformed = executor.transform_request(&body, "glm-5.3-flash");
        assert!(transformed.get("tool_stream").is_none());
    }

    #[test]
    fn glm_skips_tool_stream_for_claude_shaped_tools() {
        // glm via the anthropic transport carries Claude-style tools
        // (no `function` key); tool_stream must not be sent there.
        let executor = DefaultExecutor::new("glm", Arc::new(ClientPool::new()), None).unwrap();
        let body = serde_json::json!({
            "model": "glm-5.3-flash",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "name": "get_weather",
                "description": "Get weather",
                "input_schema": {"type": "object", "properties": {}}
            }]
        });
        let transformed = executor.transform_request(&body, "glm-5.3-flash");
        assert!(transformed.get("tool_stream").is_none());
    }

    #[test]
    fn glm_preserves_client_tool_stream_choice() {
        let executor = DefaultExecutor::new("glm", Arc::new(ClientPool::new()), None).unwrap();
        let body = serde_json::json!({
            "model": "glm-5.3-flash",
            "stream": true,
            "tool_stream": false,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "type": "function",
                "function": {"name": "get_weather", "parameters": {"type": "object"}}
            }]
        });
        let transformed = executor.transform_request(&body, "glm-5.3-flash");
        assert_eq!(transformed["tool_stream"], serde_json::json!(false));
    }

    #[test]
    fn other_providers_do_not_get_tool_stream() {
        let executor = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let body = serde_json::json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "type": "function",
                "function": {"name": "get_weather", "parameters": {"type": "object"}}
            }]
        });
        let transformed = executor.transform_request(&body, "gpt-4o");
        assert!(transformed.get("tool_stream").is_none());
    }

    #[test]
    fn openrouter_sends_attribution_headers_and_gateways_omit_them() {
        let credentials = ProviderConnection {
            api_key: Some("sk-or-test".to_string()),
            ..ProviderConnection::default()
        };
        let openrouter =
            DefaultExecutor::new("openrouter", Arc::new(ClientPool::new()), None).unwrap();
        let headers = openrouter
            .build_headers("meta/llama-3.1-8b-instruct:free", &credentials, false)
            .unwrap();
        assert_eq!(headers["HTTP-Referer"], "https://endpoint-proxy.local");
        assert_eq!(headers["X-Title"], "Endpoint Proxy");

        // OpenRouter-fronted gateways must not claim OpenRouter attribution.
        for provider in ["kilocode", "nvidia"] {
            let executor = DefaultExecutor::new(provider, Arc::new(ClientPool::new()), None);
            if let Ok(executor) = executor {
                let headers = executor
                    .build_headers("tencent/hy3:free", &credentials, false)
                    .unwrap();
                assert!(
                    !headers.contains_key("HTTP-Referer"),
                    "{provider} must omit HTTP-Referer"
                );
            }
        }
    }

    #[test]
    fn kilocode_posts_to_live_openrouter_gateway_endpoint() {
        // Live-verified: POST https://api.kilo.ai/api/openrouter/chat/completions → 200.
        let executor = DefaultExecutor::new("kilocode", Arc::new(ClientPool::new()), None).unwrap();
        let credentials = ProviderConnection {
            api_key: Some("kc-test".to_string()),
            ..ProviderConnection::default()
        };
        let url = executor
            .build_url("tencent/hy3:free", false, &credentials)
            .unwrap();
        assert_eq!(url, "https://api.kilo.ai/api/openrouter/chat/completions");
    }

    #[test]
    fn commandcode_preserves_translated_body_and_scopes_zdr_header() {
        let executor =
            DefaultExecutor::new("commandcode", Arc::new(ClientPool::new()), None).unwrap();
        let body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "messages": [{"role": "developer", "content": "keep"}],
            "unknown_vendor_field": true
        });
        assert_eq!(executor.transform_request(&body, "gpt-5.6-sol"), body);

        let credentials = ProviderConnection {
            api_key: Some("test-key".to_string()),
            ..ProviderConnection::default()
        };
        let client_headers = BTreeMap::from([("x-cmd-zdr".to_string(), "1".to_string())]);
        let headers = executor
            .build_headers_for_request("gpt-5.6-sol", &credentials, false, &client_headers)
            .unwrap();
        assert_eq!(headers["x-cmd-zdr"], "1");

        let mut messages_credentials = credentials.clone();
        messages_credentials.runtime_transport = Some(crate::types::RuntimeTransport {
            base_url: Some("https://api.commandcode.ai/provider/v1/messages".to_string()),
        });
        assert_eq!(
            executor
                .build_url("claude-sonnet-5", false, &messages_credentials)
                .unwrap(),
            "https://api.commandcode.ai/provider/v1/messages"
        );
        let headers = executor
            .build_headers_for_request(
                "claude-sonnet-5",
                &messages_credentials,
                false,
                &BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(headers["anthropic-version"], "2023-06-01");

        let openai = DefaultExecutor::new("openai", Arc::new(ClientPool::new()), None).unwrap();
        let headers = openai
            .build_headers_for_request("gpt-5.6-sol", &credentials, false, &client_headers)
            .unwrap();
        assert!(!headers.contains_key("x-cmd-zdr"));
    }
}
