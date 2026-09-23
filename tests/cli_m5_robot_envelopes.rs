//! M5 CLI integration tests — translator.
//!
//! Exercises the `openproxy` binary against a wiremock server and asserts the
//! `--robot` JSON envelopes. We hit one happy-path per subcommand group; the
//! detailed handler tests live in unit tests inside each `cli/*.rs` module.

#![cfg(test)]

use assert_cmd::prelude::*;
use serde_json::{json, Value};
use std::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "test-cli-key";

async fn boot_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    server
}

fn op(server: &MockServer, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("openproxy")
        .expect("locate openproxy binary")
        .env("OPENPROXY_URL", server.uri())
        .env("OPENPROXY_API_KEY", API_KEY)
        .env(
            "DATA_DIR",
            tempfile::tempdir()
                .expect("tempdir")
                .path()
                .to_string_lossy()
                .to_string(),
        )
        .args(args)
        .output()
        .expect("run openproxy")
}

fn op_stdin(server: &MockServer, args: &[&str], stdin: &str) -> std::process::Output {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::cargo_bin("openproxy")
        .expect("locate openproxy binary")
        .env("OPENPROXY_URL", server.uri())
        .env("OPENPROXY_API_KEY", API_KEY)
        .env(
            "DATA_DIR",
            tempfile::tempdir()
                .expect("tempdir")
                .path()
                .to_string_lossy()
                .to_string(),
        )
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn openproxy");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait")
}

fn parse_robot(stdout: &[u8]) -> Value {
    let s = std::str::from_utf8(stdout).expect("utf8 stdout");
    serde_json::from_str(s.trim()).unwrap_or_else(|e| {
        panic!("invalid robot envelope: {e}\nraw: {s}");
    })
}

// ─── translator ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn translator_formats_emits_envelope() {
    let server = boot_server().await;
    Mock::given(method("GET"))
        .and(path("/api/translator/formats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": "openai", "name": "OpenAI", "description": "Chat Completions"},
            {"id": "claude", "name": "Claude", "description": "Messages"},
        ])))
        .mount(&server)
        .await;

    let out = op(&server, &["--robot", "translator", "formats"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.translator.formats");
    assert_eq!(env["data"].as_array().map(Vec::len), Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn translator_preset_save_posts_to_translator_save() {
    let server = boot_server().await;
    Mock::given(method("POST"))
        .and(path("/api/translator/save"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .mount(&server)
        .await;

    let out = op_stdin(
        &server,
        &["--robot", "translator", "preset", "save", "my-preset"],
        r#"{"foo": "bar"}"#,
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env = parse_robot(&out.stdout);
    assert_eq!(env["schema"], "openproxy.v1.translator.preset.save");
}
