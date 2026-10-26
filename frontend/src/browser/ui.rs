use dioxus::prelude::*;
use std::rc::Rc;
use wasm_bindgen::{closure::Closure, JsCast};

pub const INPUT_CLASS: &str = super::INPUT_CLASS;
pub const CARD_CLASS: &str =
    "bg-canvas border border-hairline text-ink rounded-mini-lg shadow-none p-5";

#[component]
pub fn Button(
    label: String,
    #[props(default = "primary")] variant: &'static str,
    #[props(default)] icon: Option<String>,
    #[props(default)] full_width: bool,
    #[props(default)] disabled: bool,
    #[props(default)] loading: bool,
    #[props(default)] small: bool,
    #[props(default)] extra_class: &'static str,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    let variant = match variant {
        "secondary" => "bg-canvas text-ink border border-hairline hover:border-ink/40 hover:bg-surface-soft disabled:border-hairline disabled:text-muted",
        "ghost" => "bg-transparent text-ink hover:bg-surface-card disabled:text-muted",
        "danger" => "bg-[color:var(--color-danger)] text-white hover:opacity-90 disabled:bg-hairline disabled:text-muted",
        _ => "bg-brand-coral text-on-primary hover:bg-brand-coral-active disabled:bg-primary-disabled disabled:text-muted-soft",
    };
    let width = if full_width { "w-full" } else { "" };
    let size = if small {
        "h-8 px-3 text-[12px] font-medium"
    } else {
        "h-10 px-4 text-[14px] font-medium"
    };
    let class = format!("inline-flex items-center justify-center gap-2 rounded-mini-md leading-none transition-colors duration-150 ease-out cursor-pointer tracking-tight active:scale-[0.99] disabled:cursor-not-allowed disabled:active:scale-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-coral/30 focus-visible:ring-offset-2 focus-visible:ring-offset-canvas {variant} {size} {width} {extra_class}");
    rsx! {
        button { class, disabled: disabled || loading, onclick,
            if loading {
                span { class: "material-symbols-outlined animate-spin text-[18px]", "progress_activity" }
            } else if let Some(icon) = icon {
                span { class: "material-symbols-outlined text-[18px]", "{icon}" }
            }
            "{label}"
        }
    }
}

#[component]
pub fn Toggle(
    checked: bool,
    #[props(default)] small: bool,
    label: String,
    onchange: EventHandler<bool>,
) -> Element {
    let track = if small { "w-8 h-4" } else { "w-11 h-6" };
    let size = if small { "size-3" } else { "size-5" };
    let color = if checked {
        "bg-brand-500"
    } else {
        "bg-surface-3"
    };
    let translate = if checked {
        if small {
            "translate-x-4"
        } else {
            "translate-x-5"
        }
    } else {
        "translate-x-0.5"
    };
    rsx! {
        div { class: "flex items-center gap-3",
            button {
                r#type: "button", role: "switch", aria_checked: "{checked}", aria_label: "{label}",
                class: "relative inline-flex shrink-0 cursor-pointer rounded-full transition-colors duration-200 ease-in-out focus:outline-none focus:ring-2 focus:ring-brand-500/30 {color} {track}",
                onclick: move |_| onchange.call(!checked),
                span { class: "pointer-events-none inline-block rounded-full bg-white shadow-sm transform transition duration-200 ease-in-out {translate} {size} mt-0.5" }
            }
        }
    }
}

// Each reference modal closes on Escape, but not by clicking the dimmed overlay.
struct ModalGuard {
    document: web_sys::Document,
    handler: Closure<dyn FnMut(web_sys::KeyboardEvent)>,
}

impl Drop for ModalGuard {
    fn drop(&mut self) {
        let _ = self
            .document
            .remove_event_listener_with_callback("keydown", self.handler.as_ref().unchecked_ref());
        if let Some(body) = self.document.body() {
            let _ = body.style().remove_property("overflow");
        }
    }
}

#[component]
pub fn Modal(
    title: String,
    #[props(default)] small: bool,
    on_close: EventHandler<()>,
    children: Element,
    #[props(default)] footer: Option<Element>,
) -> Element {
    use_hook(move || {
        let document = web_sys::window().and_then(|w| w.document())?;
        if let Some(body) = document.body() {
            let _ = body.style().set_property("overflow", "hidden");
        }
        let handler = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
            move |event: web_sys::KeyboardEvent| {
                if event.key() == "Escape" {
                    on_close.call(());
                }
            },
        );
        let _ =
            document.add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref());
        Some(Rc::new(ModalGuard { document, handler }))
    });
    let size = if small { "max-w-sm" } else { "max-w-md" };
    rsx! {
        div { class: "fixed inset-0 z-50 flex items-center justify-center p-4",
            div { class: "absolute inset-0 bg-black/50 backdrop-blur-[2px] fade-in" }
            div { class: "relative w-full bg-surface border border-border-subtle rounded-[14px] shadow-[var(--shadow-elev)] fade-in {size}", role: "dialog", aria_modal: "true", aria_label: "{title}",
                div { class: "flex items-center justify-between p-2 border-b border-border-subtle",
                    div { class: "flex items-center", h2 { class: "text-lg font-semibold text-text-main", "{title}" } }
                    button { class: "p-1.5 rounded-[10px] text-text-muted hover:bg-surface-2 hover:text-text-main transition-colors", aria_label: "Close", onclick: move |_| on_close.call(()),
                        span { class: "material-symbols-outlined text-[20px]", "close" }
                    }
                }
                div { class: "p-6 max-h-[calc(85vh-100px)] overflow-y-auto custom-scrollbar", {children} }
                if let Some(footer) = footer {
                    div { class: "flex items-center justify-end gap-3 p-6 border-t border-border-subtle", {footer} }
                }
            }
        }
    }
}

#[component]
pub fn CardSkeleton() -> Element {
    rsx! {
        div { class: "p-6 rounded-[14px] border border-border-subtle bg-surface shadow-[var(--shadow-soft)]",
            div { class: "flex items-center justify-between mb-4",
                div { class: "animate-pulse rounded-[10px] bg-surface-2 h-4 w-24" }
                div { class: "animate-pulse rounded-[10px] bg-surface-2 size-10 rounded-[10px]" }
            }
            div { class: "animate-pulse rounded-[10px] bg-surface-2 h-8 w-16 mb-2" }
            div { class: "animate-pulse rounded-[10px] bg-surface-2 h-3 w-20" }
        }
    }
}
