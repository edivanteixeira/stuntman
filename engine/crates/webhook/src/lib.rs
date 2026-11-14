//! Webhook inboxes: endpoints the app sends its own webhooks to (H1–H2). Each inbox answers as
//! configured (errors, retries, hangs, resets, traps) and checks signatures, timestamps and
//! replays, so the app's sending side is tested like a careful customer would.

pub mod config;
pub mod listener;
pub mod sign;
pub mod store;
pub mod tools;

use std::sync::{atomic::{AtomicU16, Ordering}, Mutex};

use axum::{extract::State, http::header, response::IntoResponse, routing::get, Router};
use stuntman_core::{Ctx, Module, Tool};

static PORT: AtomicU16 = AtomicU16::new(8027);
pub static BIND_ERROR: Mutex<Option<String>> = Mutex::new(None);

pub fn port() -> u16 {
    PORT.load(Ordering::Relaxed)
}

pub struct Webhook {
    /// The inboxes' own HTTP port (resets and empty replies need a socket of their own).
    pub port: u16,
}

impl Module for Webhook {
    fn id(&self) -> &'static str {
        "webhook"
    }

    fn migrations(&self) -> &'static str {
        store::SQL
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn extra_routes(&self) -> Router<Ctx> {
        Router::new().route("/api/webhooks/inboxes.yaml", get(|State(ctx): State<Ctx>| async move { ([(header::CONTENT_TYPE, "application/yaml")], config::yaml_text(&ctx)).into_response() }))
    }

    fn listen(&self, envs: stuntman_core::env::Envs) {
        PORT.store(self.port, Ordering::Relaxed);
        tokio::spawn(listener::serve(envs, self.port));
    }

    fn start(&self, ctx: Ctx) {
        tokio::spawn(async move {
            while !ctx.state.gone() {
                let _ = store::sweep(&ctx).await;
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        });
    }
}
