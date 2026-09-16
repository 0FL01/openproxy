use futures_util::StreamExt;
use gloo_net::eventsource::futures::EventSource;
use leptos::{
    prelude::*,
    task::{spawn_local, spawn_local_scoped_with_cancellation},
};

use super::DashboardShell;
use crate::api;

#[component]
pub fn ConsoleLogPage() -> impl IntoView {
    let lines = RwSignal::new(Vec::<String>::new());
    let status = RwSignal::new("Connecting…".to_string());

    Effect::new(
        move || match EventSource::new("/api/translator/console-logs/stream") {
            Ok(mut source) => {
                let Ok(mut stream) = source.subscribe("message") else {
                    status.set("Unable to subscribe.".into());
                    return;
                };
                status.set("Connected".into());
                spawn_local_scoped_with_cancellation(async move {
                    let _source = source;
                    while let Some(event) = stream.next().await {
                        match event {
                            Ok((_, event)) => {
                                if let Some(data) = event.data().as_string() {
                                    lines.update(|items| items.push(data));
                                }
                            }
                            Err(error) => status.set(format!("Stream error: {error}")),
                        }
                    }
                });
            }
            Err(error) => status.set(format!("Connection failed: {error}")),
        },
    );

    let clear = move |_| {
        spawn_local(async move {
            if api::send_empty("DELETE", "/api/translator/console-logs")
                .await
                .is_ok()
            {
                lines.set(Vec::new());
            }
        })
    };
    view! { <DashboardShell title="Console Log"><section class="card stack"><div class="row between"><span class="badge">{move || status.get()}</span><button class="button danger" on:click=clear>"Clear"</button></div><pre class="console-view">{move || lines.get().join("\n")}</pre></section></DashboardShell> }
}
