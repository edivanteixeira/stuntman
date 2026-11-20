//! HTTP APIs (A1–A5): providers as plugin files. Each plugin declares its routes (receive → keep →
//! decide → respond → send), named answers, data tables and a service block (auth, billing,
//! limits). One port serves every plugin of every environment.

pub mod ca;
pub mod catalog;
pub mod certs;
pub mod engine;
pub mod expr;
pub mod hbs;
pub mod mcp;
pub mod mocks;
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
pub mod xmldsig;

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

fn file(kind: &'static str, name: &str, body: Vec<u8>) -> axum::response::Response {
    ([(header::CONTENT_TYPE, kind.to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))], body).into_response()
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
        SQL.get_or_init(|| format!("{}{}{}", store::SQL, steps::SQL, certs::SQL))
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn start(&self, ctx: Ctx) {
        store::upgrade(&ctx);
    }

    fn extra_routes(&self) -> Router<Ctx> {
        let ca = || async { file("application/x-pem-file", "stuntman-test-ca.pem", ca::get().pem.clone().into_bytes()) };
        // DER: what macOS Keychain, the Windows store and Android take; .crt and .der are the same bytes.
        let der = |name: &'static str| move || async move { file("application/pkix-cert", name, ca::der_of(&ca::get().pem).first().map(|d| d.to_vec()).unwrap_or_default()) };
        // PKCS#12 with the CA only, password changeit: a Java truststore and a certificate-only PFX.
        let p12 = |name: &'static str| move || async move {
            match ca::get().truststore() {
                Ok(b) => file("application/x-pkcs12", name, b),
                Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
            }
        };
        // The public roots of this machine plus the Test CA, one file: SSL_CERT_FILE, REQUESTS_CA_BUNDLE,
        // NODE_EXTRA_CA_CERTS or curl --cacert take it, so the app still trusts the real internet.
        let bundle = || async {
            let roots = [std::env::var("SSL_CERT_FILE").unwrap_or_default().as_str(), "/etc/ssl/certs/ca-certificates.crt", "/etc/pki/tls/certs/ca-bundle.crt", "/etc/ssl/cert.pem"]
                .iter().filter(|p| !p.is_empty()).find_map(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
            file("application/x-pem-file", "stuntman-ca-bundle.pem", format!("{}\n# Stuntman Test CA\n{}", roots.trim_end(), ca::get().pem).into_bytes())
        };
        Router::new().route("/api/keyvault/test-ca.pem", get(ca)).route("/api/test-ca.pem", get(ca)).route("/api/ca-bundle.pem", get(bundle))
            .route("/api/test-ca.crt", get(der("stuntman-test-ca.crt"))).route("/api/test-ca.der", get(der("stuntman-test-ca.der")))
            .route("/api/test-ca.p12", get(p12("stuntman-test-ca.p12"))).route("/api/test-ca.pfx", get(p12("stuntman-test-ca.pfx"))).route("/api/plugins/{service}/yaml", get(|State(ctx): State<Ctx>, Path(s): Path<String>| async move {
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
        tokio::spawn(tls::serve(self.tls_port, envs.clone(), app));
        // Webhooks kept for later go when the environment's clock reaches them (A10c).
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                for c in envs.all() {
                    if !c.state.gone() {
                        engine::fire_due(&c).await;
                    }
                }
            }
        });
    }
}
