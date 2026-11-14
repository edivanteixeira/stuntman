//! Secrets (Azure Key Vault first): a provider-neutral core (vault.rs, store.rs) with the Azure Key
//! Vault REST adapter (azure.rs) on its own HTTPS port, a managed identity endpoint (IMDS shape) on
//! the main port, and the K1–K5 tools.

pub mod azure;
pub mod config;
pub mod crypto;
pub mod store;
pub mod tls;
pub mod tools;
pub mod vault;

use std::sync::atomic::{AtomicU16, Ordering};

use axum::{extract::{Query, State}, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, routing::get, Json, Router};
use stuntman_core::{Ctx, Module, Tool};
use serde_json::json;

static PORT: AtomicU16 = AtomicU16::new(8444);

pub fn port() -> u16 {
    PORT.load(Ordering::Relaxed)
}

pub struct KeyVault {
    pub port: u16,
}

/// IMDS / App Service managed identity endpoint: the identity is client_id, mi_res_id, object_id,
/// or the X-IDENTITY-HEADER value (set IDENTITY_HEADER=<identity> next to IDENTITY_ENDPOINT).
async fn imds(State(ctx): State<Ctx>, Query(q): Query<std::collections::HashMap<String, String>>, h: HeaderMap) -> Response {
    let prov = store::provoked(&ctx, "*", "imds", "").await.ok().flatten().map(|p| p.0);
    match prov.as_deref() {
        Some("500") => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "internal_error", "error_description": "IMDS is unavailable (provoked in Stuntman)"}))).into_response(),
        Some("slow") => tokio::time::sleep(std::time::Duration::from_secs(5)).await,
        Some("no_identity") => return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_request", "error_description": "Identity not found"}))).into_response(),
        _ => {}
    }
    if h.get("metadata").is_none() && h.get("x-identity-header").is_none() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_request", "error_description": "Required metadata header not specified"}))).into_response();
    }
    let id = ["client_id", "mi_res_id", "object_id", "principal_id"].iter().find_map(|k| q.get(*k).cloned())
        .or_else(|| h.get("x-identity-header").and_then(|v| v.to_str().ok()).map(String::from)).unwrap_or("app-mi".into());
    let resource = q.get("resource").cloned().unwrap_or("https://vault.azure.net".into());
    let (token, exp) = vault::mint(&id, &resource);
    Json(json!({"access_token": token, "client_id": id, "expires_in": "86399", "expires_on": exp.to_string(), "ext_expires_in": "86399", "not_before": (exp - 86_399).to_string(),
        "resource": resource, "token_type": "Bearer"})).into_response()
}

/// K3 · Download .cer (public part) or .pfx (with the private key, no password), as the vault gives them.
async fn download(State(ctx): State<Ctx>, axum::extract::Path(n): axum::extract::Path<String>, Query(q): Query<std::collections::HashMap<String, String>>) -> Response {
    let Ok(v) = config::pick(&ctx, q.get("vault").cloned()) else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(Some(c)) = store::current(&ctx, &v.name, vault::CERT, &n).await else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(der) = vault::cert_der_for(&c, None) else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    let (body, ext, mime) = if q.get("as").map(String::as_str) == Some("pfx") {
        match crypto::pfx(&c.value, &der, "") { Ok(p) => (p, "pfx", "application/x-pkcs12"), Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    } else { (der, "cer", "application/pkix-cert") };
    ([(axum::http::header::CONTENT_TYPE, mime.to_string()), (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"{n}.{ext}\""))], body).into_response()
}

impl Module for KeyVault {
    fn id(&self) -> &'static str {
        "keyvault"
    }
    fn migrations(&self) -> &'static str {
        store::SQL
    }
    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }
    fn extra_routes(&self) -> Router<Ctx> {
        Router::new().route("/metadata/identity/oauth2/token", get(imds)).route("/api/keyvault/certificates/{name}/download", get(download)).route("/api/keyvault/vaults/{name}/yaml", get(|State(ctx): State<Ctx>, axum::extract::Path(n): axum::extract::Path<String>| async move {
            ([(axum::http::header::CONTENT_TYPE, "application/yaml")], config::text(&ctx, &n)).into_response()
        }))
    }
    fn listen(&self, envs: stuntman_core::env::Envs) {
        PORT.store(self.port, Ordering::Relaxed);
        tokio::spawn(tls::serve(self.port, envs));
    }
    fn start(&self, ctx: Ctx) {
        tokio::spawn(async move {
            while !ctx.state.gone() {
                vault::sweep(&ctx).await;
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        });
    }
    fn reset(&self, ctx: &Ctx) {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            for v in config::list(&ctx).into_iter().filter(|v| v.behaviour.seed_on_reset) {
                for (k, val) in config::seed_values(&ctx, &v) {
                    let _ = vault::set_secret(&ctx, &v, &k, &val, "text/plain", json!({}), None, None, true).await;
                }
            }
        });
    }
}
