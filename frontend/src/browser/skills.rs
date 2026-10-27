use super::{endpoint::copy_text, ui::CARD_CLASS};
use dioxus::prelude::*;

const RAW_URL: &str = "https://raw.githubusercontent.com/0FL01/openproxy/refs/heads/main/.agents/skills/openproxy/SKILL.md";
const BLOB_URL: &str =
    "https://github.com/0FL01/openproxy/blob/main/.agents/skills/openproxy/SKILL.md";
const REPO_URL: &str = "https://github.com/0FL01/openproxy/tree/main/.agents/skills";

#[component]
pub(super) fn SkillsPage() -> Element {
    let copied = use_signal(String::new);
    let generation = use_signal(|| 0);
    rsx! {
        div { class: "max-w-4xl mx-auto space-y-6 pb-8",
            div { class: CARD_CLASS,
                div { class: "text-xs text-body mb-2", "Paste this to your AI:" }
                div { class: "px-3 py-2 rounded-lg bg-surface-soft font-mono text-[12px] text-ink", "Read this skill and use it: {RAW_URL}" }
            }
            div { class: "space-y-2",
                div { class: "flex items-start gap-3 p-4 rounded-xl border transition-colors border-brand-coral/40 bg-brand-coral/[0.04]",
                    div { class: "size-9 rounded-lg flex items-center justify-center shrink-0 bg-brand-coral text-on-primary",
                        span { class: "material-symbols-outlined text-[18px]", "hub" }
                    }
                    div { class: "min-w-0 flex-1",
                        div { class: "flex items-center gap-2 flex-wrap",
                            h3 { class: "font-semibold text-sm text-ink", "OpenProxy (Entry)" }
                            span { class: "inline-flex items-center gap-1.5 font-semibold leading-none rounded-full bg-brand-coral/15 text-brand-coral px-2 py-0.5 text-[10px] tracking-wide", "START HERE" }
                        }
                        p { class: "text-xs text-body mt-0.5", "Build this fork from source or Compose, initialize the server, configure providers, and connect an AI client." }
                        a { href: BLOB_URL, target: "_blank", rel: "noreferrer", class: "text-[11px] text-body hover:text-brand-coral mt-1 inline-flex items-center gap-1 break-all",
                            "{RAW_URL}" span { class: "material-symbols-outlined text-[12px]", "open_in_new" }
                        }
                    }
                    button { title: RAW_URL, class: "px-2 py-1 rounded-lg bg-surface-soft text-ink text-[11px] font-medium hover:bg-surface-card transition-colors cursor-pointer shrink-0 inline-flex items-center gap-1", onclick: move |_| copy_text(RAW_URL.to_owned(), "skill".into(), copied, generation),
                        span { class: "material-symbols-outlined text-[12px]", if copied() == "skill" { "check" } else { "content_copy" } }
                        if copied() == "skill" { "Copied!" } else { "Copy link" }
                    }
                }
            }
            div { class: CARD_CLASS,
                div { class: "flex items-center justify-between gap-3 flex-wrap",
                    div { h2 { class: "text-sm font-semibold text-ink", "More on GitHub" } p { class: "text-xs text-body mt-0.5", "Browse source, README, and examples." } }
                    a { href: REPO_URL, target: "_blank", rel: "noreferrer", class: "text-sm text-brand-coral hover:underline inline-flex items-center gap-1", span { class: "material-symbols-outlined text-[16px]", "open_in_new" } "View on GitHub" }
                }
            }
        }
    }
}
