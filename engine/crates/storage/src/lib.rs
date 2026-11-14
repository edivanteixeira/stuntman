//! Storage (S1–S5): Azure Blob and S3 (what Cloudflare R2 speaks) on their own ports, one
//! vocabulary for both — account, container (bucket), path. Objects with versions and history,
//! links (SAS / presigned) checked like the cloud, events (Event Grid / S3 notifications) with the
//! provider's retries, rules that provoke failures, account settings as YAML, seeds.

pub mod auth;
pub mod azure;
pub mod config;
pub mod events;
pub mod gate;
pub mod http;
pub mod links;
pub mod ops;
pub mod rules;
pub mod s3;
pub mod store;
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
use stuntman_core::{ApiError, Ctx, Module, Tool};

static BLOB_PORT: AtomicU16 = AtomicU16::new(10000);
static S3_PORT: AtomicU16 = AtomicU16::new(9000);
pub static BLOB_BIND_ERROR: Mutex<Option<String>> = Mutex::new(None);
pub static S3_BIND_ERROR: Mutex<Option<String>> = Mutex::new(None);

pub fn blob_port() -> u16 {
    BLOB_PORT.load(Ordering::Relaxed)
}

pub fn s3_port() -> u16 {
    S3_PORT.load(Ordering::Relaxed)
}

pub struct Storage {
    pub blob_port: u16,
    pub s3_port: u16,
}

impl Module for Storage {
    fn id(&self) -> &'static str {
        "storage"
    }

    fn migrations(&self) -> &'static str {
        store::SQL
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn extra_routes(&self) -> Router<Ctx> {
        Router::new()
            .route("/api/storage/{account}/{container}/content/{*path}", get(content))
            .route("/api/services/{name}/accounts/{account}/yaml", get(|State(ctx): State<Ctx>, Path((_, a)): Path<(String, String)>| async move {
                ([(header::CONTENT_TYPE, "application/yaml")], config::text(&ctx, &a).0).into_response()
            }))
            .route("/api/services/{name}/rules.yaml", get(|State(ctx): State<Ctx>, Path(s): Path<String>| async move {
                ([(header::CONTENT_TYPE, "application/yaml")], std::fs::read_to_string(rules::file_path(&ctx, &s)).unwrap_or_default()).into_response()
            }))
    }

    fn listen(&self, envs: stuntman_core::env::Envs) {
        BLOB_PORT.store(self.blob_port, Ordering::Relaxed);
        S3_PORT.store(self.s3_port, Ordering::Relaxed);
        let a = Router::new().fallback(azure::handle).with_state(envs.clone());
        let s = Router::new().fallback(s3::handle).with_state(envs);
        tokio::spawn(http::serve(self.blob_port, a, "Azure Blob", &BLOB_BIND_ERROR));
        tokio::spawn(http::serve(self.s3_port, s, "S3", &S3_BIND_ERROR));
    }

    fn start(&self, ctx: Ctx) {
        tokio::spawn(async move {
            // First start of an environment: accounts with a seed folder get their files.
            for a in config::list(&ctx) {
                if a.behaviour.seed_on_reset && ctx.path(&a.seed).is_dir() && store::seed_info(&ctx, &a.account).await.ok().flatten().is_none() {
                    if let Err(e) = tools::load_seed(&ctx, &a).await {
                        eprintln!("storage seed {}: {}", a.account, e.message);
                    }
                }
            }
            let mut n = 0u64;
            while !ctx.state.gone() {
                events::tick(&ctx).await;
                n += 1;
                if n % 300 == 0 {
                    for a in config::list(&ctx) {
                        let soft = (a.behaviour.soft_delete != "off").then(|| stuntman_core::util::parse_ms(&a.behaviour.soft_delete)).flatten();
                        let _ = store::sweep(&ctx, &a.account, soft.or(Some(0))).await;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
    }

    /// The environment was reset to seeds: accounts load their seed folder again.
    fn reset(&self, ctx: &Ctx) {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            for a in config::list(&ctx).into_iter().filter(|a| a.behaviour.seed_on_reset) {
                let _ = tools::load_seed(&ctx, &a).await;
            }
        });
    }
}

/// The bytes of an object for the screen (preview, Download); not a provider request.
async fn content(State(ctx): State<Ctx>, Path((account, container, path)): Path<(String, String, String)>) -> Result<impl IntoResponse, ApiError> {
    let b = store::get(&ctx, &account, &container, &path, true).await?.ok_or_else(|| ApiError::not_found("No such object"))?;
    let name = path.rsplit('/').next().unwrap_or("file").replace('"', "");
    Ok(([(header::CONTENT_TYPE, b.ctype.clone()), (header::CONTENT_DISPOSITION, format!("inline; filename=\"{name}\""))], b.data))
}
