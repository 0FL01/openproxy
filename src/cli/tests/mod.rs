pub mod cli_tool_tests;

#[test]
fn direct_route_ignores_legacy_cooldown_fields() {
    use std::collections::HashSet;
    use std::sync::Arc;

    use chrono::{Duration, Utc};
    use serde_json::Value;

    use super::select_connection_cli;
    use crate::types::{AppDb, ProviderConnection};

    let until = (Utc::now() + Duration::hours(1)).to_rfc3339();
    let mut preferred = ProviderConnection {
        id: "preferred".into(),
        provider: "openai".into(),
        auth_type: "apikey".into(),
        api_key: Some("test-key".into()),
        is_active: Some(true),
        priority: Some(1),
        default_model: Some("gpt-4.1".into()),
        rate_limited_until: Some(until.clone()),
        ..ProviderConnection::default()
    };
    preferred
        .extra
        .insert("modelLock_gpt-4.1".into(), Value::String(until.clone()));
    preferred
        .extra
        .insert("degradedUntil".into(), Value::String(until));

    let snapshot = Arc::new(AppDb {
        provider_connections: vec![preferred],
        ..AppDb::default()
    });
    let selected = select_connection_cli(&snapshot, "openai", "gpt-4.1", &HashSet::new())
        .expect("legacy cooldown fields must not suppress direct CLI routing");

    assert_eq!(selected.id, "preferred");
}
