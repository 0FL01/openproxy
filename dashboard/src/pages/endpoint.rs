use leptos::{prelude::*, task::spawn_local};
use serde::{Deserialize, Serialize};

use super::DashboardShell;
use crate::api;

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiKey {
    id: String,
    name: String,
    key: String,
    #[serde(default = "default_true")]
    is_active: bool,
}

#[derive(Default, Deserialize)]
struct KeysResponse {
    #[serde(default)]
    keys: Vec<ApiKey>,
}

#[derive(Serialize)]
struct CreateKey<'a> {
    name: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToggleKey {
    is_active: bool,
}

#[component]
pub fn EndpointPage() -> impl IntoView {
    let keys = RwSignal::new(Vec::<ApiKey>::new());
    let name = RwSignal::new(String::new());
    let message = RwSignal::new(String::new());
    let base_url = web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .map(|origin| format!("{origin}/v1"))
        .unwrap_or_else(|| "http://127.0.0.1:4623/v1".to_string());
    load_keys(keys, message);

    let create = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        let value = name.get_untracked();
        if value.trim().is_empty() {
            return;
        }
        spawn_local(async move {
            match api::send_json::<_, serde_json::Value>(
                "POST",
                "/api/keys",
                &CreateKey { name: value.trim() },
            )
            .await
            {
                Ok(_) => {
                    name.set(String::new());
                    load_keys(keys, message);
                }
                Err(error) => message.set(error),
            }
        });
    };

    view! {
        <DashboardShell title="Endpoint">
            <div class="stack">
                <section class="card stack"><h2>"OpenAI-compatible endpoint"</h2><code>{base_url}</code></section>
                <section class="card stack">
                    <div class="row between"><h2>"API keys"</h2><span class="badge">{move || keys.get().len()}</span></div>
                    <form class="row gap" on:submit=create><input bind:value=name placeholder="Key name" /><button class="button primary" type="submit">"Create"</button></form>
                    <Show when=move || !message.get().is_empty()><p>{move || message.get()}</p></Show>
                    {move || keys.get().into_iter().map(|key| {
                        let toggle_id = key.id.clone(); let delete_id = key.id.clone(); let active = key.is_active;
                        view! { <div class="list-row"><div><strong>{key.name}</strong><small>{mask_key(&key.key)}</small></div><div class="row gap"><button class="button" on:click=move |_| {
                            let path = format!("/api/keys/{toggle_id}"); let body = ToggleKey { is_active: !active };
                            spawn_local(async move { let _ = api::send_json::<_, serde_json::Value>("PUT", &path, &body).await; load_keys(keys, message); });
                        }>{if active { "Pause" } else { "Enable" }}</button><button class="button danger" on:click=move |_| {
                            let path = format!("/api/keys/{delete_id}"); spawn_local(async move { let _ = api::send_empty("DELETE", &path).await; load_keys(keys, message); });
                        }>"Delete"</button></div></div> }
                    }).collect_view()}
                </section>
            </div>
        </DashboardShell>
    }
}

fn load_keys(keys: RwSignal<Vec<ApiKey>>, message: RwSignal<String>) {
    spawn_local(async move {
        match api::get_json::<KeysResponse>("/api/keys").await {
            Ok(value) => keys.set(value.keys),
            Err(error) => message.set(error),
        }
    });
}

fn mask_key(key: &str) -> String {
    if key.len() > 8 {
        format!("{}…", &key[..8])
    } else {
        key.to_string()
    }
}

fn default_true() -> bool {
    true
}
