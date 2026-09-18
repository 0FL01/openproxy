//! C41: only confirmed-dead modules/dependencies go; everything kept is used.
//!
//! Census gate: every direct dependency must be referenced in code, the
//! modules deleted by earlier checkpoints must stay deleted with no dangling
//! references, and untouched areas (HTTP pool, security state, cryptography,
//! allocator, runtime threads) must show no drive-by changes. Each removal
//! group is its own revertible commit.

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Dependency names declared under a `[section]` header in Cargo.toml.
fn section_deps(manifest: &str, section: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            inside = trimmed == section;
            continue;
        }
        if !inside || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(name) = trimmed.split(['=', ' ']).next() {
            let name = name.trim().to_string();
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    names
}

fn referenced(crate_name: &str, dirs: &[&str]) -> bool {
    let ident = crate_name.replace('-', "_");
    let use_pattern = format!("use {ident}");
    let path_pattern = format!("{ident}::");
    dirs.iter().any(|dir| {
        let mut stack = vec![root().join(dir)];
        while let Some(path) = stack.pop() {
            let entries = std::fs::read_dir(&path).expect("read source dir");
            for entry in entries {
                let entry = entry.expect("dir entry");
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                    continue;
                }
                let content = std::fs::read_to_string(&path).expect("read source");
                if content.contains(&use_pattern) || content.contains(&path_pattern) {
                    return true;
                }
            }
        }
        false
    })
}

#[test]
fn every_direct_dependency_is_referenced() {
    let manifest = std::fs::read_to_string(root().join("Cargo.toml")).expect("read manifest");
    let mut orphaned = Vec::new();
    for name in section_deps(&manifest, "[dependencies]") {
        if name == "sha2_rsa_compat" {
            // Renamed package: sources refer to it as `sha2`.
            assert!(
                referenced("sha2", &["src"]),
                "renamed sha2 compat crate must stay referenced as sha2"
            );
            continue;
        }
        if !referenced(&name, &["src"]) {
            orphaned.push(name);
        }
    }
    assert!(
        orphaned.is_empty(),
        "unreferenced production dependencies must be justified and removed: {orphaned:?}"
    );
    for name in section_deps(&manifest, "[dev-dependencies]") {
        if !referenced(&name, &["src", "tests"]) {
            orphaned.push(name);
        }
    }
    assert!(
        orphaned.is_empty(),
        "unreferenced dev-dependencies must be justified and removed: {orphaned:?}"
    );
}

#[test]
fn deleted_replay_modules_stay_deleted() {
    for removed in [
        "src/core/utils/kiro_session_replay.rs",
        "src/core/utils/claude_header_cache.rs",
    ] {
        assert!(
            !root().join(removed).exists(),
            "deleted replay module must not return: {removed}"
        );
    }
    // No dangling references to the deleted replay symbols anywhere.
    let mut stack = vec![root().join("src"), root().join("tests")];
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path).expect("read dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            // Skip this guard file itself: it names the deleted modules.
            if path.file_name().and_then(|name| name.to_str())
                == Some("lean_proxy_dead_code_c41.rs")
            {
                continue;
            }
            let content = std::fs::read_to_string(&path).expect("read source");
            for symbol in [
                "kiro_session_replay",
                "claude_header_cache",
                "KiroSessionReplay",
                "ClaudeHeaderCache",
            ] {
                assert!(
                    !content.contains(symbol),
                    "dangling reference to deleted {symbol} in {}",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn protected_areas_show_no_drive_by_changes() {
    // C41 must not touch the HTTP pool, security state, cryptography, the
    // allocator, or runtime threads. Pin their key markers present.
    let lib_markers = [
        ("src/core/executor/default.rs", "ClientPool"),
        ("src/db/crypto.rs", "aes"),
        ("src/main.rs", "tokio::main"),
    ];
    for (rel, marker) in lib_markers {
        let content = std::fs::read_to_string(root().join(rel))
            .unwrap_or_else(|_| panic!("must exist: {rel}"));
        assert!(content.contains(marker), "{rel} must keep {marker}");
    }
    let manifest = std::fs::read_to_string(root().join("Cargo.toml")).expect("read manifest");
    for dep in ["hyper", "reqwest", "rustls", "aes-gcm", "argon2", "tokio"] {
        assert!(
            manifest.contains(dep),
            "protected dependency must not be dropped without its own checkpoint: {dep}"
        );
    }
    // C41 is not an optimization-profile experiment: the release profile
    // must match the pre-existing baseline exactly (opt-level "s" predates
    // this checkpoint; a 3-vs-s comparison needs its own checkpoint).
    for baseline in [
        "lto = \"thin\"",
        "codegen-units = 1",
        "strip = \"symbols\"",
        "panic = \"abort\"",
        "opt-level = \"s\"",
    ] {
        assert!(
            manifest.contains(baseline),
            "release profile baseline must stay untouched: {baseline}"
        );
    }
}
