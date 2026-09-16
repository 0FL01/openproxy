use leptos::{prelude::*, task::spawn_local};

use super::DashboardShell;
use crate::api;

#[component]
pub fn ApplicationLogsPage() -> impl IntoView {
    view! { <JsonPage title="Application Logs" endpoint="/api/request-logs?page=1&pageSize=100" /> }
}

#[component]
pub fn QuotaPage() -> impl IntoView {
    view! { <JsonPage title="Quota" endpoint="/api/providers" /> }
}

#[component]
pub fn ProxyPoolsPage() -> impl IntoView {
    view! { <MutableJsonPage title="Proxy Pools" endpoint="/api/proxy-pools?includeUsage=true" create_endpoint="/api/proxy-pools" create_method="POST" /> }
}

#[component]
pub fn DbBackupsPage() -> impl IntoView {
    view! { <MutableJsonPage title="Database Backups" endpoint="/api/db-backups" create_endpoint="/api/db-backups" create_method="PUT" /> }
}

#[component]
pub fn SkillsPage() -> impl IntoView {
    const SKILLS: &[(&str, &str)] = &[
        ("Chat completions", "/v1/chat/completions"),
        ("Models", "/v1/models"),
        ("Web fetch", "/v1/web/fetch"),
    ];
    view! { <DashboardShell title="Skills"><div class="card-grid">{SKILLS.iter().map(|(name, endpoint)| view! { <div class="card provider-card"><strong>{*name}</strong><code>{*endpoint}</code></div> }).collect_view()}</div></DashboardShell> }
}

#[component]
fn JsonPage(title: &'static str, endpoint: &'static str) -> impl IntoView {
    let content = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let refresh = move |_| load_json(endpoint, content, error);
    load_json(endpoint, content, error);
    view! { <DashboardShell title=title><section class="card stack"><div class="row between"><p class="muted">{endpoint}</p><button class="button" on:click=refresh>"Refresh"</button></div><Show when=move || !error.get().is_empty()><p class="error">{move || error.get()}</p></Show><pre class="json-view">{move || content.get()}</pre></section></DashboardShell> }
}

#[component]
fn MutableJsonPage(
    title: &'static str,
    endpoint: &'static str,
    create_endpoint: &'static str,
    create_method: &'static str,
) -> impl IntoView {
    let content = RwSignal::new(String::new());
    let draft = RwSignal::new("{}".to_string());
    let message = RwSignal::new(String::new());
    load_json(endpoint, content, message);
    let create = move |_| {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&draft.get_untracked()) else {
            message.set("Payload must be valid JSON.".into());
            return;
        };
        spawn_local(async move {
            match api::send_json::<_, serde_json::Value>(create_method, create_endpoint, &value)
                .await
            {
                Ok(_) => {
                    message.set("Saved.".into());
                    load_json(endpoint, content, message);
                }
                Err(error) => message.set(error),
            }
        });
    };
    view! { <DashboardShell title=title><div class="stack"><section class="card stack"><h2>"Create"</h2><textarea class="settings-editor" bind:value=draft></textarea><button class="button primary" on:click=create>"Submit"</button></section><section class="card stack"><div class="row between"><h2>"Current data"</h2><button class="button" on:click=move |_| load_json(endpoint, content, message)>"Refresh"</button></div><Show when=move || !message.get().is_empty()><p>{move || message.get()}</p></Show><pre class="json-view">{move || content.get()}</pre></section></div></DashboardShell> }
}

fn load_json(endpoint: &'static str, content: RwSignal<String>, error: RwSignal<String>) {
    spawn_local(async move {
        match api::get_json::<serde_json::Value>(endpoint).await {
            Ok(value) => {
                content.set(serde_json::to_string_pretty(&value).unwrap_or_default());
                error.set(String::new());
            }
            Err(message) => error.set(message),
        }
    });
}
