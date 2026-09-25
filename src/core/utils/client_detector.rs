//! Detects which CLI client is making the request for the few transport
//! decisions that are genuinely client-specific (for example DeepSeek TUI's
//! stream preference). Protocol passthrough is intentionally decided from the
//! request and upstream formats, never from this identity hint.

use serde_json::Value;
use std::collections::HashMap;

/// Identifier for a recognised CLI client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTool {
    Claude,
    Antigravity,
    Codex,
    GithubCopilot,
    DeepseekTui,
}

impl ClientTool {
    pub fn as_str(self) -> &'static str {
        match self {
            ClientTool::Claude => "claude",
            ClientTool::Antigravity => "antigravity",
            ClientTool::Codex => "codex",
            ClientTool::GithubCopilot => "github-copilot",
            ClientTool::DeepseekTui => "deepseek-tui",
        }
    }
}

/// Detect which CLI tool is making the request.
///
/// Headers must already be lower-cased (callers responsibility — Rust HTTP
/// stacks usually do this). Returns `None` if no recognised client.
pub fn detect_client_tool(headers: &HashMap<String, String>, body: &Value) -> Option<ClientTool> {
    let ua = headers
        .get("user-agent")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let x_app = headers
        .get("x-app")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let openai_intent = headers
        .get("openai-intent")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let initiator = headers
        .get("x-initiator")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    // 9router clientDetector.js:24 originator header (lower-cased), used by the
    // codex branch below to catch codex_work_desktop etc.
    let originator = headers
        .get("originator")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    // Antigravity: detected via body field, not header.
    if body.get("userAgent").and_then(|v| v.as_str()) == Some("antigravity") {
        return Some(ClientTool::Antigravity);
    }

    if ua.contains("githubcopilotchat")
        || openai_intent == "conversation-panel"
        || initiator == "user"
    {
        return Some(ClientTool::GithubCopilot);
    }

    if ua.contains("claude-cli") || ua.contains("claude-code") || x_app == "cli" {
        return Some(ClientTool::Claude);
    }

    // 9router clientDetector.js:43-44 — match codex-tui, codex-cli,
    // codex_cli_rs, "codex desktop", or an originator starting with "codex_"
    // (catches codex_work_desktop etc.). Missing these meant Codex Desktop and
    // codex-tui were translated instead of passed through losslessly.
    if ua.contains("codex-tui")
        || ua.contains("codex-cli")
        || ua.contains("codex_cli_rs")
        || ua.contains("codex desktop")
        || originator.starts_with("codex_")
    {
        return Some(ClientTool::Codex);
    }

    if ua.contains("deepseek-tui") {
        return Some(ClientTool::DeepseekTui);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn h(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn detects_claude_via_user_agent() {
        let headers = h(&[("user-agent", "claude-cli/2.1.282")]);
        assert_eq!(
            detect_client_tool(&headers, &json!({})),
            Some(ClientTool::Claude)
        );
    }

    #[test]
    fn detects_antigravity_via_body() {
        let body = json!({"userAgent": "antigravity"});
        assert_eq!(
            detect_client_tool(&HashMap::new(), &body),
            Some(ClientTool::Antigravity)
        );
    }

    #[test]
    fn detects_copilot_via_intent() {
        let headers = h(&[("openai-intent", "conversation-panel")]);
        assert_eq!(
            detect_client_tool(&headers, &json!({})),
            Some(ClientTool::GithubCopilot)
        );
    }

    #[test]
    fn detects_codex_tui_and_desktop() {
        // 9router clientDetector.js:43-44 — all four UA strings + originator.
        let cases: &[(&str, &str)] = &[
            ("user-agent", "codex-tui/0.5.0"),
            ("user-agent", "codex_cli_rs/0.2.0"),
            ("user-agent", "Codex Desktop"),
            ("user-agent", "codex-cli"),
            ("originator", "codex_work_desktop"),
        ];
        for (k, v) in cases {
            let headers = h(&[(*k, *v)]);
            assert_eq!(
                detect_client_tool(&headers, &json!({})),
                Some(ClientTool::Codex),
                "expected Codex for {k}={v}"
            );
        }
        // A non-codex originator is not misdetected.
        let headers = h(&[("originator", "claude_work_desktop")]);
        assert_ne!(
            detect_client_tool(&headers, &json!({})),
            Some(ClientTool::Codex)
        );
    }
}
