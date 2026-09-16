//! OAuth client-secret resolution.
//!
//! Client secrets for OAuth providers are resolved from environment variables
//! (e.g. `ANTIGRAVITY_CLIENT_SECRET`) with a hardcoded fallback so existing flows
//! keep working. Operators should set the env vars to rotate/revoke the
//! bundled secrets. 9router tracks the same values; the fallbacks here mirror
//! them so a fresh checkout works out of the box.

use once_cell::sync::Lazy;
use std::sync::Mutex;

fn resolve(name: &str, fallback: &'static str) -> &'static str {
    // Resolve once per name; the value is static for the process lifetime.
    static CACHE: Lazy<Mutex<std::collections::HashMap<String, &'static str>>> =
        Lazy::new(|| Mutex::new(std::collections::HashMap::new()));
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(v) = cache.get(name) {
        return v;
    }
    let value = std::env::var(name).unwrap_or_else(|_| fallback.to_string());
    let leaked: &'static str = Box::leak(value.into_boxed_str());
    cache.insert(name.to_string(), leaked);
    leaked
}

/// antigravity OAuth client secret (env `ANTIGRAVITY_CLIENT_SECRET`).
pub fn antigravity_client_secret() -> &'static str {
    resolve(
        "ANTIGRAVITY_CLIENT_SECRET",
        "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",
    )
}

/// gemini-cli OAuth client secret (env `GEMINI_CLIENT_SECRET`).
pub fn gemini_cli_client_secret() -> &'static str {
    resolve(
        "GEMINI_CLIENT_SECRET",
        "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",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_and_gemini_secrets_resolve() {
        std::env::remove_var("ANTIGRAVITY_CLIENT_SECRET");
        std::env::remove_var("GEMINI_CLIENT_SECRET");
        assert_eq!(
            antigravity_client_secret(),
            "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"
        );
        assert_eq!(
            gemini_cli_client_secret(),
            "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"
        );
    }
}
