//! OAuth provider constants — field-tested values matching 9router.
//!
//! Each provider function returns a static config that encodes the
//! authorize / token URLs, scopes, PKCE usage, extra query parameters,
//! and a refresh lead time.

use once_cell::sync::Lazy;
use url::form_urlencoded;

/// Antigravity extra params — client_secret resolved from env at first access.
static ANTIGRAVITY_EXTRA_PARAMS: Lazy<&'static [(&'static str, &'static str)]> = Lazy::new(|| {
    Box::leak(
        vec![
            (
                "client_secret",
                crate::oauth::secret::antigravity_client_secret(),
            ),
            ("access_type", "offline"),
            ("prompt", "consent"),
            (
                "user_info_url",
                "https://www.googleapis.com/oauth2/v1/userinfo",
            ),
        ]
        .into_boxed_slice(),
    )
});

/// Static OAuth provider configuration.
#[derive(Debug, Clone, Copy)]
pub struct OAuthProviderConfig {
    pub id: &'static str,
    pub client_id: &'static str,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub scopes: &'static [&'static str],
    pub uses_pkce: bool,
    pub extra_params: &'static [(&'static str, &'static str)],
    pub refresh_lead_ms: u64,
}

impl OAuthProviderConfig {
    /// Build a full authorization URL (PKCE auth-code flow).
    pub fn build_auth_url(
        &self,
        client_id: &str,
        redirect_uri: &str,
        state: &str,
        code_challenge: &str,
    ) -> String {
        let mut pairs: Vec<(String, String)> = vec![
            ("client_id".to_string(), client_id.to_string()),
            ("redirect_uri".to_string(), redirect_uri.to_string()),
            ("response_type".to_string(), "code".to_string()),
            ("state".to_string(), state.to_string()),
        ];

        if self.uses_pkce {
            pairs.push(("code_challenge".to_string(), code_challenge.to_string()));
            pairs.push(("code_challenge_method".to_string(), "S256".to_string()));
        }

        if !self.scopes.is_empty() {
            pairs.push(("scope".to_string(), self.scopes.join(" ")));
        }

        for (key, value) in self.extra_params.iter() {
            pairs.push((key.to_string(), value.to_string()));
        }

        let query_string = pairs
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    k,
                    form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("&");

        format!("{}?{}", self.authorize_url, query_string)
    }

    /// Look up a custom extra parameter by key.
    pub fn get_param(&self, key: &str) -> Option<&'static str> {
        self.extra_params
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
    }
}

// ---------------------------------------------------------------------------
// Provider definitions
// ---------------------------------------------------------------------------

/// Claude (Anthropic subscription OAuth, C46): single source of truth.
/// Identity values verified against the live Claude Code 2.1.289 bundle
/// (built 2026-10-03; MITM-verified 2026-10-05):
/// `claude-cli/${VERSION} (external, cli)`,
/// `x-app: cli`, beta `claude-code-20250219` + `oauth-2025-04-20`.
///
/// Bump procedure for [`CLAUDE_CLI_VERSION`]: `npm view @anthropic-ai/claude-code version`.
/// Official Claude Code OAuth client id.
pub const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Subscription OAuth authorize endpoint. Claude Code 2.1.289 pins the
/// console/manual-code flow to `platform.claude.com` (binary
/// `CONSOLE_AUTHORIZE_URL`; `claude.ai` remains only behind a bot-wall
/// redirect for the browser flow). Env override keeps escape hatch.
pub const CLAUDE_AUTHORIZE_URL: &str = "https://platform.claude.com/oauth/authorize";
/// Subscription OAuth token endpoint (relay-parity default; env-overridable).
pub const CLAUDE_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Account profile endpoint for email/display-name enrichment (fail-open).
pub const CLAUDE_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// Registered redirect: shows the code for manual copy (`code=true`).
pub const CLAUDE_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
/// Subscription scopes (relay-parity set).
pub const CLAUDE_SCOPES: &[&str] = &[
    "org:create_api_key",
    "user:profile",
    "user:inference",
    "user:sessions:claude_code",
];
/// Pinned Claude Code CLI version (single source; see bump procedure above).
pub const CLAUDE_CLI_VERSION: &str = "2.1.289";
/// `@anthropic-ai/sdk` version that Claude Code `CLAUDE_CLI_VERSION` bundles
/// (`x-stainless-package-version`; MITM census 628/628). Bump together with
/// the CLI pin: `npm view @anthropic-ai/sdk version` inside the pinned CLI.
pub const CLAUDE_STAINLESS_PACKAGE_VERSION: &str = "0.128.0";
/// Node runtime version the pinned CLI reports (`x-stainless-runtime-version`;
/// census 628/628). Bump when re-mining the CLI pin from a live capture.
pub const CLAUDE_STAINLESS_RUNTIME_VERSION: &str = "v26.3.0";
/// User-Agent the CLI's OAuth stack sends to `platform.claude.com` during
/// code/token exchange and refresh (live MITM 2026-10-06: `axios/1.15.2`).
/// Bump together with the CLI pin.
pub const CLAUDE_OAUTH_EXCHANGE_UA: &str = "axios/1.15.2";
/// User-Agent the CLI sends when validating a fresh token against the
/// profile endpoint (live MITM: `claude-code/<version>`).
pub fn claude_profile_user_agent() -> String {
    format!("claude-code/{CLAUDE_CLI_VERSION}")
}
/// Accept header the CLI's OAuth requests carry (axios default form;
/// exchange/refresh captured live, refresh body shape verified against
/// the CLI binary — no refresh capture exists in the corpus).
pub const CLAUDE_OAUTH_ACCEPT: &str = "application/json, text/plain, */*";

/// Scopes the authorize URL requests that the issued grant is missing.
///
/// The server may issue a strictly narrower grant than requested
/// (consent checkboxes, subscription-gated scopes). A grant missing
/// `user:inference` cannot chat; missing scopes must be visible at
/// login/refresh time, not discovered as a runtime 403.
pub fn missing_claude_scopes(granted: Option<&str>) -> Vec<&'static str> {
    let granted_set: std::collections::HashSet<&str> =
        granted.unwrap_or_default().split_whitespace().collect();
    CLAUDE_SCOPES
        .iter()
        .filter(|scope| !granted_set.contains(*scope))
        .copied()
        .collect()
}

/// Canonical Claude Code CLI User-Agent: `claude-cli/<VERSION> (external, cli)`.
pub fn claude_user_agent() -> String {
    format!("claude-cli/{CLAUDE_CLI_VERSION} (external, cli)")
}

/// Authorize URL (allows `OPENPROXY_CLAUDE_AUTHORIZE_URL` override).
pub fn claude_authorize_url() -> String {
    std::env::var("OPENPROXY_CLAUDE_AUTHORIZE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| CLAUDE_AUTHORIZE_URL.to_string())
}

/// Token URL (allows `OPENPROXY_CLAUDE_TOKEN_URL` override).
pub fn claude_token_url() -> String {
    std::env::var("OPENPROXY_CLAUDE_TOKEN_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| CLAUDE_TOKEN_URL.to_string())
}

/// Profile URL (allows `OPENPROXY_CLAUDE_PROFILE_URL` override).
pub fn claude_profile_url() -> String {
    std::env::var("OPENPROXY_CLAUDE_PROFILE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| CLAUDE_PROFILE_URL.to_string())
}

pub fn claude() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "claude",
        client_id: CLAUDE_CLIENT_ID,
        authorize_url: CLAUDE_AUTHORIZE_URL,
        token_url: CLAUDE_TOKEN_URL,
        scopes: CLAUDE_SCOPES,
        uses_pkce: true,
        extra_params: &[("code", "true")],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

pub fn codex() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "codex",
        client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
        authorize_url: "https://auth.openai.com/oauth/authorize",
        token_url: "https://auth.openai.com/oauth/token",
        scopes: &["openid", "profile", "email", "offline_access"],
        uses_pkce: true,
        extra_params: &[
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "codex_cli_rs"),
        ],
        refresh_lead_ms: 5 * 24 * 60 * 60 * 1000,
    }
}

/// GitHub — device-code flow (not PKCE auth code).
/// The `authorize_url` is the device-code endpoint.
pub fn github() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "github",
        client_id: "Iv1.b507a08c87ecfe98",
        authorize_url: "https://github.com/login/device/code",
        token_url: "https://github.com/login/oauth/access_token",
        scopes: &["read:user"],
        uses_pkce: false,
        extra_params: &[],
        refresh_lead_ms: 0,
    }
}

/// Kimi Coding — device-code flow (dual-auth provider merged in 68566f5).
pub fn kimi_coding() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "kimi-coding",
        client_id: "17e5f671-d194-4dfb-9706-5516cb48c098",
        authorize_url: "https://auth.kimi.com/api/oauth/device_authorization",
        token_url: "https://auth.kimi.com/api/oauth/token",
        scopes: &[],
        uses_pkce: false,
        extra_params: &[],
        refresh_lead_ms: 300_000,
    }
}

/// Kimi — dual-auth alias of kimi-coding (68566f5 merge).
pub fn kimi() -> OAuthProviderConfig {
    let mut config = kimi_coding();
    config.id = "kimi";
    config
}

/// KiloCode — custom device auth flow (matches OmniRoute).
pub fn kilocode() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "kilocode",
        client_id: "openproxy",
        authorize_url: "https://api.kilo.ai/api/device-auth/codes",
        token_url: "https://api.kilo.ai/api/device-auth/codes",
        scopes: &[],
        uses_pkce: false,
        extra_params: &[
            ("initiate_url", "https://api.kilo.ai/api/device-auth/codes"),
            ("poll_url_base", "https://api.kilo.ai/api/device-auth/codes"),
        ],
        refresh_lead_ms: 0,
    }
}

/// Kimchi — browser_token OAuth flow (user copies token from kimchi.dev).
pub fn kimchi() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "kimchi",
        client_id: "openproxy",
        authorize_url: "",
        token_url: "",
        scopes: &[],
        uses_pkce: false,
        extra_params: &[
            ("web_app_url", "https://app.kimchi.dev"),
            (
                "validation_url",
                "https://api.cast.ai/v1/llm/openai/supported-providers",
            ),
            ("user_info_url", "https://app.kimchi.dev/api/v1/me"),
        ],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

/// xAI — PKCE auth-code flow with 96-byte verifier.
pub fn xai() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "xai",
        client_id: "b1a00492-073a-47ea-816f-4c329264a828",
        authorize_url: "https://auth.x.ai/oauth2/authorize",
        token_url: "https://auth.x.ai/oauth2/token",
        scopes: &[
            "openid",
            "profile",
            "email",
            "offline_access",
            "grok-cli:access",
            "api:access",
        ],
        uses_pkce: true,
        extra_params: &[("plan", "generic"), ("referrer", "cli-proxy-api")],
        refresh_lead_ms: 5 * 60 * 1000,
    }
}

pub fn clinepass() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "clinepass",
        client_id: "openproxy",
        authorize_url: "https://api.cline.bot/api/v1/auth/authorize",
        token_url: "https://api.cline.bot/api/v1/auth/token",
        scopes: &[],
        uses_pkce: true,
        extra_params: &[("refresh_url", "https://api.cline.bot/api/v1/auth/refresh")],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

pub fn zed() -> OAuthProviderConfig {
    // Zed uses a non-standard RSA keypair callback flow (NOT standard OAuth):
    // the app generates an RSA-2048 keypair, opens a local port, and
    // https://zed.dev/native_app_signin?native_app_port=...&native_app_public_key=...
    // redirects back with a base64 RSA-OAEP-encrypted access token. No
    // client_id / token_url / refresh_url — the token is long-lived.
    OAuthProviderConfig {
        id: "zed",
        client_id: "", // no client id — RSA keypair flow
        authorize_url: "https://zed.dev/native_app_signin",
        token_url: "", // token delivered via the local callback, not a token URL
        scopes: &[],
        uses_pkce: false,
        extra_params: &[("rsaKeyExchange", "true")],
        refresh_lead_ms: 0,
    }
}

pub fn cline() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "cline",
        client_id: "openproxy",
        authorize_url: "https://api.cline.bot/api/v1/auth/authorize",
        token_url: "https://api.cline.bot/api/v1/auth/token",
        scopes: &[],
        uses_pkce: true,
        extra_params: &[("refresh_url", "https://api.cline.bot/api/v1/auth/refresh")],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

/// CodeBuddy CN — device-code flow (Tencent Copilot, distinct from intl codebuddy).
pub fn codebuddy_cn() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "codebuddy-cn",
        client_id: "openproxy",
        authorize_url: "https://copilot.tencent.com/v2/plugin/auth/state",
        token_url: "https://copilot.tencent.com/v2/plugin/auth/token",
        scopes: &[],
        uses_pkce: false,
        extra_params: &[
            (
                "refresh_url",
                "https://copilot.tencent.com/v2/plugin/auth/token/refresh",
            ),
            ("user_agent", "CLI/2.63.2 CodeBuddy/2.63.2"),
            ("platform", "CLI"),
            ("poll_interval", "5000"),
        ],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

/// CodeBuddy Intl — device-code flow (www.codebuddy.ai, platform=ide).
/// Distinct from codebuddy-cn (copilot.tencent.com, platform=CLI). The
/// state/token/refresh URLs differ only by host, but platform MUST be "ide".
/// 9router parity: `open-sse/providers/registry/codebuddy-intl.js:64-72`.
pub fn codebuddy_intl() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "codebuddy-intl",
        client_id: "openproxy",
        authorize_url: "https://www.codebuddy.ai/v2/plugin/auth/state",
        token_url: "https://www.codebuddy.ai/v2/plugin/auth/token",
        scopes: &[],
        uses_pkce: false,
        extra_params: &[
            (
                "refresh_url",
                "https://www.codebuddy.ai/v2/plugin/auth/token/refresh",
            ),
            // OAuth user-agent differs from the transport User-Agent
            // ("IDE/2.108.1 CodeBuddy/2.108.1") — keep distinct as in JS.
            ("user_agent", "IDE/2.63.2 CodeBuddy/2.63.2"),
            ("platform", "ide"),
            ("poll_interval", "5000"),
        ],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

/// Trae (ByteDance marscode) — custom JSON-body refresh flow.
/// 9router parity: `open-sse/providers/registry/trae.js:48-62`.
pub fn trae() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "trae",
        client_id: "ono9krqynydwx5",
        authorize_url: "https://api.marscode.com/cloudide/api/v3/trae/GetLoginGuidance",
        token_url: "https://api.marscode.com/cloudide/api/v3/trae/oauth/ExchangeToken",
        scopes: &[],
        uses_pkce: false,
        extra_params: &[
            ("client_secret", "-"),
            ("platform", "trae"),
            ("poll_interval", "1500"),
        ],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

/// Antigravity — Google OAuth authorization-code flow (with client_secret).
pub fn antigravity() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "antigravity",
        client_id: "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com",
        authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
        token_url: "https://oauth2.googleapis.com/token",
        scopes: &[
            "https://www.googleapis.com/auth/cloud-platform",
            "https://www.googleapis.com/auth/userinfo.email",
            "https://www.googleapis.com/auth/userinfo.profile",
            "https://www.googleapis.com/auth/cclog",
            "https://www.googleapis.com/auth/experimentsandconfigs",
        ],
        uses_pkce: false,
        extra_params: &ANTIGRAVITY_EXTRA_PARAMS,
        refresh_lead_ms: 5 * 60 * 1000,
    }
}

/// GitLab (cloud) — PKCE auth-code flow.
pub fn gitlab() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "gitlab",
        client_id: "openproxy",
        authorize_url: "https://gitlab.com/oauth/authorize",
        token_url: "https://gitlab.com/oauth/token",
        scopes: &["api", "read_user"],
        uses_pkce: true,
        extra_params: &[],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

/// CodeBuddy — device-code flow.
pub fn codebuddy() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "codebuddy",
        client_id: "openproxy",
        authorize_url: "https://copilot.tencent.com/v2/plugin/auth/state",
        token_url: "https://copilot.tencent.com/v2/plugin/auth/token",
        scopes: &[],
        uses_pkce: false,
        extra_params: &[],
        refresh_lead_ms: 0,
    }
}

/// OpenAI Native — PKCE auth-code flow (not codex).
pub fn openai_native() -> OAuthProviderConfig {
    OAuthProviderConfig {
        id: "openai-native",
        client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
        authorize_url: "https://auth.openai.com/oauth/authorize",
        token_url: "https://auth.openai.com/oauth/token",
        scopes: &["openid", "profile", "email", "offline_access"],
        uses_pkce: true,
        extra_params: &[("originator", "openai_native")],
        refresh_lead_ms: 5 * 24 * 60 * 60 * 1000,
    }
}

/// Self-hosted GitLab — dynamic base URL constructor.
/// Uses `Box::leak` internally so the returned config lives for the
/// program's lifetime (acceptable since this is called once at setup).
pub fn gitlab_with_baseurl(base_url: &str) -> OAuthProviderConfig {
    let base = base_url.trim_end_matches('/');
    let authorize_url = alloc_string(&format!("{}/oauth/authorize", base));
    let token_url = alloc_string(&format!("{}/oauth/token", base));
    OAuthProviderConfig {
        id: "gitlab-selfhost",
        client_id: "openproxy",
        authorize_url,
        token_url,
        scopes: &["api", "read_user"],
        uses_pkce: true,
        extra_params: &[],
        refresh_lead_ms: 4 * 60 * 60 * 1000,
    }
}

fn alloc_string(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

pub fn get_config(provider: &str) -> Option<OAuthProviderConfig> {
    match provider {
        "claude" => Some(claude()),
        "codex" => Some(codex()),
        "github" => Some(github()),
        "kimi" => Some(kimi()),
        "kimi-coding" => Some(kimi_coding()),
        "kilocode" => Some(kilocode()),
        "cline" => Some(cline()),
        "clinepass" => Some(clinepass()),
        "zed" => Some(zed()),
        "gitlab" => Some(gitlab()),
        "codebuddy" => Some(codebuddy()),
        "openai-native" => Some(openai_native()),
        "xai" => Some(xai()),
        "kimchi" => Some(kimchi()),
        "antigravity" => Some(antigravity()),
        "codebuddy-cn" => Some(codebuddy_cn()),
        "codebuddy-intl" => Some(codebuddy_intl()),
        "trae" => Some(trae()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zed_oauth_no_refresh_endpoint() {
        // 9router zed.js: RSA keypair flow — no client_id / token_url / refresh.
        let cfg = zed();
        assert_eq!(cfg.id, "zed");
        assert!(cfg.token_url.is_empty(), "zed has no token_url");
        assert!(cfg.client_id.is_empty(), "zed has no client_id");
        assert_eq!(cfg.authorize_url, "https://zed.dev/native_app_signin");
        assert!(!cfg.uses_pkce);
        assert_eq!(cfg.get_param("rsaKeyExchange"), Some("true"));
        // Dispatcher resolves it.
        assert!(get_config("zed").is_some());
    }

    #[test]
    fn claude_oauth_identity_pins_match_live_cli() {
        // Live MITM 2026-10-06 (`claude setup-token` flow, CC 2.1.289):
        // platform.claude.com exchange/refresh rides axios/1.15.2 with the
        // axios accept form; the fresh-token profile validation runs as
        // claude-code/<CLI version>. These pins are consumed by
        // exchange_claude_compat, refresh_claude_oauth_token,
        // fetch_claude_profile, and the dashboard-test refresh copy.
        assert_eq!(CLAUDE_OAUTH_EXCHANGE_UA, "axios/1.15.2");
        assert_eq!(CLAUDE_OAUTH_ACCEPT, "application/json, text/plain, */*");
        assert_eq!(
            claude_profile_user_agent(),
            format!("claude-code/{CLAUDE_CLI_VERSION}")
        );
    }

    #[test]
    fn missing_claude_scopes_diffs_requested_vs_granted() {
        // Full grant → nothing missing.
        assert!(missing_claude_scopes(Some(
            "org:create_api_key user:profile user:inference user:sessions:claude_code"
        ))
        .is_empty());
        // The live 2026-10-06 incident shape: only two of four scopes issued.
        let missing = missing_claude_scopes(Some("org:create_api_key user:profile"));
        assert_eq!(missing, vec!["user:inference", "user:sessions:claude_code"]);
        // Order-insensitive / extra granted scopes tolerated.
        let missing = missing_claude_scopes(Some(
            "user:sessions:claude_code user:inference org:create_api_key user:profile user:office",
        ));
        assert!(missing.is_empty());
        // Absent scope string = nothing granted.
        assert_eq!(missing_claude_scopes(None), CLAUDE_SCOPES.to_vec());
    }
}
