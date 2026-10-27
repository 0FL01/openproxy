//! Standalone OAuth callback relay. All values are browser-local and never logged.
use super::{JsCast, JsValue, CALLBACK_INITIAL_URL};
use dioxus::prelude::*;
use gloo_timers::{callback::Timeout, future::TimeoutFuture};
use serde_json::json;
use std::rc::Rc;
use wasm_bindgen_futures::JsFuture;
use web_sys::{BroadcastChannel, Document, HtmlTextAreaElement, Window};

const STYLE: &str = r#"
body.callback-page, body.callback-page *, body.callback-page *::before, body.callback-page *::after { box-sizing: border-box; margin: 0; padding: 0; }
body.callback-page { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, sans-serif; font-feature-settings: normal; -webkit-font-smoothing: auto; -moz-osx-font-smoothing: auto; background: #0d0d0d; color: #e5e5e5; display: flex; align-items: center; justify-content: center; min-height: 100vh; line-height: 1.6; }
body.callback-page #main { display: contents; }
body.callback-page .card { background: #1a1a1a; border: 1px solid #2a2a2a; border-radius: 12px; padding: 2rem; max-width: 480px; width: 90%; text-align: center; }
body.callback-page h1 { font-size: 1.25rem; font-weight: bold; letter-spacing: normal; margin-bottom: 0.75rem; }
body.callback-page p { font-size: 0.875rem; color: #9ca3af; }
body.callback-page .error { color: #ef4444; }
body.callback-page .success { color: #22c55e; }
body.callback-page .fallback-url { margin-top: 1rem; font-size: 0.75rem; color: #6b7280; word-break: break-all; }
body.callback-page .copy-url { font: revert; margin-top: 0.75rem; padding: 0.5rem 1rem; background: #6366f1; color: white; border: none; border-radius: 6px; cursor: pointer; font-size: 0.8rem; }
"#;

struct PageGuard {
    document: Document,
    title: String,
}

impl Drop for PageGuard {
    fn drop(&mut self) {
        if let Some(body) = self.document.body() {
            let _ = body.class_list().remove_1("callback-page");
        }
        self.document.set_title(&self.title);
    }
}

fn opener(window: &Window) -> Option<Window> {
    let value = window.opener().ok()?;
    if value.is_null() || value.is_undefined() {
        return None;
    }
    let window: Window = value.unchecked_into();
    if window.closed().ok()? {
        return None;
    }
    Some(window)
}

#[derive(Clone)]
struct Relay {
    url: String,
    error: Option<String>,
    code: Option<String>,
    fallback: bool,
}

fn relay() -> Relay {
    let Some(window) = web_sys::window() else {
        return Relay {
            url: String::new(),
            error: None,
            code: None,
            fallback: true,
        };
    };
    let url = CALLBACK_INITIAL_URL
        .with(|initial| initial.borrow_mut().take())
        .or_else(|| window.location().href().ok())
        .unwrap_or_default();
    let parsed = web_sys::Url::new(&url).ok();
    // The router canonicalizes away query/fragment on mount. Preserve the
    // original callback URL for the address bar and manual-copy fallback.
    if let Some(parsed) = &parsed
        && parsed.pathname() == "/callback"
        && window.location().origin().ok().as_deref() == Some(&parsed.origin())
        && window.location().pathname().ok().as_deref() == Some("/callback")
        && let Ok(history) = window.history()
    {
        let path = format!("{}{}{}", parsed.pathname(), parsed.search(), parsed.hash());
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&path));
    }
    let params = parsed.map(|url| url.search_params());
    let param = |name| params.as_ref().and_then(|params| params.get(name));
    let code = param("code").filter(|s| !s.is_empty());
    let error = param("error").filter(|s| !s.is_empty());
    let description = param("error_description");
    let data = json!({"code":param("code"),"state":param("state"),"provider":param("provider"),"error":param("error"),"errorDescription":description,"attemptNonce":param("openproxy_oauth_nonce"),"timestamp":js_sys::Date::now()});
    let serialized = data.to_string();
    let js_data = js_sys::JSON::parse(&serialized).ok();
    let mut sent = false;
    if let (Ok(channel), Some(data)) = (BroadcastChannel::new("oauth_callback"), js_data.as_ref()) {
        sent |= channel.post_message(data).is_ok();
        channel.close();
    }
    if let Some(storage) = window.local_storage().ok().flatten()
        && storage.set_item("oauth_callback", &serialized).is_ok()
    {
        sent = true;
        Timeout::new(1000, move || {
            // Do not remove a newer callback that arrived during this delay.
            if storage.get_item("oauth_callback").ok().flatten().as_deref() == Some(&serialized) {
                let _ = storage.remove_item("oauth_callback");
            }
        })
        .forget();
    }
    if let Some(opener) = opener(&window)
        && let Ok(origin) = window.location().origin()
        && opener.location().origin().ok().as_deref() == Some(&origin)
        && let Ok(message) =
            js_sys::JSON::parse(&json!({"type":"oauth_callback","data":data}).to_string())
    {
        // A target-origin mismatch silently drops postMessage; don't count it
        // as delivery or send credentials to an unknown opener.
        sent |= opener.post_message(&message, &origin).is_ok();
    }
    if error.is_none() && code.is_some() {
        Timeout::new(2000, move || {
            if opener(&window).is_some() {
                let _ = window.close();
            }
        })
        .forget();
    }
    Relay {
        url,
        error: error.map(|e| {
            data["errorDescription"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(&e)
                .to_owned()
        }),
        code,
        fallback: !sent,
    }
}

fn fallback_copy(document: &Document, value: &str) {
    let Some(body) = document.body() else { return };
    let Ok(element) = document.create_element("textarea") else {
        return;
    };
    let Ok(input) = element.dyn_into::<HtmlTextAreaElement>() else {
        return;
    };
    input.set_value(value);
    let _ = input.style().set_property("position", "fixed");
    let _ = input.style().set_property("opacity", "0");
    if body.append_child(&input).is_err() {
        return;
    }
    input.select();
    if let Ok(command) = js_sys::Reflect::get(document.as_ref(), &JsValue::from_str("execCommand"))
        .and_then(|v| v.dyn_into::<js_sys::Function>())
    {
        let _ = command.call1(document.as_ref(), &JsValue::from_str("copy"));
    }
    let _ = body.remove_child(&input);
}

#[component]
pub(super) fn CallbackPage() -> Element {
    let _guard = use_hook(|| {
        let document = web_sys::window()?.document()?;
        let title = document.title();
        document.set_title("OpenProxy — OAuth Callback");
        if let Some(body) = document.body() {
            let _ = body.class_list().add_1("callback-page");
        }
        Some(Rc::new(PageGuard { document, title }))
    });
    let relay = use_hook(relay);
    let mut copied = use_signal(|| false);
    let mut generation = use_signal(|| 0_u32);
    let title = if relay.error.is_some() {
        "Authentication Failed"
    } else if relay.code.is_some() {
        "Authentication Complete"
    } else {
        "No Auth Data Found"
    };
    let title_class = if relay.error.is_some() {
        "error"
    } else if relay.code.is_some() {
        "success"
    } else {
        ""
    };
    let url = relay.url.clone();
    rsx! {
        style { "{STYLE}" }
        div { class: "card", id: "card", "data-i18n-skip": "true",
            h1 { id: "title", class: "{title_class}", "{title}" }
            if let Some(error) = &relay.error { p { id: "message", "{error}" } }
            else if relay.code.is_some() { p { id: "message", "You can close this window and return to the dashboard." } }
            else { p { id: "message", "The URL did not contain OAuth parameters. Please copy the full URL from your browser's address bar and paste it into the " strong { "Callback URL" } " field in the OpenProxy dashboard." } }
            if relay.fallback {
                p { class: "fallback-url", "Full URL: {relay.url}" }
                button { class: "copy-url", onclick: move |_| {
                    let value = url.clone(); copied.set(true); generation += 1; let current = generation();
                    spawn(async move {
                        if let Some(window) = web_sys::window() {
                            let clipboard = js_sys::Reflect::get(window.navigator().as_ref(), &JsValue::from_str("clipboard")).ok().and_then(|v| v.dyn_into::<web_sys::Clipboard>().ok());
                            let copied = if let Some(clipboard) = clipboard { JsFuture::from(clipboard.write_text(&value)).await.is_ok() } else { false };
                            if !copied && let Some(document) = window.document() { fallback_copy(&document, &value); }
                        }
                        TimeoutFuture::new(2000).await;
                        if generation() == current { copied.set(false); }
                    });
                }, if copied() { "Copied!" } else { "Copy URL" } }
            }
        }
    }
}
