#[cfg(target_family = "wasm")]
mod app;
#[cfg(target_family = "wasm")]
mod ecash;
#[cfg(target_family = "wasm")]
mod exchange;
#[cfg(target_family = "wasm")]
mod qr;
#[cfg(target_family = "wasm")]
mod scanner;
#[cfg(target_family = "wasm")]
mod storage;

#[cfg(target_family = "wasm")]
fn main() {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();
    leptos::mount::mount_to_body(app::App);
}

#[cfg(not(target_family = "wasm"))]
fn main() {
    println!("fm-offline-pos is a wasm-only app. Use `trunk serve` or `trunk build`.");
}
