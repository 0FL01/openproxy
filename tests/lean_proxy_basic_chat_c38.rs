//! C38: the optional BasicChat demo surface is gone from the lean product.
//!
//! The proxy must not grow a second chat/harness UI with its own history and
//! attachments. Providers, Available Models, OpenCode model discovery, and the
//! backend dashboard-chat route (also serving the `openproxy chat` CLI) stay
//! intact; browser `basic-chat.*` localStorage data is never deleted by this
//! change.

use std::path::PathBuf;

fn web_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src")
}

fn read_rel(path: &str) -> String {
    std::fs::read_to_string(web_src().join(path))
        .unwrap_or_else(|_| panic!("protected surface must exist: web/src/{path}"))
}

#[test]
fn basic_chat_demo_surface_is_removed() {
    assert!(
        !web_src()
            .join("components/BasicChatPageClient.tsx")
            .exists(),
        "demo chat component must be removed"
    );
    assert!(
        !web_src()
            .join("pages/dashboard/basic-chat/index.astro")
            .exists(),
        "demo chat route must be removed"
    );
    // No dangling references anywhere in the dashboard sources.
    let mut offenders = Vec::new();
    let mut stack = vec![web_src()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read web/src") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if !matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("tsx" | "ts" | "astro" | "js" | "mjs" | "css")
            ) {
                continue;
            }
            let content = std::fs::read_to_string(&path).expect("read source");
            if content.contains("BasicChatPageClient") || content.contains("basic-chat") {
                offenders.push(path);
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "dangling BasicChat references: {offenders:?}"
    );
}

#[test]
fn protected_product_surfaces_stay_intact() {
    // 1. Providers page with Available Models.
    let providers = read_rel("components/providers/ProviderDetailPageClient.tsx");
    assert!(
        providers.contains("Available Models") || providers.contains("available"),
        "providers page must keep Available Models"
    );
    // 2. OpenCode discovery path.
    assert!(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/openproxy-models.js")
            .exists(),
        "OpenCode models plugin must stay"
    );
    // 3. Backend dashboard-chat route stays: it also serves the CLI
    // (`src/cli/chat.rs`), not just the removed demo page.
    let api = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/server/api/mod.rs"),
    )
    .expect("read api routes");
    assert!(
        api.contains("/api/dashboard/chat/completions"),
        "backend dashboard-chat route must stay for the CLI"
    );
    let cli =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/cli/chat.rs"))
            .expect("read cli chat");
    assert!(
        cli.contains("/api/dashboard/chat/completions"),
        "CLI chat consumer must keep working"
    );
    // 4. Dashboard layout has no demo-chat special casing left.
    let layout = read_rel("shared/components/layouts/DashboardLayout.tsx");
    assert!(
        !layout.contains("basic-chat"),
        "layout must not special-case the removed route"
    );
}
