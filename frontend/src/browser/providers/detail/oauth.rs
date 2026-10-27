use super::{dashboard_request, encoded, js_trim, json, json_body, oauth_events, text, Value};
use crate::auth_callback;
use crate::browser::{
    endpoint::copy_text,
    request,
    shell::SessionRecheck,
    ui::{Button, Modal, INPUT_CLASS},
};
use dioxus::prelude::*;
use futures_util::future::{select, Either};
use gloo_timers::future::TimeoutFuture;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Default)]
struct ProxySlot {
    generation: u64,
    owner: Option<u64>,
    stopping: bool,
    needs_stop: bool,
    waiters: Vec<futures_channel::oneshot::Sender<()>>,
}

const STOP_WAIT_TIMEOUT_MS: u32 = 3_000;
const STOP_PENDING_MESSAGE: &str =
    "Previous proxy cleanup has not completed. Retry after it finishes.";

thread_local! {
    // The backend listener is provider-wide, not tied to an OAuth state. Keep
    // ownership across modal instances and serialize an in-flight stop before
    // a later start in this browser application.
    static PROXY_SLOTS: RefCell<HashMap<String, ProxySlot>> = RefCell::new(HashMap::new());
}

struct ProxyLease {
    provider: String,
    generation: u64,
    closed: Cell<bool>,
}

struct StopProxy(Option<Rc<ProxyLease>>);

impl StopProxy {
    fn new(provider: &str) -> Self {
        if !matches!(provider, "codex" | "xai") {
            return Self(None);
        }
        let generation = PROXY_SLOTS.with(|slots| {
            let mut slots = slots.borrow_mut();
            let slot = slots.entry(provider.to_owned()).or_default();
            slot.generation = slot.generation.wrapping_add(1);
            slot.owner = Some(slot.generation);
            slot.generation
        });
        Self(Some(Rc::new(ProxyLease {
            provider: provider.to_owned(),
            generation,
            closed: Cell::new(false),
        })))
    }
}

impl Drop for StopProxy {
    fn drop(&mut self) {
        if let Some(lease) = &self.0 {
            lease.closed.set(true);
            PROXY_SLOTS.with(|slots| {
                if let Some(slot) = slots.borrow_mut().get_mut(&lease.provider)
                    && slot.owner == Some(lease.generation)
                {
                    slot.owner = None;
                }
            });
            stop_if_idle(&lease.provider);
        }
    }
}

fn stop_if_idle(provider: &str) {
    let should_stop = PROXY_SLOTS.with(|slots| {
        let mut slots = slots.borrow_mut();
        let Some(slot) = slots.get_mut(provider) else {
            return false;
        };
        if slot.owner.is_some() || slot.stopping || !slot.needs_stop {
            return false;
        }
        slot.stopping = true;
        slot.needs_stop = false;
        true
    });
    if !should_stop {
        return;
    }
    let provider = provider.to_owned();
    wasm_bindgen_futures::spawn_local(async move {
        // A newer attempt waits for this response before it can start a proxy.
        let response = request(
            "GET",
            &format!("/api/oauth/{provider}/stop-proxy"),
            None,
            None,
        )
        .await;
        if !response.is_ok_and(|response| response.ok()) {
            // Neither a failed transport nor a non-success HTTP response proves
            // the provider-wide stop will not still run. Keep the fence closed.
            return;
        }
        let waiters = PROXY_SLOTS.with(|slots| {
            let mut slots = slots.borrow_mut();
            let Some(slot) = slots.get_mut(&provider) else {
                return Vec::new();
            };
            slot.stopping = false;
            std::mem::take(&mut slot.waiters)
        });
        for waiter in waiters {
            let _ = waiter.send(());
        }
        // A start that was already on the wire may have completed after this
        // stop took effect. Its completion marks a second cleanup as needed.
        stop_if_idle(&provider);
    });
}

fn wait_for_stop(provider: &str) -> Option<futures_channel::oneshot::Receiver<()>> {
    PROXY_SLOTS.with(|slots| {
        let mut slots = slots.borrow_mut();
        let slot = slots.get_mut(provider)?;
        if !slot.stopping {
            return None;
        }
        slot.waiters.retain(|waiter| !waiter.is_canceled());
        let (tx, rx) = futures_channel::oneshot::channel();
        slot.waiters.push(tx);
        Some(rx)
    })
}

enum ProxyStartError {
    Cancelled,
    CleanupPending,
    Request(String),
}

fn owns_proxy(lease: &ProxyLease) -> bool {
    !lease.closed.get()
        && PROXY_SLOTS.with(|slots| {
            slots
                .borrow()
                .get(&lease.provider)
                .is_some_and(|slot| slot.owner == Some(lease.generation))
        })
}

#[derive(Clone, Copy)]
pub(super) enum Completion {
    Exchange,
    DevicePoll,
    ProxyStatus,
}

pub(super) struct OwnedRequest {
    pub attempt: u64,
    pub method: &'static str,
    pub path: String,
    pub body: Option<Value>,
    pub completion: Completion,
    pub claimed: Option<Rc<Cell<bool>>>,
    pub popup: Option<oauth_events::PopupClose>,
    pub reply: futures_channel::oneshot::Sender<Result<Value, String>>,
}

async fn owned_request(
    callback: Callback<OwnedRequest>,
    attempt: u64,
    method: &'static str,
    path: String,
    body: Option<Value>,
    completion: Completion,
    effects: (Option<Rc<Cell<bool>>>, Option<oauth_events::PopupClose>),
) -> Result<Value, String> {
    let (reply, response) = futures_channel::oneshot::channel();
    callback.call(OwnedRequest {
        attempt,
        method,
        path,
        body,
        completion,
        claimed: effects.0,
        popup: effects.1,
        reply,
    });
    response
        .await
        .unwrap_or_else(|_| Err("Authentication cancelled".into()))
}

fn safe_url(url: &str) -> bool {
    // Do not let a compromised response open a javascript:, file:, or other
    // unexpected scheme. Plain HTTP is allowed only for loopback development.
    let Ok(parsed) = web_sys::Url::new(url) else {
        return false;
    };
    parsed.protocol() == "https:"
        || parsed.protocol() == "http:"
            && matches!(
                parsed.hostname().as_str(),
                "localhost" | "127.0.0.1" | "[::1]"
            )
}

fn open_url(url: &str) {
    if safe_url(url)
        && let Some(window) = web_sys::window()
    {
        let _ = window.open_with_url_and_target_and_features(url, "_blank", "noopener,noreferrer");
    }
}

fn open_auth_popup(url: &str) -> Option<web_sys::Window> {
    if !safe_url(url) {
        return None;
    }
    let window = web_sys::window()?;
    let popup = window
        .open_with_url_and_target("about:blank", "_blank")
        .ok()??;
    // Clear the opener before the external authorization page can run. If
    // that cannot be confirmed, do not navigate or expose the parent window.
    if popup.set_opener(&wasm_bindgen::JsValue::NULL).is_err()
        || !popup
            .opener()
            .is_ok_and(|value| value.is_null() || value.is_undefined())
        || popup.location().set_href(url).is_err()
    {
        let _ = popup.close();
        return None;
    }
    Some(popup)
}

fn attempt_nonce() -> Option<String> {
    let mut bytes = [0_u8; 32];
    web_sys::window()?
        .crypto()
        .ok()?
        .get_random_values_with_u8_array(&mut bytes)
        .ok()?;
    Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

async fn call(method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
    let response = dashboard_request(method, path, body)
        .await
        .map_err(|_| "Network error".to_owned())?;
    let data = json_body(&response).await.unwrap_or_default();
    if response.ok() {
        Ok(data)
    } else {
        Err(data["error"]
            .as_str()
            .unwrap_or("Authentication failed")
            .to_owned())
    }
}

async fn start_proxy(
    path: String,
    lease: Rc<ProxyLease>,
    recheck: SessionRecheck,
) -> Result<Value, ProxyStartError> {
    let (tx, rx) = futures_channel::oneshot::channel();
    // Starting a local proxy can finish after its modal is dismissed. Keep the
    // request alive, but never let an old completion stop a newer owner.
    wasm_bindgen_futures::spawn_local(async move {
        if let Some(stop) = wait_for_stop(&lease.provider)
            && !matches!(
                select(stop, TimeoutFuture::new(STOP_WAIT_TIMEOUT_MS)).await,
                Either::Left((Ok(()), _))
            )
        {
            let _ = tx.send(Err(ProxyStartError::CleanupPending));
            return;
        }
        if !owns_proxy(&lease) {
            let _ = tx.send(Err(ProxyStartError::Cancelled));
            return;
        }
        PROXY_SLOTS.with(|slots| {
            let mut slots = slots.borrow_mut();
            let slot = slots.entry(lease.provider.clone()).or_default();
            slot.needs_stop = true;
        });
        let result = match request("GET", &path, None, None).await {
            Ok(response) => {
                if response.status() == 401 {
                    recheck.call();
                }
                let ok = response.ok();
                let data = json_body(&response).await.unwrap_or_default();
                if ok {
                    Ok(data)
                } else {
                    Err(ProxyStartError::Request(
                        data["error"]
                            .as_str()
                            .unwrap_or("Authentication failed")
                            .to_owned(),
                    ))
                }
            }
            Err(_) => Err(ProxyStartError::Request("Network error".into())),
        };
        // Even a transport error can occur after the backend started a listener.
        PROXY_SLOTS.with(|slots| {
            if let Some(slot) = slots.borrow_mut().get_mut(&lease.provider) {
                slot.needs_stop = true;
            }
        });
        stop_if_idle(&lease.provider);
        let _ = tx.send(result);
    });
    rx.await.unwrap_or(Err(ProxyStartError::Cancelled))
}

#[component]
pub(super) fn OAuthFlow(
    provider: String,
    name: String,
    attempt: u64,
    on_close: EventHandler<()>,
    on_request: Callback<OwnedRequest>,
) -> Element {
    let proxy_owner = use_hook({
        let provider = provider.clone();
        move || Rc::new(StopProxy::new(&provider))
    });
    let mut state = use_signal(Value::default);
    let mut error = use_signal(String::new);
    let mut callback = use_signal(String::new);
    let mut submitting = use_signal(|| false);
    let mut popup = use_signal(|| None::<web_sys::Window>);
    let copied = use_signal(String::new);
    let generation = use_signal(|| 0_u32);
    let claimed = use_hook(|| Rc::new(Cell::new(false)));
    let recheck = use_context::<SessionRecheck>();
    let device = matches!(
        provider.as_str(),
        "github" | "kimi" | "kimi-coding" | "kilocode" | "codebuddy" | "codebuddy-cn" | "kimchi"
    );
    let mut flow = {
        let provider = provider.clone();
        let claimed = claimed.clone();
        use_future(move || {
            let provider = provider.clone();
            let recheck = recheck.clone();
            let proxy_owner = proxy_owner.0.clone();
            let claimed = claimed.clone();
            async move {
                claimed.set(false);
                error.set(String::new());
                state.set(Value::Null);
                popup.set(None);
                if device {
                    let data = match call(
                        "GET",
                        &format!("/api/oauth/{}/device-code", encoded(&provider)),
                        None,
                    )
                    .await
                    {
                        Ok(data) => data,
                        Err(e) => {
                            error.set(e);
                            return;
                        }
                    };
                    let code = text(&data, "device_code").to_owned();
                    let verifier = text(&data, "codeVerifier").to_owned();
                    if code.is_empty() {
                        error.set("No device code returned".into());
                        return;
                    }
                    let url = data["verification_uri_complete"]
                        .as_str()
                        .filter(|v| !v.is_empty())
                        .unwrap_or_else(|| text(&data, "verification_uri"));
                    if !url.is_empty() {
                        open_url(url);
                    }
                    let mut interval = data["interval"]
                        .as_f64()
                        .filter(|n| n.is_finite() && *n > 0.0)
                        .unwrap_or(5.0);
                    let expires = data["expires_in"]
                        .as_f64()
                        .filter(|n| n.is_finite() && *n > 0.0)
                        .unwrap_or(120.0);
                    state.set(data);
                    let deadline = js_sys::Date::now() + expires * 1000.0;
                    while js_sys::Date::now() < deadline {
                        TimeoutFuture::new((interval * 1000.0).clamp(1.0, u32::MAX as f64) as u32)
                            .await;
                        let path = format!("/api/oauth/{}/poll", encoded(&provider));
                        // Poll returns meaningful OAuth error JSON even when HTTP is non-2xx.
                        let data = match owned_request(on_request, attempt, "POST", path, Some(json!({"deviceCode":code,"codeVerifier":verifier,"extraData":Value::Null})), Completion::DevicePoll, (None, None)).await { Ok(data) => data, Err(e) => { error.set(e); return; } };
                        if data["success"] == true {
                            return;
                        }
                        match text(&data, "error") {
                            "expired_token" | "access_denied" => {
                                error.set(
                                    data["errorDescription"]
                                        .as_str()
                                        .unwrap_or_else(|| text(&data, "error"))
                                        .into(),
                                );
                                return;
                            }
                            "slow_down" => interval = (interval + 5.0).min(30.0),
                            _ => {}
                        }
                    }
                    error.set("Authorization timeout".into());
                    return;
                }
                let Some(window) = web_sys::window() else {
                    error.set("Browser unavailable".into());
                    return;
                };
                let location = window.location();
                let port = location
                    .port()
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| {
                        if location.protocol().ok().as_deref() == Some("https:") {
                            "443".into()
                        } else {
                            "80".into()
                        }
                    });
                let mut redirect = match provider.as_str() {
                    "codex" => "http://localhost:1455/auth/callback".to_owned(),
                    "xai" => "http://127.0.0.1:56121/callback".to_owned(),
                    _ => format!("http://localhost:{port}/callback"),
                };
                let nonce = if provider == "cline" {
                    let Some(nonce) = attempt_nonce() else {
                        error.set("Unable to securely start authorization".into());
                        return;
                    };
                    redirect.push_str(&format!("?openproxy_oauth_nonce={nonce}"));
                    Some(nonce)
                } else {
                    None
                };
                let path = format!(
                    "/api/oauth/{}/authorize?redirect_uri={}",
                    encoded(&provider),
                    encoded(&redirect)
                );
                let mut data = match call("GET", &path, None).await {
                    Ok(data) => data,
                    Err(e) => {
                        error.set(e);
                        return;
                    }
                };
                data["redirectUri"] = json!(redirect);
                data["attemptNonce"] = json!(nonce);
                if text(&data, "authUrl").is_empty() {
                    error.set("No authorization URL returned from OAuth provider".into());
                    return;
                }
                if matches!(provider.as_str(), "codex" | "xai")
                    && let Ok(params) = web_sys::UrlSearchParams::new()
                {
                    params.append("app_port", &port);
                    params.append("state", text(&data, "state"));
                    params.append("code_verifier", text(&data, "codeVerifier"));
                    params.append("redirect_uri", text(&data, "redirectUri"));
                    let query: String = params.to_string().into();
                    let Some(proxy_owner) = proxy_owner else {
                        return;
                    };
                    match start_proxy(
                        format!("/api/oauth/{}/start-proxy?{query}", encoded(&provider)),
                        proxy_owner,
                        recheck,
                    )
                    .await
                    {
                        Ok(proxy) => {
                            if provider == "xai"
                                && proxy["success"] != true
                                && proxy["reason"] == "port_busy"
                            {
                                error.set(
                                    "Port 56121 in use; close the conflicting process and retry"
                                        .into(),
                                );
                                return;
                            }
                            data["serverSide"] = json!(proxy["serverSide"] == true);
                        }
                        Err(ProxyStartError::CleanupPending) => {
                            error.set(STOP_PENDING_MESSAGE.into());
                            return;
                        }
                        Err(ProxyStartError::Cancelled) => return,
                        Err(ProxyStartError::Request(e)) if provider == "xai" => {
                            error.set(e);
                            return;
                        }
                        Err(ProxyStartError::Request(_)) => {}
                    }
                }
                popup.set(open_auth_popup(text(&data, "authUrl")));
                let server_side = data["serverSide"] == true;
                let saved_state = text(&data, "state").to_owned();
                state.set(data);
                if server_side && !saved_state.is_empty() {
                    for _ in 0..200 {
                        TimeoutFuture::new(1500).await;
                        if let Ok(status) = owned_request(
                            on_request,
                            attempt,
                            "GET",
                            format!(
                                "/api/oauth/{}/poll-status?state={}",
                                encoded(&provider),
                                encoded(&saved_state)
                            ),
                            None,
                            Completion::ProxyStatus,
                            (Some(claimed.clone()), None),
                        )
                        .await
                        {
                            match text(&status, "status") {
                                "done" => {
                                    return;
                                }
                                "error" => {
                                    error.set(
                                        status["error"]
                                            .as_str()
                                            .unwrap_or("Authentication failed")
                                            .into(),
                                    );
                                    return;
                                }
                                _ => {}
                            }
                        }
                    }
                    error.set("Authentication timeout".into());
                }
            }
        })
    };
    let on_callback = {
        let provider = provider.clone();
        let claimed = claimed.clone();
        use_callback(move |delivery: oauth_events::Delivery| {
            if device || claimed.get() || *submitting.peek() {
                return;
            }
            let auth = state.peek().clone();
            let expected_state = text(&auth, "state");
            if !auth_callback::valid(
                &delivery.data,
                &provider,
                expected_state,
                auth["attemptNonce"].as_str(),
                delivery.storage_value.is_some(),
                js_sys::Date::now(),
            ) || delivery.origin.as_ref().is_some_and(|origin| {
                !oauth_events::valid_origin(origin, text(&auth, "redirectUri"))
            }) {
                return;
            }
            claimed.set(true);
            if let Some(raw) = &delivery.storage_value {
                oauth_events::remove_stored(raw);
            }
            if let Some(failure) = delivery.data["error"].as_str().filter(|v| !v.is_empty()) {
                error.set(
                    delivery.data["errorDescription"]
                        .as_str()
                        .filter(|v| !v.is_empty())
                        .unwrap_or(failure)
                        .to_owned(),
                );
                return;
            }
            let code = delivery.data["token"]
                .as_str()
                .filter(|v| !v.is_empty())
                .or_else(|| delivery.data["code"].as_str().filter(|v| !v.is_empty()))
                .unwrap_or_default()
                .to_owned();
            let close_popup = if delivery.data["token"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
            {
                None
            } else {
                popup.peek().clone().and_then(|popup| {
                    let origin = web_sys::window()?.location().origin().ok()?;
                    oauth_events::PopupClose::capture(popup, origin, &delivery.data)
                })
            };
            let body = json!({"code":code,"redirectUri":auth["redirectUri"],"codeVerifier":auth["codeVerifier"],"state":expected_state});
            let path = format!("/api/oauth/{}/exchange", encoded(&provider));
            submitting.set(true);
            spawn(async move {
                if let Err(failure) = owned_request(
                    on_request,
                    attempt,
                    "POST",
                    path,
                    Some(body),
                    Completion::Exchange,
                    (None, close_popup),
                )
                .await
                {
                    error.set(failure);
                }
                submitting.set(false);
            });
        })
    };
    let _listeners = use_hook(|| oauth_events::Listeners::new(on_callback).map(Rc::new));
    let poll_generation = use_hook(|| Rc::new(Cell::new(0_u64)));
    use_effect({
        let claimed = claimed.clone();
        let poll_generation = poll_generation.clone();
        move || {
            let active_popup = popup();
            let auth = state();
            let generation = poll_generation.get().wrapping_add(1);
            poll_generation.set(generation);
            let Some(active_popup) = active_popup.filter(|_| !auth.is_null()) else {
                return;
            };
            let Some(origin) = web_sys::window().and_then(|w| w.location().origin().ok()) else {
                return;
            };
            let claimed = claimed.clone();
            let poll_generation = poll_generation.clone();
            spawn(async move {
                while poll_generation.get() == generation && !claimed.get() {
                    if active_popup.closed().unwrap_or(true) {
                        popup.set(None);
                        return;
                    }
                    if let Some(data) = oauth_events::popup_data(&active_popup, &origin) {
                        on_callback.call(oauth_events::Delivery {
                            data,
                            origin: Some(origin.clone()),
                            storage_value: None,
                        });
                    }
                    if !claimed.get() {
                        TimeoutFuture::new(200).await;
                    }
                }
            });
        }
    });
    use_effect(move || {
        if !device && !state().is_null() {
            oauth_events::read_stored(on_callback);
        }
    });
    let title = if provider == "xai" {
        "Connect Grok Build OAuth".to_owned()
    } else {
        format!("Connect {name}")
    };
    let device_login_url = {
        let data = state();
        data["verification_uri_complete"]
            .as_str()
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| text(&data, "verification_uri"))
            .to_owned()
    };
    let manual_placeholder = if provider == "xai" {
        "http://127.0.0.1:56121/callback?code=... or copied code".to_owned()
    } else {
        web_sys::window()
            .and_then(|window| window.location().origin().ok())
            .map(|origin| format!("{origin}/callback?code=..."))
            .unwrap_or_else(|| "/callback?code=...".to_owned())
    };
    rsx! { Modal { title, large: true, on_close,
        div { class: "flex flex-col gap-4",
            if !error().is_empty() { div { role: "alert", class: "text-sm text-red-500", "{error}" } Button { label: "Retry".to_owned(), variant: "secondary", onclick: move |_| flow.restart() } }
            else if device {
                if state().is_null() { p { class: "text-sm text-text-muted", "Starting authorization..." } }
                else {
                    div { class: "text-center py-4",
                        p { class: "text-sm text-text-muted mb-4", "Visit the login URL below and authorize:" }
                        div { class: "bg-sidebar p-4 rounded-lg mb-4",
                            p { class: "text-xs text-text-muted mb-1", "Login URL" }
                            div { class: "flex items-center gap-2",
                                code { class: "flex-1 text-sm break-all", "{device_login_url}" }
                                button { r#type: "button", aria_label: "Copy login URL", class: "inline-flex items-center justify-center gap-2 rounded-mini-md leading-none transition-colors duration-150 ease-out cursor-pointer tracking-tight active:scale-[0.99] disabled:cursor-not-allowed disabled:active:scale-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-coral/30 focus-visible:ring-offset-2 focus-visible:ring-offset-canvas bg-transparent text-ink hover:bg-surface-card disabled:text-muted h-8 px-3 text-[12px] font-medium", disabled: device_login_url.is_empty(), onclick: { let device_login_url = device_login_url.clone(); move |_| copy_text(device_login_url.clone(), "login_url".into(), copied, generation) },
                                    span { class: "material-symbols-outlined text-[18px]", if copied() == "login_url" { "check" } else { "content_copy" } }
                                }
                                Button { label: "Open".to_owned(), small: true, variant: "ghost", icon: "open_in_new".to_owned(), disabled: !safe_url(&device_login_url), onclick: { let device_login_url = device_login_url.clone(); move |_| open_url(&device_login_url) } }
                            }
                        }
                        div { class: "bg-primary/10 p-4 rounded-lg",
                            p { class: "text-xs text-text-muted mb-1", "Your Code" }
                            div { class: "flex items-center justify-center gap-2",
                                p { class: "text-2xl font-mono font-bold text-primary", "{text(&state(), \"user_code\")}" }
                                button { r#type: "button", aria_label: "Copy user code", class: "inline-flex items-center justify-center gap-2 rounded-mini-md leading-none transition-colors duration-150 ease-out cursor-pointer tracking-tight active:scale-[0.99] disabled:cursor-not-allowed disabled:active:scale-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-coral/30 focus-visible:ring-offset-2 focus-visible:ring-offset-canvas bg-transparent text-ink hover:bg-surface-card disabled:text-muted h-8 px-3 text-[12px] font-medium", onclick: move |_| copy_text(text(&state(), "user_code").to_owned(), "user_code".into(), copied, generation),
                                    span { class: "material-symbols-outlined text-[18px]", if copied() == "user_code" { "check" } else { "content_copy" } }
                                }
                            }
                        }
                    }
                    div { class: "flex items-center justify-center gap-2 text-sm text-text-muted",
                        span { class: "material-symbols-outlined animate-spin", "progress_activity" }
                        "Waiting for authorization..."
                        }
                }
            } else if state().is_null() { p { class: "text-sm text-text-muted", "Starting authorization..." } }
            else {
                div { class: "flex items-center gap-2 px-3 py-2 border border-border rounded-lg bg-sidebar/50", role: "status",
                    span { aria_hidden: "true", class: "material-symbols-outlined text-base text-primary animate-spin", "progress_activity" }
                    span { class: "text-sm", if provider == "xai" { "Waiting for Grok Build OAuth…" } else { "Waiting for popup authorization…" } }
                }
                div { class: "flex items-center gap-3 my-1",
                    div { class: "flex-1 h-px bg-border" }
                    span { class: "text-xs text-text-muted uppercase tracking-wider", "Or paste callback URL manually" }
                    div { class: "flex-1 h-px bg-border" }
                }
                div { class: "space-y-4",
                    div {
                        p { class: "text-sm font-medium mb-2", if provider == "xai" { "Step 1: Open this Grok Build OAuth URL in your browser" } else { "Step 1: Open this URL in your browser" } }
                        div { class: "flex gap-2",
                            div { class: "flex flex-col gap-1.5 flex-1 font-mono text-xs",
                                div { class: "relative",
                                    input { r#type: "text", readonly: true, aria_label: "Authorization URL", value: "{text(&state(), \"authUrl\")}", class: INPUT_CLASS }
                                }
                            }
                            Button { label: "Copy".to_owned(), variant: "secondary", icon: if copied() == "auth_url" { "check".to_owned() } else { "content_copy".to_owned() }, disabled: text(&state(), "authUrl").is_empty(), onclick: move |_| copy_text(text(&state(), "authUrl").to_owned(), "auth_url".into(), copied, generation) }
                        }
                        if popup().is_none() { Button { label: "Open Authorization".to_owned(), variant: "secondary", onclick: move |_| popup.set(open_auth_popup(text(&state(), "authUrl"))) } }
                    }
                    div {
                        p { class: "text-sm font-medium mb-2",
                            "Step 2: Paste the "
                            if provider == "xai" { "callback URL or copied code" } else { "callback URL" }
                            " here"
                        }
                        p { class: "text-xs text-text-muted mb-2", if provider == "xai" { "If xAI shows a code instead of redirecting, paste that code here." } else { "After authorization, copy the full URL from your browser." } }
                        div { class: "flex flex-col gap-1.5 font-mono text-xs",
                            div { class: "relative",
                                input { r#type: "text", aria_label: "Callback URL", placeholder: "{manual_placeholder}", value: "{callback}", class: INPUT_CLASS, oninput: move |e| callback.set(e.value()) }
                            }
                        }
                    }
                }
                div { class: "flex gap-2",
                Button { label: if submitting() { "Connecting...".to_owned() } else { "Connect".to_owned() }, full_width: true, disabled: submitting() || state().is_null() || js_trim(&callback()).is_empty(), onclick: { let provider = provider.clone(); let claimed = claimed.clone(); move |_| {
                    if submitting() || claimed.get() { return; }
                    let input = js_trim(&callback()); let auth = state(); let provider = provider.clone();
                    let raw = input.starts_with("eyJ") && input.contains('.') || provider == "claude" && !input.contains("://");
                    let xai_raw = provider == "xai" && !input.contains("://") && !input.contains('?') && !input.contains("code=");
                    let parsed = if raw || xai_raw { Some((input, Value::Null)) } else if let Ok(url) = web_sys::Url::new(&input) {
                        let params = url.search_params();
                        if let Some(err) = params.get("error") { error.set(params.get("error_description").unwrap_or(err)); None }
                        else { params.get("token").or_else(|| params.get("code")).map(|code| (code, json!(params.get("state").unwrap_or_default()))) }
                    } else { None };
                    let Some((code, callback_state)) = parsed else { if error().is_empty() { error.set("No authorization code found in URL".into()); } return; };
                    claimed.set(true);
                    submitting.set(true);
                    spawn(async move {
                        let (path, body) = if xai_raw { ("/api/oauth/xai/manual-code".to_owned(), json!({"code":code,"state":auth["state"]})) } else { (format!("/api/oauth/{}/exchange", encoded(&provider)), json!({"code":code,"redirectUri":auth["redirectUri"],"codeVerifier":auth["codeVerifier"],"state":callback_state})) };
                        if let Err(e) = owned_request(on_request, attempt, "POST", path, Some(body), Completion::Exchange, (None, None)).await { error.set(e); }
                        submitting.set(false);
                    });
                } } }
                Button { label: "Cancel".to_owned(), variant: "ghost", full_width: true, onclick: move |_| on_close.call(()) }
                }
            }
            if state().is_null() || !error().is_empty() { Button { label: "Cancel".to_owned(), variant: "ghost", onclick: move |_| on_close.call(()) } }
        }
    } }
}
