//! Canonical Antigravity project metadata helpers.
//!
//! Project ids belong to the configured [`ProviderConnection`].  These
//! helpers are deliberately pure: generation must never discover or cache a
//! project id, and setup/control-plane discovery publishes validated values
//! through the normal database snapshot.

use serde_json::Value;

use crate::types::ProviderConnection;

/// Read the project id from canonical connection metadata.
///
/// The typed field is authoritative.  The provider-specific fallbacks remain
/// readable for non-destructive compatibility with imported legacy data.
pub fn antigravity_project_id(connection: &ProviderConnection) -> Option<String> {
    trimmed(connection.project_id.as_deref())
        .or_else(|| provider_string(connection, "projectId"))
        .or_else(|| provider_string(connection, "project"))
}

fn provider_string(connection: &ProviderConnection, key: &str) -> Option<String> {
    trimmed(
        connection
            .provider_specific_data
            .get(key)
            .and_then(Value::as_str),
    )
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Extract a validated Google Cloud project id from a `loadCodeAssist`
/// response.  Both response shapes observed in existing connection flows are
/// accepted; empty ids are rejected.
pub fn extract_google_project_id(payload: &Value) -> Option<String> {
    let project = payload.get("cloudaicompanionProject")?;
    trimmed(
        project
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| project.as_str()),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    #[test]
    fn canonical_field_precedes_preserved_legacy_values() {
        let connection = ProviderConnection {
            project_id: Some(" canonical ".into()),
            provider_specific_data: BTreeMap::from([
                ("projectId".into(), json!("legacy-id")),
                ("project".into(), json!("legacy-project")),
                ("unknown".into(), json!({"preserved": true})),
            ]),
            ..Default::default()
        };

        assert_eq!(
            antigravity_project_id(&connection).as_deref(),
            Some("canonical")
        );
        assert_eq!(
            connection.provider_specific_data.get("unknown"),
            Some(&json!({"preserved": true}))
        );
    }

    #[test]
    fn legacy_values_remain_readable_without_state() {
        let project_id = ProviderConnection {
            provider_specific_data: BTreeMap::from([("projectId".into(), json!(" p-id "))]),
            ..Default::default()
        };
        let project = ProviderConnection {
            provider_specific_data: BTreeMap::from([("project".into(), json!(" p-short "))]),
            ..Default::default()
        };

        assert_eq!(antigravity_project_id(&project_id).as_deref(), Some("p-id"));
        assert_eq!(antigravity_project_id(&project).as_deref(), Some("p-short"));
        assert!(antigravity_project_id(&ProviderConnection::default()).is_none());
    }

    #[test]
    fn extracts_supported_load_code_assist_shapes() {
        assert_eq!(
            extract_google_project_id(&json!({
                "cloudaicompanionProject": {"id": "projects/nested"}
            }))
            .as_deref(),
            Some("projects/nested")
        );
        assert_eq!(
            extract_google_project_id(&json!({
                "cloudaicompanionProject": " projects/flat "
            }))
            .as_deref(),
            Some("projects/flat")
        );
        assert!(extract_google_project_id(&json!({})).is_none());
        assert!(extract_google_project_id(&json!({
            "cloudaicompanionProject": {"id": " "}
        }))
        .is_none());
    }
}
