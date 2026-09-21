pub mod antigravity;
mod api_key;
mod azure;
mod bounded_body;
mod client_pool;
mod codebuddy_cn;
mod codebuddy_intl;
mod codex;
mod codex_search;
mod default;
mod devin_cli;
mod github;
mod kimchi;
mod mimo_free;
mod ollama;
mod opencode;
mod provider;
mod strip_unsupported;
mod trae;
mod vertex;
mod zed;

pub use antigravity::{
    AntigravityExecutionRequest, AntigravityExecutor, AntigravityExecutorError,
    AntigravityExecutorResponse, ANTIGRAVITY_BASE_URL,
};
pub use api_key::{
    get_api_key_provider_config, is_api_key_provider, ApiKeyExecutionRequest, ApiKeyExecutor,
    ApiKeyExecutorError, ApiKeyExecutorResponse,
};
pub use azure::{AzureExecutionRequest, AzureExecutor, AzureExecutorError, AzureExecutorResponse};
pub use bounded_body::{
    diagnostic_body_limit, read_reqwest_body, read_reqwest_diagnostic, read_upstream_body,
    read_upstream_diagnostic, success_body_limit, BoundedBodyError, DiagnosticBody,
    DEFAULT_DIAGNOSTIC_BODY_LIMIT_BYTES, DEFAULT_SUCCESS_BODY_LIMIT_BYTES,
    DIAGNOSTIC_BODY_LIMIT_ENV, DIAGNOSTIC_TRANSPORT_MARKER, DIAGNOSTIC_TRUNCATION_MARKER,
    SUCCESS_BODY_LIMIT_ENV,
};
pub use client_pool::{
    ClientPool, ClientTimeout, DirectHyperClient, CLIENT_POOL_IDLE_TIMEOUT,
    CLIENT_POOL_MAX_IDLE_PER_HOST, CLIENT_POOL_TCP_KEEPALIVE, DEFAULT_CONNECT_TIMEOUT,
    DEFAULT_STREAM_TIMEOUT,
};
pub use codebuddy_cn::CodeBuddyCNExecutor;
pub use codebuddy_intl::CodeBuddyIntlExecutor;
pub use codex::{
    convert_openai_sse_to_standard, CodexExecutionRequest, CodexExecutor, CodexExecutorError,
    CodexExecutorResponse,
};
pub use codex_search::{
    CodexSearchExecutionRequest, CodexSearchExecutor, CodexSearchExecutorError,
    CODEX_STANDALONE_SEARCH_URL,
};
pub use default::{
    provider_config_base_url, select_anthropic_beta, DefaultExecutor, ExecutionRequest,
    ExecutionResponse, ExecutorError, PreparedUpstreamBody, ProviderConfig, TransportKind,
    UpstreamResponse, MAX_PREPARED_UPSTREAM_BODY_BYTES,
};
pub use devin_cli::{DevinCliExecutor, DevinExecutionRequest, DevinExecutorResponse};
pub use github::{
    GithubExecutionRequest, GithubExecutor, GithubExecutorError, GithubExecutorResponse,
};
pub use kimchi::KimchiExecutor;
pub use mimo_free::{MimoFreeExecutionRequest, MimoFreeExecutor, MimoFreeExecutorResponse};
pub use ollama::{
    OllamaExecutionRequest, OllamaExecutor, OllamaExecutorError, OllamaExecutorResponse,
};
pub use opencode::{
    OpenCodeExecutionRequest, OpenCodeExecutor, OpenCodeExecutorError, OpenCodeExecutorResponse,
    OpenCodeTier,
};
pub use provider::{
    LogEntry, LogLevel, ProviderExecutionRequest, ProviderExecutionResponse, ProviderExecutor,
    ProviderExecutorConfig, ProviderExecutorError, ProviderFormat, ProxyOptions, UnifiedExecutor,
};
pub use trae::{TraeExecutionRequest, TraeExecutor, TraeExecutorError, TraeExecutorResponse};
pub use vertex::{
    VertexExecutionRequest, VertexExecutor, VertexExecutorError, VertexExecutorResponse,
};
pub use zed::{ZedExecutionRequest, ZedExecutor, ZedExecutorResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorKind {
    Default,
}
