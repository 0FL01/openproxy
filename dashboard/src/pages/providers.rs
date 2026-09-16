use std::collections::BTreeMap;

use leptos::{prelude::*, task::spawn_local};
use leptos_router::components::A;
use serde::{Deserialize, Serialize};

use super::DashboardShell;
use crate::{api, browser::redirect, model_data::load_model_state};

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogResponse {
    #[serde(default)]
    provider_id_to_alias: BTreeMap<String, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateProviderRequest {
    provider: String,
    name: String,
    api_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
}

#[component]
pub fn ProvidersPage() -> impl IntoView {
    let state = RwSignal::new(None);
    let error = RwSignal::new(String::new());
    let query = RwSignal::new(String::new());

    Effect::new(move || {
        spawn_local(async move {
            match load_model_state().await {
                Ok(value) => state.set(Some(value)),
                Err(message) => error.set(message),
            }
        });
    });

    view! {
        <DashboardShell title="Providers">
            <div class="stack">
                <div class="row between wrap">
                    <input class="search" type="search" placeholder="Search providers…" bind:value=query />
                    <A href="/dashboard/providers/new" attr:class="button primary">"Add provider"</A>
                </div>
                <Show when=move || !error.get().is_empty()>
                    <p class="error">{move || error.get()}</p>
                </Show>
                <div class="card-grid">
                    {move || state.get().map(|state| {
                        let mut providers = state.provider_aliases.into_iter().collect::<Vec<_>>();
                        providers.sort_by(|left, right| left.0.cmp(&right.0));
                        providers.into_iter().map(|(id, alias)| {
                            let count = state.connections.iter().filter(|connection| connection.provider == id).count();
                            let search = format!("{id} {alias}").to_lowercase();
                            let href = format!("/dashboard/providers/{id}");
                            view! {
                                <A
                                    href=href
                                    attr:class="card provider-card"
                                    class:hidden=move || {
                                    let needle = query.get().trim().to_lowercase();
                                    !needle.is_empty() && !search.contains(&needle)
                                }>
                                    <div class="row between">
                                        <strong>{humanize(&id)}</strong>
                                        <span class="badge">{count}</span>
                                    </div>
                                    <small class="muted">{format!("alias: {alias}")}</small>
                                </A>
                            }
                        }).collect_view()
                    })}
                </div>
            </div>
        </DashboardShell>
    }
}

#[component]
pub fn ProviderNewPage() -> impl IntoView {
    let provider = RwSignal::new(String::new());
    let name = RwSignal::new(String::new());
    let api_key = RwSignal::new(String::new());
    let base_url = RwSignal::new(String::new());
    let provider_ids = RwSignal::new(Vec::<String>::new());
    let error = RwSignal::new(String::new());
    let saving = RwSignal::new(false);

    Effect::new(move || {
        spawn_local(async move {
            if let Ok(catalog) = api::get_json::<CatalogResponse>("/api/catalog").await {
                provider_ids.set(catalog.provider_id_to_alias.into_keys().collect());
            }
        });
    });

    let submit = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if provider.get_untracked().is_empty()
            || name.get_untracked().is_empty()
            || api_key.get_untracked().is_empty()
        {
            error.set("Provider, name, and API key are required.".to_string());
            return;
        }
        saving.set(true);
        error.set(String::new());
        spawn_local(async move {
            let request = CreateProviderRequest {
                provider: provider.get_untracked(),
                name: name.get_untracked(),
                api_key: api_key.get_untracked(),
                base_url: (!base_url.get_untracked().trim().is_empty())
                    .then(|| base_url.get_untracked()),
            };
            match api::send_json::<_, serde_json::Value>("POST", "/api/providers", &request).await {
                Ok(_) => redirect(&format!("/dashboard/providers/{}", request.provider)),
                Err(message) => error.set(message),
            }
            saving.set(false);
        });
    };

    view! {
        <DashboardShell title="Add Provider">
            <form class="card stack" on:submit=submit>
                <label><span>"Provider"</span><select bind:value=provider>
                    <option value="">"Select provider"</option>
                    {move || provider_ids.get().into_iter().map(|id| view! { <option value=id.clone()>{humanize(&id)}</option> }).collect_view()}
                </select></label>
                <label><span>"Connection name"</span><input bind:value=name placeholder="Primary" /></label>
                <label><span>"API key or token"</span><input type="password" bind:value=api_key /></label>
                <label><span>"Base URL (optional)"</span><input bind:value=base_url placeholder="https://api.example.com/v1" /></label>
                <Show when=move || !error.get().is_empty()><p class="error">{move || error.get()}</p></Show>
                <button class="button primary" type="submit" disabled=move || saving.get()>{move || if saving.get() { "Saving…" } else { "Save connection" }}</button>
            </form>
        </DashboardShell>
    }
}

pub(super) fn humanize(id: &str) -> String {
    id.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}
