use dioxus::prelude::*;

const BASE: &str = "inline-flex items-center justify-center gap-2 rounded-mini-md leading-none transition-colors duration-150 ease-out cursor-pointer tracking-tight active:scale-[0.99] disabled:cursor-not-allowed disabled:active:scale-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-coral/30 focus-visible:ring-offset-2 focus-visible:ring-offset-canvas h-8 px-3 text-[12px] font-medium w-9 px-0";
const OUTLINE: &str = "bg-transparent text-ink border border-ink/80 hover:bg-ink/5 disabled:border-hairline disabled:text-muted";
const GHOST: &str = "bg-transparent text-ink hover:bg-surface-card disabled:text-muted";
const PRIMARY: &str = "bg-brand-coral text-on-primary hover:bg-brand-coral-active disabled:bg-primary-disabled disabled:text-muted-soft";

#[component]
pub(super) fn Pagination(
    page: usize,
    page_size: usize,
    total: usize,
    on_page: EventHandler<usize>,
    on_size: EventHandler<usize>,
) -> Element {
    let pages = total.div_ceil(page_size);
    let start = if total > 0 {
        (page - 1) * page_size + 1
    } else {
        0
    };
    let end = (page * page_size).min(total);
    let mut first = page.saturating_sub(2).max(1);
    let last = pages.min(first + 4);
    if last.saturating_sub(first) + 1 < 5 {
        first = last.saturating_sub(4).max(1);
    }
    rsx! {
        div { class: "flex flex-col sm:flex-row items-center justify-between gap-4 py-4 px-2 border-t border-border px-4",
            if total > 0 { div { class: "text-sm text-text-muted", "Showing " span { class: "font-medium text-text-main", "{start}" } " to " span { class: "font-medium text-text-main", "{end}" } " of " span { class: "font-medium text-text-main", "{total}" } " results" } }
            div { class: "flex flex-wrap items-center justify-center gap-2 sm:gap-4",
                div { class: "flex items-center gap-2", span { class: "text-sm text-text-muted", "Rows:" } select { aria_label: "Rows per page", value: "{page_size}", style: "color-scheme: auto", class: "h-9 rounded-lg border border-black/10 dark:border-white/10 bg-surface text-sm text-text-main focus:outline-none focus:ring-2 focus:ring-primary/20 cursor-pointer", onchange: move |e| { if let Ok(size) = e.value().parse() { on_size.call(size); } }, for size in [10, 20, 50] { option { value: "{size}", "{size}" } } } }
                if pages > 1 { div { class: "flex items-center gap-1",
                    button { aria_label: "Previous page", class: "{BASE} {OUTLINE}", disabled: page == 1, onclick: move |_| on_page.call(page - 1), span { class: "material-symbols-outlined text-[18px]", "chevron_left" } }
                    if first > 1 { button { class: "{BASE} {GHOST} hidden sm:inline-flex", onclick: move |_| on_page.call(1), "1" } if first > 2 { span { class: "text-text-muted px-1 hidden sm:inline", "..." } } }
                    for n in first..=last { button { key: "{n}", class: if n == page { format!("{BASE} {PRIMARY} inline-flex") } else { format!("{BASE} {GHOST} hidden sm:inline-flex") }, onclick: move |_| on_page.call(n), "{n}" } }
                    if last < pages { if last < pages - 1 { span { class: "text-text-muted px-1 hidden sm:inline", "..." } } button { class: "{BASE} {GHOST} hidden sm:inline-flex", onclick: move |_| on_page.call(pages), "{pages}" } }
                    button { aria_label: "Next page", class: "{BASE} {OUTLINE}", disabled: page == pages, onclick: move |_| on_page.call(page + 1), span { class: "material-symbols-outlined text-[18px]", "chevron_right" } }
                } }
            }
        }
    }
}
