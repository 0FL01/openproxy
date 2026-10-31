//! Supervision of the original Claude Code through the official Agent SDK.
//!
//! The child owns native credentials and transcripts. This module only frames
//! SDK operations, bounds their transport, and scopes client session identities.
mod login;
#[cfg(unix)]
mod pty;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::{ChildStdin, Command};
use tokio::sync::{mpsc, Mutex, OwnedRwLockReadGuard, RwLock};
use tokio_util::codec::{FramedRead, LinesCodec};
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::types::ProviderConnection;

pub(crate) const PROVIDER: &str = "claude-agent";
pub(crate) const INVENTORY_KEY: &str = "claudeAgentModels";
const MAX_FRAME: usize = 32 * 1024 * 1024 + 4096;
const START_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CALLS: usize = 64;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(90);

#[derive(Clone, Debug)]
pub(crate) struct NativeError {
    pub message: String,
    pub code: String,
}

impl NativeError {
    pub(crate) fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn unavailable() -> Self {
        Self::new(
            "claude_agent_unavailable",
            "Claude Agent runtime unavailable; check the installed Node runtime and Claude Code",
        )
    }
}

pub(crate) enum BridgeEvent {
    Part(Value),
    Result(Value),
    Error(NativeError),
    Done,
}

type Pending = Arc<StdMutex<HashMap<String, mpsc::Sender<BridgeEvent>>>>;

struct Bridge {
    writer: Arc<Mutex<ChildStdin>>,
    stop: CancellationToken,
    exited: CancellationToken,
}

pub(crate) struct ClaudeAgent {
    db: Arc<Db>,
    profile: PathBuf,
    executable: PathBuf,
    node: PathBuf,
    script: PathBuf,
    bridge: Mutex<Option<Bridge>>,
    // Startup/probes may run together, but native login changes the profile
    // exclusively after draining already-started generations.
    profile_gate: Arc<RwLock<()>>,
    pending: Pending,
    shutdown: CancellationToken,
    login: Mutex<Option<Arc<login::LoginAttempt>>>,
}

impl ClaudeAgent {
    pub(crate) fn new(db: Arc<Db>) -> Self {
        let profile = std::path::absolute(db.data_dir.join(PROVIDER))
            .unwrap_or_else(|_| db.data_dir.join(PROVIDER));
        Self {
            db,
            profile,
            executable: std::env::var_os("OPENPROXY_CLAUDE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| "claude".into()),
            node: "node".into(),
            script: std::env::var_os("OPENPROXY_CLAUDE_RUNTIME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/claude-agent/bridge.mjs")
                }),
            bridge: Mutex::new(None),
            profile_gate: Arc::new(RwLock::new(())),
            pending: Arc::new(StdMutex::new(HashMap::new())),
            shutdown: CancellationToken::new(),
            login: Mutex::new(None),
        }
    }

    async fn prepare_profile(&self) -> Result<(), NativeError> {
        tokio::fs::create_dir_all(self.profile.join("home"))
            .await
            .map_err(|_| NativeError::unavailable())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&self.profile, std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(|_| NativeError::unavailable())?;
        }
        Ok(())
    }

    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        // In particular do not inherit Anthropic API/cloud credentials or the
        // proxy's own secrets. HOME is isolated from unrelated native profiles.
        command.env_clear();
        for name in [
            "PATH",
            "LANG",
            "LC_ALL",
            "TZ",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "no_proxy",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "NODE_EXTRA_CA_CERTS",
            "CLAUDE_CODE_CERT_STORE",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .env("HOME", self.profile.join("home"))
            .env("XDG_CONFIG_HOME", self.profile.join("home/.config"))
            .env("CLAUDE_CONFIG_DIR", &self.profile)
            .env("OPENPROXY_CLAUDE_PATH", &self.executable)
            .env("TERM", "dumb")
            .current_dir(&self.profile)
            .kill_on_drop(true);
        command
    }

    async fn writer(self: &Arc<Self>) -> Result<Arc<Mutex<ChildStdin>>, NativeError> {
        if self.shutdown.is_cancelled() {
            return Err(NativeError::unavailable());
        }
        let mut slot = self.bridge.lock().await;
        if let Some(bridge) = slot.as_ref().filter(|bridge| !bridge.exited.is_cancelled()) {
            return Ok(bridge.writer.clone());
        }
        self.prepare_profile().await?;
        let mut command = self.command(&self.node);
        command
            .arg(&self.script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The SDK may print prompts/credential-bearing diagnostics here.
            // Never forward this stream into the application/TRACE logs.
            .stderr(Stdio::null());
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|_| NativeError::unavailable())?;
        let pid = child.id();
        let writer = Arc::new(Mutex::new(
            child.stdin.take().ok_or_else(NativeError::unavailable)?,
        ));
        let stdout = child.stdout.take().ok_or_else(NativeError::unavailable)?;
        let stop = CancellationToken::new();
        let exited = CancellationToken::new();
        let pending = self.pending.clone();
        let task_stop = stop.clone();
        let task_exited = exited.clone();
        let shutdown = self.shutdown.clone();
        let task_writer = writer.clone();
        tokio::spawn(async move {
            let read = async {
                let mut lines = FramedRead::new(stdout, LinesCodec::new_with_max_length(MAX_FRAME));
                while let Some(Ok(line)) = lines.next().await {
                    let Ok(event) = serde_json::from_str::<Value>(&line) else {
                        break;
                    };
                    let Some(id) = event.get("id").and_then(Value::as_str) else {
                        break;
                    };
                    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
                    let sender = {
                        let mut map = pending.lock().unwrap();
                        if kind == "done" {
                            map.remove(id)
                        } else {
                            map.get(id).cloned()
                        }
                    };
                    let Some(sender) = sender else { continue };
                    let message = match kind {
                        "part" => BridgeEvent::Part(event["part"].clone()),
                        "result" => BridgeEvent::Result(event["result"].clone()),
                        "error" => BridgeEvent::Error(NativeError::new(
                            event["error"]["code"]
                                .as_str()
                                .unwrap_or("claude_agent_error"),
                            event["error"]["message"]
                                .as_str()
                                .unwrap_or("Claude Agent request failed"),
                        )),
                        "done" => BridgeEvent::Done,
                        _ => break,
                    };
                    // One disconnected/slow client cannot retain unlimited SDK
                    // output. Closing the bridge also stops its native workers.
                    if !matches!(
                        tokio::time::timeout(START_TIMEOUT, sender.send(message)).await,
                        Ok(Ok(()))
                    ) {
                        pending.lock().unwrap().remove(id);
                        if kind != "done" {
                            let cancel = format!(
                                "{}\n",
                                json!({"op":"cancel","id":uuid::Uuid::new_v4().to_string(),"targetId":id})
                            );
                            let _ = tokio::time::timeout(Duration::from_secs(1), async {
                                task_writer.lock().await.write_all(cancel.as_bytes()).await
                            })
                            .await;
                        }
                    }
                }
            };
            tokio::select! {
                _ = read => {},
                _ = task_stop.cancelled() => {},
                _ = shutdown.cancelled() => {},
                _ = child.wait() => {},
            }
            let _ = tokio::time::timeout(Duration::from_secs(1), async {
                task_writer
                    .lock()
                    .await
                    .write_all(b"{\"op\":\"shutdown\",\"id\":\"shutdown\"}\n")
                    .await
            })
            .await;
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await;
            // Also terminate descendants if Node itself exited unexpectedly.
            terminate_group(pid, false);
            if tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
            terminate_group(pid, true);
            let _ = child.wait().await;
            let senders = pending
                .lock()
                .unwrap()
                .drain()
                .map(|(_, sender)| sender)
                .collect::<Vec<_>>();
            for sender in senders {
                let _ = sender.try_send(BridgeEvent::Error(NativeError::unavailable()));
            }
            task_exited.cancel();
        });
        *slot = Some(Bridge {
            writer: writer.clone(),
            stop,
            exited,
        });
        Ok(writer)
    }

    pub(crate) async fn call(
        self: &Arc<Self>,
        mut command: Value,
    ) -> Result<BridgeCall, NativeError> {
        let startup = if command["op"] == "run" {
            Some(self.profile_gate.clone().try_read_owned().map_err(|_| {
                NativeError::new(
                    "BUSY",
                    "Wait for the native Claude account change to finish",
                )
            })?)
        } else {
            None
        };
        let writer = self.writer().await?;
        let id = uuid::Uuid::new_v4().to_string();
        command["id"] = json!(id);
        let mut bytes = serde_json::to_vec(&command).map_err(|_| NativeError::unavailable())?;
        if bytes.len() > MAX_FRAME {
            return Err(NativeError::new(
                "claude_agent_body_too_large",
                "Native request exceeds 32 MiB",
            ));
        }
        bytes.push(b'\n');
        let (sender, receiver) = mpsc::channel(4);
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.len() >= MAX_CALLS {
                return Err(NativeError::new(
                    "claude_agent_capacity",
                    "Claude Agent is at capacity",
                ));
            }
            pending.insert(id.clone(), sender);
        }
        let call = BridgeCall {
            id,
            receiver,
            agent: Arc::downgrade(self),
            finished: false,
            startup,
        };
        let write = async { writer.lock().await.write_all(&bytes).await };
        if !matches!(tokio::time::timeout(START_TIMEOUT, write).await, Ok(Ok(()))) {
            return Err(NativeError::unavailable());
        }
        Ok(call)
    }

    async fn probe(self: &Arc<Self>) -> Result<Value, NativeError> {
        // The SDK bounds initialization and optional context discovery at 30s
        // each. Do not cut off its second control-plane phase at the first cap.
        tokio::time::timeout(Duration::from_secs(65), async {
            let mut call = self.call(json!({"op":"probe"})).await?;
            let mut result = None;
            while let Some(event) = call.recv().await {
                match event {
                    BridgeEvent::Result(value) => {
                        let account = &value["account"];
                        if account["tokenSource"] == "none"
                            || !account["apiKeySource"].is_null()
                            || account["apiProvider"]
                                .as_str()
                                .is_some_and(|provider| provider != "firstParty")
                        {
                            return Err(NativeError::new(
                                "claude_agent_needs_auth",
                                "Native Claude Code requires your Claude subscription login",
                            ));
                        }
                        result = Some(value);
                    }
                    BridgeEvent::Error(error) => return Err(error),
                    BridgeEvent::Done => return result.ok_or_else(NativeError::unavailable),
                    BridgeEvent::Part(_) => return Err(NativeError::unavailable()),
                }
            }
            Err(NativeError::unavailable())
        })
        .await
        .map_err(|_| {
            NativeError::new(
                "claude_agent_start_timeout",
                "Claude Code initialization timed out",
            )
        })?
    }

    pub(crate) async fn refresh_inventory(self: &Arc<Self>) -> Result<Value, NativeError> {
        if self.login_active().await {
            return Err(NativeError::new(
                "claude_agent_login_active",
                "Wait for native Claude login to finish",
            ));
        }
        let _profile = self.profile_gate.clone().try_read_owned().map_err(|_| {
            NativeError::new(
                "BUSY",
                "Wait for the native Claude account change to finish",
            )
        })?;
        // SDK probes already report authentication and share the eight-process
        // cap. Do not spawn an extra status CLI per concurrent management call.
        let probe = self.probe().await?;
        self.save_inventory(&probe, None).await?;
        Ok(probe)
    }

    pub(crate) async fn observe_inventory(&self, part: &Value) {
        if let Some(inventory) = part
            .get("providerMetadata")
            .and_then(|value| value.get(PROVIDER))
            .and_then(|value| value.get("inventory"))
        {
            if self.save_inventory(inventory, None).await.is_err() {
                tracing::warn!(target: "openproxy::claude_agent", "Could not save native model inventory");
            }
        }
    }

    async fn save_inventory(
        &self,
        probe: &Value,
        name: Option<&str>,
    ) -> Result<String, NativeError> {
        let mut models = normalized_models(probe)?;
        let snapshot = self.db.snapshot();
        let existing = snapshot
            .provider_connections
            .iter()
            .find(|connection| {
                connection.provider == PROVIDER && connection.auth_type == "external"
            })
            .cloned();
        if existing.is_none() && name.is_none() {
            return Err(NativeError::new(
                "claude_agent_needs_auth",
                "Native connection no longer exists",
            ));
        }
        if let Some(connection) = &existing {
            for model in &mut models {
                if model.get("contextWindow").is_none() {
                    if let Some(context) = inventory(connection)
                        .iter()
                        .find(|entry| entry["id"] == model["id"])
                        .and_then(|entry| entry.get("contextWindow"))
                    {
                        model["contextWindow"] = context.clone();
                    }
                }
            }
        }
        let id = existing
            .map(|connection| connection.id)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let now = chrono::Utc::now().to_rfc3339();
        let saved = self
            .db
            .update(|db| {
                if !db
                    .provider_connections
                    .iter()
                    .any(|connection| connection.id == id)
                {
                    if name.is_none() {
                        return;
                    }
                    db.provider_connections.push(ProviderConnection {
                        id: id.clone(),
                        provider: PROVIDER.into(),
                        auth_type: "external".into(),
                        name: Some(name.unwrap_or("Claude Agent").to_string()),
                        is_active: Some(true),
                        priority: Some(1),
                        created_at: Some(now.clone()),
                        ..Default::default()
                    });
                }
                let connection = db
                    .provider_connections
                    .iter_mut()
                    .find(|connection| connection.id == id)
                    .unwrap();
                connection.updated_at = Some(now.clone());
                connection.test_status = Some("active".into());
                connection.last_tested = Some(now.clone());
                connection.last_error = None;
                connection.email = probe["account"]["email"].as_str().map(str::to_string);
                connection
                    .provider_specific_data
                    .insert(INVENTORY_KEY.into(), json!(models));
                for field in ["version", "subscriptionType", "apiProvider", "tokenSource"] {
                    let value = if field == "version" {
                        &probe[field]
                    } else {
                        &probe["account"][field]
                    };
                    if let Some(value) = value.as_str().filter(|value| value.len() <= 256) {
                        connection
                            .provider_specific_data
                            .insert(field.into(), json!(value));
                    } else {
                        connection.provider_specific_data.remove(field);
                    }
                }
            })
            .await
            .map_err(|_| {
                NativeError::new(
                    "claude_agent_persistence",
                    "Could not save native connection metadata",
                )
            })?;
        if !saved
            .provider_connections
            .iter()
            .any(|connection| connection.id == id)
        {
            return Err(NativeError::new(
                "claude_agent_needs_auth",
                "Native connection no longer exists",
            ));
        }
        Ok(id)
    }

    async fn stop_bridge(&self) {
        let mut slot = self.bridge.lock().await;
        if let Some(bridge) = slot.as_ref() {
            let shutdown = b"{\"op\":\"shutdown\",\"id\":\"shutdown\"}\n";
            let _ = tokio::time::timeout(Duration::from_secs(1), async {
                bridge.writer.lock().await.write_all(shutdown).await
            })
            .await;
            if tokio::time::timeout(Duration::from_secs(100), bridge.exited.cancelled())
                .await
                .is_err()
            {
                bridge.stop.cancel();
                let _ =
                    tokio::time::timeout(Duration::from_secs(8), bridge.exited.cancelled()).await;
            }
        }
        slot.take();
    }

    pub(crate) fn signal_shutdown(&self) {
        self.shutdown.cancel();
    }

    pub(crate) async fn shutdown(&self) {
        self.signal_shutdown();
        self.shutdown_login().await;
        self.stop_bridge().await;
    }
}

pub(crate) struct BridgeCall {
    id: String,
    receiver: mpsc::Receiver<BridgeEvent>,
    agent: Weak<ClaudeAgent>,
    finished: bool,
    startup: Option<OwnedRwLockReadGuard<()>>,
}

impl BridgeCall {
    pub(crate) async fn recv(&mut self) -> Option<BridgeEvent> {
        let event = self.receiver.recv().await?;
        if matches!(&event, BridgeEvent::Part(part) if part["type"] == "stream-start")
            || matches!(&event, BridgeEvent::Error(_) | BridgeEvent::Done)
        {
            self.startup.take();
        }
        if matches!(event, BridgeEvent::Done) {
            self.finished = true;
        }
        Some(event)
    }
}

impl Drop for BridgeCall {
    fn drop(&mut self) {
        let Some(agent) = self.agent.upgrade() else {
            return;
        };
        agent.pending.lock().unwrap().remove(&self.id);
        if self.finished {
            return;
        }
        let id = self.id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let bridge = agent.bridge.lock().await;
                if let Some(bridge) = bridge
                    .as_ref()
                    .filter(|bridge| !bridge.exited.is_cancelled())
                {
                    let bytes = format!(
                        "{}\n",
                        json!({"op":"cancel","id":uuid::Uuid::new_v4().to_string(),"targetId":id})
                    );
                    let _ = tokio::time::timeout(Duration::from_secs(1), async {
                        bridge.writer.lock().await.write_all(bytes.as_bytes()).await
                    })
                    .await;
                }
            });
        }
    }
}

fn terminate_group(pid: Option<u32>, force: bool) {
    #[cfg(unix)]
    if let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) {
        // Both supervisors create their own process group/session before exec.
        unsafe {
            libc::kill(-pid, if force { libc::SIGKILL } else { libc::SIGTERM });
        }
    }
}

pub(crate) fn inventory(connection: &ProviderConnection) -> &[Value] {
    connection
        .provider_specific_data
        .get(INVENTORY_KEY)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn normalized_models(probe: &Value) -> Result<Vec<Value>, NativeError> {
    let models = probe["models"]
        .as_array()
        .filter(|models| models.len() <= 1024)
        .ok_or_else(|| {
            NativeError::new("claude_agent_inventory", "Invalid native model inventory")
        })?;
    let mut result = Vec::new();
    for model in models {
        let id = model["value"]
            .as_str()
            .or_else(|| model["id"].as_str())
            .filter(|id| !id.is_empty() && id.len() <= 256 && !id.contains(char::is_whitespace))
            .ok_or_else(|| NativeError::new("claude_agent_inventory", "Invalid native model ID"))?;
        if id == "default" || result.iter().any(|model: &Value| model["id"] == id) {
            continue;
        }
        let name = model["displayName"]
            .as_str()
            .or_else(|| model["name"].as_str())
            .unwrap_or(id);
        let efforts = model
            .get("supportedEffortLevels")
            .or_else(|| model.get("efforts"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|value| {
                matches!(
                    value.as_str(),
                    Some("low" | "medium" | "high" | "xhigh" | "max")
                )
            })
            .collect::<Vec<_>>();
        let mut row = json!({"id":id,"name":name,"kind":"llm","efforts":efforts});
        if let Some(context) = model["contextWindow"]
            .as_u64()
            .filter(|value| *value > 0 && *value <= u32::MAX as u64)
        {
            row["contextWindow"] = json!(context);
        }
        result.push(row);
    }
    Ok(result)
}

#[cfg(all(test, unix))]
mod tests;
