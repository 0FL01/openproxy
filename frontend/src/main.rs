#[cfg(any(target_arch = "wasm32", test))]
mod auth;

#[cfg(any(target_arch = "wasm32", test))]
mod auth_callback;

#[cfg(any(target_arch = "wasm32", test))]
mod available_models;

#[cfg(any(target_arch = "wasm32", test))]
mod thinking;

#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
fn main() {
    browser::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("This frontend is a browser/WASM application; build it with dx build --web.");
}
