//! HTTP APIs (A1–A5): providers as plugin files. Each plugin declares its routes (receive → keep →
//! decide → respond → send), named answers, data tables and a service block (auth, billing,
//! limits). One port serves every plugin of every environment.

pub mod ca;
pub mod catalog;
pub mod engine;
pub mod expr;
pub mod openapi;
pub mod record;
pub mod pades;
pub mod people;
pub mod restpki;
pub mod soap;
pub mod steps;
pub mod plugin;
pub mod store;
pub mod tls;
pub mod tools;

use std::sync::{
    atomic::{AtomicU16, Ordering},
    Mutex,
};

use axum::{
    extract::{Path, State},
    http::header,
    response::IntoResponse,
    routing::get,
    Router,
};
use stuntman_core::{Ctx, Module, Tool};

static PORT: AtomicU16 = AtomicU16::new(8028);
static TLS_PORT: AtomicU16 = AtomicU16::new(8443);
pub static BIND_ERROR: Mutex<Option<String>> = Mutex::new(None);
pub static TLS_BIND_ERROR: Mutex<Option<String>> = Mutex::new(None);

pub fn tls_port() -> u16 {
    TLS_PORT.load(Ordering::Relaxed)
}

pub fn port() -> u16 {
    PORT.load(Ordering::Relaxed)
}

pub struct Apis {
    pub port: u16,
    /// HTTPS with the Stuntman Test CA (real host names, mTLS).
    pub tls_port: u16,
}

impl Module for Apis {
    fn id(&self) -> &'static str {
        "apis"
    }

    fn migrations(&self) -> &'static str {
        static SQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        SQL.get_or_init(|| format!("{}{}", store::SQL, steps::SQL))
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn extra_routes(&self) -> Router<Ctx> {
        let ca = || async { ([(header::CONTENT_TYPE, "application/x-pem-file"), (header::CONTENT_DISPOSITION, "attachment; filename=\"stuntman-test-ca.pem\"")], ca::get().pem.clone()).into_response() };
        Router::new().route("/api/keyvault/test-ca.pem", get(ca)).route("/api/test-ca.pem", get(ca)).route("/api/plugins/{service}/yaml", get(|State(ctx): State<Ctx>, Path(s): Path<String>| async move {
            ([(header::CONTENT_TYPE, "application/yaml")], plugin::text(&ctx, &s)).into_response()
        }))
    }

    fn listen(&self, envs: stuntman_core::env::Envs) {
        PORT.store(self.port, Ordering::Relaxed);
        TLS_PORT.store(self.tls_port, Ordering::Relaxed);
        if let Some(c) = envs.default_env() {
            if let Some(root) = c.data_dir.parent().and_then(|p| p.parent()) {
                ca::set_dir(&root.join("keys"));
            }
        }
        let app = Router::new().fallback(engine::handle).with_state(envs.clone());
        tokio::spawn(stuntman_core::netserve::serve(self.port, app.clone(), "APIs", &BIND_ERROR));
        tokio::spawn(tls::serve(self.tls_port, envs, app));
    }
}
