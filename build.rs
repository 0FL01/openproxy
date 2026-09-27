//! Build script.
//!
//! When the `embed-web` feature is enabled (default for release builds), the
//! Astro static output at `web/dist/` is baked into the binary via
//! `rust-embed`. This script fails the build early with a clear message if
//! `web/dist/index.html` is missing, instead of producing a binary with an
//! empty dashboard.
//!
//! To intentionally build without the embedded UI (smaller binary, requires
//! `--dashboard-sidecar-url` or `--web-dir` at runtime):
//!     cargo build --release --no-default-features

fn main() {
    embed_build_commit();

    let embed_enabled = std::env::var("CARGO_FEATURE_EMBED_WEB").is_ok();
    if !embed_enabled {
        return;
    }

    let dist = std::path::Path::new("web/dist/index.html");
    if !dist.exists() {
        // `cargo:warning=` lines are printed without colour but are visible in
        // release builds. We also panic so the build actually fails.
        println!(
            "cargo:warning=web/dist/index.html is missing. \
             Build the dashboard first: (cd web && pnpm install --frozen-lockfile && pnpm run build)"
        );
        panic!(
            "web/dist not built. Run:\n  \
             (cd web && pnpm install --frozen-lockfile && pnpm run build)\n\
             Or build without the embedded UI:\n  \
             cargo build --release --no-default-features"
        );
    }

    // Trigger a rebuild whenever the embedded assets change. Without this,
    // editing `web/dist/...` won't invalidate the existing rust-embed cache
    // and the binary will keep serving stale assets.
    println!("cargo:rerun-if-changed=web/dist");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_EMBED_WEB");
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn valid_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn embed_build_commit() {
    println!("cargo:rerun-if-env-changed=OPENPROXY_BUILD_COMMIT");

    // --git-path resolves the per-worktree HEAD and shared refs correctly.
    // Watch packed refs as well as loose refs (including branch switches).
    if std::path::Path::new(".git").is_file() {
        println!("cargo:rerun-if-changed=.git");
    }
    for name in ["HEAD", "packed-refs", "refs"] {
        if let Some(path) = git_output(&["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(reference) = git_output(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git_output(&["rev-parse", "--git-path", &reference]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    let commit = match std::env::var("OPENPROXY_BUILD_COMMIT") {
        // Empty is the default Docker ARG when no source revision is supplied.
        Ok(value) if value.is_empty() => {
            git_output(&["rev-parse", "HEAD"]).filter(|value| valid_commit(value))
        }
        Ok(value) => {
            assert!(
                valid_commit(&value),
                "OPENPROXY_BUILD_COMMIT must be a full 40- or 64-character hexadecimal commit SHA"
            );
            Some(value)
        }
        Err(std::env::VarError::NotPresent) => {
            git_output(&["rev-parse", "HEAD"]).filter(|value| valid_commit(value))
        }
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("OPENPROXY_BUILD_COMMIT must be Unicode hexadecimal text")
        }
    };
    // Always emit a value so an incremental rebuild cannot retain old metadata.
    println!(
        "cargo:rustc-env=OPENPROXY_BUILD_COMMIT={}",
        commit.unwrap_or_default()
    );
}
