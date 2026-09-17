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
    assert_eq!(
        manifest["refresh_coordination"]["prepared_checkpoint"],
        "C16"
    );
    assert_eq!(manifest["refresh_coordination"]["full_token_in_key"], false);
    assert_eq!(
        manifest["refresh_coordination"]["new_coordinator_completed_result_cache"],
        false
    );
    assert_eq!(
        manifest["refresh_coordination"]["waiter_cancellation_aborts_operation"],
        false
    );
    assert_eq!(
        manifest["refresh_coordination"]["legacy_token_cache_removed_in"],
        "C18"
    );
    assert_eq!(
        manifest["refresh_coordination"]["foreground_migrated_checkpoint"],
        "C17A"
    );
    assert_eq!(
        manifest["refresh_coordination"]["foreground_owner"],
        "connection_refresh_coordinator"
    );
    assert_eq!(
        manifest["refresh_coordination"]["foreground_direct_dispatch_calls"],
        false
    );
    assert_eq!(
        manifest["refresh_coordination"]["non_token_forbidden_triggers_refresh"],
        false
    );
    assert_eq!(
        manifest["refresh_coordination"]["control_background_migrated_checkpoint"],
        "C17B"
    );
    assert_eq!(
        manifest["refresh_coordination"]["control_background_direct_dispatch_calls"],
        false
    );
    assert_eq!(
        manifest["refresh_coordination"]["stale_background_result_can_overwrite_newer_generation"],
        false
    );
    assert_eq!(
        manifest["refresh_coordination"]["idle_coordinator_entries"],
        0
    );
    assert_eq!(
        manifest["refresh_coordination"]["legacy_token_cache_present"],
        false
    );
    assert_eq!(
        manifest["refresh_coordination"]["legacy_token_cache_removed_checkpoint"],
        "C18"
    );
    assert_eq!(
        manifest["refresh_coordination"]["completed_refresh_result_cache"],
        false
    );
    assert_eq!(
        manifest["refresh_coordination"]["historical_rotation_entries_retained"],
        0
    );
    assert_eq!(
        manifest["model_catalog_publication"]["generation_remote_http"],
        false
    );
    assert_eq!(
        manifest["model_catalog_publication"]["generation_refresh_lock_wait"],
        false
    );
    assert_eq!(
        manifest["model_catalog_publication"]["failed_refresh_replaces_snapshot"],
        false
    );
    assert_eq!(
        manifest["model_catalog_publication"]["canonical_opencode_source_preserved"],
        true
    );
    assert_eq!(
        manifest["codex_catalog_publication"]["generation_remote_http"],
        false
    );
    assert_eq!(
        manifest["codex_catalog_publication"]["generation_refresh_lock_wait"],
        false
    );
    assert_eq!(
        manifest["codex_catalog_publication"]["unknown_cold_model_routes_arbitrary_account"],
        false
    );
    assert_eq!(
        manifest["codex_catalog_publication"]["union_rebuilt_per_incoming_request"],
        false
    );
}
