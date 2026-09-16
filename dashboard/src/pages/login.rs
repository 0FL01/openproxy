use leptos::{prelude::*, task::spawn_local};
use serde::{Deserialize, Serialize};

use crate::{api, browser::redirect};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    #[serde(default)]
    require_login: bool,
    #[serde(default)]
    authenticated: bool,
}

#[derive(Serialize)]
struct LoginRequest {
    password: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PasswordRequest {
    current_password: String,
    new_password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    authenticated: bool,
    #[serde(default)]
    must_change_password: bool,
    error: Option<String>,
}

#[component]
pub fn LoginPage() -> impl IntoView {
    let password = RwSignal::new(String::new());
    let error = RwSignal::new(String::new());
    let loading = RwSignal::new(false);
    let must_change = RwSignal::new(false);
    let new_password = RwSignal::new(String::new());
    let confirm_password = RwSignal::new(String::new());

    Effect::new(move || {
        spawn_local(async move {
            if let Ok(status) = api::get_json::<AuthStatus>("/api/auth/status").await
                && (!status.require_login || status.authenticated)
            {
                redirect("/dashboard");
            }
        });
    });

    let submit = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        if loading.get_untracked() {
            return;
        }
        loading.set(true);
        error.set(String::new());
        spawn_local(async move {
            let request = LoginRequest {
                password: password.get_untracked(),
            };
            match api::send_json::<_, LoginResponse>("POST", "/api/auth/login", &request).await {
                Ok(response) if response.success || response.authenticated => {
                    if response.must_change_password {
                        must_change.set(true);
                        error.set(String::new());
                    } else {
                        redirect("/dashboard");
                    }
                }
                Ok(response) => error.set(
                    response
                        .error
                        .unwrap_or_else(|| "Invalid password".to_string()),
                ),
                Err(message) => error.set(message),
            }
            loading.set(false);
        });
    };

    let change_password = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        let next = new_password.get_untracked();
        if next.len() < 8 {
            error.set("New password must be at least 8 characters.".into());
            return;
        }
        if next != confirm_password.get_untracked() {
            error.set("Passwords do not match.".into());
            return;
        }
        loading.set(true);
        let request = PasswordRequest {
            current_password: password.get_untracked(),
            new_password: next,
        };
        spawn_local(async move {
            match api::send_json::<_, serde_json::Value>("POST", "/api/auth/password", &request)
                .await
            {
                Ok(response) => {
                    if response
                        .get("sessionsInvalidated")
                        .and_then(|value| value.as_bool())
                        == Some(true)
                    {
                        redirect("/login");
                    } else {
                        redirect("/dashboard");
                    }
                }
                Err(message) => error.set(message),
            }
            loading.set(false);
        });
    };

    view! {
        <main class="centered-page">
            <form class="card login-card stack" class:hidden=move || must_change.get() on:submit=submit>
                <div>
                    <p class="eyebrow">"OPENPROXY"</p>
                    <h1>"Sign in"</h1>
                    <p class="muted">"Enter your dashboard password."</p>
                </div>
                <label>
                    <span>"Password"</span>
                    <input type="password" autocomplete="current-password" bind:value=password />
                </label>
                <Show when=move || !error.get().is_empty()>
                    <p class="error" role="alert">{move || error.get()}</p>
                </Show>
                <button class="button primary" type="submit" disabled=move || loading.get()>
                    {move || if loading.get() { "Signing in…" } else { "Sign in" }}
                </button>
            </form>
            <form class="card login-card stack" class:hidden=move || !must_change.get() on:submit=change_password>
                <div><p class="eyebrow">"PASSWORD ROTATION"</p><h1>"Choose a new password"</h1><p class="muted">"Set a new password before continuing."</p></div>
                <label><span>"New password"</span><input type="password" autocomplete="new-password" bind:value=new_password /></label>
                <label><span>"Confirm password"</span><input type="password" autocomplete="new-password" bind:value=confirm_password /></label>
                <Show when=move || !error.get().is_empty()><p class="error" role="alert">{move || error.get()}</p></Show>
                <button class="button primary" type="submit" disabled=move || loading.get()>"Set password"</button>
            </form>
        </main>
    }
}
