//! C39: standalone web tools stay outside the proxy's agent boundary.
//!
//! Inventory result: the only standalone execution family in the core is
//! `src/server/api/web_fetch.rs` (`/v1/web/fetch`). It has confirmed
//! consumers (integration tests, the public CORS contract, the dashboard
//! skills page), so breaking it with a removal or an undeclared 404 is
//! forbidden: retention is the evidence-backed disposition. The guard below
//! pins the exact execution inventory and forbids agent-runner capabilities
//! (tool loops, history/session stores, retry schedulers, background tasks)
//! from appearing in the module. Provider-native `web_search` forwarding,
//! `tool_result`, and `/responses/compact` are transport APIs that must not
//! depend on the convenience route.

use std::path::PathBuf;

fn src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_src(rel: &str) -> String {
    std::fs::read_to_string(src().join(rel)).expect("source must exist")
}

/// Production code only: stops before the unit-test module so test helpers
/// never pollute the boundary inventory.
fn production_code(source: &str) -> &str {
    match source.find("#[cfg(test)]") {
        Some(index) => &source[..index],
        None => source,
    }
}

/// Top-level function names (column 0): skips methods inside `impl` blocks
/// and doc comments that merely name the forbidden capabilities.
fn fn_names(source: &str) -> Vec<String> {
    production_code(source)
        .lines()
        .filter_map(|line| {
            let rest = line
                .strip_prefix("pub async fn ")
                .or_else(|| line.strip_prefix("pub fn "))
                .or_else(|| line.strip_prefix("async fn "))
                .or_else(|| line.strip_prefix("fn "))?;
            rest.split(['(', '<', ' ']).next().map(str::to_string)
        })
        .collect()
}

/// Non-comment, non-test code lines for capability scanning.
fn code_lines(source: &str) -> impl Iterator<Item = &str> {
    production_code(source).lines().filter(|line| {
        let trimmed = line.trim_start();
        !trimmed.starts_with("//!") && !trimmed.starts_with("//")
    })
}

#[test]
fn standalone_execution_inventory_is_exact() {
    let module = read_src("server/api/web_fetch.rs");
    let mut names = fn_names(&module);
    names.sort();
    let mut expected = vec![
        "build_fetch_request",
        "check_private_ip",
        "connection_has_credentials",
        "cors_json_response",
        "cors_options",
        "default_format",
        "do_fetch",
        "execute_single_fetch",
        "fetch_error",
        "handle_web_fetch",
        "is_private_ip",
        "normalize_fetch_response",
        "resolve_fetch_provider",
        "routes",
        "select_fetch_connection",
    ];
    expected.sort();
    assert_eq!(
        names, expected,
        "web_fetch execution inventory changed: add the new helper to the C39 \
         allowlist only with a boundary review proving it is not an agent runner"
    );
}

#[test]
fn no_agent_runner_capabilities_in_web_fetch() {
    let module = read_src("server/api/web_fetch.rs");
    let code: Vec<&str> = code_lines(&module).collect();
    for forbidden in [
        "tokio::spawn",
        "spawn_blocking",
        ".sleep(",
        "sleep(",
        "history",
        "History",
        "session",
        "Session",
        "tool_calls",
        "conversation",
        "max_retries",
        "retry_after",
        "Retry-After",
        "interval(",
    ] {
        assert!(
            !code.iter().any(|line| line.contains(forbidden)),
            "agent-runner marker must not appear in web_fetch: {forbidden}"
        );
    }
    // The only loop is the bounded per-account fallback over configured
    // connections; it must stay free of sleeps/backoff (see above) and of
    // unbounded growth.
    assert!(
        module.contains("excluded.insert"),
        "account fallback exclusion set must stay"
    );
}

#[test]
fn native_tool_paths_do_not_depend_on_web_fetch() {
    // Provider-native web_search forwarding, tool_result handling, and the
    // compact transport API must resolve without the convenience route.
    for rel in [
        "core/translator/request/openai_responses.rs",
        "core/translator/request/claude_format.rs",
        "core/executor/codex.rs",
        "core/executor/grok_cli.rs",
        "server/api/compat.rs",
        "server/api/chat.rs",
    ] {
        let content = read_src(rel);
        assert!(
            !content.contains("web_fetch::") && !content.contains("api::web_fetch"),
            "{rel} must not depend on the web_fetch convenience module"
        );
    }
    // The convenience route itself is mounted exactly once.
    let mounts = read_src("server/api/mod.rs")
        .matches("web_fetch::routes()")
        .count();
    assert_eq!(mounts, 1, "web_fetch routes must be mounted exactly once");
}

#[test]
fn convenience_route_consumers_are_real() {
    // Removal would break a tested public API, so retention (not removal) is
    // the justified disposition. Pin the consumers that prove it.
    let integration = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/web_fetch_api.rs"),
    )
    .expect("web_fetch integration tests must exist");
    assert!(
        integration.matches("async fn ").count() >= 8,
        "web_fetch consumer integration coverage must stay"
    );
    let cors = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/v1_public_cors_api.rs"),
    )
    .expect("CORS contract must exist");
    assert!(
        cors.contains("/v1/web/fetch"),
        "public CORS contract must keep covering the route"
    );
    let skills = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/shared/constants/skills.ts"),
    )
    .expect("skills page constants must exist");
    assert!(
        skills.contains("/v1/web/fetch"),
        "skills discovery must keep advertising the route"
    );
    let v1_root = read_src("server/api/mod.rs");
    assert!(
        v1_root.contains("\"/v1/web/fetch\""),
        "v1 endpoint index must keep listing the route"
    );
}
