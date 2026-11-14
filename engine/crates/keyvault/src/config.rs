//! A vault's settings (secrets/<vault>.yaml, the source of truth): who can do what (identities with
//! roles and name scopes, or any token), behaviour (throttling, latency, soft delete) and who gets
//! its events. Items and their versions live in the database; the seed file holds starting values.

use stuntman_core::{util, yamlfile, ApiError, Ctx};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Vault {
    pub kind: String,
    pub name: String,
    pub environment: String,
    pub access: Access,
    pub behaviour: Behaviour,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subscribers: Vec<String>,
    pub seed: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Access {
    pub any_token: bool,
    pub identities: Vec<Identity>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Identity {
    /// The managed identity's name (the token's oid, name or appid).
    pub id: String,
    pub roles: Vec<String>,
    /// Item name patterns (smtp-*); empty = the whole vault.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Behaviour {
    /// "2000/10s": more operations than that in the window get 429.
    pub throttle: String,
    pub latency: String,
    pub soft_delete_days: u32,
    pub seed_on_reset: bool,
}

impl Default for Vault {
    fn default() -> Self {
        Self { kind: "azure-key-vault".into(), name: String::new(), environment: String::new(), access: Access::default(), behaviour: Behaviour::default(), subscribers: vec![], seed: "seed/secrets.yaml".into() }
    }
}
impl Default for Access {
    fn default() -> Self {
        Self { any_token: true, identities: vec![] }
    }
}
impl Default for Behaviour {
    fn default() -> Self {
        Self { throttle: "2000/10s".into(), latency: "none".into(), soft_delete_days: 90, seed_on_reset: true }
    }
}

/// Azure's built-in roles, by the YAML name.
pub const ROLES: [(&str, &str); 7] = [
    ("administrator", "Key Vault Administrator"),
    ("secrets-user", "Key Vault Secrets User"),
    ("secrets-officer", "Key Vault Secrets Officer"),
    ("crypto-user", "Key Vault Crypto User"),
    ("crypto-officer", "Key Vault Crypto Officer"),
    ("certificate-user", "Key Vault Certificate User"),
    ("certificates-officer", "Key Vault Certificates Officer"),
];

pub fn role_label(r: &str) -> String {
    ROLES.iter().find(|(k, _)| *k == r).map(|(_, l)| l.to_string()).unwrap_or(r.to_string())
}

impl Behaviour {
    /// (max operations, window ms)
    pub fn throttle(&self) -> Option<(i64, i64)> {
        let (n, w) = self.throttle.split_once('/')?;
        Some((n.trim().replace([',', '_'], "").parse().ok()?, util::parse_ms(w.trim())? as i64))
    }
    pub fn latency_ms(&self) -> u64 {
        if self.latency == "none" { 0 } else { util::parse_ms(&self.latency).unwrap_or(0) }
    }
}

/// Item name matches a scope pattern ("smtp-*", or the exact name).
pub fn in_scope(scope: &[String], name: &str) -> bool {
    scope.is_empty() || scope.iter().any(|p| match p.strip_suffix('*') { Some(pre) => name.starts_with(pre), None => p == name })
}

fn dir(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("secrets")
}

pub fn valid_name(n: &str) -> Result<(), ApiError> {
    let ok = n.len() >= 3 && n.len() <= 24 && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && n.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false);
    if ok { Ok(()) } else { Err(ApiError::bad("Vault name: 3 to 24 lowercase letters, digits or dashes, starting with a letter")) }
}

pub fn item_name_ok(n: &str) -> Result<(), ApiError> {
    if !n.is_empty() && n.len() <= 127 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') { Ok(()) } else { Err(ApiError::bad("Names are 1 to 127 letters, digits or dashes")) }
}

pub fn list(ctx: &Ctx) -> Vec<Vault> {
    let mut v: Vec<Vault> = std::fs::read_dir(dir(ctx)).map(|r| r.filter_map(|e| e.ok()).filter(|e| e.path().extension().map(|x| x == "yaml").unwrap_or(false))
        .filter_map(|e| yamlfile::load::<Vault>(&e.path()).ok()).filter(|v| !v.name.is_empty()).collect()).unwrap_or_default();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

pub fn get(ctx: &Ctx, name: &str) -> Result<Vault, ApiError> {
    let p = dir(ctx).join(format!("{name}.yaml"));
    if !p.exists() {
        return Err(ApiError::not_found(format!("No vault {name} in {}", ctx.env)));
    }
    yamlfile::load(&p)
}

/// The vault named, or the only/first one.
pub fn pick(ctx: &Ctx, name: Option<String>) -> Result<Vault, ApiError> {
    match name.filter(|n| !n.is_empty()) {
        Some(n) => get(ctx, &n),
        None => list(ctx).into_iter().next().ok_or_else(|| ApiError::not_found(format!("No vault in {} yet: create one (set_vault)", ctx.env))),
    }
}

pub fn save(ctx: &Ctx, v: &Vault) -> Result<(), ApiError> {
    yamlfile::save(&dir(ctx).join(format!("{}.yaml", v.name)), v)
}

pub fn text(ctx: &Ctx, name: &str) -> String {
    std::fs::read_to_string(dir(ctx).join(format!("{name}.yaml"))).unwrap_or_default()
}

pub fn seed_values(ctx: &Ctx, v: &Vault) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(ctx.path(&v.seed)).ok().and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default()
}

/// Host name of a vault: <vault>.vault.<environment base domain>.
pub fn host(ctx: &Ctx, vault: &str) -> String {
    let base = ctx.state.file.read().unwrap().base_domain.clone();
    format!("{vault}.vault.{base}")
}

#[cfg(test)]
mod t {
    #[test]
    fn scope_and_throttle() {
        assert!(super::in_scope(&["smtp-*".into(), "zenvia-*".into()], "smtp-password"));
        assert!(!super::in_scope(&["smtp-*".into()], "db-connection"));
        assert!(super::in_scope(&[], "anything"));
        assert_eq!(super::Behaviour::default().throttle(), Some((2000, 10_000)));
    }
}
