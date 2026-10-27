//! Browser callback transports; listeners are scoped to one open OAuth modal.
use dioxus::prelude::*;
use gloo_timers::callback::Timeout;
use serde_json::{json, Value};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use web_sys::{BroadcastChannel, MessageEvent, StorageEvent, Window};

pub(super) struct Delivery {
    pub data: Value,
    pub origin: Option<String>,
    pub storage_value: Option<String>,
}

/// Read only an exact same-origin callback. Cross-origin access raises a
/// SecurityError while the provider page is open and is simply retried later.
pub(super) fn popup_data(popup: &Window, origin: &str) -> Option<Value> {
    let location = popup.location();
    if location.origin().ok()?.as_str() != origin
        || location.pathname().ok()?.as_str() != "/callback"
    {
        return None;
    }
    let params = web_sys::UrlSearchParams::new_with_str(&location.search().ok()?).ok()?;
    Some(
        json!({"code": params.get("code"), "state": params.get("state"), "provider": params.get("provider"), "error": params.get("error"), "errorDescription": params.get("error_description"), "attemptNonce": params.get("openproxy_oauth_nonce")}),
    )
}

/// A matching, already-consumed popup may be closed only after exchange
/// succeeds, and only if it still shows that same callback when the timer fires.
pub(super) struct PopupClose {
    popup: Window,
    origin: String,
    callback: Value,
    consumed_at: f64,
}

impl PopupClose {
    pub fn capture(popup: Window, origin: String, data: &Value) -> Option<Self> {
        let current = popup_data(&popup, &origin)?;
        let code = data["code"].as_str().filter(|v| !v.is_empty())?;
        if current["code"].as_str() != Some(code)
            || current["state"] != data["state"]
            || current["attemptNonce"] != data["attemptNonce"]
            || current["provider"] != data["provider"]
            || current["error"].as_str().is_some_and(|v| !v.is_empty())
        {
            return None;
        }
        Some(Self {
            popup,
            origin,
            callback: current,
            consumed_at: js_sys::Date::now(),
        })
    }

    pub fn after_success(self) {
        let remaining =
            (2_000.0 - (js_sys::Date::now() - self.consumed_at)).clamp(0.0, 2_000.0) as u32;
        Timeout::new(remaining, move || {
            if popup_data(&self.popup, &self.origin).as_ref() == Some(&self.callback) {
                let _ = self.popup.close();
            }
        })
        .forget();
    }
}

fn decode(value: &JsValue) -> Option<Value> {
    let text: String = js_sys::JSON::stringify(value).ok()?.as_string()?;
    serde_json::from_str(&text).ok()
}

pub(super) fn valid_origin(origin: &str, redirect_uri: &str) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    if window.location().origin().ok().as_deref() == Some(origin) {
        return true;
    }
    web_sys::Url::new(redirect_uri).ok().is_some_and(|url| {
        matches!(url.protocol().as_str(), "http:" | "https:") && url.origin() == origin
    })
}

pub(super) fn remove_stored(raw: &str) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten())
        && storage.get_item("oauth_callback").ok().flatten().as_deref() == Some(raw)
    {
        let _ = storage.remove_item("oauth_callback");
    }
}

pub(super) fn read_stored(callback: Callback<Delivery>) {
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        return;
    };
    let Some(raw) = storage.get_item("oauth_callback").ok().flatten() else {
        return;
    };
    let Ok(data) = serde_json::from_str::<Value>(&raw) else {
        remove_stored(&raw);
        return;
    };
    let fresh = data["timestamp"].as_f64().is_some_and(|timestamp| {
        let now = js_sys::Date::now();
        timestamp.is_finite() && timestamp <= now && now - timestamp < 30_000.0
    });
    if !fresh {
        remove_stored(&raw);
        return;
    }
    callback.call(Delivery {
        data,
        origin: None,
        storage_value: Some(raw),
    });
}

pub(super) struct Listeners {
    window: Window,
    channel: Option<BroadcastChannel>,
    message: Closure<dyn FnMut(MessageEvent)>,
    storage: Closure<dyn FnMut(StorageEvent)>,
    channel_message: Closure<dyn FnMut(MessageEvent)>,
}

impl Listeners {
    pub fn new(callback: Callback<Delivery>) -> Option<Self> {
        let window = web_sys::window()?;
        let message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let Some(message) = decode(&event.data()) else {
                return;
            };
            if message["type"] == "oauth_callback" && message["data"].is_object() {
                callback.call(Delivery {
                    data: message["data"].clone(),
                    origin: Some(event.origin()),
                    storage_value: None,
                });
            }
        });
        let storage = Closure::<dyn FnMut(StorageEvent)>::new(move |event: StorageEvent| {
            if event.key().as_deref() != Some("oauth_callback") {
                return;
            }
            let Some(raw) = event.new_value() else { return };
            if let Ok(data) = serde_json::from_str::<Value>(&raw) {
                callback.call(Delivery {
                    data,
                    origin: None,
                    storage_value: Some(raw),
                });
            }
        });
        let channel_message =
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                if let Some(data) = decode(&event.data()) {
                    callback.call(Delivery {
                        data,
                        origin: None,
                        storage_value: None,
                    });
                }
            });
        let _ =
            window.add_event_listener_with_callback("message", message.as_ref().unchecked_ref());
        let _ =
            window.add_event_listener_with_callback("storage", storage.as_ref().unchecked_ref());
        let channel = BroadcastChannel::new("oauth_callback").ok();
        if let Some(channel) = &channel {
            channel.set_onmessage(Some(channel_message.as_ref().unchecked_ref()));
        }
        Some(Self {
            window,
            channel,
            message,
            storage,
            channel_message,
        })
    }
}

impl Drop for Listeners {
    fn drop(&mut self) {
        let _ = self
            .window
            .remove_event_listener_with_callback("message", self.message.as_ref().unchecked_ref());
        let _ = self
            .window
            .remove_event_listener_with_callback("storage", self.storage.as_ref().unchecked_ref());
        if let Some(channel) = &self.channel {
            channel.set_onmessage(None);
            channel.close();
        }
        // Keep the channel handler alive through close.
        let _ = &self.channel_message;
    }
}
