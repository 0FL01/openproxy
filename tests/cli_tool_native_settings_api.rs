#![allow(clippy::await_holding_lock)]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use once_cell::sync::Lazy;
use openproxy::db::Db;
use openproxy::server::state::AppState;
use openproxy::types::ApiKey;
use serde_json::json;
use tempfile::tempdir;
use tower::util::ServiceExt;

static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

fn active_key(key: &str) -> ApiKey {
    ApiKey {
        id: format!("{key}-id"),
        name: "Local".into(),
        key: key.into(),
        machine_id: None,
        is_active: Some(true),
        created_at: None,
        extra: BTreeMap::new(),
    }
}

async fn app_state() -> AppState {
    let temp = tempdir().expect("tempdir");
    let db = Arc::new(Db::load_from(temp.path()).await.expect("db"));
    db.update(|state| {
        state.api_keys = vec![active_key("valid-bearer")];
    })
    .await
    .expect("seed db");
    AppState::new(db)
}

fn authorized_request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer valid-bearer")
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

struct EnvVarGuard {
    key: &'static str,
    old_value: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let old_value = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, old_value }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(value) = self.old_value.take() {
            unsafe { std::env::set_var(self.key, value) };
        } else {
            unsafe { std::env::remove_var(self.key) };
        }
    }
}

fn claude_settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

fn codex_config_path(home: &Path) -> PathBuf {
    home.join(".codex").join("config.toml")
}

fn codex_auth_path(home: &Path) -> PathBuf {
    home.join(".codex").join("auth.json")
}

fn opencode_config_path(home: &Path) -> PathBuf {
    home.join(".config").join("opencode").join("opencode.jsonc")
}

#[tokio::test]
async fn deepseek_tui_settings_use_openproxy_key_by_default() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/deepseek-tui-settings",
            Body::from(
                serde_json::to_vec(&json!({
                    "baseUrl": "http://127.0.0.1:4623",
                    "model": "deepseek-chat",
                }))
                .unwrap(),
            ),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let config = tokio::fs::read_to_string(home.path().join(".deepseek/config.toml"))
        .await
        .unwrap();
    assert!(config.contains("api_key = \"sk_openproxy\""));
    assert!(!config.contains("sk_9router"));
}

#[tokio::test]
async fn claude_settings_get_reports_not_installed_without_binary_or_config() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/claude-settings",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "installed": false,
            "settings": null,
            "message": "Claude CLI is not installed"
        })
    );
}

#[tokio::test]
async fn claude_settings_post_get_and_delete_match_openproxy_behavior() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let settings_path = claude_settings_path(home.path());
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&json!({
            "foo": "bar",
            "env": {
                "KEEP": "1",
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "old-opus"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let app = openproxy::build_app(app_state().await);
    let post = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/claude-settings",
            Body::from(
                r#"{"env":{"ANTHROPIC_BASE_URL":"https://proxy.example.com","ANTHROPIC_AUTH_TOKEN":"token-123","OTHER":"value"}}"#,
            ),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(post).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "Settings updated successfully"
        })
    );

    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(saved["hasCompletedOnboarding"], true);
    assert_eq!(saved["foo"], "bar");
    assert_eq!(saved["env"]["KEEP"], "1");
    assert_eq!(saved["env"]["OTHER"], "value");
    assert_eq!(
        saved["env"]["ANTHROPIC_BASE_URL"],
        "https://proxy.example.com/v1"
    );

    let get = app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/claude-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(get).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["hasOpenProxy"], true);
    assert_eq!(
        json["settingsPath"],
        settings_path.to_string_lossy().to_string()
    );

    let delete = app
        .clone()
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/cli-tools/claude-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(delete).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "Settings reset successfully"
        })
    );

    let reset: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(reset["env"]["KEEP"], "1");
    assert_eq!(reset["env"]["OTHER"], "value");
    assert!(reset["env"].get("ANTHROPIC_BASE_URL").is_none());
    assert!(reset["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
    assert!(reset["env"].get("ANTHROPIC_DEFAULT_OPUS_MODEL").is_none());
}

#[tokio::test]
async fn codex_settings_get_reports_not_installed_without_binary_or_config() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/codex-settings",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "installed": false,
            "config": null,
            "message": "Codex CLI is not installed"
        })
    );
}

#[tokio::test]
async fn codex_settings_post_get_and_delete_match_openproxy_file_behavior() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let config_path = codex_config_path(home.path());
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        "[existing]\nvalue = \"keep\"\nmodel = \"other\"\n",
    )
    .unwrap();
    std::fs::write(
        codex_auth_path(home.path()),
        serde_json::to_vec_pretty(&json!({
            "refresh_token": "keep-me",
            "auth_mode": "chatgpt"
        }))
        .unwrap(),
    )
    .unwrap();

    let app = openproxy::build_app(app_state().await);
    let post = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/codex-settings",
            Body::from(
                r#"{"baseUrl":"https://proxy.example.com","apiKey":"sk-openproxy","model":"oa/gpt-4.1","subagentModel":"oa/gpt-4.1-mini"}"#,
            ),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(post).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "Codex settings applied successfully!",
            "configPath": config_path.to_string_lossy().to_string()
        })
    );

    let saved_config = std::fs::read_to_string(&config_path).unwrap();
    assert!(saved_config.contains("model = \"oa/gpt-4.1\""));
    assert!(saved_config.contains("model_provider = \"openproxy\""));
    assert!(saved_config.contains("[model_providers.openproxy]"));
    assert!(saved_config.contains("base_url = \"https://proxy.example.com/v1\""));
    assert!(saved_config.contains("wire_api = \"responses\""));
    assert!(saved_config.contains("[agents.subagent]"));
    assert!(saved_config.contains("model = \"oa/gpt-4.1-mini\""));
    assert!(saved_config.contains("[existing]"));

    let saved_auth: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(codex_auth_path(home.path())).unwrap())
            .unwrap();
    assert_eq!(saved_auth["OPENAI_API_KEY"], "sk-openproxy");
    assert_eq!(saved_auth["auth_mode"], "apikey");
    assert_eq!(saved_auth["refresh_token"], "keep-me");

    let get = app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/codex-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(get).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["hasOpenProxy"], true);
    assert_eq!(
        json["configPath"],
        config_path.to_string_lossy().to_string()
    );
    assert_eq!(json["config"], saved_config);

    let delete = app
        .clone()
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/cli-tools/codex-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(delete).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "OpenProxy settings removed successfully"
        })
    );

    let reset_config = std::fs::read_to_string(&config_path).unwrap();
    assert!(!reset_config.contains("model_provider = \"openproxy\""));
    assert!(!reset_config.contains("[model_providers.openproxy]"));
    assert!(!reset_config.contains("[agents.subagent]"));
    assert!(reset_config.contains("[existing]"));

    let reset_auth: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(codex_auth_path(home.path())).unwrap())
            .unwrap();
    assert!(reset_auth.get("OPENAI_API_KEY").is_none());
    assert!(reset_auth.get("auth_mode").is_none());
    assert_eq!(reset_auth["refresh_token"], "keep-me");
}

#[tokio::test]
async fn opencode_settings_get_reports_not_installed_without_binary_or_config() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let app = openproxy::build_app(app_state().await);
    let response = app
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/opencode-settings",
            Body::empty(),
        ))
        .await
        .unwrap();

    let (status, json) = response_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "installed": false,
            "config": null,
            "message": "OpenCode CLI is not installed"
        })
    );
}

#[tokio::test]
async fn opencode_settings_post_patch_and_delete_match_openproxy_file_behavior() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempdir().unwrap();
    let path = tempdir().unwrap();
    let _home = EnvVarGuard::set_path("HOME", home.path());
    let _path = EnvVarGuard::set_path("PATH", path.path());

    let config_path = opencode_config_path(home.path());
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(
        &config_path,
        serde_json::to_vec_pretty(&json!({
            "provider": {
                "other": { "keep": true },
                "openproxy": {
                    "npm": "@ai-sdk/openai-compatible",
                    "options": {
                        "region": "keep",
                        "baseURL": "https://old.example.com/v1",
                        "apiKey": "old-key",
                        "headers": {
                            "X-Keep": "yes"
                        }
                    },
                    "models": {
                        "old/model": { "name": "old/model" },
                        "oa/gpt-4.1": {
                            "name": "Custom label",
                            "options": {"keep": true}
                        }
                    }
                }
            },
            "model": "other/model",
            "mcp": {
                "other": {"keep": true},
                "codex_web": {"type": "remote", "url": "https://old.example/mcp"}
            },
            "agent": {
                "keep": { "still": true },
                "explorer": {
                    "description": "legacy",
                    "mode": "subagent",
                    "model": "other/model"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let app = openproxy::build_app(app_state().await);
    let missing_key = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/opencode-settings",
            Body::from(r#"{"baseUrl":"https://proxy.example.com","models":["oa/gpt-4.1"]}"#),
        ))
        .await
        .unwrap();
    assert_eq!(missing_key.status(), StatusCode::BAD_REQUEST);

    let post = app
        .clone()
        .oneshot(authorized_request(
            Method::POST,
            "/api/cli-tools/opencode-settings",
            Body::from(
                r#"{"baseUrl":"https://proxy.example.com","apiKey":"sk-openproxy","models":["oa/gpt-4.1","oa/gpt-4.1-mini"],"activeModel":"oa/gpt-4.1-mini","subagentModel":"oa/gpt-4.1-nano"}"#,
            ),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(post).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "OpenCode settings applied successfully!",
            "configPath": config_path.to_string_lossy().to_string()
        })
    );

    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(saved["provider"]["other"]["keep"], true);
    assert_eq!(
        saved["provider"]["openproxy"]["npm"],
        "@ai-sdk/openai-compatible"
    );
    assert_eq!(saved["provider"]["openproxy"]["options"]["region"], "keep");
    assert_eq!(
        saved["provider"]["openproxy"]["options"]["baseURL"],
        "https://proxy.example.com/v1"
    );
    assert_eq!(
        saved["provider"]["openproxy"]["options"]["apiKey"],
        "sk-openproxy"
    );
    assert_eq!(
        saved["provider"]["openproxy"]["options"]["headers"]["X-Keep"],
        "yes"
    );
    assert_eq!(saved["mcp"]["other"]["keep"], true);
    assert_eq!(saved["mcp"]["codex_web"]["type"], "remote");
    assert_eq!(
        saved["mcp"]["codex_web"]["url"],
        "https://proxy.example.com/v1/mcp"
    );
    assert_eq!(saved["mcp"]["codex_web"]["enabled"], true);
    assert_eq!(saved["mcp"]["codex_web"]["oauth"], false);
    assert_eq!(saved["mcp"]["codex_web"]["timeout"], 30000);
    assert_eq!(
        saved["mcp"]["codex_web"]["headers"]["Authorization"],
        "Bearer sk-openproxy"
    );
    assert_eq!(
        saved["provider"]["openproxy"]["models"]["old/model"]["name"],
        "old/model"
    );
    assert_eq!(
        saved["provider"]["openproxy"]["models"]["oa/gpt-4.1"]["name"],
        "Custom label"
    );
    assert_eq!(
        saved["provider"]["openproxy"]["models"]["oa/gpt-4.1"]["options"]["keep"],
        true
    );
    assert_eq!(
        saved["provider"]["openproxy"]["models"]["oa/gpt-4.1-mini"]["name"],
        "oa/gpt-4.1-mini"
    );
    assert_eq!(saved["model"], "openproxy/oa/gpt-4.1-mini");
    assert_eq!(saved["agent"]["keep"]["still"], true);
    assert_eq!(
        saved["agent"]["explorer"]["model"],
        "openproxy/oa/gpt-4.1-nano"
    );

    let get = app
        .clone()
        .oneshot(authorized_request(
            Method::GET,
            "/api/cli-tools/opencode-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(get).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["installed"], true);
    assert_eq!(json["hasOpenProxy"], true);
    assert_eq!(json["config"], saved);
    assert_eq!(
        json["configPath"],
        config_path.to_string_lossy().to_string()
    );
    let models = json["opencode"]["models"].as_array().unwrap();
    assert_eq!(models.len(), 3);
    assert!(models.contains(&json!("old/model")));
    assert!(models.contains(&json!("oa/gpt-4.1")));
    assert!(models.contains(&json!("oa/gpt-4.1-mini")));
    assert_eq!(json["opencode"]["activeModel"], "oa/gpt-4.1-mini");
    assert_eq!(json["opencode"]["baseURL"], "https://proxy.example.com/v1");
    assert_eq!(json["opencode"]["mcpConfigured"], true);

    let patch = app
        .clone()
        .oneshot(authorized_request(
            Method::PATCH,
            "/api/cli-tools/opencode-settings",
            Body::from(r#"{"clearActiveModel":true}"#),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(patch).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "Settings updated"
        })
    );

    let patched: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(patched["model"], "");

    let delete_one = app
        .clone()
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/cli-tools/opencode-settings?model=oa/gpt-4.1",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(delete_one).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "Model \"oa/gpt-4.1\" removed"
        })
    );

    let deleted_one: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert!(deleted_one["provider"]["openproxy"]["models"]
        .get("oa/gpt-4.1")
        .is_none());
    assert!(deleted_one["provider"]["openproxy"]["models"]
        .get("old/model")
        .is_some());
    assert!(deleted_one["provider"]["openproxy"]["models"]
        .get("oa/gpt-4.1-mini")
        .is_some());
    assert!(deleted_one["agent"].get("explorer").is_none());
    assert_eq!(deleted_one["agent"]["keep"]["still"], true);
    assert_eq!(deleted_one["model"], "");
    assert_eq!(
        deleted_one["mcp"]["codex_web"]["url"],
        "https://proxy.example.com/v1/mcp"
    );

    let delete_all = app
        .clone()
        .oneshot(authorized_request(
            Method::DELETE,
            "/api/cli-tools/opencode-settings",
            Body::empty(),
        ))
        .await
        .unwrap();
    let (status, json) = response_json(delete_all).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json,
        json!({
            "success": true,
            "message": "OpenProxy settings removed from OpenCode"
        })
    );

    let reset: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert!(reset["provider"].get("openproxy").is_none());
    assert_eq!(reset["provider"]["other"]["keep"], true);
    assert_eq!(reset["agent"]["keep"]["still"], true);
    assert_eq!(reset["model"], "");
    assert!(reset["mcp"].get("codex_web").is_none());
    assert_eq!(reset["mcp"]["other"]["keep"], true);
}
