use leptos::prelude::*;
use leptos_router::components::A;

#[component]
pub fn NotFound() -> impl IntoView {
    view! {
        <main class="centered-page">
            <div class="card">
                <h1>"Page not found"</h1>
                <p>"The requested OpenProxy dashboard page does not exist."</p>
                <A href="/dashboard" attr:class="button primary">"Back to dashboard"</A>
            </div>
        </main>
    }
}
