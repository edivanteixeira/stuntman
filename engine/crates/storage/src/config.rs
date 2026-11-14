//! S5 · Accounts: one YAML file per account under storage/accounts/<name>.yaml (Azure Blob or S3).
//! The form, the API and MCP read and write the same file. Key values live in secrets.yaml.

use std::{collections::BTreeMap, path::PathBuf};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use stuntman_core::{util, ApiError, Ctx};
use rand::RngCore;
use serde::{Deserialize, Serialize};

pub const AZURE: &str = "azure-blob";
pub const S3: &str = "s3";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Access {
    /// Any key or signature is accepted (expiry and permissions of links still count).
    #[serde(default = "yes")]
    pub any_key: bool,
    /// Key names; values are secrets named storage.<account>.<key>. For S3 the name is the access key id.
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub managed_identity: bool,
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Container {
    /// Azure: private | blob (public read of blobs) | container (public list). S3: private | public-read.
    #[serde(default = "private")]
    pub access: String,
}
fn private() -> String {
    "private".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Behaviour {
    /// none | same_region | other_region | slow_network | <n>ms
    #[serde(default = "none")]
    pub latency: String,
    /// none | 100MB | 1GB | full (every upload 507)
    #[serde(default = "none")]
    pub limit: String,
    #[serde(default)]
    pub versions: bool,
    /// off | 7d…
    #[serde(default = "off")]
    pub soft_delete: String,
    /// Load the seed folder when the account (or the environment) is reset.
    #[serde(default = "yes")]
    pub seed_on_reset: bool,
}
fn none() -> String {
    "none".into()
}
fn off() -> String {
    "off".into()
}

impl Default for Behaviour {
    fn default() -> Self {
        Self { latency: none(), limit: none(), versions: false, soft_delete: off(), seed_on_reset: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Account {
    /// azure-blob | s3
    pub kind: String,
    pub account: String,
    pub access: Access,
    #[serde(default)]
    pub containers: BTreeMap<String, Container>,
    #[serde(default)]
    pub behaviour: Behaviour,
    /// Folder (relative to the environment) whose files are loaded on reset: <seed>/<container>/<path>.
    #[serde(default)]
    pub seed: String,
}

impl Account {
    pub fn new(kind: &str, name: &str) -> Self {
        Account {
            kind: kind.into(),
            account: name.into(),
            access: Access { any_key: true, keys: vec![], managed_identity: false },
            containers: BTreeMap::new(),
            behaviour: Behaviour::default(),
            seed: format!("seed/storage/{name}"),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.kind != AZURE && self.kind != S3 {
            return Err(format!("kind: {AZURE} or {S3}"));
        }
        valid_account(&self.account)?;
        for (n, c) in &self.containers {
            valid_container(n)?;
            let ok: &[&str] = if self.kind == AZURE { &["private", "blob", "container"] } else { &["private", "public-read"] };
            if !ok.contains(&c.access.as_str()) {
                return Err(format!("containers.{n}.access: {}", ok.join(" | ")));
            }
        }
        latency_ms(&self.behaviour.latency).ok_or("behaviour.latency: none, same_region, other_region, slow_network or a number of ms")?;
        if self.limit_bytes().is_none() {
            return Err("behaviour.limit: none, full, or a size like 100MB".into());
        }
        if self.behaviour.soft_delete != "off" && util::parse_ms(&self.behaviour.soft_delete).is_none() {
            return Err("behaviour.soft_delete: off or a duration like 7d".into());
        }
        Ok(())
    }

    /// None: not a size. Some(u64::MAX): no limit. Some(0): full now.
    pub fn limit_bytes(&self) -> Option<u64> {
        let s = self.behaviour.limit.trim().to_uppercase();
        match s.as_str() {
            "NONE" | "" => return Some(u64::MAX),
            "FULL" => return Some(0),
            _ => {}
        }
        let n: f64 = s.trim_end_matches(|c: char| c.is_alphabetic() || c == ' ').trim().parse().ok()?;
        let f: u64 = if s.ends_with("GB") { 1 << 30 } else if s.ends_with("MB") { 1 << 20 } else if s.ends_with("KB") { 1 << 10 } else { 1 };
        Some((n * f as f64) as u64)
    }
}

/// (ms, jitter ms) added to every answer.
pub fn latency_ms(l: &str) -> Option<(u64, u64)> {
    Some(match l.trim() {
        "none" | "" => (0, 0),
        "same_region" => (20, 5),
        "other_region" => (120, 20),
        "slow_network" => (300, 200),
        x => (util::parse_ms(x)?, 0),
    })
}

/// 3 to 24 lowercase letters, numbers or dashes (Azure itself allows no dashes; names here are
/// also URL paths, and staging names like acme-docs-stg are common).
pub fn valid_account(n: &str) -> Result<(), String> {
    let ok = (3..=24).contains(&n.len()) && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !n.starts_with('-') && !n.ends_with('-');
    if ok { Ok(()) } else { Err("Account name: 3 to 24 lowercase letters, numbers or dashes, like acme-docs-stg".into()) }
}

pub fn valid_container(n: &str) -> Result<(), String> {
    let ok = (3..=63).contains(&n.len()) && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.') && !n.starts_with('-');
    if ok { Ok(()) } else { Err(format!("Container “{n}”: 3 to 63 lowercase letters, numbers and dashes")) }
}

fn dir(ctx: &Ctx) -> PathBuf {
    ctx.path("storage/accounts")
}

pub fn file(ctx: &Ctx, name: &str) -> PathBuf {
    dir(ctx).join(format!("{name}.yaml"))
}

pub fn rel_file(name: &str) -> String {
    format!("storage/accounts/{name}.yaml")
}

pub fn list(ctx: &Ctx) -> Vec<Account> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir(ctx)).into_iter().flatten().flatten() {
        if e.path().extension().map(|x| x == "yaml").unwrap_or(false) {
            if let Some(a) = std::fs::read_to_string(e.path()).ok().and_then(|t| serde_yaml::from_str::<Account>(&t).ok()) {
                out.push(a);
            }
        }
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.account.cmp(&b.account)));
    out
}

pub fn get(ctx: &Ctx, name: &str) -> Result<Account, ApiError> {
    let t = std::fs::read_to_string(file(ctx, name)).map_err(|_| ApiError::not_found(format!("No storage account “{name}”")))?;
    serde_yaml::from_str(&t).map_err(|e| ApiError::bad(format!("{}: {e}", rel_file(name))))
}

/// The file text and its etag (for If-Match).
pub fn text(ctx: &Ctx, name: &str) -> (String, String) {
    let t = std::fs::read_to_string(file(ctx, name)).unwrap_or_default();
    let e = etag(&t);
    (t, e)
}

pub fn etag(t: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    format!("\"{:016x}\"", h.finish())
}

/// The account that owns an S3 bucket name.
pub fn by_bucket(ctx: &Ctx, bucket: &str) -> Option<Account> {
    list(ctx).into_iter().find(|a| a.kind == S3 && a.containers.contains_key(bucket))
}

pub fn save(ctx: &Ctx, a: &Account) -> Result<(), ApiError> {
    a.validate().map_err(ApiError::bad)?;
    let t = serde_yaml::to_string(a).map_err(|e| ApiError::internal(e.to_string()))?;
    std::fs::create_dir_all(dir(ctx)).map_err(|e| ApiError::internal(e.to_string()))?;
    std::fs::write(file(ctx, &a.account), t).map_err(|e| ApiError::internal(e.to_string()))?;
    ctx.live.send("storage.account", &a.account, "updated");
    Ok(())
}

pub fn secret_name(account: &str, key: &str) -> String {
    format!("storage.{account}.{key}")
}

/// The value of a key (Azure: base64 account key; S3: secret access key).
pub fn key_value(ctx: &Ctx, account: &str, key: &str) -> String {
    util::resolve_secret(&format!("secret://{}", secret_name(account, key)), &ctx.data_dir)
}

/// Makes a fresh key value and stores it; returns the value (shown once, then masked).
pub fn new_key(ctx: &Ctx, a: &Account, key: &str) -> Result<String, ApiError> {
    let v = if a.kind == AZURE {
        let mut b = [0u8; 64];
        rand::thread_rng().fill_bytes(&mut b);
        B64.encode(b)
    } else {
        let mut b = [0u8; 30];
        rand::thread_rng().fill_bytes(&mut b);
        B64.encode(b).replace(['+', '/'], "x")
    };
    util::set_secret(&ctx.data_dir, &secret_name(&a.account, key), &v).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(v)
}

/// "Eby8vdM0…k3Ag=="
pub fn mask(v: &str) -> String {
    if v.len() < 12 { return "…".into(); }
    format!("{}…{}", &v[..8], &v[v.len() - 6..])
}

/// A new account with its keys made: Azure key1/key2; S3 one access key id.
pub fn create(ctx: &Ctx, kind: &str, name: &str, containers: &[(String, String)]) -> Result<Account, ApiError> {
    if file(ctx, name).exists() {
        return Err(ApiError::new(409, format!("{name} already exists")));
    }
    let mut a = Account::new(kind, name);
    a.access.keys = if kind == AZURE { vec!["key1".into(), "key2".into()] } else { vec![format!("STUNTMAN{}", name.to_uppercase().replace('-', "").chars().take(12).collect::<String>())] };
    for (c, acc) in containers {
        a.containers.insert(c.clone(), Container { access: if acc.is_empty() { "private".into() } else { acc.clone() } });
    }
    a.validate().map_err(ApiError::bad)?;
    for k in a.access.keys.clone() {
        new_key(ctx, &a, &k)?;
    }
    save(ctx, &a)?;
    Ok(a)
}

/// What the schema-drawn form shows (get_service_schema).
pub fn schema(kind: &str) -> serde_json::Value {
    let containers = if kind == AZURE { vec!["private", "blob", "container"] } else { vec!["private", "public-read"] };
    serde_json::json!({
        "service": kind,
        "fields": [
            {"key": "account", "type": "text", "label": "Account name", "help": "used in URLs and keys", "rule": "3 to 24 lowercase letters, numbers or dashes"},
            {"key": "access.any_key", "type": "bool", "label": "Accept any key", "help": "off: only the keys below; others get 403"},
            {"key": "access.keys", "type": "keys", "label": "Keys", "help": if kind == AZURE { "values in secrets.yaml; Rotate makes a new one" } else { "access key ids; secrets in secrets.yaml" }},
            {"key": "access.managed_identity", "type": "bool", "label": "Accept managed identity tokens", "help": "from Logins", "only": AZURE},
            {"key": "containers", "type": "map", "label": if kind == AZURE { "Containers" } else { "Buckets" }, "values": containers},
            {"key": "behaviour.latency", "type": "select", "label": "Latency", "help": "added to every answer",
             "options": [["none", "None"], ["same_region", "Same region · about 20 ms"], ["other_region", "Another region · about 120 ms"], ["slow_network", "Slow network · 300 ms ± 200"]]},
            {"key": "behaviour.limit", "type": "select", "label": "Storage limit", "help": "over it, uploads get 507",
             "options": [["none", "No limit"], ["100MB", "100 MB"], ["1GB", "1 GB"], ["full", "Full now · every upload 507"]]},
            {"key": "behaviour.versions", "type": "bool", "label": "Keep versions"},
            {"key": "behaviour.soft_delete", "type": "text", "label": "Soft delete", "help": "off or a duration like 7d"},
            {"key": "behaviour.seed_on_reset", "type": "bool", "label": "Load the seed folder on reset"},
        ]
    })
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn validation_and_limits() {
        let mut a = Account::new(AZURE, "acme-docs-stg");
        a.containers.insert("documents".into(), Container { access: "private".into() });
        assert!(a.validate().is_ok());
        assert_eq!(a.limit_bytes(), Some(u64::MAX));
        a.behaviour.limit = "100MB".into();
        assert_eq!(a.limit_bytes(), Some(100 << 20));
        a.behaviour.limit = "full".into();
        assert_eq!(a.limit_bytes(), Some(0));
        a.containers.insert("x".into(), Container { access: "private".into() });
        assert!(a.validate().is_err());
        assert!(valid_account("Acme_Docs").is_err());
        assert_eq!(latency_ms("slow_network"), Some((300, 200)));
    }
}
