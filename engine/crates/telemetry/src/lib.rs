//! Telemetry: an OTLP receiver that stands in for the APM in staging (T1 traces, T2 logs, T3
//! exporter settings, T4 metrics). It joins the app's spans to the trips that reached Stuntman, so
//! a trace shows where the app waited on a provider.

pub mod config;
pub mod metrics;
pub mod query;
pub mod receiver;
pub mod store;
pub mod tools;

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use axum::{extract::State, http::header, response::IntoResponse, routing::get, Router};
use stuntman_core::{store::Db, Ctx, Module, Tool};

static DBS: OnceLock<Mutex<HashMap<String, Db>>> = OnceLock::new();
static PORTS: OnceLock<(u16, u16)> = OnceLock::new();

fn dbs() -> &'static Mutex<HashMap<String, Db>> {
    DBS.get_or_init(Default::default)
}

/// Telemetry's own SQLite file per environment: high write volume never waits behind the other
/// services.
pub fn db(ctx: &Ctx) -> Db {
    let mut m = dbs().lock().unwrap();
    m.entry(ctx.env.to_string())
        .or_insert_with(|| {
            let db = Db::open(&ctx.path("telemetry.sqlite")).expect("telemetry.sqlite");
            db.migrate(store::SQL).expect("telemetry migrations");
            db
        })
        .clone()
}

/// (OTLP/HTTP, OTLP/gRPC) defaults for a new settings file.
pub fn ports() -> (u16, u16) {
    *PORTS.get().unwrap_or(&(4318, 4317))
}

pub struct Telemetry {
    pub http_port: u16,
    pub grpc_port: u16,
}

impl Module for Telemetry {
    fn id(&self) -> &'static str {
        "telemetry"
    }

    fn migrations(&self) -> &'static str {
        ""
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn extra_routes(&self) -> Router<Ctx> {
        Router::new().route("/api/telemetry/settings.yaml", get(|State(ctx): State<Ctx>| async move {
            ([(header::CONTENT_TYPE, "application/yaml")], std::fs::read_to_string(config::file(&ctx)).unwrap_or_default()).into_response()
        }))
    }

    fn start(&self, ctx: Ctx) {
        let _ = PORTS.set((self.http_port, self.grpc_port));
        db(&ctx);
        if let Err(e) = config::get(&ctx) {
            eprintln!("telemetry settings: {}", e.message);
        }
        tokio::spawn(async move {
            while !ctx.state.gone() {
                if let Ok(s) = config::get(&ctx) {
                    let _ = store::sweep(&ctx, &s).await;
                }
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            }
        });
    }

    /// The OTLP ports are shared: the default environment's settings own them (and "refused"
    /// there takes them down); each batch goes to the environment of its host or /e/<name> prefix.
    fn listen(&self, envs: stuntman_core::env::Envs) {
        let _ = PORTS.set((self.http_port, self.grpc_port));
        tokio::spawn(async move {
            loop {
                // Also picks up provoke expiry and edits made to the YAML file by hand.
                receiver::reconcile(&envs).await;
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            }
        });
    }

    fn stop(&self, ctx: &Ctx) {
        dbs().lock().unwrap().remove(&*ctx.env);
    }
}
