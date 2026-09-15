use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use openproxy::core::combo::{execute_combo, get_combo_models_from_data, ComboAttemptError};
use openproxy::types::Combo;

fn combo(name: &str, models: &[&str]) -> Combo {
    Combo {
        id: format!("{name}-id"),
        name: name.to_string(),
        models: models.iter().map(|value| value.to_string()).collect(),
        disabled_models: Vec::new(),
        kind: None,
        created_at: None,
        updated_at: None,
        extra: BTreeMap::new(),
    }
}

#[test]
fn combo_lookup_returns_configured_order() {
    let combos = vec![combo("writer", &["openai/gpt-4.1", "claude/sonnet"])];

    assert_eq!(
        get_combo_models_from_data("writer", &combos),
        Some(vec!["openai/gpt-4.1".into(), "claude/sonnet".into()])
    );
    assert_eq!(get_combo_models_from_data("openai/gpt-4.1", &combos), None);
}

#[tokio::test]
async fn combo_tries_enabled_members_in_order_until_success() {
    let models = vec!["first".into(), "muted".into(), "second".into()];
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let seen = attempts.clone();

    let result = execute_combo(&models, &["muted".into()], move |model| {
        let model = model.to_string();
        let seen = seen.clone();
        async move {
            seen.lock().unwrap().push(model.clone());
            if model == "second" {
                Ok(model)
            } else {
                Err(ComboAttemptError::new(503, "unavailable"))
            }
        }
    })
    .await;

    assert_eq!(result, Ok("second".to_string()));
    assert_eq!(*attempts.lock().unwrap(), ["first", "second"]);
}
