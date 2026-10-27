use super::{dashboard_request, js_trim, json, json_body, Value};
use crate::browser::ui::{Button, Modal};
use dioxus::prelude::*;

// This is the reference's example field name, not a credential.
const PLACEHOLDER: &str = concat!(
    r#"{
  "auth_mode": "chatgpt",
  "OPENAI_API_KEY": null,
  "tokens": {
    "id_token": "eyJhbGc...",
    "access_token": "eyJhbGc...",
    "refresh"#,
    "_",
    r#"token": "rt_...",
    "account_id": "account-id"
  },
  "last_refresh": "2026-01-01T00:00:00Z"
}"#
);

fn parse_accounts(input: &str) -> Result<Vec<Value>, String> {
    let text = js_trim(input);
    let initial = js_sys::JSON::parse(&text);
    let parsed = match initial {
        Ok(value) => value,
        Err(error) => {
            // As in Astro, terminal copy may include a shell prompt after the
            // first complete JSON object/array, including nested delimiters.
            let parsed = text.char_indices().find_map(|(index, character)| {
                if matches!(character, '}' | ']') {
                    js_sys::JSON::parse(&text[..index + 1]).ok()
                } else {
                    None
                }
            });
            parsed.ok_or_else(|| {
                let message = js_sys::Reflect::get(&error, &"message".into())
                    .ok()
                    .and_then(|value| value.as_string())
                    .unwrap_or_else(|| "Unable to parse input".to_owned());
                format!("Invalid JSON: {message}")
            })?
        }
    };
    // JSON.stringify reproduces the values JSON.parse would send in the
    // browser, while the existing request helper owns JSON serialization.
    let text = js_sys::JSON::stringify(&parsed)
        .ok()
        .and_then(|value| value.as_string())
        .ok_or_else(|| "Invalid JSON".to_owned())?;
    let parsed: Value = serde_json::from_str(&text).map_err(|_| "Invalid JSON".to_owned())?;
    let accounts = match parsed {
        Value::Array(accounts) => accounts,
        Value::Object(mut object) => match object.get("accounts") {
            Some(Value::Array(_)) => object
                .remove("accounts")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default(),
            _ => vec![Value::Object(object)],
        },
        _ => Vec::new(),
    };
    if accounts.is_empty() {
        Err("No accounts found in input".to_owned())
    } else {
        Ok(accounts)
    }
}

#[component]
pub(super) fn BulkImportCodex(on_close: Callback<()>, on_success: Callback<()>) -> Element {
    let mut input = use_signal(String::new);
    let mut submitting = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut result = use_signal(|| None::<Value>);
    let close = use_callback(move |()| {
        if !*submitting.peek() {
            on_close.call(());
        }
    });
    rsx! {
        Modal { title: "Bulk Add Codex Accounts".to_owned(), on_close: move |_| close.call(()),
            div { class: "flex flex-col gap-4",
                p { class: "text-xs text-text-muted", "Paste a Codex auth.json object or an array of accounts. Terminal prompts after the JSON are ignored." }
                textarea {
                    class: "w-full rounded border border-accent/30 bg-sidebar p-2 text-sm font-mono resize-y min-h-[240px] focus:outline-none focus:ring-1 focus:ring-primary",
                    placeholder: PLACEHOLDER, aria_label: "Codex accounts JSON", disabled: submitting(),
                    oninput: move |event| input.set(event.value()),
                }
                if !error().is_empty() { p { role: "alert", class: "text-xs text-red-500 break-words", "{error}" } }
                if let Some(data) = result() {
                    div { class: "flex flex-col gap-2",
                        div { class: if data["failed"].as_u64().unwrap_or(0) > 0 { "text-sm font-medium text-yellow-400" } else { "text-sm font-medium text-green-400" },
                            if data["success"].as_u64().unwrap_or(0) > 0 { "✓ {data[\"success\"]} added" }
                            if data["failed"].as_u64().unwrap_or(0) > 0 { ", ✗ {data[\"failed\"]} failed" }
                        }
                        if let Some(items) = data["results"].as_array() {
                            if items.iter().any(|item| item["ok"] == false) {
                                ul { class: "rounded border border-accent/20 bg-sidebar/50 p-2 text-xs font-mono max-h-40 overflow-y-auto",
                                    for item in items.iter().filter(|item| item["ok"] == false) {
                                        li { class: "text-red-400", "[{item[\"index\"]}] {item[\"error\"].as_str().unwrap_or_default()}" }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "flex gap-2",
                    Button { label: if submitting() { "Importing...".to_owned() } else { "Import All".to_owned() }, full_width: true, disabled: submitting() || js_trim(&input()).is_empty(), onclick: move |_| {
                        if *submitting.peek() || js_trim(&input.peek()).is_empty() { return; }
                        error.set(String::new());
                        result.set(None);
                        let accounts = match parse_accounts(&input.peek()) {
                            Ok(accounts) => accounts,
                            Err(message) => { error.set(message); return; }
                        };
                        submitting.set(true);
                        spawn(async move {
                            match dashboard_request("POST", "/api/oauth/codex/bulk-import", Some(&json!({"accounts":accounts}))).await {
                                Ok(response) => {
                                    let status = response.status();
                                    let ok = response.ok();
                                    let data = json_body(&response).await;
                                    if !ok {
                                        error.set(data.as_ref().and_then(|data| data["error"].as_str()).filter(|message| !message.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("Request failed: {status}")));
                                    } else if let Some(data) = data.filter(|data| data["success"].as_u64().is_some() && data["failed"].as_u64().is_some()) {
                                        if data["success"].as_u64().unwrap_or(0) > 0 {
                                            on_success.call(());
                                        } else {
                                            result.set(Some(data));
                                        }
                                    } else {
                                        error.set("Invalid import response; refresh connections before retrying.".to_owned());
                                    }
                                }
                                Err(_) => error.set("Request failed".to_owned()),
                            }
                            submitting.set(false);
                        });
                    } }
                    Button { label: "Close".to_owned(), variant: "ghost", full_width: true, disabled: submitting(), onclick: move |_| close.call(()) }
                }
            }
        }
    }
}
