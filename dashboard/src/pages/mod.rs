pub mod callback;
pub mod chat;
pub mod cli_tools;
pub mod console_log;
pub mod data_pages;
pub mod endpoint;
pub mod login;
pub mod not_found;
pub mod opencode;
pub mod placeholder;
pub mod profile;
pub mod provider_detail;
pub mod providers;
pub mod translator;

use leptos::{prelude::*, task::spawn_local};
use leptos_router::components::A;
use serde::Deserialize;

use crate::{api, browser::redirect};

const NAV_ITEMS: &[(&str, &str)] = &[
    ("Endpoint", "/dashboard/endpoint"),
    ("Providers", "/dashboard/providers"),
    ("CLI Tools", "/dashboard/cli-tools"),
    ("Quota", "/dashboard/quota"),
    ("Proxy Pools", "/dashboard/proxy-pools"),
    ("DB Backups", "/dashboard/db-backups"),
    ("Logs", "/dashboard/logs"),
    ("Console", "/dashboard/console-log"),
    ("Translator", "/dashboard/translator"),
    ("Profile", "/dashboard/profile"),
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    #[serde(default)]
    authenticated: bool,
    #[serde(default)]
    require_login: bool,
}

#[component]
pub fn DashboardShell(title: &'static str, children: Children) -> impl IntoView {
    let theme = expect_context::<RwSignal<String>>();
    spawn_local(async move {
        if let Ok(status) = api::get_json::<AuthStatus>("/api/auth/status").await
            && status.require_login
            && !status.authenticated
        {
            redirect("/login");
        }
    });
    let toggle_theme = move |_| {
        theme.update(|value| {
            *value = match value.as_str() {
                "light" => "dark",
                "dark" => "system",
                _ => "light",
            }
            .to_string()
        });
    };
    let logout = move |_| {
        spawn_local(async move {
            let _ = api::send_empty("POST", "/api/auth/logout").await;
            redirect("/login");
        });
    };
    view! {
        <div class="dashboard-shell">
            <aside class="sidebar">
                <A href="/dashboard" attr:class="brand">"OpenProxy"</A>
                <nav>
                    {NAV_ITEMS
                        .iter()
                        .map(|(label, href)| view! { <A href=*href>{*label}</A> })
                        .collect_view()}
                </nav>
            </aside>
            <main class="dashboard-main">
                <header class="page-header"><h1>{title}</h1><div class="row gap"><button class="button" on:click=toggle_theme>{move || format!("Theme: {}", theme.get())}</button><button class="button" on:click=logout>"Log out"</button></div></header>
                <section class="page-content">{children()}</section>
            </main>
        </div>
    }
}
