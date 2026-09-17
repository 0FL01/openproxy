use openproxy::core::translator::registry::{get_target_format_for_provider, Format};
use serde_json::Value;
use std::path::Path;

fn manifest() -> Value {
    serde_json::from_str(include_str!("../contracts/lean-proxy.json"))
        .expect("lean proxy contract must be valid JSON")
}

#[test]
fn contract_has_one_owner_for_each_lean_boundary() {
    let manifest = manifest();
    let owners = manifest["owners"]
        .as_object()
        .expect("owners must be an object");

    assert_eq!(owners.len(), 2);
    assert_eq!(owners["harness"].as_array().map(Vec::len), Some(5));
    assert_eq!(owners["proxy"].as_array().map(Vec::len), Some(5));

    let mut concerns = owners
        .values()
        .flat_map(|owner| owner.as_array().into_iter().flatten())
        .map(|concern| concern.as_str().expect("concerns must be strings"))
        .collect::<Vec<_>>();
    let original_len = concerns.len();
    concerns.sort_unstable();
    concerns.dedup();
    assert_eq!(concerns.len(), original_len, "a concern has two owners");
}

#[test]
fn contract_provider_formats_match_the_runtime_registry() {
    let cases = [
        ("openai", Format::OpenAi),
        ("anthropic", Format::Claude),
        ("glm", Format::OpenAi),
        ("gemini", Format::Gemini),
        ("vertex", Format::Vertex),
        ("codex", Format::OpenAiResponses),
        ("cursor", Format::Cursor),
        ("kiro", Format::Kiro),
        ("ollama", Format::Ollama),
        ("antigravity", Format::Antigravity),
        ("commandcode", Format::CommandCode),
        ("openai-compatible-acme", Format::OpenAi),
        ("openai-compatible-responses-acme", Format::OpenAiResponses),
        ("anthropic-compatible-acme", Format::Claude),
        ("default-provider", Format::OpenAi),
    ];

    for (provider, expected) in cases {
        assert_eq!(
            get_target_format_for_provider(provider),
            expected,
            "{provider}"
        );
    }
}

#[test]
fn contract_references_existing_authoritative_sources() {
    let manifest = manifest();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sources = manifest["source_registries"]
        .as_array()
        .expect("source_registries must be an array");

    for source in sources {
        let source = source.as_str().expect("source entry must be a string");
        let path = source.split(':').next().expect("source path");
        assert!(root.join(path).exists(), "missing contract source: {path}");
    }
}

#[test]
fn contract_freezes_preservation_and_removal_lists() {
    let manifest = manifest();
    let preserved = manifest["preserved_contracts"]
        .as_array()
        .expect("preserved_contracts must be an array");
    let removed = manifest["automatic_actions_to_remove"]
        .as_array()
        .expect("automatic_actions_to_remove must be an array");

    assert!(preserved.len() >= 10);
    assert!(removed.len() >= 10);
    assert_eq!(
        manifest["legacy_data_policy"]["destructive_read_migration"],
        false
    );
    assert_eq!(
        manifest["legacy_data_policy"]["silent_reinterpretation"],
        false
    );
    assert_eq!(manifest["versions"]["custom_harness"], "unknown");
    assert_eq!(
        manifest["context_limit_transition"]["context_management_owner"],
        "client"
    );
    assert_eq!(
        manifest["context_limit_transition"]["proxy_memory_limit_unit"],
        "bytes"
    );
    assert_eq!(
        manifest["context_limit_transition"]["codex_values_are_verified_upstream_limits"],
        false
    );
    assert_eq!(
        manifest["generation_retry_policy"]["codex_executor_same_request_retries"],
        false
    );
    assert_eq!(
        manifest["generation_retry_policy"]["codex_first_event_preflight_max_inspected_bytes"],
        65_536
    );
    assert_eq!(
        manifest["generation_retry_policy"]["codex_retries_after_downstream_commitment"],
        false
    );
    assert_eq!(
        manifest["generation_retry_policy"]["antigravity_executor_same_request_retries"],
        false
    );
    assert_eq!(
        manifest["generation_retry_policy"]["auth_recovery_owner"],
        "request_scoped_planner"
    );
    assert_eq!(
        manifest["generation_retry_policy"]["auth_recovery_attempts_per_incoming_request"],
        1
    );
    assert_eq!(
        manifest["generation_retry_policy"]["cross_request_cooldown_routing"],
        false
    );
}
