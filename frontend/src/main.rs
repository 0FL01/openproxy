#[cfg(any(target_arch = "wasm32", test))]
mod auth;

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
