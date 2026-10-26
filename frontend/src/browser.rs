use crate::auth::{self, LoginDecision, StatusDecision};
use dioxus::prelude::*;
use gloo_timers::{callback::Timeout, future::TimeoutFuture};
use serde_json::{json, Value};
use std::cell::RefCell;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortController, AbortSignal, Request, RequestCredentials, RequestInit, Response};

mod endpoint;
mod i18n;
mod providers;
mod shell;
mod ui;

// Preserve the reference bytes; dx otherwise re-minifies CSS in the public dir.
// The CLI injects this in the initial HTML head, before WASM starts.
#[used]
static REFERENCE_CSS: Asset = asset!(
    "/assets/dashboard.BZnXx_Nc.css",
    AssetOptions::css()
        .with_minify(false)
        .with_static_head(true)
);

const INPUT_CLASS: &str = "w-full h-10 py-2.5 px-3 text-[14px] text-ink bg-canvas rounded-mini-md border border-hairline placeholder:text-muted-soft focus:outline-none focus:border-brand-coral focus:ring-2 focus:ring-brand-coral/25 focus:[border-width:1px] transition-colors duration-150 ease-out disabled:opacity-50 disabled:cursor-not-allowed text-[16px] sm:text-[14px]";
const BUTTON_CLASS: &str = "inline-flex items-center justify-center gap-2 rounded-mini-md leading-none transition-colors duration-150 ease-out cursor-pointer tracking-tight active:scale-[0.99] disabled:cursor-not-allowed disabled:active:scale-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-coral/30 focus-visible:ring-offset-2 focus-visible:ring-offset-canvas bg-brand-coral text-on-primary hover:bg-brand-coral-active disabled:bg-primary-disabled disabled:text-muted-soft h-10 px-4 text-[14px] font-medium w-full";
const ERROR_CLASS: &str = "text-sm text-[color:var(--color-danger)] bg-[color:var(--color-danger)]/10 border border-[color:var(--color-danger)]/20 rounded-mini-md px-3 py-2";

thread_local! {
    // The router canonicalizes untyped query/fragment parts on startup. Capture
    // the provider deep link first so it can be handled without losing data.
    static PROVIDER_INITIAL_LOCATION: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

pub fn start() {
    if let Some(window) = web_sys::window() {
        let location = window.location();
        if location
            .pathname()
            .is_ok_and(|path| path == "/dashboard/providers" || path == "/dashboard/providers/new")
        {
            PROVIDER_INITIAL_LOCATION.with(|initial| {
                *initial.borrow_mut() = Some((
                    location.search().unwrap_or_default(),
                    location.hash().unwrap_or_default(),
                ));
            });
        }
    }
    apply_theme(&current_theme());
    dioxus::launch(App);
}

fn current_theme() -> String {
    let raw = web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item("theme").ok().flatten());
    raw.as_deref().map_or_else(
        || "system".to_owned(),
        |raw| match serde_json::from_str::<Value>(raw) {
            Ok(value) => value["state"]["theme"]
                .as_str()
                .unwrap_or("system")
                .to_owned(),
            Err(_) => raw.to_owned(),
        },
    )
}

fn theme_is_dark(theme: &str) -> bool {
    theme == "dark"
        || (theme == "system"
            && web_sys::window()
                .and_then(|window| {
                    window
                        .match_media("(prefers-color-scheme: dark)")
                        .ok()
                        .flatten()
                })
                .is_some_and(|query| query.matches()))
}

fn apply_theme(theme: &str) {
    let Some(root) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
    else {
        return;
    };
    let dark = theme_is_dark(theme);
    if dark {
        let _ = root.class_list().add_1("dark");
        let _ = root.set_attribute("style", "background-color: #181715; color-scheme: dark;");
    } else {
        let _ = root.class_list().remove_1("dark");
        let _ = root.set_attribute("style", "background-color: #FAF9F5; color-scheme: light;");
    }
}

#[derive(Clone, Debug, PartialEq, Routable)]
enum Route {
    #[route("/")]
    Root {},
    #[route("/dashboard")]
    Dashboard {},
    #[route("/dashboard/endpoint")]
    Endpoint {},
    #[route("/dashboard/providers")]
    Providers {},
    #[route("/dashboard/providers/new")]
    ProvidersNew {},
    #[route("/login")]
    Login {},
}

#[component]
fn Root() -> Element {
    rsx! { shell::Dashboard {} }
}

#[component]
fn Dashboard() -> Element {
    rsx! { shell::Dashboard {} }
}

#[component]
fn Endpoint() -> Element {
    rsx! { shell::Dashboard {} }
}

#[component]
fn Providers() -> Element {
    // Keep the deep link in the address bar even while session verification is
    // pending or fails; the provider page consumes it only after auth succeeds.
    use_effect(|| restore_provider_location("/dashboard/providers"));
    rsx! { shell::Dashboard {} }
}

#[component]
fn ProvidersNew() -> Element {
    use_effect(|| restore_provider_location("/dashboard/providers/new"));
    rsx! { shell::Dashboard {} }
}

fn restore_provider_location(path: &str) {
    PROVIDER_INITIAL_LOCATION.with(|initial| {
        let initial = initial.borrow();
        let Some((search, hash)) = initial.as_ref() else {
            return;
        };
        let Some(window) = web_sys::window() else {
            return;
        };
        let location = window.location();
        if location.pathname().is_ok_and(|current| current == path)
            && (location.search().unwrap_or_default() != *search
                || location.hash().unwrap_or_default() != *hash)
        {
            let next = format!("{path}{search}{hash}");
            if let Ok(history) = window.history() {
                let _ = history.replace_state_with_url(&js_sys::Object::new(), "", Some(&next));
            }
        }
    });
}

#[component]
fn App() -> Element {
    let i18n = use_hook(i18n::I18n::new);
    use_context_provider(|| i18n.clone());
    rsx! { Router::<Route> {} }
}

async fn request(
    method: &str,
    path: &str,
    body: Option<&Value>,
    signal: Option<&AbortSignal>,
) -> Result<Response, JsValue> {
    let init = RequestInit::new();
    init.set_method(method);
    init.set_credentials(RequestCredentials::SameOrigin);
    if path == "/api/auth/status" || path == "/api/build" {
        init.set_cache(web_sys::RequestCache::NoStore);
    }
    if let Some(body) = body {
        init.set_body(&JsValue::from_str(&body.to_string()));
    }
    if let Some(signal) = signal {
        init.set_signal(Some(signal));
    }
    let request = Request::new_with_str_and_init(path, &init)?;
    if body.is_some() {
        request.headers().set("Content-Type", "application/json")?;
    }
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("Window unavailable"))?;
    JsFuture::from(window.fetch_with_request(&request))
        .await?
        .dyn_into()
}

async fn json_body(response: &Response) -> Option<Value> {
    let text = JsFuture::from(response.text().ok()?)
        .await
        .ok()?
        .as_string()?;
    serde_json::from_str(&text).ok()
}

fn navigate(path: &str) -> Result<(), JsValue> {
    web_sys::window()
        .ok_or_else(|| JsValue::from_str("Window unavailable"))?
        .location()
        .assign(path)
}

struct AbortOnDrop(AbortController);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn get_status() -> StatusDecision {
    let Ok(controller) = AbortController::new() else {
        return auth::status_decision(false, None);
    };
    let timeout_controller = controller.clone();
    let _timeout = Timeout::new(5000, move || timeout_controller.abort());
    let _abort_on_drop = AbortOnDrop(controller.clone());
    let Ok(response) = request("GET", "/api/auth/status", None, Some(&controller.signal())).await
    else {
        return auth::status_decision(false, None);
    };
    if !response.ok() {
        return auth::status_decision(false, None);
    }
    auth::status_decision(true, json_body(&response).await.as_ref())
}

#[component]
fn PasswordInput(
    id: &'static str,
    label: &'static str,
    placeholder: &'static str,
    autocomplete: &'static str,
    mut value: Signal<String>,
    disabled: bool,
    autofocus: bool,
) -> Element {
    rsx! {
        div { class: "flex flex-col gap-1.5",
            label { class: "type-body-sm-medium text-ink", r#for: id,
                "{label}"
                span { class: "text-[color:var(--color-danger)] ml-1", "*" }
            }
            div { class: "relative",
                input {
                    id,
                    r#type: "password",
                    placeholder,
                    value: "{value}",
                    disabled,
                    class: INPUT_CLASS,
                    autofocus,
                    autocomplete,
                    oninput: move |event| value.set(event.value()),
                    onmounted: move |event| async move {
                        if autofocus {
                            let _ = event.set_focus(true).await;
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn SubmitButton(label: String, loading: bool, disabled: bool) -> Element {
    rsx! {
        button { r#type: "submit", class: BUTTON_CLASS, disabled: disabled || loading,
            if loading {
                span { class: "material-symbols-outlined animate-spin text-[18px]", "progress_activity" }
            }
            "{label}"
        }
    }
}

#[component]
fn Login() -> Element {
    let i18n = use_context::<i18n::I18n>();
    use_future(move || {
        let i18n = i18n.clone();
        async move {
            i18n.reload(&shell::cookie_locale()).await;
        }
    });
    let mut password = use_signal(String::new);
    let mut new_password = use_signal(String::new);
    let mut confirm_password = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut reset_hint = use_signal(String::new);
    let mut retry_after = use_signal(|| 0_u64);
    let mut loading = use_signal(|| false);
    let mut status_loading = use_signal(|| true);
    let mut has_password = use_signal(|| None::<bool>);
    let mut must_change = use_signal(|| false);

    use_future(move || async move {
        match get_status().await {
            StatusDecision::Redirect => {
                if navigate("/dashboard").is_err() {
                    has_password.set(Some(true));
                }
            }
            StatusDecision::ShowLogin {
                has_password: exists,
            } => has_password.set(Some(exists)),
        }
        status_loading.set(false);
    });

    use_effect(move || {
        let seconds = retry_after();
        if seconds > 0 {
            spawn(async move {
                TimeoutFuture::new(1000).await;
                if retry_after() == seconds {
                    retry_after.set(seconds - 1);
                }
            });
        }
    });

    let handle_login = move |event: FormEvent| {
        event.prevent_default();
        if retry_after() > 0 || loading() {
            return;
        }
        let submitted_password = password();
        loading.set(true);
        error.set(String::new());
        reset_hint.set(String::new());
        spawn(async move {
            match request(
                "POST",
                "/api/auth/login",
                Some(&json!({"password": submitted_password})),
                None,
            )
            .await
            {
                Ok(response) => {
                    let retry_header = response.headers().get("Retry-After").ok().flatten();
                    let data = json_body(&response).await.unwrap_or_else(|| json!({}));
                    match auth::login_decision(response.ok(), &data, retry_header.as_deref()) {
                        LoginDecision::Redirect => {
                            if navigate("/dashboard").is_err() {
                                error.set("Connection failed. Is the server running?".into());
                            }
                        }
                        LoginDecision::ChangePassword => {
                            must_change.set(true);
                            new_password.set(String::new());
                            confirm_password.set(String::new());
                        }
                        LoginDecision::ConnectionFailure => {
                            error.set("Connection failed. Is the server running?".into());
                        }
                        LoginDecision::Failure {
                            message,
                            reset_hint: hint,
                            retry_after: wait,
                        } => {
                            error.set(message);
                            reset_hint.set(hint);
                            if wait > 0 {
                                retry_after.set(wait);
                            }
                            password.set(String::new());
                        }
                    }
                }
                Err(_) => error.set("Connection failed. Is the server running?".into()),
            }
            loading.set(false);
        });
    };

    let handle_password = move |event: FormEvent| {
        event.prevent_default();
        if loading() {
            return;
        }
        error.set(String::new());
        let new = new_password();
        if let Some(message) = auth::validate_new_password(&new, &confirm_password()) {
            error.set(message.into());
            return;
        }
        let body = auth::password_payload(&password(), &new);
        loading.set(true);
        spawn(async move {
            let mut response = request("POST", "/api/auth/password", Some(&body), None).await;
            if response
                .as_ref()
                .is_ok_and(|res| auth::use_settings_fallback(res.status()))
            {
                response = request("PATCH", "/api/settings", Some(&body), None).await;
            }
            match response {
                Ok(response) => {
                    let data = json_body(&response).await.unwrap_or_else(|| json!({}));
                    if data.is_null() {
                        error.set("An error occurred. Please try again.".into());
                    } else if response.ok() {
                        if navigate(auth::password_destination(&data)).is_err() {
                            error.set("An error occurred. Please try again.".into());
                        }
                    } else {
                        error.set(auth::password_error(&data).into());
                    }
                }
                Err(_) => error.set("An error occurred. Please try again.".into()),
            }
            loading.set(false);
        });
    };

    if status_loading() {
        return rsx! {
            div { class: "min-h-screen flex items-center justify-center bg-canvas px-4 py-12 relative overflow-hidden",
                div { class: "landing-grid absolute inset-0 pointer-events-none -z-10", aria_hidden: "true" }
                div { class: "text-center relative z-10",
                    div { class: "inline-block h-8 w-8 animate-spin rounded-full border-2 border-brand-coral border-t-transparent" }
                    p { class: "mt-4 text-[14px] text-muted", "Loading…" }
                }
            }
        };
    }

    let subtitle = if must_change() {
        "Choose a new password before continuing"
    } else {
        "Enter your password to access the dashboard"
    };
    rsx! {
        div { class: "min-h-screen flex items-center justify-center bg-canvas px-4 py-12 relative overflow-hidden",
            div { class: "landing-grid absolute inset-0 pointer-events-none -z-10", aria_hidden: "true" }
            div { class: "w-full max-w-md relative z-10",
                div { class: "text-center mb-10",
                    div { class: "inline-flex items-center justify-center w-14 h-14 rounded-mini-md bg-surface-card border border-hairline mb-5",
                        svg { width: "28", height: "28", view_box: "0 0 24 24", fill: "currentColor", class: "text-brand-coral", "aria-hidden": "true",
                            path { d: "M11.0 2.2 L13.0 2.2 L13.4 10.0 L20.0 8.4 L20.5 10.3 L13.6 11.45 L20.5 13.7 L20.0 15.6 L13.4 14.0 L13.0 21.8 L11.0 21.8 L10.6 14.0 L4.0 15.6 L3.5 13.7 L10.4 11.45 L3.5 10.3 L4.0 8.4 L10.6 10.0 Z" }
                        }
                    }
                    h1 { class: "font-serif font-normal text-[40px] leading-tight tracking-[-0.02em] text-ink mb-2", "OpenProxy" }
                    p { class: "text-[15px] text-body", "{subtitle}" }
                }
                div { class: "bg-surface-card border border-hairline-soft text-ink rounded-mini-xl shadow-none p-8",
                    if must_change() {
                        form { onsubmit: handle_password, class: "flex flex-col gap-5",
                            p { class: "text-[13px] text-center text-amber-700 dark:text-amber-400 bg-amber-500/10 border border-amber-500/20 rounded-mini-md px-3 py-2",
                                "You signed in with the default password from a remote client. Set a new password before accessing the dashboard."
                            }
                            PasswordInput { id: "new-password", label: "New password", placeholder: "At least 8 characters", value: new_password, disabled: false, autofocus: true, autocomplete: "new-password" }
                            PasswordInput { id: "confirm-password", label: "Confirm password", placeholder: "Re-enter new password", value: confirm_password, disabled: false, autofocus: false, autocomplete: "new-password" }
                            if !error().is_empty() { p { class: ERROR_CLASS, "{error}" } }
                            SubmitButton { label: "Set password".to_owned(), loading: loading(), disabled: new_password().is_empty() || confirm_password().is_empty() }
                        }
                    } else {
                        div { class: "flex flex-col gap-5",
                            form { onsubmit: handle_login, class: "flex flex-col gap-5",
                                PasswordInput { id: "password", label: "Password", placeholder: "Enter your password", value: password, disabled: retry_after() > 0, autofocus: true, autocomplete: "current-password" }
                                if !error().is_empty() { p { class: ERROR_CLASS, "{error}" } }
                                if retry_after() > 0 {
                                    p { class: "text-[12px] text-center text-amber-700 dark:text-amber-400",
                                        "Locked. Retry in "
                                        span { class: "font-mono font-medium", "{retry_after}s" }
                                        "."
                                    }
                                }
                                if !reset_hint().is_empty() {
                                    p { class: "text-[12px] text-muted text-center",
                                        "Forgot password? Run "
                                        code { class: "px-1.5 py-0.5 rounded bg-canvas border border-hairline-soft font-mono text-ink", "openproxy auth reset-password" }
                                        " on the host to restore the generated initial password."
                                    }
                                }
                                SubmitButton { label: if retry_after() > 0 { format!("Wait {}s", retry_after()) } else { "Sign In".to_owned() }, loading: loading(), disabled: retry_after() > 0 }
                                if has_password() == Some(false) {
                                    p { class: "text-[12px] text-center text-amber-700 dark:text-amber-400", "Security risk: no password set. You will be asked to set one when logging in remotely." }
                                }
                            }
                        }
                    }
                }
                p { class: "mt-8 text-center text-[12px] text-muted-soft", "OpenProxy · local AI router for Claude Code, Codex, Cursor & more" }
            }
        }
    }
}
