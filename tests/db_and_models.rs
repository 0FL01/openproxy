use std::collections::BTreeMap;
use std::sync::Arc;

use openproxy::core::model::{
    get_model_info, parse_model, resolve_model_alias_from_map, resolve_provider_alias,
    ModelRouteKind,
};
use openproxy::db::Db;
use openproxy::types::{
    ApiKey, AppDb, Combo, ModelAliasTarget, ProviderConnection, ProviderModelRef, ProviderNode,
    Settings,
};
use tempfile::tempdir;

#[test]
fn app_db_round_trips_through_serde() {
    let db = AppDb {
        provider_connections: vec![ProviderConnection {
            id: "conn-1".into(),
            provider: "openai".into(),
            auth_type: "apikey".into(),
            name: Some("Primary".into()),
            priority: Some(1),
            is_active: Some(true),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            display_name: None,
            email: None,
            global_priority: None,
            default_model: Some("gpt-4.1".into()),
            access_token: None,
            refresh_token: None,
            expires_at: None,
            token_type: None,
            scope: None,
            id_token: None,
            project_id: None,
            api_key: Some("sk-test".into()),
            test_status: Some("ok".into()),
            last_tested: None,
            last_error: None,
            last_error_at: None,
            rate_limited_until: None,
            expires_in: None,
            error_code: None,
            consecutive_use_count: Some(0),
            backoff_level: None,
            consecutive_errors: None,
            proxy_url: None,
            proxy_label: None,
            use_connection_proxy: None,
            runtime_transport: None,
            provider_specific_data: BTreeMap::new(),
            extra: BTreeMap::new(),
        }],
        provider_nodes: vec![ProviderNode {
            id: "node-1".into(),
            r#type: "openai-compatible".into(),
            name: "OpenAI Node".into(),
            prefix: Some("custom".into()),
            api_type: Some("openai".into()),
            base_url: Some("https://example.com/v1".into()),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            extra: BTreeMap::new(),
        }],
        combos: vec![Combo {
            id: "combo-1".into(),
            name: "writer".into(),
            models: vec![
                "openai/gpt-4.1".into(),
                "anthropic/claude-sonnet-4-5".into(),
            ],
            disabled_models: Vec::new(),
            kind: Some("chat".into()),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            extra: BTreeMap::new(),
        }],
        model_aliases: BTreeMap::from([
            (
                "fast".into(),
                ModelAliasTarget::Path("openai/gpt-4.1-mini".into()),
            ),
            (
                "precise".into(),
                ModelAliasTarget::Mapping(ProviderModelRef {
                    provider: "anthropic".into(),
                    model: "claude-opus-4-1".into(),
                    extra: BTreeMap::new(),
                }),
            ),
        ]),
        api_keys: vec![ApiKey {
            id: "key-1".into(),
            name: "Local".into(),
            key: "pk-test".into(),
            machine_id: Some("machine-1".into()),
            is_active: Some(true),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            extra: BTreeMap::new(),
        }],
        settings: Settings::default(),
        ..AppDb::default()
    };

    let encoded = serde_json::to_value(&db).expect("encode app db");
    let decoded: AppDb = serde_json::from_value(encoded).expect("decode app db");

    assert_eq!(decoded, db);
}

#[tokio::test]
async fn db_loads_normalizes_and_persists_json_files() {
    let temp = tempdir().expect("tempdir");
    let db_json = temp.path().join("db.json");

    tokio::fs::write(
        &db_json,
        serde_json::to_vec_pretty(&serde_json::json!({
            "providerConnections": [],
            "apiKeys": [{ "id": "k1", "name": "Local", "key": "pk-test" }],
            "settings": { "outboundProxyUrl": "http://127.0.0.1:8080" }
        }))
        .expect("serialize db json"),
    )
    .await
    .expect("write db json");

    let db = Db::load_from(temp.path()).await.expect("load db");
    let snapshot = db.snapshot();

    assert!(snapshot.api_keys[0].is_active());
    assert!(snapshot.settings.outbound_proxy_enabled);
    assert!(db.data_dir.join("openproxy.sqlite").exists());

    db.update(|state| {
        state.model_aliases.insert(
            "draft".into(),
            ModelAliasTarget::Path("openai/gpt-4.1-mini".into()),
        );
    })
    .await
    .expect("update db");

    let reloaded = Db::load_from(temp.path()).await.expect("reload db");
    assert!(reloaded.snapshot().model_aliases.contains_key("draft"));
}

#[tokio::test]
async fn db_updates_are_serialized_and_snapshots_remain_lock_free() {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("load db"));
    let baseline = db.snapshot();

    let mut tasks = Vec::new();
    for index in 0..4 {
        let db = Arc::clone(&db);
        tasks.push(tokio::spawn(async move {
            db.update(|state| {
                state.combos.push(Combo {
                    id: format!("combo-{index}"),
                    name: format!("combo-{index}"),
                    models: vec![format!("openai/gpt-{index}")],
                    disabled_models: Vec::new(),
                    kind: None,
                    created_at: None,
                    updated_at: None,
                    extra: BTreeMap::new(),
                });
            })
            .await
            .expect("serialized update");
        }));
    }

    for task in tasks {
        task.await.expect("task joins");
    }

    assert!(baseline.combos.is_empty());
    assert_eq!(db.snapshot().combos.len(), 4);

    let reloaded = Db::load_from(temp.path()).await.expect("reload db");
    assert_eq!(reloaded.snapshot().combos.len(), 4);

    let temp_files = std::fs::read_dir(temp.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
        .count();
    assert_eq!(temp_files, 0);
}

#[tokio::test]
async fn db_preserves_valid_sections_when_legacy_fields_are_null_or_invalid() {
    let temp = tempdir().expect("tempdir");
    let db_json = temp.path().join("db.json");

    tokio::fs::write(
        &db_json,
        serde_json::to_vec_pretty(&serde_json::json!({
            "providerConnections": [
                {
                    "id": "cookie-1",
                    "provider": "grok-web",
                    "authType": "cookie",
                    "name": "Web",
                    "isActive": true,
                    "providerSpecificData": { "cookie": "session=1" },
                    "unexpectedField": { "keep": true }
                }
            ],
            "providerNodes": null,
            "proxyPools": [
                {
                    "id": "pool-1",
                    "name": "Proxy",
                    "proxyUrl": "http://localhost:8080",
                    "strictProxy": true
                }
            ],
            "modelAliases": { "draft": { "provider": "openai", "model": "gpt-4.1-mini" } },
            "customModels": [{ "providerAlias": "openai", "id": "gpt-custom", "type": "llm", "name": "Custom" }],
            "mitmAlias": { "codex": { "chatgpt-4o-latest": "openai/gpt-4o" } },
            "combos": [{ "id": "combo-1", "name": "writer", "models": ["draft"] }],
            "apiKeys": [{ "id": "k1", "name": "Local", "key": "pk-test", "isActive": null }],
            "settings": { "requireLogin": null, "outboundProxyUrl": "http://127.0.0.1:8080" }
        }))
        .expect("serialize db json"),
    )
    .await
    .expect("write db json");

    let db = Db::load_from(temp.path()).await.expect("load db");
    let snapshot = db.snapshot();
    assert_eq!(snapshot.provider_connections.len(), 1);
    assert_eq!(snapshot.provider_connections[0].auth_type, "cookie");
    assert!(snapshot.provider_connections[0]
        .extra
        .contains_key("unexpectedField"));
    assert!(snapshot.provider_nodes.is_empty());
    assert_eq!(snapshot.proxy_pools.len(), 1);
    assert_eq!(snapshot.custom_models.len(), 1);
    assert_eq!(
        snapshot.mitm_alias["codex"]["chatgpt-4o-latest"],
        "openai/gpt-4o"
    );
    assert!(snapshot.api_keys[0].is_active());
    assert!(snapshot.settings.outbound_proxy_enabled);
}

#[test]
fn model_resolution_supports_aliases_nodes_and_combos() {
    let db = AppDb {
        provider_nodes: vec![ProviderNode {
            id: "node-openai".into(),
            r#type: "openai-compatible".into(),
            name: "Custom".into(),
            prefix: Some("custom".into()),
            api_type: Some("openai".into()),
            base_url: Some("https://example.com/v1".into()),
            created_at: None,
            updated_at: None,
            extra: BTreeMap::new(),
        }],
        model_aliases: BTreeMap::from([(
            "draft".into(),
            ModelAliasTarget::Path("cc/claude-sonnet-4-5".into()),
        )]),
        combos: vec![Combo {
            id: "combo-1".into(),
            name: "writer".into(),
            models: vec!["draft".into(), "openai/gpt-4.1".into()],
            disabled_models: Vec::new(),
            kind: None,
            created_at: None,
            updated_at: None,
            extra: BTreeMap::new(),
        }],
        ..AppDb::default()
    };

    let parsed = parse_model("cc/claude-opus-4-7");
    assert_eq!(parsed.provider.as_deref(), Some("claude"));
    assert_eq!(parsed.provider_alias.as_deref(), Some("cc"));

    assert_eq!(resolve_provider_alias("kr"), "kiro");
    assert_eq!(resolve_provider_alias("custom"), "custom");

    let alias = resolve_model_alias_from_map("draft", &db.model_aliases).expect("resolve alias");
    assert_eq!(alias.provider, "claude");
    assert_eq!(alias.model, "claude-sonnet-4-5");

    let combo = get_model_info("writer", &db);
    assert_eq!(combo.route_kind, ModelRouteKind::Combo);
    assert_eq!(combo.provider, None);

    let explicit_combo = get_model_info("combo:writer", &db);
    assert_eq!(explicit_combo.route_kind, ModelRouteKind::Combo);
    assert_eq!(explicit_combo.model, "writer");

    let compatible = get_model_info("custom/gpt-4.1", &db);
    // JS model.js: provider = matchedOpenAI.id (node id), not display name.
    assert_eq!(compatible.provider.as_deref(), Some("node-openai"));
    assert_eq!(compatible.model, "gpt-4.1");

    let inferred = get_model_info("gpt-4.1-mini", &db);
    assert_eq!(inferred.provider.as_deref(), Some("openai"));

    let unknown_alias = get_model_info("mystery-model", &db);
    assert_eq!(unknown_alias.provider.as_deref(), Some("openai"));

    let empty = parse_model("");
    assert_eq!(empty.provider, None);
    assert_eq!(empty.model, None);

    let missing_model = parse_model("cc/");
    assert_eq!(missing_model.provider.as_deref(), Some("claude"));
    assert_eq!(missing_model.model.as_deref(), Some(""));
}
