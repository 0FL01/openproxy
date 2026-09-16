use leptos::{prelude::*, task::spawn_local};
use leptos_router::hooks::use_params_map;
use serde::Serialize;

use super::{providers::humanize, DashboardShell};
use crate::{
    api,
    components::oauth_panel::OAuthPanel,
    model_data::{load_model_state, ModelState},
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DisabledRequest {
    provider_alias: String,
    ids: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CustomModelRequest {
    provider_alias: String,
    id: String,
    #[serde(rename = "type")]
    model_type: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ActiveRequest {
    is_active: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FilterRequest {
    alias: String,
    free_only: bool,
}

#[component]
pub fn ProviderDetailPage() -> impl IntoView {
    let params = use_params_map();
    let provider_id = params.read().get("id").unwrap_or_default();
    let title: &'static str = "Provider";
    let state = RwSignal::new(None::<ModelState>);
    let error = RwSignal::new(String::new());
    let custom_id = RwSignal::new(String::new());
    let refresh_callback = Callback::new(move |_| load_state(state, error));
    let detail_provider_id = provider_id.clone();

    load_state(state, error);

    let disable = move |alias: String, id: String| {
        spawn_local(async move {
            let request = DisabledRequest {
                provider_alias: alias,
                ids: vec![id],
            };
            if let Err(message) =
                api::send_json::<_, serde_json::Value>("POST", "/api/models/disabled", &request)
                    .await
            {
                error.set(message);
            }
            load_state(state, error);
        });
    };
    let enable = move |alias: String, id: String| {
        spawn_local(async move {
            let path = format!(
                "/api/models/disabled?providerAlias={}&id={}",
                urlencoding::encode(&alias),
                urlencoding::encode(&id)
            );
            if let Err(message) = api::send_empty("DELETE", &path).await {
                error.set(message);
            }
            load_state(state, error);
        });
    };
    let add_custom = Callback::new({
        let provider_id = provider_id.clone();
        move |event: web_sys::SubmitEvent| {
            event.prevent_default();
            let id = custom_id.get_untracked().trim().to_string();
            let Some(model_state) = state.get_untracked() else {
                return;
            };
            if id.is_empty() {
                return;
            }
            let request = CustomModelRequest {
                provider_alias: model_state.alias(&provider_id),
                id,
                model_type: "llm".into(),
            };
            spawn_local(async move {
                match api::send_json::<_, serde_json::Value>("POST", "/api/models/custom", &request)
                    .await
                {
                    Ok(_) => custom_id.set(String::new()),
                    Err(message) => error.set(message),
                }
                load_state(state, error);
            });
        }
    });

    view! {
        <DashboardShell title=title>
            <div class="stack">
                <div><p class="eyebrow">"PROVIDER"</p><h2>{humanize(&provider_id)}</h2></div>
                <Show when=move || !error.get().is_empty()><p class="error">{move || error.get()}</p></Show>
                {move || state.get().map(|model_state| {
                    let add_custom = add_custom.clone();
                    let alias = model_state.alias(&detail_provider_id);
                    let free_only = model_state.free_only(&detail_provider_id);
                    let inventory = model_state.inventory(&detail_provider_id);
                    let connections = model_state.connections.iter().filter(|connection| connection.provider == detail_provider_id).cloned().collect::<Vec<_>>();
                    view! {
                        <section class="card stack">
                            <div class="row between"><h3>"Connections"</h3><span class="badge">{connections.len()}</span></div>
                            {connections.into_iter().map(|connection| {
                                let id = connection.id.clone();
                                let active = connection.is_active;
                                let test_id = connection.id.clone();
                                let delete_id = connection.id.clone();
                                view! {
                                    <div class="list-row">
                                        <div>
                                            <strong>{connection.name.unwrap_or_else(|| connection.id.clone())}</strong>
                                            <small>{format!("{}{}{}", connection.auth_type.unwrap_or_default(), connection.email.map(|email| format!(" · {email}")).unwrap_or_default(), connection.test_status.map(|status| format!(" · {status}")).unwrap_or_default())}</small>
                                        </div>
                                        <div class="row gap"><button class="button" on:click=move |_| {
                                            let request = ActiveRequest { is_active: !active };
                                            let path = format!("/api/providers/{id}");
                                            spawn_local(async move {
                                                if let Err(message) = api::send_json::<_, serde_json::Value>("PUT", &path, &request).await { error.set(message); }
                                                load_state(state, error);
                                            });
                                        }>{if active { "Disable" } else { "Enable" }}</button><button class="button" on:click=move |_| {
                                            let path = format!("/api/providers/{test_id}/test");
                                            spawn_local(async move { match api::send_empty("POST", &path).await { Ok(()) => error.set("Connection test completed.".into()), Err(message) => error.set(message) } });
                                        }>"Test"</button><button class="button danger" on:click=move |_| {
                                            let path = format!("/api/providers/{delete_id}");
                                            spawn_local(async move { if let Err(message) = api::send_empty("DELETE", &path).await { error.set(message); } load_state(state, error); });
                                        }>"Delete"</button></div>
                                    </div>
                                }
                            }).collect_view()}
                        </section>
                        <section class="card stack">
                            <div class="row between"><h3>"Available Models"</h3><span class="badge">{inventory.enabled_rows.len()}</span></div>
                            <label class="checkbox"><input type="checkbox" prop:checked=free_only on:change={
                                let filter_alias = alias.clone();
                                move |_| {
                                    let request = FilterRequest { alias: filter_alias.clone(), free_only: !free_only };
                                    spawn_local(async move { if let Err(message) = api::send_json::<_, serde_json::Value>("PUT", "/api/providers/filters", &request).await { error.set(message); } load_state(state, error); });
                                }
                            } /><span>"Show free core models only"</span></label>
                            <form class="row" on:submit=move |event| add_custom.run(event)><input bind:value=custom_id placeholder="Custom model ID" /><button class="button primary" type="submit">"Add"</button></form>
                            <div class="model-list">
                                {inventory.enabled_rows.into_iter().map(|model| {
                                    let disable_alias = alias.clone();
                                    let disable_id = model.id.clone();
                                    view! { <div class="list-row"><div><strong>{model.name}</strong><small>{model.full_model}</small></div><button class="button" on:click=move |_| disable(disable_alias.clone(), disable_id.clone())>"Disable"</button></div> }
                                }).collect_view()}
                                {inventory.disabled_core_rows.into_iter().map(|model| {
                                    let enable_alias = alias.clone();
                                    let enable_id = model.id.clone();
                                    view! { <div class="list-row disabled"><div><strong>{model.name}</strong><small>{model.full_model}</small></div><button class="button" on:click=move |_| enable(enable_alias.clone(), enable_id.clone())>"Enable"</button></div> }
                                }).collect_view()}
                            </div>
                        </section>
                    }
                })}
                <OAuthPanel provider_id=provider_id.clone() on_success=refresh_callback />
            </div>
        </DashboardShell>
    }
}

fn load_state(state: RwSignal<Option<ModelState>>, error: RwSignal<String>) {
    spawn_local(async move {
        match load_model_state().await {
            Ok(value) => state.set(Some(value)),
            Err(message) => error.set(message),
        }
    });
}
