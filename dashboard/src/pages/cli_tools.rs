use leptos::prelude::*;
use leptos_router::components::A;

use super::DashboardShell;

const TOOLS: &[(&str, &str)] = &[
    ("opencode", "OpenCode"),
    ("claude", "Claude Code"),
    ("cline", "Cline"),
    ("kilo", "Kilo Code"),
    ("codex", "Codex"),
    ("droid", "Droid"),
    ("openclaw", "OpenClaw"),
    ("hermes", "Hermes"),
    ("cowork", "Cowork"),
    ("deepseek-tui", "DeepSeek TUI"),
    ("jcode", "JCode"),
    ("copilot", "GitHub Copilot"),
    ("grok-build", "Grok Build"),
];

#[component]
pub fn CliToolsPage() -> impl IntoView {
    view! {
        <DashboardShell title="CLI Tools">
            <div class="card-grid">
                {TOOLS.iter().map(|(id, name)| {
                    let href = format!("/dashboard/cli-tools/{id}");
                    view! { <A href=href attr:class="card provider-card"><strong>{*name}</strong><small class="muted">{format!("Configure {name} to use OpenProxy")}</small></A> }
                }).collect_view()}
            </div>
        </DashboardShell>
    }
}

pub fn tool_settings_endpoint(id: &str) -> Option<String> {
    TOOLS
        .iter()
        .any(|(tool, _)| *tool == id)
        .then(|| format!("/api/cli-tools/{id}-settings"))
}
