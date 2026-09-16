use leptos::{prelude::*, task::spawn_local};
use leptos_router::hooks::use_params_map;
use serde::{Deserialize, Serialize};

use super::DashboardShell;
use crate::{
    api,
    components::model_picker::ModelPicker,
    model_data::{load_model_state, ModelState},
    pages::cli_tools::tool_settings_endpoint,
};

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenCodeStatus {
    #[serde(default)]
    installed: bool,
    #[serde(default)]
    has_open_proxy: bool,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    active_model: Option<String>,
    #[serde(default)]
    codex_web_search: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplyRequest {
    base_url: String,
    api_key: String,
    models: Vec<String>,
    active_model: String,
    subagent_model: String,
    codex_web_search: bool,
}

#[component]
pub fn CliToolPage() -> impl IntoView {
    let params = use_params_map();
    let tool_id = params.read().get("id").unwrap_or_default();
    if tool_id != "opencode" {
        return view! { <GenericCliToolPage tool_id=tool_id /> }.into_any();
    }
    view! { <OpenCodePage /> }.into_any()
}

#[component]
fn GenericCliToolPage(tool_id: String) -> impl IntoView {
    let endpoint = tool_settings_endpoint(&tool_id);
    let status = RwSignal::new(String::new());
    let payload = RwSignal::new("{}".to_string());
    let message = RwSignal::new(String::new());
    if let Some(endpoint) = endpoint.clone() {
        load_generic(endpoint, status, message);
    }
    let apply_endpoint = endpoint.clone();
    let reset_endpoint = endpoint.clone();
    let apply = move |_| {
        let Some(endpoint) = apply_endpoint.clone() else {
            return;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload.get_untracked()) else {
            message.set("Payload must be valid JSON.".into());
            return;
        };
        spawn_local(async move {
            match api::send_json::<_, serde_json::Value>("POST", &endpoint, &value).await {
                Ok(_) => {
                    message.set("Settings applied.".into());
                    load_generic(endpoint, status, message);
                }
                Err(error) => message.set(error),
            }
        });
    };
    let reset = move |_| {
        let Some(endpoint) = reset_endpoint.clone() else {
            return;
        };
        spawn_local(async move {
            match api::send_empty("DELETE", &endpoint).await {
                Ok(()) => {
                    message.set("Settings reset.".into());
                    load_generic(endpoint, status, message);
                }
                Err(error) => message.set(error),
            }
        });
    };
    view! { <DashboardShell title="CLI Tool"><div class="stack"><section class="card stack"><h2>{tool_id}</h2><p class="muted">"Current status"</p><pre class="json-view">{move || status.get()}</pre></section><section class="card stack"><h2>"Apply settings"</h2><textarea class="settings-editor" bind:value=payload></textarea><div class="row gap"><button class="button primary" on:click=apply>"Apply JSON"</button><button class="button danger" on:click=reset>"Reset"</button></div><Show when=move || !message.get().is_empty()><p>{move || message.get()}</p></Show></section></div></DashboardShell> }
}

fn load_generic(endpoint: String, status: RwSignal<String>, message: RwSignal<String>) {
    spawn_local(async move {
        match api::get_json::<serde_json::Value>(&endpoint).await {
            Ok(value) => status.set(serde_json::to_string_pretty(&value).unwrap_or_default()),
            Err(error) => message.set(error),
        }
    });
}

#[component]
fn OpenCodePage() -> impl IntoView {
    let model_state = RwSignal::new(None::<ModelState>);
    let status = RwSignal::new(None::<OpenCodeStatus>);
    let selected = RwSignal::new(Vec::<String>::new());
    let base_url = RwSignal::new(default_base_url());
    let api_key = RwSignal::new("sk_openproxy".to_string());
    let active_model = RwSignal::new(String::new());
    let subagent_model = RwSignal::new(String::new());
    let codex_web_search = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let saving = RwSignal::new(false);

    refresh(
        model_state,
        status,
        selected,
        active_model,
        codex_web_search,
        message,
    );

    let apply = move |_| {
        let models = selected.get_untracked();
        if models.is_empty() {
            message.set("Select at least one model.".to_string());
            return;
        }
        saving.set(true);
        let active = if active_model.get_untracked().is_empty() {
            models[0].clone()
        } else {
            active_model.get_untracked()
        };
        let request = ApplyRequest {
            base_url: normalize_base_url(&base_url.get_untracked()),
            api_key: api_key.get_untracked(),
            models,
            active_model: active,
            subagent_model: subagent_model.get_untracked(),
            codex_web_search: codex_web_search.get_untracked(),
        };
        spawn_local(async move {
            match api::send_json::<_, serde_json::Value>(
                "POST",
                "/api/cli-tools/opencode-settings",
                &request,
            )
            .await
            {
                Ok(_) => message.set("OpenCode settings applied.".to_string()),
                Err(error) => message.set(error),
            }
            saving.set(false);
        });
    };
    let reset = move |_| {
        spawn_local(async move {
            match api::send_empty("DELETE", "/api/cli-tools/opencode-settings").await {
                Ok(()) => {
                    selected.set(Vec::new());
                    active_model.set(String::new());
                    subagent_model.set(String::new());
                    message.set("OpenCode settings reset.".into());
                }
                Err(error) => message.set(error),
            }
        })
    };

    view! {
        <DashboardShell title="OpenCode">
            <div class="stack">
                {move || status.get().map(|status| view! { <div class="row gap"><span class="badge">{if status.installed { "Installed" } else { "Not installed" }}</span><span class="badge">{if status.has_open_proxy { "Configured" } else { "Not configured" }}</span></div> })}
                <section class="card stack">
                    <label><span>"Base URL"</span><input bind:value=base_url /></label>
                    <label><span>"API key"</span><input type="password" bind:value=api_key /></label>
                    {move || model_state.get().map(|state| view! { <ModelPicker state=state selected=selected title="Models" /> })}
                    <label><span>"Active model"</span><select bind:value=active_model><option value="">"First selected model"</option>{move || selected.get().into_iter().map(|model| view! { <option value=model.clone()>{model.clone()}</option> }).collect_view()}</select></label>
                    <label><span>"Explorer subagent model"</span><select bind:value=subagent_model><option value="">"Use active model"</option>{move || selected.get().into_iter().map(|model| view! { <option value=model.clone()>{model.clone()}</option> }).collect_view()}</select></label>
                    <label class="checkbox"><input type="checkbox" bind:checked=codex_web_search /><span>"Enable Codex web search header"</span></label>
                    <Show when=move || !message.get().is_empty()><p>{move || message.get()}</p></Show>
                    <div class="row gap"><button class="button primary" on:click=apply disabled=move || saving.get()>"Apply"</button><button class="button" on:click=reset>"Reset"</button></div>
                </section>
            </div>
        </DashboardShell>
    }
}

fn refresh(
    model_state: RwSignal<Option<ModelState>>,
    status: RwSignal<Option<OpenCodeStatus>>,
    selected: RwSignal<Vec<String>>,
    active: RwSignal<String>,
    codex: RwSignal<bool>,
    message: RwSignal<String>,
) {
    spawn_local(async move {
        match load_model_state().await {
            Ok(value) => model_state.set(Some(value)),
            Err(error) => message.set(error),
        }
        if let Ok(value) = api::get_json::<OpenCodeStatus>("/api/cli-tools/opencode-settings").await
        {
            selected.set(value.models.clone());
            active.set(value.active_model.clone().unwrap_or_default());
            codex.set(value.codex_web_search);
            status.set(Some(value));
        }
    });
}

fn default_base_url() -> String {
    web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .unwrap_or_else(|| "http://127.0.0.1:4623".into())
}

fn normalize_base_url(value: &str) -> String {
    let value = value.trim_end_matches('/');
    if value.ends_with("/v1") {
        value.to_string()
    } else {
        format!("{value}/v1")
    }
}
