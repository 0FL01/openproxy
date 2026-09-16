use leptos::{prelude::*, task::spawn_local};
use serde::{Deserialize, Serialize};
use wasm_bindgen::{closure::Closure, JsCast};

use crate::{api, browser::window};

const DEVICE_PROVIDERS: &[&str] = &[
    "github",
    "qwen",
    "kiro",
    "kimi",
    "kimi-coding",
    "kilocode",
    "codebuddy",
    "codebuddy-cn",
    "grok-cli",
    "kimchi",
];

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizeResponse {
    #[serde(default)]
    auth_url: String,
    #[serde(default)]
    redirect_uri: String,
    #[serde(default)]
    code_verifier: String,
    #[serde(default)]
    state: String,
}

#[derive(Clone, Default, Deserialize)]
struct DeviceResponse {
    #[serde(default)]
    device_code: String,
    #[serde(default)]
    user_code: String,
    #[serde(default)]
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default, rename = "codeVerifier")]
    code_verifier: String,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExchangeRequest {
    code: String,
    redirect_uri: String,
    code_verifier: String,
    state: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PollRequest {
    device_code: String,
    code_verifier: String,
    extra_data: serde_json::Value,
}

#[derive(Deserialize)]
struct CallbackData {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    #[serde(rename = "errorDescription")]
    error_description: Option<String>,
}

#[component]
pub fn OAuthPanel(provider_id: String, on_success: Callback<()>) -> impl IntoView {
    let authorize = RwSignal::new(None::<AuthorizeResponse>);
    let device = RwSignal::new(None::<DeviceResponse>);
    let callback_url = RwSignal::new(String::new());
    let message = RwSignal::new(String::new());
    let busy = RwSignal::new(false);

    if let Ok(channel) = web_sys::BroadcastChannel::new("oauth_callback") {
        let callback = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
            move |event: web_sys::MessageEvent| {
                let Ok(data) = serde_wasm_bindgen::from_value::<CallbackData>(event.data()) else {
                    return;
                };
                if let Some(error) = data.error {
                    message.set(data.error_description.unwrap_or(error));
                    return;
                }
                if let Some(code) = data.code {
                    let state = data
                        .state
                        .map(|state| format!("&state={}", urlencoding::encode(&state)))
                        .unwrap_or_default();
                    callback_url.set(format!(
                        "https://callback.invalid/?code={}{}",
                        urlencoding::encode(&code),
                        state
                    ));
                    message.set("OAuth callback received. Complete the connection.".into());
                }
            },
        );
        channel.set_onmessage(Some(callback.as_ref().unchecked_ref()));
        let resource = send_wrapper::SendWrapper::new((channel, callback));
        on_cleanup(move || {
            resource.0.set_onmessage(None);
            resource.0.close();
        });
    }

    let start_provider = provider_id.clone();
    let start = move |_| {
        let provider = start_provider.clone();
        busy.set(true);
        spawn_local(async move {
            if DEVICE_PROVIDERS.contains(&provider.as_str()) {
                let path = format!("/api/oauth/{provider}/device-code");
                match api::get_json::<DeviceResponse>(&path).await {
                    Ok(value) => {
                        let verification = value
                            .verification_uri_complete
                            .clone()
                            .unwrap_or_else(|| value.verification_uri.clone());
                        if let Some(window) = window() {
                            let _ = window.open_with_url_and_target(&verification, "_blank");
                        }
                        device.set(Some(value));
                        message.set("Complete authorization, then poll for the token.".into());
                    }
                    Err(error) => message.set(error),
                }
            } else {
                let redirect_uri = oauth_redirect(&provider);
                let path = format!(
                    "/api/oauth/{provider}/authorize?redirect_uri={}",
                    urlencoding::encode(&redirect_uri)
                );
                match api::get_json::<AuthorizeResponse>(&path).await {
                    Ok(mut value) => {
                        if value.redirect_uri.is_empty() {
                            value.redirect_uri = redirect_uri;
                        }
                        if let Some(window) = window() {
                            let _ = window.open_with_url_and_target(&value.auth_url, "_blank");
                        }
                        authorize.set(Some(value));
                        message.set("Paste the callback URL after authorization.".into());
                    }
                    Err(error) => message.set(error),
                }
            }
            busy.set(false);
        });
    };

    let exchange_provider = provider_id.clone();
    let exchange = Callback::new(move |_| {
        let Some(auth) = authorize.get_untracked() else {
            return;
        };
        let raw = callback_url.get_untracked();
        let parsed = url::Url::parse(&raw).ok();
        let code = parsed
            .as_ref()
            .and_then(|url| {
                url.query_pairs()
                    .find(|(key, _)| key == "code")
                    .map(|(_, value)| value.into_owned())
            })
            .unwrap_or(raw);
        let state = parsed
            .as_ref()
            .and_then(|url| {
                url.query_pairs()
                    .find(|(key, _)| key == "state")
                    .map(|(_, value)| value.into_owned())
            })
            .or_else(|| (!auth.state.is_empty()).then_some(auth.state));
        let body = ExchangeRequest {
            code,
            redirect_uri: auth.redirect_uri,
            code_verifier: auth.code_verifier,
            state,
        };
        let path = format!("/api/oauth/{exchange_provider}/exchange");
        busy.set(true);
        spawn_local(async move {
            match api::send_json::<_, serde_json::Value>("POST", &path, &body).await {
                Ok(_) => {
                    message.set("OAuth connection saved.".into());
                    on_success.run(());
                }
                Err(error) => message.set(error),
            }
            busy.set(false);
        });
    });

    let poll_provider = provider_id;
    let poll = Callback::new(move |_| {
        let Some(value) = device.get_untracked() else {
            return;
        };
        let body = PollRequest {
            device_code: value.device_code,
            code_verifier: value.code_verifier,
            extra_data: serde_json::Value::Object(value.extra),
        };
        let path = format!("/api/oauth/{poll_provider}/poll");
        busy.set(true);
        spawn_local(async move {
            match api::send_json::<_, serde_json::Value>("POST", &path, &body).await {
                Ok(result)
                    if result.get("success").and_then(|value| value.as_bool()) == Some(true) =>
                {
                    message.set("OAuth connection saved.".into());
                    on_success.run(());
                }
                Ok(result) => message.set(
                    result
                        .get("errorDescription")
                        .or_else(|| result.get("error"))
                        .and_then(|value| value.as_str())
                        .unwrap_or("Authorization is still pending.")
                        .to_string(),
                ),
                Err(error) => message.set(error),
            }
            busy.set(false);
        });
    });

    view! {
        <section class="card stack"><h3>"OAuth connection"</h3><button class="button primary" on:click=start disabled=move || busy.get()>"Start OAuth"</button>
            <div class="stack" class:hidden=move || device.get().is_none()><p>"Code: "<strong>{move || device.get().map(|value| value.user_code).unwrap_or_default()}</strong></p><a class="button" href=move || device.get().map(|value| value.verification_uri).unwrap_or_default() target="_blank">"Open verification page"</a><button class="button primary" on:click=move |_| poll.run(())>"Poll now"</button></div>
            <div class="stack" class:hidden=move || authorize.get().is_none()><label><span>"Callback URL or code"</span><input bind:value=callback_url /></label><button class="button primary" on:click=move |_| exchange.run(())>"Complete OAuth"</button></div>
            <Show when=move || !message.get().is_empty()><p>{move || message.get()}</p></Show>
        </section>
    }
}

fn oauth_redirect(provider: &str) -> String {
    match provider {
        "codex" => "http://localhost:1455/auth/callback".to_string(),
        "xai" => "http://127.0.0.1:56121/callback".to_string(),
        _ => window()
            .and_then(|window| window.location().origin().ok())
            .map(|origin| format!("{origin}/callback"))
            .unwrap_or_else(|| "http://localhost:4623/callback".into()),
    }
}
