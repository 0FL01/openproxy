use js_sys::Date;
use leptos::prelude::*;
use serde::Serialize;
use wasm_bindgen::JsValue;

use crate::browser::{storage_remove, storage_set, window};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CallbackData {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    timestamp: f64,
}

#[component]
pub fn CallbackPage() -> impl IntoView {
    let title = RwSignal::new("Processing authentication…".to_string());
    let message = RwSignal::new("Communicating with the OpenProxy dashboard.".to_string());
    let success = RwSignal::new(false);

    Effect::new(move || {
        let Some(window) = window() else {
            message.set("Browser window is unavailable.".to_string());
            return;
        };
        let Ok(params) =
            web_sys::UrlSearchParams::new_with_str(&window.location().search().unwrap_or_default())
        else {
            message.set("Invalid callback URL.".to_string());
            return;
        };

        let data = CallbackData {
            code: params.get("code"),
            state: params.get("state"),
            error: params.get("error"),
            error_description: params.get("error_description"),
            timestamp: Date::now(),
        };
        let value = serde_wasm_bindgen::to_value(&data).unwrap_or(JsValue::NULL);
        if let Ok(channel) = web_sys::BroadcastChannel::new("oauth_callback") {
            let _ = channel.post_message(&value);
            channel.close();
        }
        if let Ok(opener) = window.opener()
            && let Ok(opener) = opener.dyn_into::<web_sys::Window>()
        {
            let _ = opener.post_message(&value, &window.location().origin().unwrap_or_default());
        }
        if let Ok(json) = serde_json::to_string(&data) {
            let _ = storage_set("oauth_callback", &json);
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                wasm_bindgen::closure::Closure::once_into_js(storage_remove_callback)
                    .unchecked_ref(),
                1_000,
            );
        }

        if let Some(error) = data.error {
            title.set("Authentication failed".to_string());
            message.set(data.error_description.unwrap_or(error));
        } else if data.code.is_some() {
            title.set("Authentication complete".to_string());
            message.set("Return to the dashboard. If it does not complete automatically, paste this window's URL into the OAuth panel.".to_string());
            success.set(true);
        } else {
            title.set("No authentication data found".to_string());
            message.set("Copy this callback URL into the OpenProxy dashboard.".to_string());
        }
    });

    view! {
        <main class="centered-page callback-page">
            <div class="card stack">
                <h1 class:success=move || success.get()>{move || title.get()}</h1>
                <p>{move || message.get()}</p>
            </div>
        </main>
    }
}

fn storage_remove_callback() {
    storage_remove("oauth_callback");
}

use wasm_bindgen::JsCast;
