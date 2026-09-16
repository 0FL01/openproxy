use futures_util::StreamExt;
use gloo_net::http::Request;
use js_sys::Uint8Array;
use leptos::{prelude::*, task::spawn_local};
use wasm_streams::ReadableStream;

use super::DashboardShell;
use crate::{
    browser::{storage_get, storage_set},
    components::model_picker::ModelPicker,
    model_data::{load_model_state, ModelState},
};

#[component]
pub fn BasicChatPage() -> impl IntoView {
    let model_state = RwSignal::new(None::<ModelState>);
    let selected = RwSignal::new(Vec::<String>::new());
    let prompt = RwSignal::new(storage_get("basic-chat.draft").unwrap_or_default());
    let response = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let sending = RwSignal::new(false);
    let abort = RwSignal::new(None::<web_sys::AbortController>);

    spawn_local(async move {
        match load_model_state().await {
            Ok(value) => {
                if let Some(provider) = storage_get("basic-chat.activeProviderId")
                    && let Some(model) = value.inventory(&provider).enabled_rows.first()
                {
                    selected.set(vec![model.full_model.clone()]);
                }
                model_state.set(Some(value));
            }
            Err(message) => error.set(message),
        }
    });
    Effect::new(move || {
        let _ = storage_set("basic-chat.draft", &prompt.get());
        if let Some(full_model) = selected.get().first()
            && let Some(alias) = full_model.split('/').next()
            && let Some(state) = model_state.get()
            && let Some((provider, _)) = state
                .provider_aliases
                .iter()
                .find(|(_, candidate)| candidate.as_str() == alias)
        {
            let _ = storage_set("basic-chat.activeProviderId", provider);
        }
    });

    let send = move |_| {
        let Some(model) = selected.get_untracked().first().cloned() else {
            error.set("Select a model.".into());
            return;
        };
        let text = prompt.get_untracked().trim().to_string();
        if text.is_empty() {
            return;
        }
        let Ok(controller) = web_sys::AbortController::new() else {
            error.set("AbortController is unavailable.".into());
            return;
        };
        abort.set(Some(controller.clone()));
        response.set(String::new());
        error.set(String::new());
        sending.set(true);
        spawn_local(async move {
            let body = serde_json::json!({"model": model, "messages": [{"role":"user","content":text}], "stream": true});
            let result = stream_chat(body, controller, response).await;
            if let Err(message) = result {
                error.set(message);
            }
            sending.set(false);
            abort.set(None);
        });
    };
    let stop = move |_| {
        if let Some(controller) = abort.get_untracked() {
            controller.abort();
        }
    };

    view! {
        <DashboardShell title="Basic Chat">
            <div class="stack">
                {move || model_state.get().map(|state| view! { <section class="card"><ModelPicker state=state selected=selected title="Chat model" /></section> })}
                <section class="card stack">
                    <textarea rows="5" bind:value=prompt placeholder="Ask something…"></textarea>
                    <div class="row gap"><button class="button primary" on:click=send disabled=move || sending.get()>"Send"</button><button class="button danger" on:click=stop disabled=move || !sending.get()>"Stop"</button></div>
                    <Show when=move || !error.get().is_empty()><p class="error">{move || error.get()}</p></Show>
                    <div class="chat-response">{move || response.get()}</div>
                </section>
            </div>
        </DashboardShell>
    }
}

async fn stream_chat(
    body: serde_json::Value,
    controller: web_sys::AbortController,
    output: RwSignal<String>,
) -> Result<(), String> {
    let request = Request::post("/api/dashboard/chat/completions")
        .header("Accept", "text/event-stream")
        .abort_signal(Some(&controller.signal()))
        .json(&body)
        .map_err(|error| error.to_string())?;
    let response = request.send().await.map_err(|error| error.to_string())?;
    if !response.ok() {
        return Err(format!("Chat request failed ({})", response.status()));
    }
    let raw = response
        .body()
        .ok_or_else(|| "Streaming response has no body.".to_string())?;
    let mut stream = ReadableStream::from_raw(raw).into_stream();
    let mut buffer = Vec::<u8>::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("stream error: {error:?}"))?;
        buffer.extend(Uint8Array::new(&chunk).to_vec());
        while let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
            let line = buffer.drain(..=index).collect::<Vec<_>>();
            if let Ok(line) = std::str::from_utf8(&line) {
                append_sse_line(line, output);
            }
        }
    }
    if let Ok(line) = std::str::from_utf8(&buffer) {
        append_sse_line(line, output);
    }
    Ok(())
}

fn append_sse_line(line: &str, output: RwSignal<String>) {
    let Some(payload) = line.trim().strip_prefix("data:").map(str::trim) else {
        return;
    };
    if payload.is_empty() || payload == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return;
    };
    let text = value
        .pointer("/choices/0/delta/content")
        .and_then(|value| value.as_str())
        .or_else(|| {
            value
                .pointer("/choices/0/message/content")
                .and_then(|value| value.as_str())
        })
        .or_else(|| value.get("output_text").and_then(|value| value.as_str()));
    if let Some(text) = text {
        output.update(|current| current.push_str(text));
    }
}
