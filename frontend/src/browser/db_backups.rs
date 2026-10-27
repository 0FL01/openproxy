//! Database snapshot management through the existing dashboard APIs.
use super::{
    json, json_body,
    shell::{dashboard_request, SessionRecheck},
    ui::{Button, Modal, CARD_CLASS},
    Value, INPUT_CLASS,
};
use dioxus::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    File, FormData, HtmlElement, HtmlInputElement, Request, RequestCredentials, RequestInit,
    Response, Url,
};

#[derive(Clone)]
enum Confirmation {
    Restore(Value),
    Delete(Value),
    Prune,
    Import,
}

#[derive(Clone)]
enum Action {
    Create,
    Restore(Value),
    Delete(Value),
    Prune,
}

type Status = Option<(&'static str, String)>;

async fn load_list(
    mut data: Signal<Option<Value>>,
    mut loading: Signal<bool>,
    mut status: Signal<Status>,
) {
    loading.set(true);
    let result = match dashboard_request("GET", "/api/db-backups", None).await {
        Ok(response) if response.ok() => json_body(&response)
            .await
            .ok_or_else(|| "Failed to load backups".to_owned()),
        Ok(response) => Err(format!(
            "Failed to load backups: Server returned {}",
            response.status()
        )),
        Err(_) => Err("Failed to load backups: Failed to fetch".to_owned()),
    };
    match result {
        Ok(value) => data.set(Some(value)),
        Err(error) => status.set(Some(("error", error))),
    }
    loading.set(false);
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn reason_label(reason: &str) -> &str {
    match reason {
        "auto" => "Auto",
        "manual" => "Manual",
        "pre-restore" => "Pre-restore",
        "pre-import" => "Pre-import",
        other => other,
    }
}

fn format_time(value: &str) -> String {
    let locale = web_sys::window()
        .and_then(|w| w.navigator().language())
        .unwrap_or_else(|| "en-US".to_owned());
    js_sys::Date::new(&JsValue::from_str(value))
        .to_locale_string(&locale, &JsValue::UNDEFINED)
        .as_string()
        .unwrap_or_default()
}

fn save_blob(blob: &web_sys::Blob, name: &str) -> Result<(), JsValue> {
    let url = Url::create_object_url_with_blob(blob)?;
    let result = (|| {
        let document = web_sys::window()
            .and_then(|w| w.document())
            .ok_or_else(|| JsValue::from_str("Document unavailable"))?;
        let anchor = document.create_element("a")?;
        anchor.set_attribute("href", &url)?;
        anchor.set_attribute("download", name)?;
        document
            .body()
            .ok_or_else(|| JsValue::from_str("Document unavailable"))?
            .append_child(&anchor)?;
        anchor
            .dyn_ref::<HtmlElement>()
            .ok_or_else(|| JsValue::from_str("Download unavailable"))?
            .click();
        anchor.remove();
        Ok(())
    })();
    let _ = Url::revoke_object_url(&url);
    result
}

#[component]
pub(super) fn DbBackupsPage() -> Element {
    let data = use_signal(|| None::<Value>);
    let loading = use_signal(|| true);
    let mut busy = use_signal(|| false);
    let mut status = use_signal(|| None::<(&'static str, String)>);
    let mut settings = use_signal(|| json!({}));
    let mut pending = use_signal(|| None::<Confirmation>);
    let mut pending_file = use_signal(|| None::<File>);
    let mut auth_mode = use_signal(|| None::<&'static str>);
    let mut password = use_signal(String::new);
    let recheck = use_context::<SessionRecheck>();

    use_future(move || async move {
        spawn(load_list(data, loading, status));
        if let Ok(response) = dashboard_request("GET", "/api/settings", None).await
            && response.ok()
            && let Some(value) = json_body(&response).await
        {
            settings.set(value);
        }
    });

    let action = use_callback(move |action: Action| {
        if busy() {
            return;
        }
        busy.set(true);
        status.set(None);
        spawn(async move {
            let (method, path, body, prefix) = match &action {
                Action::Create => (
                    "PUT",
                    "/api/db-backups".to_owned(),
                    None,
                    "Failed to create backup",
                ),
                Action::Restore(backup) => (
                    "POST",
                    "/api/db-backups/restore".to_owned(),
                    Some(json!({"backupId": backup["id"]})),
                    "Restore failed",
                ),
                Action::Delete(backup) => (
                    "DELETE",
                    format!(
                        "/api/db-backups/{}",
                        js_sys::encode_uri_component(backup["id"].as_str().unwrap_or(""))
                    ),
                    None,
                    "Failed to delete",
                ),
                Action::Prune => (
                    "DELETE",
                    "/api/db-backups".to_owned(),
                    Some(json!({})),
                    "Cleanup failed",
                ),
            };
            let result = match dashboard_request(method, &path, body.as_ref()).await {
                Ok(response) if response.ok() => {
                    let result = json_body(&response).await.unwrap_or_default();
                    let message = match action {
                        Action::Create if result["created"] == false => (
                            "info",
                            result["message"]
                                .as_str()
                                .unwrap_or("Backup skipped.")
                                .to_owned(),
                        ),
                        Action::Create => (
                            "success",
                            format!(
                                "Snapshot created: {}",
                                result["backup"]["id"].as_str().unwrap_or("(no id)")
                            ),
                        ),
                        Action::Delete(_) => ("success", "Backup deleted.".to_owned()),
                        Action::Prune => (
                            "success",
                            format!(
                                "Cleanup ran (deleted {}, kept {}).",
                                result["result"]["deletedFiles"].as_u64().unwrap_or(0),
                                result["result"]["keptFiles"].as_u64().unwrap_or(0)
                            ),
                        ),
                        Action::Restore(backup) => (
                            "success",
                            format!(
                                "Restored {} — {} providers, {} API keys.",
                                backup["id"].as_str().unwrap_or(""),
                                result["providerCount"].as_u64().unwrap_or(0),
                                result["apiKeyCount"].as_u64().unwrap_or(0)
                            ),
                        ),
                    };
                    Ok(message)
                }
                Ok(response) => {
                    let code = response.status();
                    let detail = if matches!(action, Action::Restore(_)) {
                        if let Ok(promise) = response.text() {
                            JsFuture::from(promise)
                                .await
                                .ok()
                                .and_then(|value| value.as_string())
                                .filter(|value| !value.is_empty())
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    Err(format!(
                        "{prefix}: {}",
                        detail.unwrap_or_else(|| format!("Server returned {code}"))
                    ))
                }
                Err(_) => Err(format!("{prefix}: Failed to fetch")),
            };
            match result {
                Ok(message) => {
                    status.set(Some(message));
                    load_list(data, loading, status).await;
                }
                Err(message) => status.set(Some(("error", message))),
            }
            busy.set(false);
        });
    });

    let transfer = use_callback(move |(mode, pass): (&'static str, String)| {
        if busy() {
            return;
        }
        let file = pending_file();
        let recheck = recheck.clone();
        busy.set(true);
        status.set(None);
        spawn(async move {
            let result: Result<String, String> = async {
                let init = RequestInit::new();
                init.set_method(if mode == "export" { "GET" } else { "POST" });
                init.set_credentials(RequestCredentials::SameOrigin);
                let filename = if mode == "import" {
                    let file = file.ok_or("No file selected")?;
                    let form = FormData::new().map_err(|_| "Failed to prepare import")?;
                    form.append_with_blob_and_filename("file", &file, &file.name())
                        .map_err(|_| "Failed to prepare import")?;
                    init.set_body_opt_form_data(Some(&form));
                    Some(file.name())
                } else {
                    None
                };
                let path = if mode == "export" {
                    "/api/db-backups/export"
                } else {
                    "/api/db-backups/import"
                };
                let request = Request::new_with_str_and_init(path, &init)
                    .map_err(|_| "Failed to prepare request")?;
                if !pass.is_empty() {
                    request
                        .headers()
                        .set("x-op-password", &pass)
                        .map_err(|_| "Failed to prepare request")?;
                }
                let window = web_sys::window().ok_or("Window unavailable")?;
                let response: Response = JsFuture::from(window.fetch_with_request(&request))
                    .await
                    .map_err(|_| "Failed to fetch")?
                    .dyn_into()
                    .map_err(|_| "Invalid response")?;
                if response.status() == 401 {
                    recheck.call();
                }
                if !response.ok() {
                    let code = response.status();
                    let data = json_body(&response).await.unwrap_or_default();
                    return Err(data["error"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Server returned {code}")));
                }
                if mode == "export" {
                    let disposition = response
                        .headers()
                        .get("content-disposition")
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    let name = disposition
                        .split_once("filename=")
                        .map(|(_, name)| name.trim_matches('"').to_owned())
                        .unwrap_or_else(|| {
                            format!("openproxy-db-{}.json", js_sys::Date::now() as u64)
                        });
                    let blob: web_sys::Blob =
                        JsFuture::from(response.blob().map_err(|_| "Failed to export database")?)
                            .await
                            .map_err(|_| "Failed to export database")?
                            .dyn_into()
                            .map_err(|_| "Failed to export database")?;
                    save_blob(&blob, &name).map_err(|_| "Failed to export database")?;
                    Ok("Database exported".to_owned())
                } else {
                    let result = json_body(&response).await.unwrap_or_default();
                    Ok(format!(
                        "Imported {} — {} providers, {} API keys.",
                        filename.unwrap_or_default(),
                        result["providerCount"].as_u64().unwrap_or(0),
                        result["apiKeyCount"].as_u64().unwrap_or(0)
                    ))
                }
            }
            .await;
            match result {
                Ok(message) => {
                    status.set(Some(("success", message)));
                    if mode == "import" {
                        pending_file.set(None);
                        load_list(data, loading, status).await;
                    }
                }
                Err(message) => status.set(Some((
                    "error",
                    format!(
                        "{} failed: {message}",
                        if mode == "export" { "Export" } else { "Import" }
                    ),
                ))),
            }
            busy.set(false);
        });
    });

    let confirm_auth = use_callback(move |()| {
        if let Some(mode) = auth_mode()
            && !password().is_empty()
        {
            let pass = password();
            password.set(String::new());
            auth_mode.set(None);
            pending.set(None);
            transfer.call((mode, pass));
        }
    });
    let needs_auth = settings()["requireLogin"] == true && settings()["hasPassword"] == true;
    let list = data()
        .as_ref()
        .and_then(|d| d["backups"].as_array().cloned())
        .unwrap_or_default();
    let count = list.len();
    rsx! {
        div { class: "space-y-6",
            div { h1 { class: "text-2xl font-semibold text-ink", "Database Backups" } p { class: "text-sm text-body mt-1", "Hourly snapshots of " code { "db.json" } " with retention. Restore, export, and import full databases here." } }
            div { class: CARD_CLASS,
                div { class: "p-4 flex flex-wrap items-center gap-2 justify-between",
                    div { class: "text-sm text-body", if let Some(data) = data() {
                        span { class: "font-medium text-ink", "{count}" } " snapshot" if count != 1 { "s" } " · retention: max " span { class: "font-medium text-ink", "{data[\"maxFiles\"]}" } " files" if data["retentionDays"].as_u64().unwrap_or(0) > 0 { ", {data[\"retentionDays\"]} days" } else { ", no day cutoff" } if data["autoDisabled"] == true { " · auto-backup DISABLED" }
                    } else { "Loading…" } }
                    div { class: "flex flex-wrap gap-2",
                        Button { label: "Refresh".to_owned(), variant: "secondary", disabled: loading() || busy(), onclick: move |_| { spawn(load_list(data, loading, status)); } }
                        Button { label: if busy() { "Working…".to_owned() } else { "Create snapshot".to_owned() }, disabled: busy(), onclick: move |_| action.call(Action::Create) }
                        Button { label: "Prune".to_owned(), variant: "secondary", disabled: busy(), onclick: move |_| pending.set(Some(Confirmation::Prune)) }
                        Button { label: "Export db.json".to_owned(), variant: "secondary", disabled: busy(), onclick: move |_| { if needs_auth { password.set(String::new()); auth_mode.set(Some("export")); } else { transfer.call(("export", String::new())); } } }
                        Button { label: "Import db.json".to_owned(), variant: "secondary", disabled: busy(), onclick: move |_| { if let Some(element) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("backup-import-file")).and_then(|e| e.dyn_into::<HtmlElement>().ok()) { element.click(); } } }
                        input { id: "backup-import-file", r#type: "file", accept: ".json,application/json", class: "hidden", onchange: move |_| {
                            if let Some(input) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("backup-import-file")).and_then(|e| e.dyn_into::<HtmlInputElement>().ok()) {
                                let file = input.files().and_then(|files| files.get(0)); input.set_value("");
                                if let Some(file) = file { pending_file.set(Some(file)); pending.set(Some(Confirmation::Import)); }
                            }
                        } }
                    }
                }
                if let Some((kind, text)) = status() { div { class: format!("mt-3 rounded-md border px-3 py-2 text-sm {}", match kind { "success" => "border-green-300 bg-green-50 text-green-800 dark:border-green-700 dark:bg-green-900/30 dark:text-green-200", "error" => "border-red-300 bg-red-50 text-red-800 dark:border-red-700 dark:bg-red-900/30 dark:text-red-200", _ => "border-blue-300 bg-blue-50 text-blue-800 dark:border-blue-700 dark:bg-blue-900/30 dark:text-blue-200" }), "{text}" } }
            }
            div { class: CARD_CLASS, div { class: "overflow-x-auto", table { class: "min-w-full text-sm",
                thead { class: "text-left text-body uppercase text-xs tracking-wide", tr { for header in ["Snapshot", "Reason", "Created", "Size", "Providers", "API keys", "Actions"] { th { class: if header == "Actions" { "px-4 py-2 text-right" } else { "px-4 py-2" }, "{header}" } } } }
                tbody { class: "divide-y divide-line",
                    if loading() { tr { td { colspan: "7", class: "px-4 py-6 text-center text-body", "Loading backups…" } } }
                    else if list.is_empty() { tr { td { colspan: "7", class: "px-4 py-6 text-center text-body", "No backups yet. Click " span { class: "font-medium", "Create snapshot" } " to save one now, or wait for the hourly auto-backup." } } }
                    else { for backup in list { tr { key: "{backup[\"id\"]}", class: "hover:bg-surface-soft",
                        td { class: "px-4 py-2 font-mono text-xs text-ink", "{backup[\"id\"].as_str().unwrap_or(\"\")}" }
                        td { class: "px-4 py-2 text-body", "{reason_label(backup[\"reason\"].as_str().unwrap_or(\"\"))}" }
                        td { class: "px-4 py-2 text-body", "{format_time(backup[\"createdAt\"].as_str().unwrap_or(\"\"))}" }
                        td { class: "px-4 py-2 text-body", "{format_size(backup[\"size\"].as_u64().unwrap_or(0))}" }
                        td { class: "px-4 py-2 text-body", "{backup[\"providerCount\"]}" }
                        td { class: "px-4 py-2 text-body", "{backup[\"apiKeyCount\"]}" }
                        td { class: "px-4 py-2 text-right", div { class: "inline-flex gap-2",
                            Button { label: "Restore".to_owned(), variant: "secondary", disabled: busy(), onclick: { let backup = backup.clone(); move |_| pending.set(Some(Confirmation::Restore(backup.clone()))) } }
                            Button { label: "Delete".to_owned(), variant: "secondary", disabled: busy(), onclick: move |_| pending.set(Some(Confirmation::Delete(backup.clone()))) }
                        } }
                    } } }
                }
            } } }
        if let Some(confirm) = pending() && auth_mode().is_none() {
            { let title = match &confirm { Confirmation::Restore(_) => "Restore snapshot?", Confirmation::Delete(_) => "Delete backup?", Confirmation::Prune => "Prune backups?", Confirmation::Import => "Import database?" }; let label = match &confirm { Confirmation::Restore(_) => "Restore", Confirmation::Delete(_) => "Delete", Confirmation::Prune => "Prune", Confirmation::Import => "Import" }; rsx! {
                Modal { title: title.to_owned(), small: true, on_close: move |_| { pending.set(None); pending_file.set(None); }, footer: rsx! {
                    Button { label: "Cancel".to_owned(), variant: "ghost", disabled: busy(), onclick: move |_| { pending.set(None); pending_file.set(None); } }
                    Button { label: label.to_owned(), variant: "danger", loading: busy(), onclick: { let confirm = confirm.clone(); move |_| { match &confirm {
                        Confirmation::Restore(backup) => { pending.set(None); action.call(Action::Restore(backup.clone())); }
                        Confirmation::Delete(backup) => { pending.set(None); action.call(Action::Delete(backup.clone())); }
                        Confirmation::Prune => { pending.set(None); action.call(Action::Prune); }
                        Confirmation::Import if needs_auth => { password.set(String::new()); auth_mode.set(Some("import")); }
                        Confirmation::Import => { pending.set(None); transfer.call(("import", String::new())); }
                    } } } }
                }, p { class: "text-text-muted", match confirm {
                    Confirmation::Restore(backup) => rsx! { p { "This will replace the current database with the contents of " span { class: "font-mono", "{backup[\"id\"].as_str().unwrap_or(\"\")}" } ". A pre-restore safety snapshot will be created first." } p { class: "mt-2", "{backup[\"providerCount\"]} providers · {backup[\"apiKeyCount\"]} API keys." } },
                    Confirmation::Delete(backup) => rsx! { "Delete snapshot " span { class: "font-mono", "{backup[\"id\"].as_str().unwrap_or(\"\")}" } "? This cannot be undone." },
                    Confirmation::Prune => rsx! { "Prune backups using the current retention settings? Snapshots beyond the retention window will be removed." },
                    Confirmation::Import => rsx! { "Import " span { class: "font-mono", "{pending_file().map(|f| f.name()).unwrap_or_default()}" } "? This replaces the current database. A pre-import snapshot will be created automatically." },
                } }
            } } }
        }
        if let Some(mode) = auth_mode() { Modal { title: "Confirm Password".to_owned(), small: true, on_close: move |_| { auth_mode.set(None); password.set(String::new()); pending.set(None); pending_file.set(None); }, footer: rsx! {
            Button { label: "Cancel".to_owned(), variant: "ghost", disabled: busy(), onclick: move |_| { auth_mode.set(None); password.set(String::new()); pending.set(None); pending_file.set(None); } }
            Button { label: "Confirm".to_owned(), disabled: password().is_empty(), loading: busy(), onclick: move |_| confirm_auth.call(()) }
        }, p { class: "text-text-muted mb-3 text-sm", "Enter your current password to {mode} the database." } div { class: "flex flex-col gap-1.5", div { class: "relative", input { r#type: "password", placeholder: "Current password", autofocus: true, value: "{password}", class: INPUT_CLASS, onmounted: move |event| async move { let _ = event.set_focus(true).await; }, oninput: move |e| password.set(e.value()), onkeydown: move |e| { if e.key() == Key::Enter { confirm_auth.call(()); } } } } } } }
        }
    }
}
