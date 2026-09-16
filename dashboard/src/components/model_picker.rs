use leptos::prelude::*;

use crate::model_data::ModelState;

#[component]
pub fn ModelPicker(
    state: ModelState,
    selected: RwSignal<Vec<String>>,
    #[prop(optional)] title: Option<&'static str>,
) -> impl IntoView {
    let query = RwSignal::new(String::new());
    let providers = state.provider_ids();

    view! {
        <div class="model-picker stack">
            <div class="row between">
                <h3>{title.unwrap_or("Select models")}</h3>
                <span class="badge">{move || format!("{} selected", selected.get().len())}</span>
            </div>
            <input type="search" placeholder="Search models…" bind:value=query />
            <div class="model-groups">
                {providers
                    .into_iter()
                    .map(|provider_id| {
                        let inventory = state.inventory(&provider_id);
                        let provider_name = humanize(&provider_id);
                        view! {
                            <section class="model-group">
                                <h4>{provider_name}</h4>
                                {inventory
                                    .enabled_rows
                                    .into_iter()
                                    .map(|model| {
                                        let value = model.full_model.clone();
                                        let search_value = format!("{} {}", model.name, model.full_model).to_lowercase();
                                        let checked_value = value.clone();
                                        let toggle_value = value.clone();
                                        view! {
                                            <label
                                                class="model-option"
                                                class:hidden=move || {
                                                let needle = query.get().trim().to_lowercase();
                                                !needle.is_empty() && !search_value.contains(&needle)
                                            }>
                                                <input
                                                    type="checkbox"
                                                    prop:checked=move || selected.get().contains(&checked_value)
                                                    on:change=move |_| {
                                                        selected.update(|models| {
                                                            if let Some(index) = models.iter().position(|item| item == &toggle_value) {
                                                                models.remove(index);
                                                            } else {
                                                                models.push(toggle_value.clone());
                                                            }
                                                        });
                                                    }
                                                />
                                                <span><strong>{model.name}</strong><small>{value}</small></span>
                                            </label>
                                        }
                                    })
                                    .collect_view()}
                            </section>
                        }
                    })
                    .collect_view()}
            </div>
        </div>
    }
}

fn humanize(id: &str) -> String {
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
