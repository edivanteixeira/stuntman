//! Stuntman UI (Leptos, client-side). The shell is shared; each service brings its own folder of screens.

mod api;
mod apis;
mod catalog;
mod email;
mod environments;
mod identity;
mod keyvault;
mod messaging;
mod shell;
mod sms;
mod storage;
mod telemetry;
mod timeline;
mod ui;
mod webhook;
mod whatsapp;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(shell::App);
}
