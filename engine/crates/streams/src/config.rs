//! streams/config.yaml: the broker's settings (partitions of new topics, the host it advertises,
//! SASL) and the failure provoked on it.

use serde::{Deserialize, Serialize};
use stuntman_core::{yamlfile, ApiError, Ctx};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub default_partitions: i32,
    /// Topics are made when a client asks for one that does not exist.
    pub auto_create: bool,
    /// The host name clients connect to after Metadata (localhost, or the container's name).
    pub advertised_host: String,
    pub sasl: Sasl,
    /// "" | leader_not_available | sasl_refused | slow_fetch | duplicate
    #[serde(skip_serializing_if = "String::is_empty")]
    pub provoke: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Sasl {
    /// Every request needs a SASL login first.
    pub required: bool,
    pub mechanisms: Vec<String>,
    /// Empty: any user, any password except "wrong".
    pub users: Vec<User>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct User {
    pub user: String,
    pub password: String,
}

impl Default for Config {
    fn default() -> Self {
        Self { default_partitions: 1, auto_create: true, advertised_host: std::env::var("STUNTMAN_KAFKA_ADVERTISED_HOST").unwrap_or("localhost".into()), sasl: Sasl::default(), provoke: String::new() }
    }
}

impl Default for Sasl {
    fn default() -> Self {
        Self { required: false, mechanisms: vec!["PLAIN".into(), "SCRAM-SHA-256".into(), "SCRAM-SHA-512".into()], users: vec![] }
    }
}

pub const PROVOKE: [(&str, &str); 5] = [("registry_down", "the Schema Registry answers 503"), ("leader_not_available", "Metadata, Produce and Fetch answer LEADER_NOT_AVAILABLE"), ("sasl_refused", "SASL logins are refused"),
    ("slow_fetch", "Fetch waits 3 s"), ("duplicate", "each produced record is kept twice")];

pub fn path(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("streams/config.yaml")
}

pub fn load(ctx: &Ctx) -> Config {
    yamlfile::load(&path(ctx)).unwrap_or_default()
}

pub fn save(ctx: &Ctx, c: &Config) -> Result<(), ApiError> {
    yamlfile::save(&path(ctx), c)
}
