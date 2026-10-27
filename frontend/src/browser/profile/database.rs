use super::{dashboard_request, json_body, Value, INPUT_CLASS};
use crate::browser::{
    shell::SessionRecheck,
    ui::{Button, Modal, CARD_CLASS},
};
use dioxus::{html::FileData, prelude::*};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    Blob, BlobPropertyBag, HtmlElement, HtmlInputElement, Request, RequestCredentials, RequestInit,
    Url,
};

fn download(value: &Value) -> Result<(), JsValue> {
    let content = serde_json::to_string_pretty(value)
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    let options = BlobPropertyBag::new();
    options.set_type("application/json");
    let parts = js_sys::Array::of1(&JsValue::from_str(&content));
    let blob = Blob::new_with_str_sequence_and_options(&parts, &options)?;
    let url = Url::create_object_url_with_blob(&blob)?;
    let result = (|| {
        let document = web_sys::window()
            .and_then(|window| window.document())
            .ok_or_else(|| JsValue::from_str("Document unavailable"))?;
        let anchor = document.create_element("a")?;
        let stamp = js_sys::Date::new_0()
            .to_iso_string()
            .as_string()
            .unwrap_or_default()
            .replace(['.', ':'], "-");
        anchor.set_attribute("href", &url)?;
        anchor.set_attribute("download", &format!("openproxy-backup-{stamp}.json"))?;
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
pub(super) fn Database(settings: Signal<Value>, on_import: Callback<()>) -> Element {
    let mut loading = use_signal(|| false);
    let mut status = use_signal(|| None::<(bool, String)>);
    let mut auth_mode = use_signal(|| None::<&'static str>);
    let mut password = use_signal(String::new);
    let mut pending_file = use_signal(|| None::<FileData>);
    let recheck = use_context::<SessionRecheck>();

    let run = use_callback(move |(mode, password): (&'static str, String)| {
        if loading() {
            return;
        }
        let recheck = recheck.clone();
        let file = pending_file();
        loading.set(true);
        status.set(None);
        spawn(async move {
            let result: Result<String, String> = async {
                if mode == "export" {
                    let init = RequestInit::new();
                    init.set_method("GET");
                    init.set_credentials(RequestCredentials::SameOrigin);
                    let request = Request::new_with_str_and_init("/api/settings/database", &init)
                        .map_err(|_| "Failed to export database")?;
                    if !password.is_empty() {
                        request
                            .headers()
                            .set("x-op-password", &password)
                            .map_err(|_| "Failed to export database")?;
                    }
                    let window = web_sys::window().ok_or("Failed to export database")?;
                    let response: web_sys::Response =
                        JsFuture::from(window.fetch_with_request(&request))
                            .await
                            .map_err(|_| "Failed to fetch")?
                            .dyn_into()
                            .map_err(|_| "Failed to export database")?;
                    if response.status() == 401 {
                        recheck.call();
                    }
                    let ok = response.ok();
                    let data = json_body(&response).await;
                    if !ok {
                        return Err(data
                            .as_ref()
                            .and_then(|v| v["error"].as_str())
                            .unwrap_or("Failed to export database")
                            .to_owned());
                    }
                    download(&data.ok_or("Failed to export database")?)
                        .map_err(|_| "Failed to export database")?;
                    Ok("Database backup downloaded".to_owned())
                } else {
                    let file = file.ok_or("Invalid backup file")?;
                    let raw = file
                        .read_string()
                        .await
                        .map_err(|_| "Invalid backup file")?;
                    let mut body: Value =
                        serde_json::from_str(&raw).map_err(|_| "Invalid backup file")?;
                    if !password.is_empty() {
                        let object = body.as_object_mut().ok_or("Invalid backup file")?;
                        object.insert("password".to_owned(), Value::String(password));
                    }
                    let response = dashboard_request("POST", "/api/settings/database", Some(&body))
                        .await
                        .map_err(|_| "Failed to fetch")?;
                    let ok = response.ok();
                    let data = json_body(&response).await.unwrap_or_default();
                    if !ok {
                        return Err(data["error"]
                            .as_str()
                            .unwrap_or("Failed to import database")
                            .to_owned());
                    }
                    pending_file.set(None);
                    on_import.call(());
                    Ok(format!("Database imported from {}", file.name()))
                }
            }
            .await;
            match result {
                Ok(message) => status.set(Some((false, message))),
                Err(message) => status.set(Some((true, message))),
            }
            loading.set(false);
        });
    });
    let request_export = move |_| {
        if settings()["requireLogin"] == true && settings()["hasPassword"] == true {
            password.set(String::new());
            auth_mode.set(Some("export"));
        } else {
            run.call(("export", String::new()));
        }
    };
    let confirm = use_callback(move |()| {
        if let Some(mode) = auth_mode()
            && !password().is_empty()
        {
            let value = password();
            password.set(String::new());
            auth_mode.set(None);
            run.call((mode, value));
        }
    });
    rsx! {
        div { class: CARD_CLASS,
            div { class: "flex items-center gap-3 mb-4", div { class: "size-10 rounded-lg bg-teal-500/10 text-teal-500 flex items-center justify-center shrink-0", span { class: "material-symbols-outlined text-[20px]", "database" } } h3 { class: "text-base sm:text-lg font-semibold", "Database" } }
            div { class: "flex flex-col gap-4", p { class: "text-xs sm:text-sm text-muted-soft", "Export your entire database as JSON, or restore from a previous backup file." }
                div { class: "flex flex-col sm:flex-row gap-2",
                    Button { variant: "secondary", icon: "download".to_owned(), label: "Download Backup".to_owned(), loading: loading(), extra_class: "w-full sm:w-auto", onclick: request_export }
                    Button { variant: "outline", icon: "upload".to_owned(), label: "Import Backup".to_owned(), disabled: loading(), extra_class: "w-full sm:w-auto", onclick: move |_| { if let Some(element) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("profile-import-file")).and_then(|e| e.dyn_into::<HtmlElement>().ok()) { element.click(); } } }
                    input { id: "profile-import-file", r#type: "file", accept: "application/json,.json", class: "hidden", onchange: move |event| {
                        let file = event.files().first().cloned();
                        if let Some(element) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("profile-import-file")).and_then(|e| e.dyn_into::<HtmlInputElement>().ok()) { element.set_value(""); }
                        if let Some(file) = file { pending_file.set(Some(file)); status.set(None); if settings()["requireLogin"] == true && settings()["hasPassword"] == true { password.set(String::new()); auth_mode.set(Some("import")); } else { run.call(("import", String::new())); } }
                    } }
                }
                if let Some((error, message)) = status() { p { class: if error { "text-xs sm:text-sm text-red-500" } else { "text-xs sm:text-sm text-green-600 dark:text-green-400" }, "{message}" } }
            }
        }
        if let Some(mode) = auth_mode() { Modal { title: "Confirm Password".to_owned(), small: true, on_close: move |_| { auth_mode.set(None); password.set(String::new()); }, footer: rsx! {
            Button { label: "Cancel".to_owned(), variant: "ghost", disabled: loading(), onclick: move |_| { auth_mode.set(None); password.set(String::new()); } }
            Button { label: "Confirm".to_owned(), disabled: password().is_empty(), loading: loading(), onclick: move |_| confirm.call(()) }
        }, p { class: "text-text-muted mb-3 text-sm", "Enter your current password to {mode} the database." } div { class: "flex flex-col gap-1.5", div { class: "relative", input { r#type: "password", placeholder: "Current password", autofocus: true, value: "{password}", class: INPUT_CLASS, onmounted: move |event| async move { let _ = event.set_focus(true).await; }, oninput: move |e| password.set(e.value()), onkeydown: move |e| { if e.key() == Key::Enter { confirm.call(()); } } } } } } }
    }
}
