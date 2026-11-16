//! Stuntman UI (Leptos, client-side). The shell is shared; each service brings its own folder of screens.

mod access;
mod ai;
mod api;
mod apis;
mod catalog;
mod email;
mod environments;
mod events;
mod ftp;
mod identity;
mod keyvault;
mod kinesis;
mod messaging;
mod pubsub;
mod shell;
mod sms;
mod storage;
mod streams;
mod telemetry;
mod timeline;
mod ui;
mod webhook;
mod whatsapp;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(shell::App);
}
