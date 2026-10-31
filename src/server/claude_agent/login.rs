use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::{terminate_group, ClaudeAgent, NativeError};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_OUTPUT: usize = 64 * 1024;

pub(super) struct LoginAttempt {
    id: String,
    owner: String,
    state: Mutex<Value>,
    cancel: CancellationToken,
    finished: CancellationToken,
    #[cfg(unix)]
    pty: Arc<super::pty::NativePty>,
}

impl ClaudeAgent {
    pub(crate) async fn login_active(&self) -> bool {
        let login = self.login.lock().await;
        match login.as_ref() {
            Some(attempt) => attempt.state.lock().await["status"] == "pending",
            None => false,
        }
    }

    #[cfg(unix)]
    pub(crate) async fn start_login(
        self: &Arc<Self>,
        owner: String,
        name: String,
    ) -> Result<String, NativeError> {
        if self.shutdown.is_cancelled() {
            return Err(NativeError::unavailable());
        }
        let mut slot = self.login.lock().await;
        if let Some(previous) = slot.as_ref() {
            if previous.state.lock().await["status"] == "pending" {
                return Err(NativeError::new(
                    "claude_agent_login_active",
                    "A native Claude login is already in progress",
                ));
            }
        }
        let profile = self.profile_gate.clone().try_write_owned().map_err(|_| {
            NativeError::new(
                "BUSY",
                "Wait for native initialization or account checking to finish",
            )
        })?;
        self.prepare_profile().await?;
        // Native login can change the active account. Stop/drain SDK workers
        // before permitting that credential change; other providers are untouched.
        self.stop_bridge().await;
        let mut command = self.command(&self.executable);
        command.args(["auth", "login"]);
        let (pty, mut child) = super::pty::NativePty::spawn(command).map_err(|_| {
            NativeError::new(
                "claude_agent_login_unavailable",
                "Could not start the installed Claude Code login",
            )
        })?;
        let attempt = Arc::new(LoginAttempt {
            id: uuid::Uuid::new_v4().to_string(),
            owner,
            state: Mutex::new(json!({"status":"pending","output":""})),
            cancel: CancellationToken::new(),
            finished: CancellationToken::new(),
            pty: Arc::new(pty),
        });
        *slot = Some(attempt.clone());
        let id = attempt.id.clone();
        let agent = self.clone();
        tokio::spawn(async move {
            let _profile = profile;
            let _finished = attempt.finished.clone().drop_guard();
            let pid = child.id();
            let read_attempt = attempt.clone();
            let read = async {
                let mut buffer = [0; 4096];
                let mut utf8 = Vec::new();
                loop {
                    let Ok(count) = read_attempt.pty.read(&mut buffer).await else {
                        break;
                    };
                    if count == 0 {
                        break;
                    }
                    utf8.extend_from_slice(&buffer[..count]);
                    // Keep an incomplete UTF-8 suffix until the next PTY read.
                    let valid = match std::str::from_utf8(&utf8) {
                        Ok(_) => utf8.len(),
                        Err(error) if error.error_len().is_none() => error.valid_up_to(),
                        Err(_) => utf8.len(),
                    };
                    let text = String::from_utf8_lossy(&utf8[..valid]).into_owned();
                    utf8.drain(..valid);
                    let mut state = read_attempt.state.lock().await;
                    let output = state["output"].as_str().unwrap_or("");
                    let mut output = format!("{output}{text}");
                    if output.len() > MAX_OUTPUT {
                        let mut start = output.len() - MAX_OUTPUT;
                        while !output.is_char_boundary(start) {
                            start += 1
                        }
                        output.drain(..start);
                    }
                    state["output"] = json!(output);
                }
                // PTY EOF can precede the child's wait result; keep awaiting it.
                std::future::pending::<()>().await;
            };
            let status = tokio::select! {
                status = child.wait() => status.ok(),
                _ = read => None,
                _ = attempt.cancel.cancelled() => None,
                _ = agent.shutdown.cancelled() => None,
                _ = tokio::time::sleep(LOGIN_TIMEOUT) => None,
            };
            terminate_group(pid, false);
            if tokio::time::timeout(Duration::from_secs(2), child.wait())
                .await
                .is_err()
            {
                terminate_group(pid, true);
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            let result = if status.is_some_and(|status| status.success()) {
                tokio::select! {
                    result = async { agent.auth_status().await?; agent.probe().await } => result,
                    _ = attempt.cancel.cancelled() => Err(NativeError::new("claude_agent_login_cancelled", "Native login cancelled")),
                    _ = agent.shutdown.cancelled() => Err(NativeError::unavailable()),
                }
            } else {
                Err(NativeError::new(
                    "claude_agent_login_failed",
                    "Native Claude login did not complete; retry the login",
                ))
            };
            // Linearize final persistence against cancellation. No input/output
            // or credential data is written to the app database or logs.
            let mut state = attempt.state.lock().await;
            if state["status"] != "pending" {
                return;
            }
            match result {
                Ok(probe) => match agent.save_inventory(&probe, Some(&name)).await {
                    Ok(connection_id) => {
                        *state =
                            json!({"status":"success","output":"","connectionId":connection_id})
                    }
                    Err(error) => {
                        state["status"] = json!("error");
                        state["error"] = json!(error.message);
                    }
                },
                Err(error) => {
                    state["status"] = json!("error");
                    state["error"] = json!(error.message);
                }
            }
        });
        Ok(id)
    }

    #[cfg(not(unix))]
    pub(crate) async fn start_login(
        self: &Arc<Self>,
        _owner: String,
        _name: String,
    ) -> Result<String, NativeError> {
        Err(NativeError::new(
            "claude_agent_login_unavailable",
            "Native dashboard login requires a Unix server",
        ))
    }

    async fn owned_login(&self, owner: &str, id: &str) -> Result<Arc<LoginAttempt>, NativeError> {
        self.login
            .lock()
            .await
            .as_ref()
            .filter(|attempt| attempt.owner == owner && attempt.id == id)
            .cloned()
            .ok_or_else(|| {
                NativeError::new(
                    "claude_agent_login_not_found",
                    "Native login attempt not found",
                )
            })
    }

    pub(crate) async fn login_status(&self, owner: &str, id: &str) -> Result<Value, NativeError> {
        Ok(self
            .owned_login(owner, id)
            .await?
            .state
            .lock()
            .await
            .clone())
    }

    pub(crate) async fn login_input(
        &self,
        owner: &str,
        id: &str,
        input: &str,
    ) -> Result<(), NativeError> {
        let attempt = self.owned_login(owner, id).await?;
        if attempt.state.lock().await["status"] != "pending" {
            return Err(NativeError::new(
                "claude_agent_login_finished",
                "Native login is no longer waiting for input",
            ));
        }
        #[cfg(unix)]
        return tokio::time::timeout(
            Duration::from_secs(2),
            attempt.pty.write_all(input.as_bytes()),
        )
        .await
        .map_err(|_| NativeError::new("claude_agent_login_input", "Native login input timed out"))?
        .map_err(|_| NativeError::new("claude_agent_login_input", "Native login input is closed"));
        #[cfg(not(unix))]
        Err(NativeError::unavailable())
    }

    pub(crate) async fn cancel_login(&self, owner: &str, id: &str) -> Result<(), NativeError> {
        let attempt = self.owned_login(owner, id).await?;
        let mut state = attempt.state.lock().await;
        if state["status"] == "pending" {
            attempt.cancel.cancel();
            *state = json!({"status":"cancelled","output":""});
        }
        Ok(())
    }

    pub(super) async fn shutdown_login(&self) {
        let attempt = self.login.lock().await.clone();
        if let Some(attempt) = attempt {
            let mut state = attempt.state.lock().await;
            if state["status"] == "pending" {
                *state = json!({"status":"cancelled","output":""});
                attempt.cancel.cancel();
            }
            drop(state);
            let _ =
                tokio::time::timeout(Duration::from_secs(8), attempt.finished.cancelled()).await;
        }
    }

    pub(super) async fn auth_status(&self) -> Result<Value, NativeError> {
        self.prepare_profile().await?;
        let mut command = self.command(&self.executable);
        command
            .args(["auth", "status", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|_| NativeError::unavailable())?;
        let pid = child.id();
        let mut stdout = child.stdout.take().ok_or_else(NativeError::unavailable)?;
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let mut bytes = Vec::new();
            (&mut stdout)
                .take(32 * 1024 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| NativeError::unavailable())?;
            if bytes.len() > 32 * 1024 {
                return Err(NativeError::unavailable());
            }
            let status = child.wait().await.map_err(|_| NativeError::unavailable())?;
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| NativeError::unavailable())?;
            if !status.success() || value["loggedIn"] != true || value["authMethod"] != "claude.ai"
            {
                return Err(NativeError::new(
                    "claude_agent_needs_auth",
                    "Sign in to your Claude subscription with native Claude Code login",
                ));
            }
            Ok(value)
        })
        .await;
        terminate_group(pid, true);
        let _ = child.kill().await;
        let _ = child.wait().await;
        result.map_err(|_| {
            NativeError::new(
                "claude_agent_needs_auth",
                "Native authentication status timed out",
            )
        })?
    }
}
