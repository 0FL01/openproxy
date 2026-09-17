mod common;

use common::lean_harness::TempTestDb;
use openproxy::core::context_limit::{
    advertised_model_limits, default_provider_context_limits, AdvertisedModelLimits,
};
use openproxy::types::AppDb;
use serde_json::json;

#[test]
fn persisted_context_limit_fixtures_keep_documented_migration_semantics() {
    let missing = AppDb::from_json_value(json!({}));
    assert_eq!(
        missing.settings.provider_context_limits,
        default_provider_context_limits()
    );

    let empty = AppDb::from_json_value(json!({
        "settings": {"providerContextLimits": {}}
    }));
    assert_eq!(
        empty.settings.provider_context_limits,
        default_provider_context_limits(),
        "an empty legacy map means defaults, not policy opt-out"
    );

    let explicit = AppDb::from_json_value(json!({
        "settings": {
            "providerContextLimits": {
                "opencode": 410000,
                "glm": 220000,
                "legacy-provider": 123456
            }
        },
        "customModels": [{
            "providerAlias": "glm",
            "id": "custom-context-model",
            "type": "llm",
            "name": "Custom Context Model",
            "opencode": {
                "limit": {"context": 333000, "input": 300000, "output": 16000}
            }
        }]
    }));
    assert_eq!(
        explicit.settings.provider_context_limits["opencode-zen"],
        410_000
    );
    assert!(!explicit
        .settings
        .provider_context_limits
        .contains_key("opencode"));
    assert_eq!(explicit.settings.provider_context_limits["glm"], 220_000);
    assert_eq!(
        explicit.settings.provider_context_limits["legacy-provider"], 123_456,
        "unknown persisted entries must round-trip even though PATCH rejects new ones"
    );
    assert_eq!(
        explicit.custom_models[0].extra["opencode"]["limit"]["context"],
        333_000
    );
}

#[tokio::test]
async fn explicit_and_custom_metadata_survive_sqlite_replace_and_reload() {
    let temp = TempTestDb::new().await;
    let fixture = AppDb::from_json_value(json!({
        "settings": {
            "providerContextLimits": {
                "opencode-zen": 450000,
                "opencode-go": 460000,
                "glm": 230000,
                "codex": 510000
            }
        },
        "customModels": [{
            "providerAlias": "glm",
            "id": "custom-context-model",
            "type": "llm",
            "name": "Custom Context Model",
            "opencode": {
                "limit": {"context": 333000, "input": 300000, "output": 16000},
                "tool_call": true
            }
        }]
    }));

    temp.db
        .replace_app_db(|| fixture.clone())
        .await
        .expect("replace fixture");
    temp.db.reload_snapshot().await.expect("reload fixture");
    let restored = temp.db.snapshot();

    assert_eq!(
        restored.settings.provider_context_limits,
        fixture.settings.provider_context_limits
    );
    assert_eq!(restored.custom_models, fixture.custom_models);

    let glm = advertised_model_limits(
        "glm",
        restored.settings.provider_context_limits["glm"],
        Some(333_000),
        Some(300_000),
        Some(16_000),
    );
    assert_eq!(
        glm,
        AdvertisedModelLimits {
            context: 230_000,
            input: Some(230_000),
            output: Some(16_000),
        }
    );
    let codex = advertised_model_limits(
        "codex",
        restored.settings.provider_context_limits["codex"],
        Some(272_000),
        None,
        None,
    );
    assert_eq!(
        codex,
        AdvertisedModelLimits {
            context: 510_000,
            input: Some(460_000),
            output: Some(128_000),
        }
    );
}
