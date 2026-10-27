use super::{
    apply_theme,
    console_log::ConsoleLogPage,
    current_theme,
    endpoint::EndpointPage,
    i18n::I18n,
    json_body, navigate,
    providers::{NewProviderPage, ProviderDetailPage, ProvidersPage},
    request, request_with_cache,
    skills::SkillsPage,
    theme_is_dark,
    ui::Button,
    ui::Modal,
    AbortOnDrop,
};
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::{json, Value};
use std::{cell::Cell, rc::Rc};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use web_sys::{AbortController, AbortSignal, Response};

thread_local! {
    static DASHBOARD_ACTIVE: Cell<bool> = const { Cell::new(false) };
    static SESSION_CHECKING: Cell<bool> = const { Cell::new(false) };
    static TOAST_ID: Cell<u32> = const { Cell::new(0) };
}

// All requests made by this dashboard are same-origin. Like the reference
// fetch observer, a 401 causes one shared session recheck, not an immediate
// redirect: a 401 can be unrelated to the dashboard session.
pub async fn dashboard_request(
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Response, JsValue> {
    dashboard_request_with_signal(method, path, body, None).await
}

pub async fn dashboard_get_no_store(path: &str) -> Result<Response, JsValue> {
    let response = request_with_cache("GET", path, None, None, true).await?;
    if response.status() == 401 && path.starts_with("/api/") && path != "/api/auth/status" {
        recheck_session();
    }
    Ok(response)
}

async fn dashboard_request_with_signal(
    method: &str,
    path: &str,
    body: Option<&Value>,
    signal: Option<&AbortSignal>,
) -> Result<Response, JsValue> {
    let response = request(method, path, body, signal).await?;
    if response.status() == 401 && path.starts_with("/api/") && path != "/api/auth/status" {
        recheck_session();
    }
    Ok(response)
}

fn recheck_session() {
    if let Some(recheck) = dioxus::core::try_consume_context::<SessionRecheck>() {
        recheck.call();
    }
}

#[derive(Clone)]
pub(in crate::browser) struct SessionRecheck {
    callback: Callback<()>,
    alive: std::rc::Weak<()>,
}

impl SessionRecheck {
    pub(in crate::browser) fn call(&self) {
        if self.alive.upgrade().is_some() {
            self.callback.call(());
        }
    }
}

// Called in the Dashboard callback's scope, so its task outlives key rows
// but is still canceled (and its request aborted) when the dashboard unmounts.
fn start_session_recheck() {
    let start = DASHBOARD_ACTIVE.with(Cell::get)
        && SESSION_CHECKING.with(|checking| {
            if checking.get() {
                false
            } else {
                checking.set(true);
                true
            }
        });
    if start {
        let guard = SessionCheckGuard;
        spawn(async move {
            let _guard = guard;
            let _ = session_status().await;
        });
    }
}

struct SessionCheckGuard;
impl Drop for SessionCheckGuard {
    fn drop(&mut self) {
        SESSION_CHECKING.with(|checking| checking.set(false));
    }
}

async fn session_status() -> Result<bool, ()> {
    let controller = AbortController::new().map_err(|_| ())?;
    let _guard = AbortOnDrop(controller.clone());
    let response = request("GET", "/api/auth/status", None, Some(&controller.signal()))
        .await
        .map_err(|_| ())?;
    if !response.ok() {
        return Err(());
    }
    let data = json_body(&response).await.ok_or(())?;
    let require = data["requireLogin"].as_bool().ok_or(())?;
    let authenticated = data["authenticated"].as_bool().ok_or(())?;
    if require && !authenticated {
        if DASHBOARD_ACTIVE.with(Cell::get)
            && let Some(window) = web_sys::window()
        {
            let _ = window.location().replace("/login");
        }
        return Ok(false);
    }
    Ok(true)
}

#[derive(Clone, PartialEq)]
pub struct Toast {
    id: u32,
    message: String,
    error: bool,
    warning: bool,
    info: bool,
    title: Option<String>,
}

pub fn notify(toasts: Signal<Vec<Toast>>, message: &str, error: bool) {
    push_toast(toasts, message, error, false, false, None);
}

pub fn notify_warning(toasts: Signal<Vec<Toast>>, message: &str, title: Option<&str>) {
    push_toast(toasts, message, false, true, false, title);
}

pub fn notify_info(toasts: Signal<Vec<Toast>>, message: &str) {
    push_toast(toasts, message, false, false, true, None);
}

fn push_toast(
    mut toasts: Signal<Vec<Toast>>,
    message: &str,
    error: bool,
    warning: bool,
    info: bool,
    title: Option<&str>,
) {
    let id = TOAST_ID.with(|id| {
        let next = id.get().wrapping_add(1);
        id.set(next);
        next
    });
    toasts.write().push(Toast {
        id,
        message: message.to_owned(),
        error,
        warning,
        info,
        title: title.map(str::to_owned),
    });
    spawn(async move {
        TimeoutFuture::new(if error { 8000 } else { 5000 }).await;
        toasts.write().retain(|toast| toast.id != id);
    });
}

struct DashboardGuard;
impl Drop for DashboardGuard {
    fn drop(&mut self) {
        DASHBOARD_ACTIVE.with(|active| active.set(false));
        if let Some(body) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.body())
        {
            let _ = body.class_list().remove_1("dashboard-ready");
        }
    }
}

#[component]
pub fn Dashboard() -> Element {
    let mut state = use_signal(|| "checking");
    let toasts = use_signal(Vec::<Toast>::new);
    use_context_provider(|| toasts);
    let recheck = use_callback(move |()| start_session_recheck());
    let alive = use_hook(|| Rc::new(()));
    use_context_provider(|| SessionRecheck {
        callback: recheck,
        alive: Rc::downgrade(&alive),
    });
    let i18n = use_context::<I18n>();
    use_future(move || {
        let i18n = i18n.clone();
        async move {
            i18n.reload(&cookie_locale()).await;
        }
    });
    use_hook(|| {
        DASHBOARD_ACTIVE.with(|active| active.set(true));
        if let Some(document) = web_sys::window().and_then(|w| w.document()) {
            document.set_title("OpenProxy - AI Infrastructure Management");
        }
        Rc::new(DashboardGuard)
    });
    let mut session_task = use_future(move || async move {
        match session_status().await {
            Ok(false) => return,
            Ok(true) => state.set("ready"),
            Err(()) => state.set("error"),
        }
        if let Some(body) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.body())
        {
            let _ = body.class_list().add_1("dashboard-ready");
        }
    });
    let is_dashboard = web_sys::window()
        .and_then(|window| window.location().pathname().ok())
        .is_some_and(|pathname| pathname.starts_with("/dashboard"));
    rsx! {
        if is_dashboard {
            div { id: "dashboard-skeleton", aria_hidden: "true",
                aside { class: "ds-sidebar",
                    div { class: "ds-sidebar-head" }
                    div { class: "ds-sidebar-nav",
                        div { class: "ds-sidebar-item" }
                        div { class: "ds-sidebar-item" }
                        div { class: "ds-sidebar-item" }
                        div { class: "ds-sidebar-item" }
                        div { class: "ds-sidebar-item" }
                        div { class: "ds-sidebar-item" }
                        div { class: "ds-sidebar-item" }
                    }
                }
                main { class: "ds-main" }
            }
        }
        if state() != "ready" {
            div { class: "fixed inset-0 z-50 flex items-center justify-center bg-canvas px-4",
                if state() == "checking" {
                    div { class: "text-center",
                        div { class: "inline-block h-8 w-8 animate-spin rounded-full border-2 border-brand-coral border-t-transparent" }
                        p { class: "mt-4 text-[14px] text-muted", "Checking session…" }
                    }
                } else {
                    div { class: "max-w-sm text-center",
                        span { class: "material-symbols-outlined text-[32px] text-brand-coral", "cloud_off" }
                        h1 { class: "mt-3 font-serif text-2xl text-ink", "Unable to verify session" }
                        p { class: "mt-2 text-sm text-muted", "Check that OpenProxy is running and try again." }
                        button { r#type: "button", class: "mt-5 rounded-mini-md bg-brand-coral px-4 py-2 text-sm font-medium text-white hover:opacity-90", onclick: move |_| { state.set("checking"); session_task.restart(); }, "Retry" }
                    }
                }
            }
        } else {
            DashboardContent {}
        }
    }
}

#[component]
fn DashboardContent() -> Element {
    let mut sidebar_open = use_signal(|| false);
    let mut toasts = use_context::<Signal<Vec<Toast>>>();
    let search = use_signal(String::new);
    let pathname = web_sys::window()
        .and_then(|window| window.location().pathname().ok())
        .unwrap_or_default();
    let is_providers = pathname == "/dashboard/providers";
    let is_providers_new = pathname == "/dashboard/providers/new";
    let provider_id = pathname
        .strip_prefix("/dashboard/providers/")
        .filter(|id| !id.is_empty() && !id.contains('/') && *id != "new")
        .map(str::to_owned);
    let is_provider_detail = provider_id.is_some();
    rsx! {
        div { class: "flex h-screen w-full overflow-hidden bg-canvas",
            div { class: "fixed top-4 right-4 z-[80] flex w-[min(92vw,380px)] flex-col gap-2",
                for toast in toasts() {
                    div { key: "{toast.id}", class: if toast.error { "relative overflow-hidden rounded-mini-md pl-4 pr-3 py-3 shadow-modal before:absolute before:left-0 before:top-0 before:bottom-0 before:w-[3px] bg-surface border border-hairline before:bg-brand-coral" } else if toast.warning { "relative overflow-hidden rounded-mini-md pl-4 pr-3 py-3 shadow-modal before:absolute before:left-0 before:top-0 before:bottom-0 before:w-[3px] bg-surface border border-hairline before:bg-accent-amber" } else if toast.info { "relative overflow-hidden rounded-mini-md pl-4 pr-3 py-3 shadow-modal before:absolute before:left-0 before:top-0 before:bottom-0 before:w-[3px] bg-surface border border-hairline before:bg-brand-blue-deep" } else { "relative overflow-hidden rounded-mini-md pl-4 pr-3 py-3 shadow-modal before:absolute before:left-0 before:top-0 before:bottom-0 before:w-[3px] bg-surface border border-hairline before:bg-success-text" },
                        div { class: "flex items-start gap-2.5",
                            span { class: if toast.error { "material-symbols-outlined text-[20px] leading-5 text-brand-coral" } else if toast.warning { "material-symbols-outlined text-[20px] leading-5 text-accent-amber" } else if toast.info { "material-symbols-outlined text-[20px] leading-5 text-brand-blue-deep" } else { "material-symbols-outlined text-[20px] leading-5 text-success-text" }, if toast.error { "error" } else if toast.warning { "warning" } else if toast.info { "info" } else { "check_circle" } }
                            div { class: "min-w-0 flex-1", if let Some(title) = toast.title { p { class: "text-[13px] font-semibold mb-0.5 text-ink", "{title}" } } p { class: "text-[12px] leading-snug text-text-muted whitespace-pre-wrap break-words", "{toast.message}" } }
                            button { r#type: "button", class: "text-text-muted hover:text-ink transition-colors", aria_label: "Dismiss notification", onclick: move |_| toasts.write().retain(|n| n.id != toast.id), span { class: "material-symbols-outlined text-[16px]", "close" } }
                        }
                    }
                }
            }
            if sidebar_open() { div { class: "fixed inset-0 z-40 bg-black/20 lg:hidden", onclick: move |_| sidebar_open.set(false) } }
            div { class: "hidden lg:contents", Sidebar { on_close: move |_| sidebar_open.set(false) } }
            div { class: if sidebar_open() { "fixed inset-y-0 left-0 z-50 transform lg:hidden transition-transform duration-300 ease-in-out translate-x-0" } else { "fixed inset-y-0 left-0 z-50 transform lg:hidden transition-transform duration-300 ease-in-out -translate-x-full" },
                Sidebar { on_close: move |_| sidebar_open.set(false) }
            }
            main { class: "flex flex-col flex-1 h-full min-w-0 relative transition-colors duration-300 isolate",
                div { class: "landing-grid absolute inset-0 pointer-events-none -z-10", aria_hidden: "true" }
                Header { on_menu: move |_| sidebar_open.set(true), search, is_providers, is_providers_new: is_providers_new || is_provider_detail, provider_id: provider_id.clone() }
                div { class: "flex-1 overflow-y-auto custom-scrollbar p-6 lg:p-10",
                    div { class: "max-w-7xl mx-auto",
                        if is_providers { ProvidersPage { search } } else if is_providers_new { NewProviderPage {} } else if let Some(id) = provider_id { ProviderDetailPage { id } } else if pathname == "/dashboard/skills" { SkillsPage {} } else if pathname == "/dashboard/console-log" { ConsoleLogPage {} } else { EndpointPage {} }
                    }
                }
            }
        }
    }
}

const NAV: [(&str, &str, &str); 5] = [
    ("/dashboard/endpoint", "Endpoint", "api"),
    ("/dashboard/providers", "Providers", "dns"),
    ("/dashboard/db-backups", "DB Backups", "backup"),
    ("/dashboard/logs", "Application Logs", "receipt_long"),
    ("/dashboard/quota", "Quota Tracker", "data_usage"),
];
const SYSTEM: [(&str, &str, &str); 4] = [
    ("/dashboard/proxy-pools", "Proxy Pools", "lan"),
    ("/dashboard/skills", "Skills", "extension"),
    ("/dashboard/console-log", "Console Log", "terminal"),
    ("/dashboard/translator", "Translator", "translate"),
];
const NAV_BASE: &str =
    "relative flex items-center gap-3 pl-4 pr-3 py-1.5 rounded-mini-md transition-colors group";
const NAV_ACTIVE: &str = "bg-surface-card text-ink font-medium before:content-[''] before:absolute before:left-0 before:top-1.5 before:bottom-1.5 before:w-[3px] before:rounded-r-full before:bg-brand-coral";
const NAV_INACTIVE: &str = "text-body hover:bg-surface-soft hover:text-ink";

#[component]
fn NavLink(
    href: &'static str,
    label: &'static str,
    icon: &'static str,
    pathname: String,
    on_close: EventHandler<()>,
) -> Element {
    let active = if href == "/dashboard/endpoint" {
        pathname == "/dashboard" || pathname.starts_with(href)
    } else {
        pathname.starts_with(href)
    };
    let class = if active { NAV_ACTIVE } else { NAV_INACTIVE };
    let icon_class = if active {
        "material-symbols-outlined text-[18px] fill-1"
    } else {
        "material-symbols-outlined text-[18px]"
    };
    rsx! { a { href, class: "{NAV_BASE} {class}", onclick: move |_| on_close.call(()),
        span { class: icon_class, "{icon}" } span { class: "text-[13px]", "{label}" }
    } }
}

#[component]
fn Sidebar(on_close: EventHandler<()>) -> Element {
    let mut commit = use_signal(|| None::<String>);
    let mut translator = use_signal(|| false);
    let mut shutdown = use_signal(|| false);
    let mut shutting_down = use_signal(|| false);
    let mut disconnected = use_signal(|| false);
    let pathname = web_sys::window()
        .and_then(|w| w.location().pathname().ok())
        .unwrap_or_default();
    use_future(move || async move {
        let Ok(controller) = AbortController::new() else {
            return;
        };
        let _guard = AbortOnDrop(controller.clone());
        if let Ok(res) =
            dashboard_request_with_signal("GET", "/api/build", None, Some(&controller.signal()))
                .await
            && res.ok()
            && let Some(value) = json_body(&res).await
            && let Some(raw) = value["commit"].as_str().filter(|s| {
                (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
            })
        {
            commit.set(Some(raw.to_ascii_lowercase()));
        }
    });
    use_future(move || async move {
        if let Ok(res) = dashboard_request("GET", "/api/settings", None).await
            && let Some(value) = json_body(&res).await
        {
            translator.set(value["enableTranslator"] == true);
        }
    });
    let commit_value = commit();
    let commit_title = commit_value.as_ref().map_or_else(
        || "Build commit unavailable".to_owned(),
        |c| format!("Commit: {c}"),
    );
    let commit_aria = commit_value.as_ref().map_or_else(
        || "Build commit unavailable".to_owned(),
        |c| format!("Build commit {c}"),
    );
    let short_commit = commit_value.as_ref().map_or("—", |c| &c[..8]);
    rsx! {
        aside { class: "flex w-72 flex-col border-r border-hairline-soft bg-canvas transition-colors duration-300 min-h-full",
            div { class: "px-6 pt-6 pb-4 flex flex-col gap-2",
                a { href: "/dashboard", class: "flex items-center gap-3 group",
                    div { class: "flex items-center justify-center size-9 rounded-mini-md bg-surface-card border border-hairline",
                        svg { role: "img", "aria-label": "OpenProxy mark", width: "20", height: "20", view_box: "0 0 24 24", fill: "currentColor", class: "inline-block shrink-0 text-brand-coral",
                            path { d: "M11.0 2.2 L13.0 2.2 L13.4 10.0 L20.0 8.4 L20.5 10.3 L13.6 11.45 L20.5 13.7 L20.0 15.6 L13.4 14.0 L13.0 21.8 L11.0 21.8 L10.6 14.0 L4.0 15.6 L3.5 13.7 L10.4 11.45 L3.5 10.3 L4.0 8.4 L10.6 10.0 Z" }
                        }
                    }
                    div { class: "flex flex-col leading-tight",
                        h1 { class: "font-serif text-[22px] font-normal tracking-[-0.02em] text-ink", "OpenProxy" }
                        span { class: "font-mono text-[11px] text-muted-soft tracking-wide", title: "{commit_title}", aria_label: "{commit_aria}", "{short_commit}" }
                    }
                }
            }
            nav { class: "flex-1 px-4 py-2 space-y-0.5 overflow-y-auto custom-scrollbar",
                for (href, label, icon) in NAV { NavLink { key: "{href}", href, label, icon, pathname: pathname.clone(), on_close } }
                div { class: "pt-4 mt-2 space-y-0.5",
                    p { class: "px-4 type-caption-uppercase text-muted-soft mb-2", "System" }
                    for (href, label, icon) in SYSTEM {
                        if href != "/dashboard/translator" || translator() { NavLink { key: "{href}", href, label, icon, pathname: pathname.clone(), on_close } }
                    }
                }
            }
            div { class: "p-3 border-t border-hairline-soft space-y-1",
                NavLink { href: "/dashboard/profile", label: "Settings", icon: "settings", pathname, on_close }
                Button { label: "Shutdown".to_owned(), variant: "secondary", full_width: true, icon: Some("power_settings_new".to_owned()), extra_class: "text-[color:var(--color-danger)] border-[color:var(--color-danger)]/30 hover:bg-[color:var(--color-danger)]/10 hover:border-[color:var(--color-danger)]/50", onclick: move |_| shutdown.set(true) }
            }
        }
        if shutdown() {
            Modal { title: "Close Proxy".to_owned(), small: true, on_close: move |_| shutdown.set(false),
                footer: rsx! {
                    Button { label: "Cancel".to_owned(), variant: "ghost", disabled: shutting_down(), onclick: move |_| shutdown.set(false) }
                    Button { label: "Close".to_owned(), variant: "danger", loading: shutting_down(), onclick: move |_| {
                        shutting_down.set(true);
                        spawn(async move { let _ = dashboard_request("POST", "/api/shutdown", None).await; shutting_down.set(false); shutdown.set(false); disconnected.set(true); });
                    } }
                },
                p { class: "text-text-muted", "Are you sure you want to close the proxy server?" }
            }
        }
        if disconnected() {
            div { class: "fixed inset-0 z-50 flex items-center justify-center bg-black/80 backdrop-blur-sm p-6",
                div { class: "text-center p-8",
                    div { class: "flex items-center justify-center size-16 rounded-full bg-red-500/20 text-red-500 mx-auto mb-4", span { class: "material-symbols-outlined text-[32px]", "power_off" } }
                    h2 { class: "text-xl font-semibold text-white mb-2", "Server Disconnected" }
                    p { class: "text-text-muted mb-6", "The proxy server has been stopped." }
                    Button { label: "Reload Page".to_owned(), variant: "secondary", onclick: move |_| { if let Some(w) = web_sys::window() { let _ = w.location().reload(); } } }
                }
            }
        }
    }
}

fn toggle_theme(mut theme: Signal<String>, mut dark: Signal<bool>) {
    // Zustand toggles the stored value: system always becomes dark, even on a dark system.
    let next = if theme.peek().as_str() == "dark" {
        "light"
    } else {
        "dark"
    };
    theme.set(next.to_owned());
    apply_theme(next);
    dark.set(next == "dark");
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(
            "theme",
            &json!({"state": {"theme": next}, "version": 0}).to_string(),
        );
    }
}

struct EventGuard {
    target: web_sys::EventTarget,
    event: &'static str,
    handler: Closure<dyn FnMut(web_sys::Event)>,
}
impl Drop for EventGuard {
    fn drop(&mut self) {
        let _ = self
            .target
            .remove_event_listener_with_callback(self.event, self.handler.as_ref().unchecked_ref());
    }
}

fn listen(
    target: web_sys::EventTarget,
    event: &'static str,
    callback: impl FnMut(web_sys::Event) + 'static,
) -> EventGuard {
    let handler = Closure::<dyn FnMut(web_sys::Event)>::new(callback);
    let _ = target.add_event_listener_with_callback(event, handler.as_ref().unchecked_ref());
    EventGuard {
        target,
        event,
        handler,
    }
}

#[component]
fn CloseMenuOnOutside(mut open: Signal<bool>) -> Element {
    use_hook(move || {
        let document = web_sys::window()?.document()?;
        Some(Rc::new(listen(
            document.into(),
            "mousedown",
            move |event| {
                let inside = event
                    .target()
                    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                    .and_then(|element| element.closest("#header-menu").ok().flatten())
                    .is_some();
                if !inside {
                    open.set(false);
                }
            },
        )))
    });
    rsx! {}
}

// Ordered exactly like the reference language picker.
const LOCALES: [(&str, &str, &str); 35] = [
    ("en", "English", "🇺🇸"),
    ("vi", "Tiếng Việt", "🇻🇳"),
    ("zh-CN", "简体中文", "🇨🇳"),
    ("zh-TW", "繁體中文", "🇹🇼"),
    ("ja", "日本語", "🇯🇵"),
    ("pt-BR", "Português (Brasil)", "🇧🇷"),
    ("pt-PT", "Português (Portugal)", "🇵🇹"),
    ("ko", "한국어", "🇰🇷"),
    ("es", "Español", "🇪🇸"),
    ("de", "Deutsch", "🇩🇪"),
    ("fr", "Français", "🇫🇷"),
    ("he", "עברית", "🇮🇱"),
    ("ar", "العربية", "🇸🇦"),
    ("ru", "Русский", "🇷🇺"),
    ("pl", "Polski", "🇵🇱"),
    ("cs", "Čeština", "🇨🇿"),
    ("nl", "Nederlands", "🇳🇱"),
    ("tr", "Türkçe", "🇹🇷"),
    ("uk", "Українська", "🇺🇦"),
    ("tl", "Tagalog", "🇵🇭"),
    ("id", "Indonesia", "🇮🇩"),
    ("th", "ไทย", "🇹🇭"),
    ("km", "ភាសាខ្មែរ", "🇰🇭"),
    ("hi", "हिन्दी", "🇮🇳"),
    ("bn", "বাংলা", "🇧🇩"),
    ("ur", "اردو", "🇵🇰"),
    ("ro", "Română", "🇷🇴"),
    ("sv", "Svenska", "🇸🇪"),
    ("it", "Italiano", "🇮🇹"),
    ("el", "Ελληνικά", "🇬🇷"),
    ("hu", "Magyar", "🇭🇺"),
    ("fi", "Suomi", "🇫🇮"),
    ("da", "Dansk", "🇩🇰"),
    ("no", "Norsk", "🇳🇴"),
    ("fa", "فارسی", "🇮🇷"),
];

pub(super) fn cookie_locale() -> String {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return "en".into();
    };
    let raw = js_sys::Reflect::get(document.as_ref(), &JsValue::from_str("cookie"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    let value = raw
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("locale="))
        .unwrap_or("en");
    let decoded = js_sys::decode_uri_component(value)
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| value.to_owned());
    let value = decoded.as_str();
    let value = if value == "zh" { "zh-CN" } else { value };
    if LOCALES.iter().any(|(id, _, _)| *id == value) {
        value.into()
    } else {
        "en".into()
    }
}

#[component]
fn Header(
    on_menu: EventHandler<()>,
    mut search: Signal<String>,
    is_providers: bool,
    is_providers_new: bool,
    provider_id: Option<String>,
) -> Element {
    let mut menu = use_signal(|| false);
    let mut lang_open = use_signal(|| false);
    let mut locale = use_signal(cookie_locale);
    let mut pending = use_signal(|| false);
    let theme = use_signal(current_theme);
    let mut dark = use_signal(|| theme_is_dark(&theme()));
    let i18n = use_context::<I18n>();
    use_hook(move || {
        let media = web_sys::window()?
            .match_media("(prefers-color-scheme: dark)")
            .ok()
            .flatten()?;
        Some(Rc::new(listen(media.into(), "change", move |_| {
            if theme.peek().as_str() == "system" {
                apply_theme("system");
                dark.set(theme_is_dark("system"));
            }
        })))
    });
    let pathname = web_sys::window()
        .and_then(|w| w.location().pathname().ok())
        .unwrap_or_default();
    let show_title = pathname == "/dashboard"
        || pathname.contains("/endpoint")
        || pathname == "/dashboard/console-log"
        || is_providers
        || is_providers_new;
    let (title, description, page_icon) = if is_providers || is_providers_new {
        ("Providers", "Manage your AI provider connections", "dns")
    } else if pathname == "/dashboard/console-log" {
        ("Console Log", "Live server console output", "monitor")
    } else {
        ("Endpoint", "API endpoint configuration", "api")
    };
    let breadcrumb = provider_id.and_then(|id| {
        let registry: Value = serde_json::from_str(include_str!("provider_data.json")).ok()?;
        ["oauth", "apiKey"].into_iter().find_map(|category| {
            registry[category]
                .as_array()?
                .iter()
                .find(|p| p["id"].as_str() == Some(&id))
                .and_then(|p| p["name"].as_str())
                .map(|name| (id.clone(), name.to_owned()))
        })
    });
    let (name, flag) = LOCALES
        .iter()
        .find(|(id, _, _)| *id == locale())
        .map_or(("English", "🇺🇸"), |(_, name, flag)| (*name, *flag));
    let menu_name = match locale().as_str() {
        "pt-BR" => "Português (BR)",
        "pt-PT" => "Português (PT)",
        "km" => "km",
        _ => name,
    };
    let menu_flag = if locale() == "km" { "🌐" } else { flag };
    let theme_label = if dark() {
        "Switch to light mode"
    } else {
        "Switch to dark mode"
    };
    let theme_icon = if dark() { "light_mode" } else { "dark_mode" };
    rsx! {
        header { class: "shrink-0 flex items-center justify-between gap-3 px-4 lg:px-8 pt-4 pb-4 border-b border-hairline-soft bg-canvas/95 backdrop-blur-xl lg:bg-canvas lg:backdrop-blur-none z-20",
            div { class: "flex items-center gap-3 lg:hidden shrink-0",
                button { class: "text-ink hover:opacity-70 transition-opacity", aria_label: "Open navigation", onclick: move |_| on_menu.call(()), span { class: "material-symbols-outlined", "menu" } }
            }
            div { class: "flex flex-col min-w-0 flex-1",
                if let Some((id, name)) = breadcrumb {
                    div { class: "flex items-center gap-2",
                        div { class: "flex items-center gap-2", a { href: "/dashboard/providers", class: "text-muted hover:text-ink transition-colors text-[13px] font-medium", "Providers" } }
                        div { class: "flex items-center gap-2", span { class: "material-symbols-outlined text-text-muted text-base", "chevron_right" }
                            div { class: "flex items-center gap-2", HeaderProviderIcon { id, name: name.clone() } h1 { class: "font-serif font-normal text-[20px] lg:text-[30px] tracking-[-0.02em] text-ink truncate leading-tight", "{name}" } }
                        }
                    }
                } else if show_title {
                    div {
                        div { class: "flex items-center gap-2.5",
                            span { class: "material-symbols-outlined text-brand-coral text-xl lg:text-2xl", "{page_icon}" }
                            h1 { class: "font-serif font-normal text-[20px] lg:text-[30px] tracking-[-0.02em] text-ink truncate leading-tight", "{title}" }
                        }
                        p { class: "hidden lg:block text-[13px] text-muted truncate mt-1", "{description}" }
                    }
                }
            }
            div { class: "flex items-center gap-1 shrink-0",
                if is_providers {
                    div { class: "relative hidden sm:flex items-center mr-1",
                        span { class: "material-symbols-outlined absolute left-2 top-1/2 -translate-y-1/2 text-text-muted text-[16px] pointer-events-none", "search" }
                        input { r#type: "text", placeholder: "Search providers...", value: "{search}", class: "pl-7 pr-2.5 py-1.5 w-[180px] md:w-[220px] rounded-mini-md border border-hairline bg-canvas text-[12px] text-ink placeholder:text-text-muted focus:outline-none focus:ring-2 focus:ring-brand-coral/30 focus:border-brand-coral/60", oninput: move |event| search.set(event.value()) }
                        if !search().is_empty() {
                            button { r#type: "button", class: "absolute right-1 top-1/2 -translate-y-1/2 text-text-muted hover:text-ink", aria_label: "Clear search", onclick: move |_| search.set(String::new()), span { class: "material-symbols-outlined text-[16px]", "close" } }
                        }
                    }
                }
                button { class: "flex items-center justify-center size-10 rounded-full text-text-muted hover:text-text-main hover:bg-surface-2 transition-colors", aria_label: theme_label, title: theme_label, onclick: move |_| toggle_theme(theme, dark),
                    span { class: "material-symbols-outlined text-[22px]", "{theme_icon}" }
                }
                div { id: "header-menu", class: "relative",
                    button { class: "flex items-center justify-center p-2 rounded-lg text-text-muted hover:text-text-main hover:bg-black/5 dark:hover:bg-white/5 transition-all", title: "Menu", aria_label: "Menu", aria_expanded: "{menu}", onclick: move |_| menu.toggle(), span { class: "material-symbols-outlined", "grid_view" } }
                    if menu() {
                        CloseMenuOnOutside { open: menu }
                        div { class: "absolute right-0 top-full mt-2 w-60 bg-surface border border-black/10 dark:border-white/10 rounded-xl shadow-2xl z-50 animate-in fade-in zoom-in-95 duration-150 overflow-hidden py-1",
                            button { class: "flex items-center gap-3 w-full px-4 py-2.5 text-sm transition-colors text-text-main hover:bg-black/5 dark:hover:bg-white/5", onclick: move |_| { menu.set(false); locale.set(cookie_locale()); lang_open.set(true); },
                                span { class: "material-symbols-outlined text-[20px] text-text-muted", "language" }
                                span { class: "flex-1 text-left", "{menu_name}" } span { class: "text-base", "{menu_flag}" }
                            }
                            button { class: "flex items-center gap-3 w-full px-4 py-2.5 text-sm transition-colors text-text-main hover:bg-black/5 dark:hover:bg-white/5", onclick: move |_| { toggle_theme(theme, dark); menu.set(false); },
                                span { class: "material-symbols-outlined text-[20px] text-text-muted", "{theme_icon}" } span { class: "flex-1 text-left", "Theme" }
                            }
                            button { class: "flex items-center gap-3 w-full px-4 py-2.5 text-sm transition-colors text-red-500 hover:bg-red-500/10", onclick: move |_| {
                                menu.set(false);
                                spawn(async move { if let Ok(res) = dashboard_request("POST", "/api/auth/logout", None).await && res.ok() { let _ = navigate("/login"); } });
                            },
                                span { class: "material-symbols-outlined text-[20px]", "logout" } span { class: "flex-1 text-left", "Logout" }
                            }
                        }
                    }
                }
                // The reference LanguageSwitcher retains its empty wrapper when
                // the trigger and portal are closed; it is a flex item with a gap.
                div { class: "" }
            }
        }
        if lang_open() {
            div { class: "fixed inset-0 z-50 flex items-center justify-center p-4", "data-i18n-skip": "true",
                div { class: "absolute inset-0 bg-black/30 backdrop-blur-sm", onclick: move |_| lang_open.set(false) }
                div { role: "dialog", aria_modal: "true", aria_label: "Select Language", class: "relative w-full bg-surface border border-black/10 dark:border-white/10 rounded-xl shadow-2xl animate-in fade-in zoom-in-95 duration-200 max-w-2xl flex flex-col max-h-[80vh]",
                    div { class: "flex items-center justify-between p-3 border-b border-black/5 dark:border-white/5",
                        h2 { class: "text-lg font-semibold text-text-main", "Select Language" }
                        button { class: "p-1.5 rounded-lg text-text-muted hover:bg-black/5 dark:hover:bg-white/5 transition-colors", aria_label: "Close", onclick: move |_| lang_open.set(false), span { class: "material-symbols-outlined text-[20px]", "close" } }
                    }
                    div { class: "p-6 overflow-y-auto flex-1",
                        div { class: "grid grid-cols-[repeat(auto-fill,minmax(100px,1fr))] gap-2",
                            for (id, name, flag) in LOCALES {
                                button { key: "{id}", disabled: pending(), title: name, class: if locale() == id { "flex flex-col items-center justify-start gap-1 px-2 py-3 rounded-lg text-xs font-medium transition-colors w-full bg-primary/15 text-primary ring-2 ring-primary" } else { "flex flex-col items-center justify-start gap-1 px-2 py-3 rounded-lg text-xs font-medium transition-colors w-full text-text-main hover:bg-black/5 dark:hover:bg-white/5" }, onclick: { let i18n = i18n.clone(); move |_| {
                                    if locale() != id && !pending() {
                                        pending.set(true); lang_open.set(false);
                                        let i18n = i18n.clone();
                                        spawn(async move { if dashboard_request("POST", "/api/locale", Some(&json!({"locale": id}))).await.is_ok() { i18n.reload(&cookie_locale()).await; locale.set(id.to_owned()); } pending.set(false); });
                                    }
                                } },
                                    span { class: "text-2xl", "{flag}" }
                                    span { class: "text-center leading-tight line-clamp-2 h-8 flex items-center", "{name}" }
                                    if locale() == id { span { class: "material-symbols-outlined text-sm", "check" } }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn HeaderProviderIcon(id: String, name: String) -> Element {
    let mut failed = use_signal(|| false);
    let class = "object-contain rounded max-w-[28px] max-h-[28px]";
    if failed() {
        let fallback = name.chars().take(2).collect::<String>().to_uppercase();
        rsx! { span { class: "inline-flex items-center justify-center font-bold rounded-lg {class}", style: "width: 28px; height: 28px; font-size: 10px", "{fallback}" } }
    } else {
        rsx! { img { src: "/providers/{id}.png", alt: "{name}", width: "28", height: "28", class, onerror: move |_| failed.set(true) } }
    }
}
