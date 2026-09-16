use leptos::prelude::*;

const THEME_KEY: &str = "theme";

pub fn redirect(path: &str) {
    if let Some(window) = window() {
        let _ = window.location().assign(path);
    }
}

pub fn storage_get(key: &str) -> Option<String> {
    window()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(key).ok().flatten())
}

pub fn storage_set(key: &str, value: &str) -> Result<(), String> {
    let storage = window()
        .and_then(|window| window.local_storage().ok().flatten())
        .ok_or_else(|| "local storage is unavailable".to_string())?;
    storage.set_item(key, value).map_err(js_error)
}

pub fn storage_remove(key: &str) {
    if let Some(storage) = window().and_then(|window| window.local_storage().ok().flatten()) {
        let _ = storage.remove_item(key);
    }
}

pub fn initialize_theme() -> RwSignal<String> {
    let initial = storage_get(THEME_KEY)
        .and_then(|raw| {
            serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .and_then(|value| value.pointer("/state/theme")?.as_str().map(str::to_owned))
                .or(Some(raw))
        })
        .filter(|theme| matches!(theme.as_str(), "light" | "dark" | "system"))
        .unwrap_or_else(|| "system".to_string());
    let theme = RwSignal::new(initial);

    Effect::new(move || {
        let value = theme.get();
        if let Some(document) = window().and_then(|window| window.document()) {
            if let Some(root) = document.document_element() {
                let classes = root.class_list();
                let prefers_dark = window()
                    .and_then(|window| {
                        window
                            .match_media("(prefers-color-scheme: dark)")
                            .ok()
                            .flatten()
                    })
                    .is_some_and(|query| query.matches());
                let dark = value == "dark" || (value == "system" && prefers_dark);
                let _ = classes.toggle_with_force("dark", dark);
            }
        }
        let encoded = serde_json::json!({"state":{"theme": value},"version":0}).to_string();
        let _ = storage_set(THEME_KEY, &encoded);
    });

    theme
}

pub fn window() -> Option<web_sys::Window> {
    web_sys::window()
}

fn js_error(error: wasm_bindgen::JsValue) -> String {
    error
        .as_string()
        .unwrap_or_else(|| "browser operation failed".to_string())
}
