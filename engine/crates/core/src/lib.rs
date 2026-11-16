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

/// The main port (screens, API, MCP, sign-in pages).
pub fn main_port() -> u16 {
    std::env::var("STUNTMAN_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8025)
}

/// STUNTMAN_PUBLIC_URL: one public address for a shared Stuntman (https://stuntman.example.com).
/// Its root lists the environments and each one lives under /e/<name>/.
pub fn shared_public_url() -> Option<String> {
    std::env::var("STUNTMAN_PUBLIC_URL").ok().map(|u| u.trim().trim_end_matches('/').to_string()).filter(|u| !u.is_empty())
}

tokio::task_local! {
    /// The address the current request came by on the main port: scheme://host[/e/<name>]
    /// (X-Forwarded-Proto and X-Forwarded-Host first, as a proxy sends them).
    pub static REQUEST_BASE: String;
}

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

    /// Where a browser reaches this environment's main port: its public_url, else the server's
    /// STUNTMAN_PUBLIC_URL + /e/<name>, else the address the current request came by, else
    /// http://localhost:<port><prefix>.
    pub fn public_base(&self) -> String {
        let u = self.state.file.read().unwrap().public_url.trim_end_matches('/').to_string();
        if !u.is_empty() {
            return u;
        }
        if let Some(p) = shared_public_url() {
            return format!("{p}/e/{}", self.env);
        }
        REQUEST_BASE.try_with(|b| b.clone()).unwrap_or_else(|_| format!("http://localhost:{}{}", main_port(), self.prefix()))
    }

    /// http://<app host>:<port><prefix>: where the app under test reaches a port of this environment.
    pub fn app_url(&self, port: u16) -> String {
        let (h, p) = self.app_host();
        format!("http://{h}:{port}{p}")
    }

    /// Host and path prefix an app uses for a port of this environment: its own base domain
    /// (no prefix: the host names it), else the host shown everywhere (localhost) and /e/<name>.
    pub fn app_host(&self) -> (String, String) {
        match self.state.own_domain() {
            Some(d) => (d, String::new()),
            None => (self.host.split(':').next().unwrap_or("localhost").to_string(), self.prefix()),
        }
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
