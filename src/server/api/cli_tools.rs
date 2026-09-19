mod claude_settings;
mod cline_settings;
mod deepseek_tui_settings;

use std::env;
use std::path::{Path as FsPath, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    response::Response,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{fs, process::Command};
use toml::{map::Map as TomlMap, Value as TomlValue};

use crate::server::state::AppState;

const MAX_OUTPUT_SIZE: usize = 64 * 1024; // 64KB max output

/// CLI command execution request
#[derive(Debug, Deserialize)]
pub struct CliCommandRequest {
    pub command: String,
    pub args: Option<Vec<String>>,
    pub timeout_secs: Option<u64>,
}

/// CLI command execution response
#[derive(Debug, Serialize)]
pub struct CliCommandResponse {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub timed_out: bool,
}

/// List available CLI tools response
#[derive(Debug, Serialize)]
pub struct CliToolsListResponse {
    pub tools: Vec<CliToolInfo>,
}

/// Information about a CLI tool
#[derive(Debug, Serialize)]
pub struct CliToolInfo {
    pub name: String,
    pub description: String,
    pub category: String,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// GET /api/cli-tools
/// List available CLI tools
pub async fn list_tools(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let tools = vec![
        CliToolInfo {
            name: "provider-list".to_string(),
            description: "List all provider connections and nodes".to_string(),
            category: "provider".to_string(),
        },
        CliToolInfo {
            name: "key-list".to_string(),
            description: "List all API keys".to_string(),
            category: "key".to_string(),
        },
        CliToolInfo {
            name: "pool-list".to_string(),
            description: "List all proxy pools".to_string(),
            category: "pool".to_string(),
        },
        CliToolInfo {
            name: "pool-status".to_string(),
            description: "Get status of a specific proxy pool".to_string(),
            category: "pool".to_string(),
        },
        CliToolInfo {
            name: "route".to_string(),
            description: "Execute a model routing request directly".to_string(),
            category: "route".to_string(),
        },
    ];

    Json(CliToolsListResponse { tools }).into_response()
}

/// POST /api/cli-tools/execute
/// Execute a CLI command
pub async fn execute_command(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CliCommandRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let timeout_secs = req.timeout_secs.unwrap_or(30).min(120); // Max 2 minutes
    let start_time = std::time::Instant::now();

    // Parse and validate command
    let (program, args) = match parse_cli_command(&req.command, req.args.as_deref()) {
        Some(cmd) => cmd,
        None => {
            return Json(CliCommandResponse {
                success: false,
                exit_code: Some(1),
                stdout: String::new(),
                stderr: "Invalid command".to_string(),
                duration_ms: start_time.elapsed().as_millis() as u64,
                timed_out: false,
            })
            .into_response()
        }
    };

    // Execute the command with a hard timeout so callers can't hang the request.
    let response = run_command_with_timeout(&program, &args, timeout_secs).await;
    let duration_ms = start_time.elapsed().as_millis() as u64;
    Json(CliCommandResponse {
        duration_ms,
        ..response
    })
    .into_response()
}

/// POST /api/cli-tools/run
/// Run a specific CLI tool by name (higher-level interface)
pub async fn run_tool(
    State(state): State<AppState>,
    Path(tool_name): Path<String>,
    headers: HeaderMap,
    Json(req): Json<CliCommandRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let timeout_secs = req.timeout_secs.unwrap_or(30).min(120);
    let start_time = std::time::Instant::now();

    let (program, args) = build_tool_command(&tool_name, req.args.unwrap_or_default());

    let response = run_command_with_timeout(&program, &args, timeout_secs).await;
    let duration_ms = start_time.elapsed().as_millis() as u64;

    Json(CliCommandResponse {
        duration_ms,
        ..response
    })
    .into_response()
}

/// Run a child process with a hard timeout. Returns a `CliCommandResponse`
/// (with `duration_ms` set to 0 so the caller can fill it in once they've
/// measured wall-clock time from before parsing).
async fn run_command_with_timeout(
    program: &str,
    args: &[String],
    timeout_secs: u64,
) -> CliCommandResponse {
    let child = match tokio::process::Command::new(program)
        .args(args)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return CliCommandResponse {
                success: false,
                exit_code: Some(-1),
                stdout: String::new(),
                stderr: format!("Failed to execute command: {}", e),
                duration_ms: 0,
                timed_out: false,
            };
        }
    };

    let wait_fut = child.wait_with_output();
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), wait_fut).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            CliCommandResponse {
                success: output.status.success(),
                exit_code: output.status.code(),
                stdout,
                stderr,
                duration_ms: 0,
                timed_out: false,
            }
        }
        Ok(Err(e)) => CliCommandResponse {
            success: false,
            exit_code: Some(-1),
            stdout: String::new(),
            stderr: format!("Failed to execute command: {}", e),
            duration_ms: 0,
            timed_out: false,
        },
        Err(_) => CliCommandResponse {
            success: false,
            exit_code: None,
            stdout: String::new(),
            stderr: format!("Command timed out after {}s", timeout_secs),
            duration_ms: 0,
            timed_out: true,
        },
    }
}

/// Parse a command string into program and arguments
fn parse_cli_command(
    command: &str,
    additional_args: Option<&[String]>,
) -> Option<(String, Vec<String>)> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    let program = parts[0].to_string();
    let mut args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();

    if let Some(extra) = additional_args {
        args.extend(extra.iter().cloned());
    }

    Some((program, args))
}

/// Build a command for a specific tool
fn build_tool_command(tool_name: &str, args: Vec<String>) -> (String, Vec<String>) {
    // Map tool names to actual commands
    match tool_name {
        "provider-list" => (
            "openproxy".to_string(),
            vec![
                "provider".to_string(),
                "list".to_string(),
                "--json".to_string(),
            ],
        ),
        "key-list" => (
            "openproxy".to_string(),
            vec!["key".to_string(), "list".to_string(), "--json".to_string()],
        ),
        "pool-list" => (
            "openproxy".to_string(),
            vec!["pool".to_string(), "list".to_string(), "--json".to_string()],
        ),
        "pool-status" => {
            let pool_name = args.first().cloned().unwrap_or_default();
            (
                "openproxy".to_string(),
                vec![
                    "pool".to_string(),
                    "status".to_string(),
                    "--name".to_string(),
                    pool_name,
                    "--json".to_string(),
                ],
            )
        }
        _ => (tool_name.to_string(), args),
    }
}

/// GET /api/cli-tools/help
/// Get help information for CLI tools
pub async fn get_help(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    Json(json!({
        "help": "CLI Tools API",
        "endpoints": {
            "GET /api/cli-tools": "List available CLI tools",
            "POST /api/cli-tools/execute": "Execute arbitrary command",
            "POST /api/cli-tools/run/{tool_name}": "Run a specific tool",
            "GET /api/cli-tools/help": "Show this help"
        },
        "tools": [
            {"name": "provider-list", "description": "List provider connections"},
            {"name": "key-list", "description": "List API keys"},
            {"name": "pool-list", "description": "List proxy pools"},
            {"name": "pool-status", "description": "Get pool status (args: [pool_name])"}
        ]
    }))
    .into_response()
}

// ═══════════════════════════════════════════════════════════════════════════
// Codex CLI Settings Endpoints
// GET/POST/DELETE /api/cli-tools/codex-settings
// ═══════════════════════════════════════════════════════════════════════════

/// Codex CLI settings stored per user
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexSettings {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub subagent_model: Option<String>,
}

/// GET /api/cli-tools/codex-settings
/// Get Codex CLI settings
async fn get_codex_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_codex_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "config": Value::Null,
            "message": "Codex CLI is not installed",
        }))
        .into_response();
    }

    match read_codex_config().await {
        Ok(config) => {
            let has_openproxy = config.as_deref().is_some_and(has_openproxy_codex_config);
            Json(json!({
                "installed": true,
                "config": config,
                "hasOpenProxy": has_openproxy,
                "configPath": codex_config_path().to_string_lossy().to_string(),
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to check codex settings: {error}") })),
        )
            .into_response(),
    }
}

/// POST /api/cli-tools/codex-settings
/// Save Codex CLI settings
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexSettingsRequest {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub subagent_model: Option<String>,
}

async fn save_codex_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CodexSettingsRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match write_codex_settings(&CodexSettings {
        base_url: Some(req.base_url),
        api_key: Some(req.api_key),
        model: Some(req.model),
        subagent_model: req.subagent_model,
    })
    .await
    {
        Ok(config_path) => Json(json!({
            "success": true,
            "message": "Codex settings applied successfully!",
            "configPath": config_path,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to update codex settings: {error}") })),
        )
            .into_response(),
    }
}

/// DELETE /api/cli-tools/codex-settings
/// Reset Codex CLI settings
async fn delete_codex_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_codex_settings().await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to reset codex settings: {error}") })),
        )
            .into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// OpenCode Settings Endpoints
// GET/POST/PATCH/DELETE /api/cli-tools/opencode-settings
// ═══════════════════════════════════════════════════════════════════════════

const OPENPROXY_CODEX_MCP_KEY: &str = "codex_web";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenCodeSettingsRequest {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub models: Option<Vec<String>>,
    pub active_model: Option<String>,
    pub subagent_model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchOpenCodeSettingsRequest {
    pub clear_active_model: bool,
}

#[derive(Debug, Deserialize)]
struct DeleteOpenCodeSettingsQuery {
    pub model: Option<String>,
}

async fn get_opencode_settings(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let installed = check_opencode_installed().await;
    if !installed {
        return Json(json!({
            "installed": false,
            "config": Value::Null,
            "message": "OpenCode CLI is not installed",
        }))
        .into_response();
    }

    match read_opencode_config().await {
        Ok(config) => {
            let provider_config = config
                .as_ref()
                .and_then(|config| config.get("provider"))
                .and_then(|provider| provider.get("openproxy"));
            let model_map = provider_config.and_then(|provider| provider.get("models"));
            let mcp_config = config
                .as_ref()
                .and_then(|config| config.get("mcp"))
                .and_then(|mcp| mcp.get(OPENPROXY_CODEX_MCP_KEY));
            let provider_base_url = provider_config
                .and_then(|provider| provider.pointer("/options/baseURL"))
                .and_then(Value::as_str);
            let provider_api_key = provider_config
                .and_then(|provider| provider.pointer("/options/apiKey"))
                .and_then(Value::as_str);
            let mcp_configured = mcp_config.is_some_and(|mcp| {
                mcp.get("type").and_then(Value::as_str) == Some("remote")
                    && mcp.get("enabled").and_then(Value::as_bool) == Some(true)
                    && mcp.get("oauth").and_then(Value::as_bool) == Some(false)
                    && mcp.get("url").and_then(Value::as_str)
                        == provider_base_url
                            .map(|base_url| format!("{base_url}/mcp"))
                            .as_deref()
                    && mcp
                        .pointer("/headers/Authorization")
                        .and_then(Value::as_str)
                        == provider_api_key
                            .map(|api_key| format!("Bearer {api_key}"))
                            .as_deref()
                    && mcp.get("timeout").and_then(Value::as_u64) == Some(30_000)
            });
            let models = model_map
                .and_then(Value::as_object)
                .map(|models| {
                    models
                        .keys()
                        .map(|model| Value::String(model.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            Json(json!({
                "installed": true,
                "config": config,
                "hasOpenProxy": provider_config.is_some(),
                "configPath": opencode_config_path().to_string_lossy().to_string(),
                "opencode": {
                    "models": models,
                    "activeModel": config
                        .as_ref()
                        .and_then(|config| config.get("model"))
                        .and_then(Value::as_str)
                        .and_then(|model| model.strip_prefix("openproxy/")),
                    "baseURL": provider_config
                        .and_then(|provider| provider.get("options"))
                        .and_then(|options| options.get("baseURL"))
                        .and_then(Value::as_str),
                    "mcpConfigured": mcp_configured,
                },
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to check opencode settings: {error}") })),
        )
            .into_response(),
    }
}

async fn save_opencode_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<OpenCodeSettingsRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    let models = req.models.clone().unwrap_or_else(|| {
        req.model
            .clone()
            .map(|model| vec![model])
            .unwrap_or_default()
    });
    if req.base_url.trim().is_empty()
        || models.is_empty()
        || req
            .api_key
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "baseUrl, apiKey, and at least one model are required" })),
        )
            .into_response();
    }

    match write_opencode_settings(&req, &models).await {
        Ok(config_path) => Json(json!({
            "success": true,
            "message": "OpenCode settings applied successfully!",
            "configPath": config_path,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to apply settings: {error}") })),
        )
            .into_response(),
    }
}

async fn patch_opencode_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PatchOpenCodeSettingsRequest>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match patch_opencode_config(&req).await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to patch settings: {error}") })),
        )
            .into_response(),
    }
}

async fn delete_opencode_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<DeleteOpenCodeSettingsQuery>,
) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    match reset_opencode_settings(params.model).await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to reset opencode settings: {error}") })),
        )
            .into_response(),
    }
}

async fn check_codex_installed() -> bool {
    command_exists("codex", true).await || fs::metadata(codex_config_path()).await.is_ok()
}

async fn read_codex_config() -> anyhow::Result<Option<String>> {
    read_string_optional(&codex_config_path()).await
}

async fn write_codex_settings(settings: &CodexSettings) -> anyhow::Result<String> {
    let config_path = codex_config_path();
    let auth_path = codex_auth_path();
    fs::create_dir_all(codex_dir()).await?;

    let mut parsed = match fs::read_to_string(&config_path).await {
        Ok(existing_config) => parse_toml_table(&existing_config).unwrap_or_default(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => TomlMap::new(),
        Err(error) => return Err(error.into()),
    };

    let base_url = settings.base_url.clone().unwrap_or_default();
    let api_key = settings.api_key.clone().unwrap_or_default();
    let model = settings.model.clone().unwrap_or_default();
    let subagent_model = settings
        .subagent_model
        .clone()
        .unwrap_or_else(|| model.clone());

    parsed.insert("model".to_string(), TomlValue::String(model));
    parsed.insert(
        "model_provider".to_string(),
        TomlValue::String("openproxy".to_string()),
    );
    set_toml_section(
        &mut parsed,
        &["model_providers", "openproxy"],
        TomlValue::Table(TomlMap::from_iter([
            (
                "name".to_string(),
                TomlValue::String("OpenProxy".to_string()),
            ),
            (
                "base_url".to_string(),
                TomlValue::String(normalize_v1_base_url(&base_url)),
            ),
            (
                "wire_api".to_string(),
                TomlValue::String("responses".to_string()),
            ),
        ])),
    );
    set_toml_section(
        &mut parsed,
        &["agents", "subagent"],
        TomlValue::Table(TomlMap::from_iter([(
            "model".to_string(),
            TomlValue::String(subagent_model),
        )])),
    );

    let config_content = toml::to_string_pretty(&TomlValue::Table(parsed))?;
    fs::write(&config_path, config_content).await?;

    let mut auth_data = match fs::read_to_string(&auth_path).await {
        Ok(existing_auth) => serde_json::from_str::<serde_json::Map<String, Value>>(&existing_auth)
            .unwrap_or_default(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err(error.into()),
    };
    auth_data.insert("OPENAI_API_KEY".to_string(), Value::String(api_key));
    auth_data.insert("auth_mode".to_string(), Value::String("apikey".to_string()));
    fs::write(
        &auth_path,
        serde_json::to_vec_pretty(&Value::Object(auth_data))?,
    )
    .await?;

    Ok(config_path.to_string_lossy().to_string())
}

async fn reset_codex_settings() -> anyhow::Result<Value> {
    let config_path = codex_config_path();
    let existing_config = match fs::read_to_string(&config_path).await {
        Ok(existing_config) => existing_config,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    let mut parsed = parse_toml_table(&existing_config)?;
    if parsed.get("model_provider").and_then(TomlValue::as_str) == Some("openproxy") {
        parsed.remove("model");
        parsed.remove("model_provider");
    }
    delete_toml_section(&mut parsed, &["model_providers", "openproxy"]);
    delete_toml_section(&mut parsed, &["agents", "subagent"]);

    let config_content = toml::to_string_pretty(&TomlValue::Table(parsed))?;
    fs::write(&config_path, config_content).await?;

    let auth_path = codex_auth_path();
    match fs::read_to_string(&auth_path).await {
        Ok(existing_auth) => {
            if let Ok(mut auth_data) =
                serde_json::from_str::<serde_json::Map<String, Value>>(&existing_auth)
            {
                auth_data.remove("OPENAI_API_KEY");
                auth_data.remove("auth_mode");
                if auth_data.is_empty() {
                    let _ = fs::remove_file(&auth_path).await;
                } else {
                    fs::write(
                        &auth_path,
                        serde_json::to_vec_pretty(&Value::Object(auth_data))?,
                    )
                    .await?;
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    Ok(json!({
        "success": true,
        "message": "OpenProxy settings removed successfully",
    }))
}

async fn read_string_optional(path: &FsPath) -> anyhow::Result<Option<String>> {
    match fs::read_to_string(path).await {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn has_openproxy_codex_config(config: &str) -> bool {
    config.contains("model_provider = \"openproxy\"")
        || config.contains("[model_providers.openproxy]")
}

fn parse_toml_table(content: &str) -> anyhow::Result<TomlMap<String, TomlValue>> {
    match toml::from_str::<TomlValue>(content)? {
        TomlValue::Table(table) => Ok(table),
        _ => Ok(TomlMap::new()),
    }
}

fn set_toml_section(table: &mut TomlMap<String, TomlValue>, path: &[&str], value: TomlValue) {
    if path.is_empty() {
        return;
    }
    if path.len() == 1 {
        table.insert(path[0].to_string(), value);
        return;
    }

    let entry = table
        .entry(path[0].to_string())
        .or_insert_with(|| TomlValue::Table(TomlMap::new()));
    if !entry.is_table() {
        *entry = TomlValue::Table(TomlMap::new());
    }
    if let TomlValue::Table(next) = entry {
        set_toml_section(next, &path[1..], value);
    }
}

fn delete_toml_section(table: &mut TomlMap<String, TomlValue>, path: &[&str]) {
    if path.is_empty() {
        return;
    }
    if path.len() == 1 {
        table.remove(path[0]);
        return;
    }
    if let Some(TomlValue::Table(next)) = table.get_mut(path[0]) {
        delete_toml_section(next, &path[1..]);
    }
}

fn normalize_v1_base_url(base_url: &str) -> String {
    if base_url.ends_with("/v1") {
        base_url.to_string()
    } else {
        format!("{base_url}/v1")
    }
}

async fn command_exists(program: &str, inject_windows_npm_path: bool) -> bool {
    let finder = if cfg!(windows) { "where" } else { "which" };
    let mut command = Command::new(finder);
    command.arg(program);
    if cfg!(windows) && inject_windows_npm_path {
        if let Some(path) = windows_npm_augmented_path() {
            command.env("PATH", path);
        }
    }
    command
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

fn windows_npm_augmented_path() -> Option<String> {
    let appdata = env::var_os("APPDATA")?;
    let current_path = env::var_os("PATH").unwrap_or_default();
    let npm_dir = PathBuf::from(appdata).join("npm");
    Some(format!(
        "{};{}",
        npm_dir.to_string_lossy(),
        PathBuf::from(current_path).to_string_lossy()
    ))
}

fn codex_dir() -> PathBuf {
    home_dir().join(".codex")
}

fn codex_config_path() -> PathBuf {
    codex_dir().join("config.toml")
}

fn codex_auth_path() -> PathBuf {
    codex_dir().join("auth.json")
}

async fn check_opencode_installed() -> bool {
    command_exists("opencode", true).await || fs::metadata(opencode_config_path()).await.is_ok()
}

async fn read_opencode_config() -> anyhow::Result<Option<Value>> {
    read_json_optional(&opencode_config_path()).await
}

async fn write_opencode_settings(
    req: &OpenCodeSettingsRequest,
    models: &[String],
) -> anyhow::Result<String> {
    let config_path = opencode_config_path();
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut config = match fs::read_to_string(&config_path).await {
        Ok(existing) => parse_json_object_required(&existing).map_err(|error| {
            anyhow::anyhow!("Cannot safely update the existing OpenCode config: {error}")
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(error) => return Err(error.into()),
    };

    let normalized_base_url = normalize_v1_base_url(&req.base_url);
    let mcp_url = format!("{normalized_base_url}/mcp");
    let api_key = req
        .api_key
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("apiKey is required"))?;
    let effective_subagent_model = req
        .subagent_model
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| models[0].clone());

    let provider = config
        .entry("provider".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !provider.is_object() {
        *provider = Value::Object(serde_json::Map::new());
    }
    let provider_map = provider
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("provider must be an object"))?;

    let existing_provider = provider_map
        .entry("openproxy".to_string())
        .or_insert_with(|| {
            json!({
                "npm": "@ai-sdk/openai-compatible",
                "options": {},
                "models": {},
            })
        });
    if !existing_provider.is_object() {
        *existing_provider = json!({
            "npm": "@ai-sdk/openai-compatible",
            "options": {},
            "models": {},
        });
    }
    let existing_provider_map = existing_provider
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("provider.openproxy must be an object"))?;

    let options = existing_provider_map
        .entry("options".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !options.is_object() {
        *options = Value::Object(serde_json::Map::new());
    }
    let options_map = options
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("provider.openproxy.options must be an object"))?;
    options_map.insert("baseURL".to_string(), Value::String(normalized_base_url));
    options_map.insert("apiKey".to_string(), Value::String(api_key.clone()));

    let existing_models = existing_provider_map
        .entry("models".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !existing_models.is_object() {
        *existing_models = Value::Object(serde_json::Map::new());
    }
    let existing_models_map = existing_models
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("provider.openproxy.models must be an object"))?;
    for model in models {
        if model.is_empty() {
            continue;
        }
        existing_models_map
            .entry(model.clone())
            .or_insert_with(|| json!({ "name": model }));
    }

    match req.active_model.as_deref() {
        Some("") => {
            config.insert("model".to_string(), Value::String(String::new()));
        }
        _ => {
            let final_active = req
                .active_model
                .clone()
                .filter(|model| !model.is_empty())
                .unwrap_or_else(|| models[0].clone());
            config.insert(
                "model".to_string(),
                Value::String(format!("openproxy/{final_active}")),
            );
        }
    }

    let agent = config
        .entry("agent".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !agent.is_object() {
        *agent = Value::Object(serde_json::Map::new());
    }
    let agent_map = agent
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("agent must be an object"))?;
    agent_map.insert(
        "explorer".to_string(),
        json!({
            "description": "Fast explorer subagent for codebase exploration",
            "mode": "subagent",
            "model": format!("openproxy/{effective_subagent_model}"),
        }),
    );

    let mcp = config
        .entry("mcp".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !mcp.is_object() {
        *mcp = Value::Object(serde_json::Map::new());
    }
    mcp.as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("mcp must be an object"))?
        .insert(
            OPENPROXY_CODEX_MCP_KEY.to_string(),
            json!({
                "type": "remote",
                "url": mcp_url,
                "enabled": true,
                "oauth": false,
                "headers": {
                    "Authorization": format!("Bearer {api_key}")
                },
                "timeout": 30000
            }),
        );

    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&Value::Object(config))?,
    )
    .await?;
    Ok(config_path.to_string_lossy().to_string())
}

async fn patch_opencode_config(req: &PatchOpenCodeSettingsRequest) -> anyhow::Result<Value> {
    let config_path = opencode_config_path();
    let mut config = match fs::read_to_string(&config_path).await {
        Ok(existing) => parse_json_object_required(&existing)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No config file found",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    if req.clear_active_model
        && config
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|model| model.starts_with("openproxy/"))
    {
        config.insert("model".to_string(), Value::String(String::new()));
    }

    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&Value::Object(config))?,
    )
    .await?;
    Ok(json!({
        "success": true,
        "message": "Settings updated",
    }))
}

async fn reset_opencode_settings(model_to_remove: Option<String>) -> anyhow::Result<Value> {
    let config_path = opencode_config_path();
    let mut config = match fs::read_to_string(&config_path).await {
        Ok(existing) => parse_json_object_required(&existing)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "success": true,
                "message": "No config file to reset",
            }));
        }
        Err(error) => return Err(error.into()),
    };

    if let Some(model_to_remove) = model_to_remove.clone() {
        let active_model_matches = config
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|model| model == format!("openproxy/{model_to_remove}"));
        let mut remove_provider = false;
        let mut next_model = None;
        if let Some(models_map) = config
            .get_mut("provider")
            .and_then(Value::as_object_mut)
            .and_then(|provider| provider.get_mut("openproxy"))
            .and_then(Value::as_object_mut)
            .and_then(|provider| provider.get_mut("models"))
            .and_then(Value::as_object_mut)
        {
            models_map.remove(&model_to_remove);
            if models_map.is_empty() {
                remove_provider = true;
            } else if active_model_matches {
                next_model = models_map.keys().next().cloned();
            }
        }
        if remove_provider {
            if let Some(provider) = config.get_mut("provider").and_then(Value::as_object_mut) {
                provider.remove("openproxy");
            }
            remove_opencode_mcp(&mut config);
            if config
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|model| model.starts_with("openproxy/"))
            {
                config.remove("model");
            }
        } else if let Some(next_model) = next_model {
            config.insert(
                "model".to_string(),
                Value::String(format!("openproxy/{next_model}")),
            );
        }
    } else {
        if let Some(provider) = config.get_mut("provider").and_then(Value::as_object_mut) {
            provider.remove("openproxy");
        }
        remove_opencode_mcp(&mut config);
        if config
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|model| model.starts_with("openproxy/"))
        {
            config.remove("model");
        }
    }

    let should_remove_explorer = config
        .get("agent")
        .and_then(Value::as_object)
        .and_then(|agent| agent.get("explorer"))
        .and_then(Value::as_object)
        .and_then(|explorer| explorer.get("model"))
        .and_then(Value::as_str)
        .is_some_and(|model| model.starts_with("openproxy/"));
    if should_remove_explorer {
        if let Some(agent) = config.get_mut("agent").and_then(Value::as_object_mut) {
            agent.remove("explorer");
            if agent.is_empty() {
                config.remove("agent");
            }
        }
    }

    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&Value::Object(config))?,
    )
    .await?;
    Ok(json!({
        "success": true,
        "message": model_to_remove
            .map(|model| Value::String(format!("Model \"{model}\" removed")))
            .unwrap_or_else(|| Value::String("OpenProxy settings removed from OpenCode".to_string())),
    }))
}

fn remove_opencode_mcp(config: &mut serde_json::Map<String, Value>) {
    let remove_mcp_root = config
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
        .is_some_and(|mcp| {
            mcp.remove(OPENPROXY_CODEX_MCP_KEY);
            mcp.is_empty()
        });
    if remove_mcp_root {
        config.remove("mcp");
    }
}

async fn read_json_optional(path: &FsPath) -> anyhow::Result<Option<Value>> {
    match fs::read_to_string(path).await {
        Ok(content) => Ok(Some(serde_json::from_str(&content)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn parse_json_object_required(content: &str) -> anyhow::Result<serde_json::Map<String, Value>> {
    match serde_json::from_str::<Value>(content)? {
        Value::Object(object) => Ok(object),
        _ => Err(anyhow::anyhow!("Expected JSON object")),
    }
}

fn opencode_config_path() -> PathBuf {
    home_dir()
        .join(".config")
        .join("opencode")
        .join("opencode.jsonc")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

// ═══════════════════════════════════════════════════════════════════════════
// Route Registration
// ═══════════════════════════════════════════════════════════════════════════

pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(claude_settings::routes())
        .merge(cline_settings::routes())
        .merge(deepseek_tui_settings::routes())
        .route("/api/cli-tools", get(list_tools))
        .route("/api/cli-tools/execute", post(execute_command))
        .route("/api/cli-tools/run/{tool_name}", post(run_tool))
        .route("/api/cli-tools/help", get(get_help))
        .route("/api/cli-tools/all-statuses", get(get_all_statuses))
        // Codex settings
        .route("/api/cli-tools/codex-settings", get(get_codex_settings))
        .route("/api/cli-tools/codex-settings", post(save_codex_settings))
        .route(
            "/api/cli-tools/codex-settings",
            delete(delete_codex_settings),
        )
        .route(
            "/api/cli-tools/opencode-settings",
            get(get_opencode_settings)
                .post(save_opencode_settings)
                .patch(patch_opencode_settings)
                .delete(delete_opencode_settings),
        )
}

/// Extract JSON body from an axum Response (best-effort; returns Null on failure).
async fn response_json_value(response: Response) -> Value {
    use axum::body::to_bytes;
    let bytes = match to_bytes(response.into_body(), 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return Value::Null,
    };
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// GET /api/cli-tools/all-statuses
/// Fetch status of all CLI tools in parallel by invoking per-tool handlers
/// in-process (no loopback HTTP). Returns {toolId: status}.
async fn get_all_statuses(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(response) = super::require_dashboard_or_management_api_key(&headers, &state) {
        return response;
    }

    // Auth already checked above; each handler re-checks and is fine with the
    // same headers. In-process avoids host/cookie/auth loopback failure modes.
    let (claude, cline, codex, opencode, deepseek_tui) = tokio::join!(
        async {
            response_json_value(
                claude_settings::get_claude_settings(State(state.clone()), headers.clone()).await,
            )
            .await
        },
        async {
            response_json_value(
                cline_settings::get_cline_settings(State(state.clone()), headers.clone()).await,
            )
            .await
        },
        async {
            response_json_value(get_codex_settings(State(state.clone()), headers.clone()).await)
                .await
        },
        async {
            response_json_value(get_opencode_settings(State(state.clone()), headers.clone()).await)
                .await
        },
        async {
            response_json_value(
                deepseek_tui_settings::get_deepseek_tui_settings(
                    State(state.clone()),
                    headers.clone(),
                )
                .await,
            )
            .await
        },
    );

    let mut statuses = serde_json::Map::new();
    statuses.insert("claude".into(), claude);
    statuses.insert("cline".into(), cline);
    statuses.insert("codex".into(), codex);
    statuses.insert("opencode".into(), opencode);
    statuses.insert("deepseek-tui".into(), deepseek_tui);

    Json(Value::Object(statuses)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::*;

    #[test]
    fn test_codex_settings_default() {
        let settings = CodexSettings::default();
        assert_eq!(settings.base_url, None);
        assert_eq!(settings.api_key, None);
        assert_eq!(settings.model, None);
        assert_eq!(settings.subagent_model, None);
    }

    #[test]
    fn test_codex_settings_serialization() {
        let settings = CodexSettings {
            base_url: Some("http://localhost:4623/v1".to_string()),
            api_key: Some("sk-test".to_string()),
            model: Some("openai/gpt-4".to_string()),
            subagent_model: Some("openai/gpt-4o".to_string()),
        };

        let json = serde_json::to_string(&settings).unwrap();
        let deserialized: CodexSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.base_url, settings.base_url);
        assert_eq!(deserialized.api_key, settings.api_key);
        assert_eq!(deserialized.model, settings.model);
        assert_eq!(deserialized.subagent_model, settings.subagent_model);
    }

    #[test]
    fn test_parse_cli_command() {
        let result = parse_cli_command("openproxy provider list", None);
        assert!(result.is_some());
        let (program, args) = result.unwrap();
        assert_eq!(program, "openproxy");
        assert_eq!(args, vec!["provider", "list"]);
    }

    #[test]
    fn test_parse_cli_command_with_additional_args() {
        let additional_args = vec!["--json".to_string()];
        let result = parse_cli_command("openproxy key list", Some(additional_args.as_slice()));
        assert!(result.is_some());
        let (program, args) = result.unwrap();
        assert_eq!(program, "openproxy");
        assert_eq!(args, vec!["key", "list", "--json"]);
    }

    #[test]
    fn test_parse_cli_command_empty() {
        let result = parse_cli_command("", None);
        assert!(result.is_none());
    }

    #[test]
    fn test_build_tool_command_provider_list() {
        let (program, args) = build_tool_command("provider-list", vec![]);
        assert_eq!(program, "openproxy");
        assert_eq!(args, vec!["provider", "list", "--json"]);
    }

    #[test]
    fn test_build_tool_command_pool_status() {
        let (program, args) = build_tool_command("pool-status", vec!["my-pool".to_string()]);
        assert_eq!(program, "openproxy");
        assert_eq!(args, vec!["pool", "status", "--name", "my-pool", "--json"]);
    }

    #[test]
    fn test_build_tool_command_unknown() {
        let (program, args) = build_tool_command("unknown-tool", vec!["arg1".to_string()]);
        assert_eq!(program, "unknown-tool");
        assert_eq!(args, vec!["arg1"]);
    }

    #[tokio::test]
    async fn run_command_with_timeout_returns_success_for_fast_command() {
        let response =
            run_command_with_timeout("/bin/sh", &["-c".to_string(), "exit 0".to_string()], 5).await;
        assert!(response.success);
        assert_eq!(response.exit_code, Some(0));
        assert!(!response.timed_out);
    }

    #[tokio::test]
    async fn run_command_with_timeout_captures_stdout() {
        let response = run_command_with_timeout(
            "/bin/sh",
            &["-c".to_string(), "printf hello".to_string()],
            5,
        )
        .await;
        assert!(response.success);
        assert_eq!(response.stdout, "hello");
        assert!(!response.timed_out);
    }

    #[tokio::test]
    async fn run_command_with_timeout_kills_long_running_process() {
        // sleep 30 should easily exceed the 1s timeout; we expect the killer
        // to fire and return timed_out=true within a couple of seconds.
        let start = std::time::Instant::now();
        let response =
            run_command_with_timeout("/bin/sh", &["-c".to_string(), "sleep 30".to_string()], 1)
                .await;
        let elapsed = start.elapsed();
        assert!(
            response.timed_out,
            "expected timed_out=true, got {response:?}"
        );
        assert!(!response.success);
        assert!(response.exit_code.is_none());
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "command should have been killed quickly, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn run_command_with_timeout_reports_failure_for_missing_binary() {
        let response =
            run_command_with_timeout("/this/definitely/does/not/exist-openproxy-test", &[], 5)
                .await;
        assert!(!response.success);
        assert_eq!(response.exit_code, Some(-1));
        assert!(response.stderr.contains("Failed to execute"));
        assert!(!response.timed_out);
    }
}
