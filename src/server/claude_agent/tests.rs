use super::*;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use std::os::unix::fs::PermissionsExt;
use tower::ServiceExt;

#[tokio::test]
async fn native_dashboard_login_scopes_io_and_persists_only_external_metadata() {
    let temporary = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::load_from(temporary.path()).await.unwrap());
    db.update(|db| {
        db.settings.require_login = true;
        db.api_keys = ["owner", "other"]
            .map(|name| crate::types::ApiKey {
                id: name.into(),
                name: name.into(),
                key: format!("synthetic-{name}"),
                is_active: Some(true),
                ..Default::default()
            })
            .into();
    })
    .await
    .unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let executable = temporary.path().join("claude");
    std::fs::copy(fixture.join("claude-agent-login.mjs"), &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut native = ClaudeAgent::new(db.clone());
    native.executable = executable;
    native.script = fixture.join("claude-agent-bridge.mjs");
    let native = Arc::new(native);
    let mut state = crate::server::state::AppState::new(db.clone());
    state.claude_agent = native.clone();
    let app = crate::build_app(state);

    // Native authentication precedes parsing/admission, independently of the
    // optional auth policy of the older generic inference endpoints.
    let response = app
        .clone()
        .oneshot(
            Request::post("/v2/claude-agent/stream")
                .header("content-type", "application/json")
                .body(Body::from("not json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/claude-agent/login")
                .header("authorization", "Bearer synthetic-owner")
                .header("origin", "https://foreign.invalid")
                .header("host", "proxy.invalid")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        !native.profile.exists(),
        "rejected requests must not spawn a native process"
    );

    let response = app
        .clone()
        .oneshot(
            Request::post("/api/claude-agent/login")
                .header("authorization", "Bearer synthetic-owner")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"Personal Claude"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    let id = body["attemptId"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let output = native.login_status("key:owner", id).await.unwrap();
            if output["output"]
                .as_str()
                .unwrap()
                .contains("synthetic-native-state")
            {
                break;
            }
            assert_eq!(output["status"], "pending", "native login failed: {output}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(native.login_status("key:other", id).await.is_err());
    assert!(native
        .login_input("key:other", id, "synthetic-native-code\n")
        .await
        .is_err());
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/api/claude-agent/login/{id}/input"))
                .header("authorization", "Bearer synthetic-owner")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"input":"synthetic-native-code\n"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = native.login_status("key:owner", id).await.unwrap();
            if status["status"] == "success" {
                break;
            }
            assert_eq!(
                status["status"], "pending",
                "login verification failed: {status}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let snapshot = db.snapshot();
    let connection = snapshot
        .provider_connections
        .iter()
        .find(|connection| connection.provider == PROVIDER)
        .unwrap();
    assert_eq!(connection.auth_type, "external");
    assert!(
        connection.access_token.is_none()
            && connection.refresh_token.is_none()
            && connection.api_key.is_none()
    );
    assert_eq!(inventory(connection)[0]["id"], "opus");
    let exported = serde_json::to_string(snapshot.as_ref()).unwrap();
    assert!(
        !exported.contains("synthetic-native-code") && !exported.contains("synthetic-native-state")
    );
    assert_eq!(
        std::fs::metadata(&native.profile)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    let request = json!({"model":"claude-agent/opus","kind":"primary","sessionID":"same-client-session",
        "system":{"agent":"","instructions":""},"prompt":[{"role":"user","content":[{"type":"text","text":"hello"}]}],"tools":[]});
    let mut replies = Vec::new();
    for key in ["owner", "other"] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/v2/claude-agent/stream")
                    .header("authorization", format!("Bearer synthetic-{key}"))
                    .header("content-type", "application/json")
                    .body(Body::from(request.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 65536).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap().to_string();
        assert!(!text.contains("same-client-session"));
        assert!(text.contains("text-delta") && text.contains("finish"));
        replies.push(text);
    }
    assert_ne!(
        replies[0], replies[1],
        "native sessions must be scoped to the authenticated key"
    );
    assert_eq!(
        inventory(&db.snapshot().provider_connections[0])[0]["contextWindow"],
        1_000_000,
        "native process-start inventory must retain previously probed context capacity"
    );

    // Account changes cannot race an already admitted native startup/probe.
    let hold = native.profile.join("hold-native-probe");
    std::fs::write(&hold, "hold").unwrap();
    let connection_id = db.snapshot().provider_connections[0].id.clone();
    let check = tokio::spawn(
        app.clone().oneshot(
            Request::post(format!("/api/providers/{connection_id}/test"))
                .header("authorization", "Bearer synthetic-owner")
                .body(Body::empty())
                .unwrap(),
        ),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !native.profile.join("native-probe-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        app.clone().oneshot(
            Request::post("/api/claude-agent/login")
                .header("authorization", "Bearer synthetic-owner")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    std::fs::remove_file(hold).unwrap();
    assert_eq!(check.await.unwrap().unwrap().status(), StatusCode::OK);

    let cancel_id = native
        .start_login("key:owner".into(), "Again".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !native.login_status("key:owner", &cancel_id).await.unwrap()["output"]
            .as_str()
            .unwrap()
            .contains("synthetic-native-state")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid: libc::pid_t = std::fs::read_to_string(native.profile.join("synthetic-native-pid"))
        .unwrap()
        .parse()
        .unwrap();
    native.cancel_login("key:owner", &cancel_id).await.unwrap();
    assert_eq!(
        native.login_status("key:owner", &cancel_id).await.unwrap()["status"],
        "cancelled"
    );
    assert_eq!(db.snapshot().provider_connections.len(), 1);
    native.shutdown().await;
    assert!(native.bridge.lock().await.is_none());
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "cancelled native login must be reaped before shutdown returns"
    );
}
