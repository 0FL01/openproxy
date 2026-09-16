use gloo_net::http::Request;
use leptos::{prelude::*, task::spawn_local};
use serde::{Deserialize, Serialize};

use super::DashboardShell;
use crate::api;

const STEPS: &[(u8, &str, &str)] = &[
    (1, "Client Request", "1_req_client.json"),
    (2, "Source Body", "2_req_source.json"),
    (3, "OpenAI Intermediate", "3_req_openai.json"),
    (4, "Target Request", "4_req_target.json"),
    (5, "Provider Response", "5_res_provider.txt"),
    (6, "OpenAI Response", "6_res_openai.txt"),
    (7, "Client Response", "7_res_client.txt"),
];

#[derive(Default, Deserialize)]
struct LoadResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    content: String,
    error: Option<String>,
}

#[derive(Serialize)]
struct SaveRequest<'a> {
    file: &'a str,
    content: &'a str,
}

#[component]
pub fn TranslatorPage() -> impl IntoView {
    let selected = RwSignal::new("1".to_string());
    let content = RwSignal::new(String::new());
    let message = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    let load = move |_| {
        let step = selected.get_untracked().parse::<u8>().unwrap_or(1);
        let file = step_file(step);
        busy.set(true);
        spawn_local(async move {
            match api::get_json::<LoadResponse>(&format!("/api/translator/load?file={file}")).await
            {
                Ok(response) if response.success => {
                    content.set(response.content);
                    message.set("Loaded.".into());
                }
                Ok(response) => {
                    message.set(response.error.unwrap_or_else(|| "File not found".into()))
                }
                Err(error) => message.set(error),
            }
            busy.set(false);
        });
    };
    let save = move |_| {
        let step = selected.get_untracked().parse::<u8>().unwrap_or(1);
        let value = content.get_untracked();
        let file = step_file(step);
        busy.set(true);
        spawn_local(async move {
            match api::send_json::<_, serde_json::Value>(
                "POST",
                "/api/translator/save",
                &SaveRequest {
                    file,
                    content: &value,
                },
            )
            .await
            {
                Ok(_) => message.set("Saved.".into()),
                Err(error) => message.set(error),
            }
            busy.set(false);
        });
    };
    let format_json =
        move |_| match serde_json::from_str::<serde_json::Value>(&content.get_untracked()) {
            Ok(value) => content.set(serde_json::to_string_pretty(&value).unwrap_or_default()),
            Err(error) => message.set(format!("Invalid JSON: {error}")),
        };
    let translate = move |_| {
        let raw = content.get_untracked();
        let Ok(body) = serde_json::from_str::<serde_json::Value>(&raw) else {
            message.set("Translation input must be valid JSON.".into());
            return;
        };
        busy.set(true);
        spawn_local(async move {
            let result = async {
                let detected = translate_step(serde_json::json!({"step": 1, "body": body})).await?;
                let metadata = detected.get("result").cloned().unwrap_or_default();
                let provider = metadata.get("provider").cloned().unwrap_or_default();
                let model = metadata.get("model").cloned().unwrap_or_default();
                let openai = translate_step(serde_json::json!({"step": 2, "body": body})).await?;
                let openai_body = openai.pointer("/result/body").cloned().unwrap_or_default();
                translate_step(serde_json::json!({"step": 3, "provider": provider, "model": model, "body": openai_body})).await
            }
            .await;
            match result {
                Ok(value)
                    if value.get("success").and_then(|value| value.as_bool()) == Some(true) =>
                {
                    let translated = value.get("result").cloned().unwrap_or_default();
                    content.set(serde_json::to_string_pretty(&translated).unwrap_or_default());
                    message.set("Translated.".into());
                }
                Ok(value) => message.set(
                    value
                        .get("error")
                        .and_then(|value| value.as_str())
                        .unwrap_or("Translation failed")
                        .to_string(),
                ),
                Err(error) => message.set(error),
            }
            busy.set(false);
        });
    };

    view! {
        <DashboardShell title="Translator">
            <div class="stack">
                <section class="card stack">
                    <label><span>"Pipeline step"</span><select bind:value=selected>{STEPS.iter().map(|(id, label, _)| view! { <option value=id.to_string()>{format!("{id}. {label}")}</option> }).collect_view()}</select></label>
                    <textarea class="translator-editor" bind:value=content spellcheck="false" aria-label="Translator content"></textarea>
                    <div class="row gap wrap"><button class="button" on:click=load disabled=move || busy.get()>"Load"</button><button class="button" on:click=format_json>"Format JSON"</button><button class="button" on:click=save disabled=move || busy.get()>"Save"</button><button class="button primary" on:click=translate disabled=move || busy.get()>"Translate"</button></div>
                    <Show when=move || !message.get().is_empty()><p>{move || message.get()}</p></Show>
                </section>
            </div>
        </DashboardShell>
    }
}

fn step_file(step: u8) -> &'static str {
    STEPS
        .iter()
        .find(|(id, _, _)| *id == step)
        .map(|(_, _, file)| *file)
        .unwrap_or(STEPS[0].2)
}

async fn translate_step(body: serde_json::Value) -> Result<serde_json::Value, String> {
    let response = Request::post("/api/translator/translate")
        .json(&body)
        .map_err(|error| error.to_string())?
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.ok() {
        return Err(format!("HTTP {}", response.status()));
    }
    let value = response
        .json::<serde_json::Value>()
        .await
        .map_err(|error| error.to_string())?;
    if value.get("success").and_then(|value| value.as_bool()) == Some(true) {
        Ok(value)
    } else {
        Err(value
            .get("error")
            .and_then(|value| value.as_str())
            .unwrap_or("Translation failed")
            .to_string())
    }
}
