//! Dashboard Profile settings.
mod database;
use super::{
    json_body,
    shell::{cookie_locale, dashboard_request, set_theme, LOCALES},
    ui::{Button, Modal, CARD_CLASS},
    INPUT_CLASS,
};
use dioxus::prelude::*;
use serde_json::{json, Value};

async fn load_settings(
    mut settings: Signal<Value>,
    mut proxy_url: Signal<String>,
    mut no_proxy: Signal<String>,
    mut loading: Signal<bool>,
) {
    if let Ok(response) = dashboard_request("GET", "/api/settings", None).await
        && response.ok()
        && let Some(value) = json_body(&response).await.filter(Value::is_object)
    {
        proxy_url.set(value["outboundProxyUrl"].as_str().unwrap_or("").to_owned());
        no_proxy.set(value["outboundNoProxy"].as_str().unwrap_or("").to_owned());
        settings.set(value);
    }
    loading.set(false);
}

#[component]
pub(super) fn ProfilePage(
    theme: Signal<String>,
    dark: Signal<bool>,
    mut lang_open: Signal<bool>,
    mut locale: Signal<String>,
) -> Element {
    let mut settings = use_signal(|| json!({}));
    let loading = use_signal(|| true);
    let mut toggling = use_signal(|| false);
    let mut current = use_signal(String::new);
    let mut new_password = use_signal(String::new);
    let mut confirm = use_signal(String::new);
    let mut password_status = use_signal(|| None::<(bool, String)>);
    let mut password_loading = use_signal(|| false);
    let mut proxy_url = use_signal(String::new);
    let mut no_proxy = use_signal(String::new);
    let mut proxy_status = use_signal(|| None::<(bool, String)>);
    let mut proxy_loading = use_signal(|| false);
    let mut proxy_testing = use_signal(|| false);
    let mut observability_saving = use_signal(|| false);
    let mut shutdown_open = use_signal(|| false);
    let mut shutting_down = use_signal(|| false);

    use_future(move || load_settings(settings, proxy_url, no_proxy, loading));

    let data = settings();
    let require_login = data["requireLogin"] == true;
    let has_password = data["hasPassword"] == true;
    let proxy_enabled = data["outboundProxyEnabled"] == true;
    let observability_enabled = data["observabilityEnabled"] == true;
    let db_path = data["databasePath"]
        .as_str()
        .filter(|path| !path.is_empty())
        .or_else(|| data["dataDir"].as_str().filter(|path| !path.is_empty()))
        .unwrap_or("~/.openproxy/openproxy.sqlite")
        .to_owned();
    let flag = LOCALES
        .iter()
        .find(|(id, _, _)| *id == locale())
        .map_or("🌐", |(_, _, flag)| *flag);
    rsx! {
        div { class: "max-w-2xl mx-auto", div { class: "flex flex-col gap-6",
            div { class: CARD_CLASS,
                div { class: "flex flex-col sm:flex-row sm:items-center sm:justify-between gap-4 mb-4",
                    div { class: "flex items-center gap-3 sm:gap-4", div { class: "size-10 sm:size-12 rounded-lg bg-green-500/10 text-green-500 flex items-center justify-center shrink-0", span { class: "material-symbols-outlined text-xl sm:text-2xl", "computer" } } div { h2 { class: "text-lg sm:text-xl font-semibold", "Appearance" } p { class: "text-sm text-muted-soft", "Theme preference" } } }
                    div { class: "inline-flex p-1 rounded-lg bg-surface-2 w-full sm:w-auto", for (option, icon) in [("light", "light_mode"), ("dark", "dark_mode"), ("system", "contrast")] { button { r#type: "button", class: if theme() == option { "flex items-center justify-center gap-1 sm:gap-1.5 px-2 sm:px-3 py-1.5 rounded-md font-medium transition-all flex-1 sm:flex-initial text-[13px] bg-canvas text-ink shadow-sm" } else { "flex items-center justify-center gap-1 sm:gap-1.5 px-2 sm:px-3 py-1.5 rounded-md font-medium transition-all flex-1 sm:flex-initial text-[13px] text-muted-soft hover:text-ink" }, onclick: move |_| set_theme(theme, dark, option), span { class: "material-symbols-outlined text-[18px]", "{icon}" } span { class: "capitalize text-xs sm:text-sm", "{option}" } } } }
                }
                div { class: "flex flex-col gap-3 pt-4 border-t border-hairline-soft", div { class: "flex flex-col sm:flex-row sm:items-center sm:justify-between p-3 rounded-lg bg-surface-card border border-hairline gap-2", div { p { class: "font-medium text-sm", "Database Location" } p { class: "text-xs sm:text-sm text-muted-soft font-mono break-all", "{db_path}" } } } }
            }
            div { class: CARD_CLASS,
                div { class: "flex items-center gap-3 mb-4", div { class: "size-10 rounded-lg bg-blue-500/10 text-blue-500 flex items-center justify-center shrink-0", span { class: "material-symbols-outlined text-[20px]", "language" } } h3 { class: "text-base sm:text-lg font-semibold", "Language" } }
                button { class: "flex items-center justify-between w-full p-3 rounded-lg bg-surface-card border border-hairline hover:border-ink/40 transition-colors", "data-i18n-skip": "true", onclick: move |_| { locale.set(cookie_locale()); lang_open.set(true); }, span { class: "text-sm text-muted-soft", "Display language" } span { class: "text-2xl", "{flag}" } }
            }
            div { class: CARD_CLASS,
                div { class: "flex items-center gap-3 mb-4",
                    div { class: "size-10 rounded-lg bg-amber-500/10 text-amber-500 flex items-center justify-center shrink-0", span { class: "material-symbols-outlined text-[20px]", "shield" } }
                    h3 { class: "text-base sm:text-lg font-semibold", "Security" }
                }
                div { class: "flex flex-col gap-4",
                    div { class: "flex items-start sm:items-center justify-between gap-4",
                        div { class: "flex-1 min-w-0", p { class: "font-medium text-sm", "Require login" } p { class: "text-xs text-muted-soft", "When ON, the dashboard requires a password to access. When OFF, access is unrestricted." } }
                        div { class: if loading() { "flex items-center gap-3 opacity-50 cursor-not-allowed" } else { "flex items-center gap-3" },
                            button { r#type: "button", role: "switch", aria_label: "Require login", aria_checked: "{require_login}", disabled: loading(), class: if require_login { "relative inline-flex shrink-0 cursor-pointer rounded-full transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-brand-500/30 bg-brand-500 w-11 h-6" } else { "relative inline-flex shrink-0 cursor-pointer rounded-full transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-brand-500/30 bg-surface-3 w-11 h-6" }, onclick: move |_| {
                                if toggling() { return; }
                                toggling.set(true);
                                spawn(async move {
                                    if let Ok(response) = dashboard_request("PATCH", "/api/settings", Some(&json!({"requireLogin": !require_login}))).await
                                        && response.ok()
                                        && let Some(value) = json_body(&response).await.filter(Value::is_object)
                                        && let (Some(saved), Some(update)) = (settings.write().as_object_mut(), value.as_object())
                                    { saved.extend(update.clone()); }
                                    toggling.set(false);
                                });
                            }, span { class: if require_login { "pointer-events-none inline-block rounded-full bg-white shadow-sm transform transition duration-200 ease-in-out translate-x-5 size-5 mt-0.5" } else { "pointer-events-none inline-block rounded-full bg-white shadow-sm transform transition duration-200 ease-in-out translate-x-0.5 size-5 mt-0.5" } } }
                        }
                    }
                    if require_login {
                        form { class: "flex flex-col gap-4 pt-4 border-t border-hairline-soft", onsubmit: move |event| {
                            event.prevent_default();
                            if password_loading() { return; }
                            if new_password() != confirm() { password_status.set(Some((true, "Passwords do not match".to_owned()))); return; }
                            let mut body = json!({"newPassword": new_password()});
                            if !current().is_empty() { body["currentPassword"] = json!(current()); }
                            password_status.set(None);
                            password_loading.set(true);
                            spawn(async move {
                                let result = dashboard_request("POST", "/api/auth/password", Some(&body)).await;
                                match result {
                                    Ok(response) => {
                                        let status = response.status();
                                        let ok = response.ok();
                                        let data = json_body(&response).await.unwrap_or_default();
                                        if ok && data["sessionsInvalidated"] == true {
                                            if let Some(window) = web_sys::window() { let _ = window.location().replace("/login"); }
                                        } else if ok {
                                            password_status.set(Some((false, "Password updated successfully.".to_owned())));
                                            current.set(String::new()); new_password.set(String::new()); confirm.set(String::new());
                                        } else {
                                            let error = data["error"].as_str().unwrap_or("Failed to update password");
                                            let message = if status == 404 || status == 501 { format!("{error} — if the dashboard password API is unavailable, use `openproxy auth set-password`.") } else { error.to_owned() };
                                            password_status.set(Some((true, message)));
                                        }
                                    }
                                    Err(_) => password_status.set(Some((true, "Failed to fetch — if the dashboard password API is unavailable, use `openproxy auth set-password`.".to_owned()))),
                                }
                                password_loading.set(false);
                            });
                        },
                            if has_password { div { class: "flex flex-col gap-2", label { class: "text-xs sm:text-sm font-medium", "Current Password" } div { class: "flex flex-col gap-1.5", div { class: "relative", input { r#type: "password", placeholder: "Enter current password", value: "{current}", required: true, class: INPUT_CLASS, oninput: move |event| current.set(event.value()) } } } } }
                            div { class: "grid grid-cols-1 sm:grid-cols-2 gap-4",
                                div { class: "flex flex-col gap-2", label { class: "text-xs sm:text-sm font-medium", if has_password { "New Password" } else { "Set Password" } } div { class: "flex flex-col gap-1.5", div { class: "relative", input { r#type: "password", placeholder: if has_password { "Enter new password" } else { "Choose a password" }, value: "{new_password}", required: true, class: INPUT_CLASS, oninput: move |event| new_password.set(event.value()) } } } }
                                div { class: "flex flex-col gap-2", label { class: "text-xs sm:text-sm font-medium", "Confirm Password" } div { class: "flex flex-col gap-1.5", div { class: "relative", input { r#type: "password", placeholder: "Confirm password", value: "{confirm}", required: true, class: INPUT_CLASS, oninput: move |event| confirm.set(event.value()) } } } }
                            }
                            if let Some((error, message)) = password_status() { p { class: if error { "text-xs sm:text-sm text-red-500" } else { "text-xs sm:text-sm text-green-600 dark:text-green-400" }, "{message}" } }
                            div { class: "pt-2", button { r#type: "submit", disabled: password_loading(), class: "inline-flex items-center justify-center gap-2 rounded-mini-md leading-none transition-colors duration-150 ease-out cursor-pointer tracking-tight active:scale-[0.99] disabled:cursor-not-allowed disabled:active:scale-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-coral/30 focus-visible:ring-offset-2 focus-visible:ring-offset-canvas bg-brand-coral text-on-primary hover:bg-brand-coral-active disabled:bg-primary-disabled disabled:text-muted-soft h-10 px-4 text-[14px] font-medium w-full sm:w-auto", if password_loading() { span { class: "material-symbols-outlined animate-spin text-[18px]", "progress_activity" } } if has_password { "Update Password" } else { "Set Password" } } }
                        }
                    }
                }
            }
            div { class: CARD_CLASS,
                div { class: "flex items-center gap-3 mb-4", div { class: "size-10 rounded-lg bg-purple-500/10 text-purple-500 flex items-center justify-center shrink-0", span { class: "material-symbols-outlined text-[20px]", "wifi" } } h3 { class: "text-base sm:text-lg font-semibold", "Network" } }
                div { class: "flex flex-col gap-4",
                    div { class: "flex items-start sm:items-center justify-between gap-4", div { class: "flex-1 min-w-0", p { class: "font-medium text-sm", "Outbound Proxy" } p { class: "text-xs text-muted-soft", "Enable proxy for OAuth and provider outbound requests" } }
                        ProfileToggle { checked: proxy_enabled, disabled: loading() || proxy_loading(), label: "Outbound Proxy", onchange: move |enabled: bool| {
                            proxy_loading.set(true); proxy_status.set(None);
                            spawn(async move {
                                match dashboard_request("PATCH", "/api/settings", Some(&json!({"outboundProxyEnabled": enabled}))).await {
                                    Ok(response) => { let ok = response.ok(); let value = json_body(&response).await.unwrap_or_default(); if ok { if let (Some(saved), Some(update)) = (settings.write().as_object_mut(), value.as_object()) { saved.extend(update.clone()); } proxy_status.set(Some((false, if enabled { "Proxy enabled" } else { "Proxy disabled" }.to_owned()))); } else { proxy_status.set(Some((true, value["error"].as_str().unwrap_or("Request failed").to_owned()))); } }
                                    Err(_) => proxy_status.set(Some((true, "Failed to fetch".to_owned()))),
                                }
                                proxy_loading.set(false);
                            });
                        } }
                    }
                    if proxy_enabled { form { class: "flex flex-col gap-4 pt-2 border-t border-hairline-soft", onsubmit: move |event| {
                        event.prevent_default(); if proxy_loading() { return; } proxy_loading.set(true); proxy_status.set(None);
                        let body = json!({"outboundProxyUrl": proxy_url(), "outboundNoProxy": no_proxy()});
                        spawn(async move { match dashboard_request("PATCH", "/api/settings", Some(&body)).await {
                            Ok(response) => { let ok = response.ok(); let value = json_body(&response).await.unwrap_or_default(); if ok { if let (Some(saved), Some(update)) = (settings.write().as_object_mut(), value.as_object()) { saved.extend(update.clone()); } proxy_status.set(Some((false, "Proxy settings applied".to_owned()))); } else { proxy_status.set(Some((true, value["error"].as_str().unwrap_or("Request failed").to_owned()))); } }
                            Err(_) => proxy_status.set(Some((true, "Failed to fetch".to_owned()))),
                        } proxy_loading.set(false); });
                    },
                        div { class: "flex flex-col gap-2", label { class: "font-medium text-sm", "Proxy URL" } div { class: "flex flex-col gap-1.5", div { class: "relative", input { placeholder: "http://127.0.0.1:7897", value: "{proxy_url}", disabled: loading() || proxy_loading(), class: INPUT_CLASS, oninput: move |e| proxy_url.set(e.value()) } } } p { class: "text-xs text-muted-soft", "Leave empty to inherit the existing environment proxy (if any)." } }
                        div { class: "flex flex-col gap-2 pt-2 border-t border-hairline-soft", label { class: "font-medium text-sm", "No Proxy" } div { class: "flex flex-col gap-1.5", div { class: "relative", input { placeholder: "localhost,127.0.0.1", value: "{no_proxy}", disabled: loading() || proxy_loading(), class: INPUT_CLASS, oninput: move |e| no_proxy.set(e.value()) } } } p { class: "text-xs text-muted-soft", "Comma-separated hostnames or domains to bypass the proxy." } }
                        div { class: "pt-2 border-t border-hairline-soft flex flex-col sm:flex-row items-stretch sm:items-center gap-2",
                            button { r#type: "button", disabled: loading() || proxy_loading() || proxy_testing(), class: "inline-flex items-center justify-center gap-2 rounded-mini-md leading-none transition-colors duration-150 ease-out cursor-pointer tracking-tight active:scale-[0.99] disabled:cursor-not-allowed disabled:active:scale-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-coral/30 focus-visible:ring-offset-2 focus-visible:ring-offset-canvas bg-canvas text-ink border border-hairline hover:border-ink/40 hover:bg-surface-soft disabled:border-hairline disabled:text-muted h-10 px-4 text-[14px] font-medium w-full sm:w-auto", onclick: move |_| {
                                let url = proxy_url().trim().to_owned(); if url.is_empty() { proxy_status.set(Some((true, "Please enter a Proxy URL to test".to_owned()))); return; } proxy_testing.set(true); proxy_status.set(None);
                                spawn(async move { match dashboard_request("POST", "/api/settings/proxy-test", Some(&json!({"proxyUrl": url}))).await {
                                    Ok(response) => { let ok = response.ok(); let data = json_body(&response).await.unwrap_or_default(); if ok && data["ok"] == true { proxy_status.set(Some((false, format!("Proxy test OK ({}) in {}ms", data["status"].as_str().unwrap_or("connected"), data["elapsedMs"].as_u64().map_or("?".to_owned(), |n| n.to_string()))))); } else { proxy_status.set(Some((true, data["error"].as_str().unwrap_or("Proxy test failed").to_owned()))); } }
                                    Err(_) => proxy_status.set(Some((true, "Proxy test request failed".to_owned()))),
                                } proxy_testing.set(false); });
                            }, if proxy_testing() { span { class: "material-symbols-outlined animate-spin text-[18px]", "progress_activity" } } "Test proxy URL" }
                            Button { label: "Apply".to_owned(), loading: proxy_loading(), extra_class: "w-full sm:w-auto", onclick: move |_| {} }
                        }
                    } }
                    if let Some((error, message)) = proxy_status() { p { class: if error { "text-xs sm:text-sm text-red-500" } else { "text-xs sm:text-sm text-green-600 dark:text-green-400" }, "{message}" } }
                }
            }
            div { class: CARD_CLASS,
                div { class: "flex items-center gap-3 mb-4", div { class: "size-10 rounded-lg bg-orange-500/10 text-orange-500 flex items-center justify-center shrink-0", span { class: "material-symbols-outlined text-[20px]", "monitoring" } } h3 { class: "text-base sm:text-lg font-semibold", "Observability" } }
                div { class: "flex items-start sm:items-center justify-between gap-4", div { class: "flex-1 min-w-0", p { class: "font-medium text-sm", "Enable Request Logging" } p { class: "text-xs text-muted-soft", "Record request details for inspection in the console log view" } }
                    ProfileToggle { checked: observability_enabled, disabled: loading(), label: "Enable Request Logging", onchange: move |enabled: bool| { if observability_saving() { return; } observability_saving.set(true); spawn(async move { if let Ok(response) = dashboard_request("PATCH", "/api/settings", Some(&json!({"observabilityEnabled": enabled}))).await && response.ok() && let Some(value) = json_body(&response).await && let (Some(saved), Some(update)) = (settings.write().as_object_mut(), value.as_object()) { saved.extend(update.clone()); } observability_saving.set(false); }); } }
                }
            }
            database::Database { settings, on_import: move |_| { spawn(load_settings(settings, proxy_url, no_proxy, loading)); } }
            div { class: "flex flex-col sm:flex-row gap-2",
                Button { variant: "outline", full_width: true, icon: "power_settings_new".to_owned(), label: "Shutdown".to_owned(), extra_class: "text-[color:var(--color-danger)] border-[color:var(--color-danger)]/40 hover:bg-[color:var(--color-danger)]/10 hover:border-[color:var(--color-danger)]/60", onclick: move |_| shutdown_open.set(true) }
                Button { variant: "outline", full_width: true, icon: "logout".to_owned(), label: "Logout".to_owned(), onclick: move |_| { spawn(async move { if dashboard_request("POST", "/api/auth/logout", Some(&json!({}))).await.is_ok_and(|response| response.ok()) && let Some(window) = web_sys::window() { let _ = window.location().set_href("/login"); } }); } }
            }
            div { class: "text-center text-xs sm:text-sm text-muted-soft py-4", p { "OpenProxy" } p { class: "mt-1", "Local Mode — All data stored on your machine" } }
        } }
        if shutdown_open() { Modal { title: "Shutdown Server".to_owned(), small: true, on_close: move |_| shutdown_open.set(false), footer: rsx! {
            Button { label: "Cancel".to_owned(), variant: "ghost", onclick: move |_| shutdown_open.set(false) }
            Button { label: "Shutdown".to_owned(), variant: "danger", loading: shutting_down(), onclick: move |_| { if shutting_down() { return; } shutting_down.set(true); spawn(async move { let _ = dashboard_request("POST", "/api/shutdown", None).await; shutting_down.set(false); shutdown_open.set(false); }); } }
        }, p { class: "text-text-muted", "Are you sure you want to stop the proxy server?" } } }
    }
}

#[component]
fn ProfileToggle(
    checked: bool,
    disabled: bool,
    label: &'static str,
    onchange: EventHandler<bool>,
) -> Element {
    rsx! { div { class: if disabled { "flex items-center gap-3 opacity-50 cursor-not-allowed" } else { "flex items-center gap-3" }, button { r#type: "button", role: "switch", aria_label: label, aria_checked: "{checked}", disabled, class: if checked { "relative inline-flex shrink-0 cursor-pointer rounded-full transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-brand-500/30 bg-brand-500 w-11 h-6" } else { "relative inline-flex shrink-0 cursor-pointer rounded-full transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-brand-500/30 bg-surface-3 w-11 h-6" }, onclick: move |_| onchange.call(!checked), span { class: if checked { "pointer-events-none inline-block rounded-full bg-white shadow-sm transform transition duration-200 ease-in-out translate-x-5 size-5 mt-0.5" } else { "pointer-events-none inline-block rounded-full bg-white shadow-sm transform transition duration-200 ease-in-out translate-x-0.5 size-5 mt-0.5" } } } } }
}
