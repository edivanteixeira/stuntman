//! Stuntman core: what every service module reuses.
//!
//! A service (e-mail today; SMS, WhatsApp, storage… later) is a [`Module`]. It brings its own
//! SQLite tables, its tools (each tool is at the same time an MCP tool and an HTTP route), its
//! background listeners (an SMTP port, a queue port…) and nothing else. The server wires them.

pub mod callback;
pub mod clock;
pub mod env;
pub mod extract;
pub mod force;
pub mod live;
pub mod netserve;
pub mod store;
pub mod tools;
pub mod trips;
pub mod usage;
pub mod util;
pub mod yamlfile;

use std::{path::PathBuf, sync::Arc};

pub use tools::{ApiError, Route, Tool, ToolResult};

/// Everything a module needs at runtime. Cheap to clone.
#[derive(Clone)]
pub struct Ctx {
    /// Environment name, e.g. "stg-a".
    pub env: Arc<str>,
    /// Pause, deletion and environment.yaml of this environment.
    pub state: Arc<env::EnvState>,
    /// All environments of this process (listeners shared by them resolve a host here).
    pub envs: env::Envs,
    pub db: store::Db,
    pub live: live::Live,
    /// The environment's folder: YAML files (the source of truth for configuration) and databases.
    pub data_dir: Arc<PathBuf>,
    pub http: reqwest::Client,
    pub signer: Arc<callback::Signer>,
    /// Host name shown in addresses and copy buttons, e.g. "stg-a.stuntman.local".
    pub host: Arc<str>,
}

impl Ctx {
    pub fn path(&self, rel: &str) -> PathBuf {
        self.data_dir.join(rel)
    }

    /// Path prefix that sends a plain localhost URL to this environment: "" for the default one,
    /// "/e/<name>" for the others (their *.<base_domain> host names work too).
    pub fn prefix(&self) -> String {
        if self.envs.default_name() == *self.env { String::new() } else { format!("/e/{}", self.env) }
    }
}

/// A service category. Add one by implementing this and registering it in the server.
pub trait Module: Send + Sync + 'static {
    /// Short id used in URLs and logs ("email").
    fn id(&self) -> &'static str;
    /// SQL run once at startup (CREATE TABLE IF NOT EXISTS …).
    fn migrations(&self) -> &'static str;
    /// Tools: MCP names plus their HTTP routes.
    fn tools(&self) -> Vec<Tool>;
    /// Routes that are not a plain JSON tool call (raw files, provider intakes such as SendGrid).
    fn extra_routes(&self) -> axum::Router<Ctx> {
        axum::Router::new()
    }
    /// Background work of one environment (its sweeps, its own ports). Called when the
    /// environment opens; loops end when `ctx.state.gone()`.
    fn start(&self, _ctx: Ctx) {}
    /// Ports shared by every environment (a request finds its environment by host or prefix).
    /// Called once.
    fn listen(&self, _envs: env::Envs) {}
    /// The environment was deleted: release what `start` holds.
    fn stop(&self, _ctx: &Ctx) {}
    /// The environment was reset to seeds (its tables emptied): load seeds again.
    fn reset(&self, _ctx: &Ctx) {}
}
