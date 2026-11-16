//! Environments (N1–N3): each one is a folder under data/environments/<name>/ with its own
//! services, state and URLs. One process serves all of them; a request finds its environment by
//! host name (*.<base_domain>), by the /e/<name>/ path prefix, or falls to the default one.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
};

use serde::{Deserialize, Serialize};

use crate::Ctx;

/// environments/<name>/environment.yaml
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct EnvFile {
    pub name: String,
    pub base_domain: String,
    /// Real provider host names Stuntman answers for (with the test CA), e.g. gateway.apiserpro.gov.br.
    pub real_hosts: Vec<String>,
    /// The address browsers use for this environment's main port, e.g. https://stuntman-a.example.com
    /// or https://stuntman.example.com/e/a. Identity's issuer and discovery use it whatever address
    /// a call came by, so a token made through the browser is valid for the app too.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub public_url: String,
    /// Real e-mail domains identity accepts for people of this environment (synthetic only by default).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_email_domains: Vec<String>,
    pub callbacks: Callbacks,
    /// kept | expires | pr_closed
    pub lifetime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    pub paused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused_since: Option<i64>,
    pub created_at: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub created_by: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cloned_from: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Callbacks {
    /// Where services send webhooks when a service has no URL of its own.
    pub default: String,
}

impl EnvFile {
    pub fn new(name: &str) -> Self {
        EnvFile { name: name.into(), base_domain: format!("{name}.stuntman.local"), lifetime: "kept".into(), created_at: crate::util::now_ms(), ..Default::default() }
    }
}

/// What a running environment shares with its listeners: pause, deletion, the file's values.
pub struct EnvState {
    pub paused: AtomicBool,
    /// Set when the environment is deleted: loops and listeners of it stop.
    pub gone: AtomicBool,
    pub file: RwLock<EnvFile>,
}

impl EnvState {
    pub fn new(f: EnvFile) -> Self {
        EnvState { paused: AtomicBool::new(f.paused), gone: AtomicBool::new(false), file: RwLock::new(f) }
    }
    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
    pub fn gone(&self) -> bool {
        self.gone.load(Ordering::Relaxed)
    }
    pub fn callback_default(&self) -> String {
        self.file.read().unwrap().callbacks.default.clone()
    }
    /// A base domain someone chose (a DNS zone, a .localhost name), not the automatic
    /// <name>.stuntman.local that needs /etc/hosts lines: apps reach this one by name.
    pub fn own_domain(&self) -> Option<String> {
        let b = self.file.read().unwrap().base_domain.clone();
        (!b.is_empty() && !b.ends_with(SUFFIX)).then_some(b)
    }
}

/// Every running environment, by name.
#[derive(Clone, Default)]
pub struct Envs(Arc<RwLock<Reg>>);

#[derive(Default)]
struct Reg {
    map: BTreeMap<String, Ctx>,
    default: String,
}

pub const SUFFIX: &str = ".stuntman.local";

impl Envs {
    pub fn insert(&self, ctx: Ctx) {
        let mut r = self.0.write().unwrap();
        if r.default.is_empty() {
            r.default = ctx.env.to_string();
        }
        r.map.insert(ctx.env.to_string(), ctx);
    }
    pub fn remove(&self, name: &str) -> Option<Ctx> {
        let mut r = self.0.write().unwrap();
        let c = r.map.remove(name);
        if r.default == name {
            r.default = r.map.keys().next().cloned().unwrap_or_default();
        }
        c
    }
    pub fn set_default(&self, name: &str) {
        self.0.write().unwrap().default = name.into();
    }
    pub fn default_name(&self) -> String {
        self.0.read().unwrap().default.clone()
    }
    pub fn get(&self, name: &str) -> Option<Ctx> {
        self.0.read().unwrap().map.get(&name.to_lowercase()).cloned()
    }
    pub fn default_env(&self) -> Option<Ctx> {
        let r = self.0.read().unwrap();
        r.map.get(&r.default).cloned()
    }
    pub fn all(&self) -> Vec<Ctx> {
        self.0.read().unwrap().map.values().cloned().collect()
    }

    /// The environment a host name belongs to: <base_domain>, anything under it, or one of its
    /// real host names. A *.stuntman.local name of no environment finds none; other names (localhost,
    /// an IP) go to the default environment.
    pub fn by_host(&self, host: Option<&str>) -> Option<Ctx> {
        let h = host.unwrap_or("").rsplit_once(':').filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit())).map(|(h, _)| h).unwrap_or(host.unwrap_or("")).trim_end_matches('.').to_lowercase();
        if h.is_empty() {
            return self.default_env();
        }
        let r = self.0.read().unwrap();
        for c in r.map.values() {
            let f = c.state.file.read().unwrap();
            let base = f.base_domain.to_lowercase();
            if h == base || h.ends_with(&format!(".{base}")) || f.real_hosts.iter().any(|x| x.eq_ignore_ascii_case(&h)) {
                return Some(c.clone());
            }
        }
        // <name>.localhost and anything under it: resolves to loopback with no hosts entry.
        if let Some(n) = h.strip_suffix(".localhost").map(|r| r.rsplit('.').next().unwrap_or(r)) {
            if let Some(c) = r.map.get(n) {
                return Some(c.clone());
            }
        }
        if h.ends_with(SUFFIX) {
            return None;
        }
        r.map.get(&r.default).cloned()
    }
}

/// lowercase letters, numbers and dashes; starts with a letter or number.
pub fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 40 && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !n.starts_with('-') && !n.ends_with('-')
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name("pr-5462"));
        assert!(!valid_name("PR-1"));
        assert!(!valid_name("-a"));
        assert!(!valid_name("a b"));
    }
}
