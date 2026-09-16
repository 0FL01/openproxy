use leptos::prelude::*;

#[component]
pub fn LandingPage() -> impl IntoView {
    view! {
        <main class="centered-page">
            <div class="hero stack">
                <p class="eyebrow">"ONE ROUTER. EVERY PROVIDER."</p>
                <h1>"OpenProxy"</h1>
                <p>"A single local endpoint for your AI providers and coding tools."</p>
                <a class="button primary" href="/dashboard">"Open dashboard"</a>
            </div>
        </main>
    }
}
