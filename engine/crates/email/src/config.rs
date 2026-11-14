//! E-mail service configuration: one YAML file per service (E3) and one extractors file (E4).
//! The files are the source of truth; the screen, the API and MCP all edit them.

use std::path::PathBuf;

use stuntman_core::{extract, yamlfile, ApiError, Ctx};
use serde::{Deserialize, Serialize};

pub const PLUGIN: &str = "email@1.0";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    #[serde(default = "kind")]
    pub kind: String,
    #[serde(default = "plugin")]
    pub plugin: String,
    #[serde(default)]
    pub env: String,
    pub name: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub intake: Intake,
    #[serde(default)]
    pub addresses: Addresses,
    #[serde(default)]
    pub events: Events,
    #[serde(default)]
    pub limits: Limits,
}
fn kind() -> String {
    "Service".into()
}
fn plugin() -> String {
    PLUGIN.into()
}

/// Who can send, per intake. mode: any | validate | refuse. `how` says how a refusal looks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Access {
    pub on: bool,
    #[serde(default = "mode_any")]
    pub mode: String,
    /// timeout | refused | tls | 503 | 429 (SMTP answers 421 for 503/429)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub how: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<Key>,
    /// Who set the current mode, for "set by claude-code 3 min ago".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_at: Option<i64>,
}
fn mode_any() -> String {
    "any".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Key {
    pub id: String,
    pub label: String,
    /// sha256 of the secret, hex. The secret is shown once.
    pub sha256: String,
    /// Last 4 characters, for "SG.stga.••••3f2a".
    pub hint: String,
    pub created: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Smtp {
    #[serde(flatten)]
    pub access: Access,
    pub port: u16,
    /// Offer STARTTLS in the EHLO answer.
    #[serde(default = "yes")]
    pub starttls: bool,
    /// none | 550 | 451: refuse RCPT TO, so the app's send call itself fails.
    #[serde(default = "none_str")]
    pub refuse_recipients: String,
}
fn yes() -> bool {
    true
}
fn none_str() -> String {
    "none".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Intake {
    pub sendgrid_api: Access,
    pub smtp: Smtp,
}

impl Default for Intake {
    fn default() -> Self {
        let a = Access { on: true, mode: "any".into(), how: None, keys: vec![], set_by: None, set_at: None };
        Intake { sendgrid_api: a.clone(), smtp: Smtp { access: a, port: 2525, starttls: true, refuse_recipients: "none".into() } }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Addresses {
    /// Domain for temporary inboxes: any address there is accepted even before it exists.
    pub domain: String,
    pub temporary_ttl: String,
    pub catch_all: bool,
}

impl Default for Addresses {
    fn default() -> Self {
        Self { domain: "inbox.stuntman.dev".into(), temporary_ttl: "30m".into(), catch_all: true }
    }
}

pub const EVENT_TYPES: [&str; 9] = ["processed", "delivered", "deferred", "bounce", "dropped", "open", "click", "spamreport", "unsubscribe"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Events {
    /// Where the provider's event webhook goes. Empty: the environment's default callback target
    /// (N3); both empty: no events are sent.
    #[serde(default)]
    pub to: String,
    #[serde(default = "sendgrid")]
    pub format: String,
    pub types: Vec<String>,
    /// ecdsa | none
    #[serde(default = "ecdsa")]
    pub sign: String,
    /// Steps for messages that land outside an inbox with its own steps.
    pub default_steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_test: Option<serde_json::Value>,
}
fn sendgrid() -> String {
    "sendgrid".into()
}
fn ecdsa() -> String {
    "ecdsa".into()
}

impl Default for Events {
    fn default() -> Self {
        Self {
            to: String::new(),
            format: sendgrid(),
            types: EVENT_TYPES[..8].iter().map(|s| s.to_string()).collect(),
            sign: ecdsa(),
            default_steps: vec!["delivered +1s".into()],
            last_test: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    /// "25MB"
    pub message_size: String,
    /// "600/min"
    pub rate: String,
    /// "7d"
    pub keep: String,
    pub max: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { message_size: "25MB".into(), rate: "600/min".into(), keep: "7d".into(), max: 10_000 }
    }
}

impl Limits {
    pub fn size_bytes(&self) -> u64 {
        let s = self.message_size.trim().to_uppercase();
        let n: f64 = s.trim_end_matches(|c: char| c.is_alphabetic() || c == ' ').trim().parse().unwrap_or(25.0);
        let f = if s.ends_with("GB") { 1 << 30 } else if s.ends_with("MB") { 1 << 20 } else if s.ends_with("KB") { 1 << 10 } else { 1 };
        (n * f as f64) as u64
    }
    pub fn per_minute(&self) -> u64 {
        self.rate.split('/').next().and_then(|n| n.trim().parse().ok()).unwrap_or(600)
    }
    pub fn keep_ms(&self) -> u64 {
        stuntman_core::util::parse_ms(&self.keep).unwrap_or(7 * 86_400_000)
    }
}

impl Service {
    pub fn new(ctx: &Ctx, name: &str) -> Self {
        let mut s = Service {
            kind: kind(),
            plugin: plugin(),
            env: ctx.env.to_string(),
            name: name.into(),
            default: false,
            intake: Intake::default(),
            addresses: Addresses::default(),
            events: Events::default(),
            limits: Limits::default(),
        };
        s.addresses.domain = format!("{}.inbox.stuntman.dev", ctx.env);
        s
    }
}

fn dir(ctx: &Ctx) -> PathBuf {
    ctx.path("email/services")
}

pub fn file(ctx: &Ctx, name: &str) -> PathBuf {
    dir(ctx).join(format!("{name}.yaml"))
}

pub fn valid_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty() || name.len() > 40 || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(ApiError::bad("Service name: lowercase letters, digits and dashes"));
    }
    Ok(())
}

pub fn list(ctx: &Ctx) -> Result<Vec<Service>, ApiError> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(dir(ctx)) {
        for e in rd.flatten() {
            if e.path().extension().map(|x| x == "yaml").unwrap_or(false) {
                let s: Option<Service> = yamlfile::load(&e.path()).ok().flatten();
                if let Some(s) = s {
                    out.push(s);
                }
            }
        }
    }
    out.sort_by(|a, b| b.default.cmp(&a.default).then(a.name.cmp(&b.name)));
    Ok(out)
}

pub fn get(ctx: &Ctx, name: &str) -> Result<Service, ApiError> {
    valid_name(name)?;
    let s: Option<Service> = yamlfile::load(&file(ctx, name))?;
    s.ok_or_else(|| ApiError::not_found(format!("No e-mail service “{name}”")))
}

pub fn default_service(ctx: &Ctx) -> Result<Service, ApiError> {
    list(ctx)?.into_iter().next().ok_or_else(|| ApiError::not_found("No e-mail service in this environment"))
}

pub fn save(ctx: &Ctx, s: &Service) -> Result<(), ApiError> {
    valid_name(&s.name)?;
    for st in &s.events.default_steps {
        crate::inbox::Step::parse(st).map_err(ApiError::bad)?;
    }
    yamlfile::save(&file(ctx, &s.name), s)
}

/// First run: one "transactional" service, default, SMTP on 2525.
pub fn ensure_default(ctx: &Ctx, smtp_port: u16) -> Result<(), ApiError> {
    if list(ctx)?.is_empty() {
        let mut s = Service::new(ctx, "transactional");
        s.default = true;
        s.intake.smtp.port = smtp_port;
        save(ctx, &s)?;
    }
    Ok(())
}

// ---------- extractors.yaml ----------

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Extractors {
    #[serde(default)]
    pub code: extract::CodeSettings,
    #[serde(default)]
    pub extractors: Vec<extract::Extractor>,
}

pub fn extractors_file(ctx: &Ctx) -> PathBuf {
    ctx.path("email/extractors.yaml")
}

pub fn extractors(ctx: &Ctx) -> Result<Extractors, ApiError> {
    yamlfile::load(&extractors_file(ctx))
}

pub fn save_extractors(ctx: &Ctx, e: &Extractors) -> Result<(), ApiError> {
    yamlfile::save(&extractors_file(ctx), e)
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn yaml_reads_like_the_screen() {
        let y = r#"
kind: Service
plugin: email@1.0
env: stg-a
name: transactional
default: true
intake:
  sendgridApi: { on: true, mode: any }
  smtp: { on: true, port: 2525, mode: any }
addresses: { domain: stg-a.inbox.stuntman.dev, temporaryTtl: 30m, catchAll: true }
events:
  to: https://acme-api.stg.example.com/hooks/sendgrid
  types: [processed, delivered]
  defaultSteps: [delivered +1s]
limits: { messageSize: 25MB, rate: 600/min, keep: 7d, max: 10000 }
"#;
        let s: Service = serde_yaml::from_str(y).unwrap();
        assert_eq!(s.intake.smtp.port, 2525);
        assert!(s.intake.smtp.starttls);
        assert_eq!(s.limits.size_bytes(), 25 << 20);
        assert_eq!(s.limits.per_minute(), 600);
        let back: Service = serde_yaml::from_str(&serde_yaml::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
