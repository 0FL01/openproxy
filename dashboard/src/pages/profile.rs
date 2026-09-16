use leptos::{prelude::*, task::spawn_local};
use serde::Serialize;

use super::DashboardShell;
use crate::{api, browser::redirect};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PasswordRequest {
    current_password: String,
    new_password: String,
}

#[component]
pub fn ProfilePage() -> impl IntoView {
    let settings = RwSignal::new(String::new());
    let current_password = RwSignal::new(String::new());
    let new_password = RwSignal::new(String::new());
    let message = RwSignal::new(String::new());
    load_settings(settings, message);

    let save_settings = move |_| {
        let raw = settings.get_untracked();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            message.set("Settings must be valid JSON.".into());
            return;
        };
        spawn_local(async move {
            match api::send_json::<_, serde_json::Value>("PATCH", "/api/settings", &value).await {
                Ok(_) => message.set("Settings saved.".into()),
                Err(error) => message.set(error),
            }
        });
    };
    let change_password = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if new_password.get_untracked().len() < 8 {
            message.set("New password must be at least 8 characters.".into());
            return;
        }
        let body = PasswordRequest {
            current_password: current_password.get_untracked(),
            new_password: new_password.get_untracked(),
        };
        spawn_local(async move {
            match api::send_json::<_, serde_json::Value>("POST", "/api/auth/password", &body).await
            {
                Ok(_) => {
                    message.set("Password updated.".into());
                    current_password.set(String::new());
                    new_password.set(String::new());
                }
                Err(error) => message.set(error),
            }
        });
    };
    let logout = move |_| {
        spawn_local(async move {
            let _ = api::send_empty("POST", "/api/auth/logout").await;
            redirect("/login");
        })
    };

    view! {
        <DashboardShell title="Profile">
            <div class="stack">
                <section class="card stack"><h2>"Settings"</h2><textarea class="settings-editor" bind:value=settings spellcheck="false"></textarea><button class="button primary" on:click=save_settings>"Save settings"</button></section>
                <form class="card stack" on:submit=change_password><h2>"Change password"</h2><label><span>"Current password"</span><input type="password" bind:value=current_password /></label><label><span>"New password"</span><input type="password" bind:value=new_password /></label><button class="button primary" type="submit">"Update password"</button></form>
                <Show when=move || !message.get().is_empty()><p>{move || message.get()}</p></Show>
                <button class="button danger" on:click=logout>"Log out"</button>
            </div>
        </DashboardShell>
    }
}

fn load_settings(settings: RwSignal<String>, message: RwSignal<String>) {
    spawn_local(async move {
        match api::get_json::<serde_json::Value>("/api/settings").await {
            Ok(value) => settings.set(serde_json::to_string_pretty(&value).unwrap_or_default()),
            Err(error) => message.set(error),
        }
    });
}
