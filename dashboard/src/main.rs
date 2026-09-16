mod api;
mod app;
mod browser;
mod components;
mod model_data;
mod pages;

use app::App;
use leptos::{mount::mount_to_body, prelude::*};

fn main() {
    mount_to_body(|| view! { <App /> });
}
