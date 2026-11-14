//! T3 · Exporter settings: telemetry/<env>.yaml, the source of truth for what Stuntman keeps, how its
//! collector answers (including provoked failures) and whether it forwards to a real APM.

use std::collections::BTreeMap;

use stuntman_core::{util, yamlfile, ApiError, Ctx};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Receive {
    pub http: u16,
    pub grpc: u16,
    /// Accept any api-key header, so an APM key setting in the app can stay.
    pub any_api_key: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Signal {
    #[serde(default = "yes")]
    pub on: bool,
    pub keep: String,
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Signals {
    pub traces: Signal,
    pub logs: Signal,
    pub metrics: Signal,
}

pub const ANSWERS: [(&str, &str); 8] = [
    ("normal", "Normally"),
    ("403", "403 Forbidden (bad license key)"),
    ("401", "401 Unauthorized"),
    ("429", "429 Too many requests"),
    ("503", "503 Collector down"),
    ("refused", "Connection refused"),
    ("slow", "Slow: 5 s per batch"),
    ("drop_half", "Drop half of the batches"),
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Collector {
    /// normal | 403 | 401 | 429 | 503 | refused | slow | drop_half
    pub answer: String,
    /// all, or a service.name
    pub only_for: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub set_by: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Forward {
    pub enabled: bool,
    pub endpoint: String,
    /// Header values are secret://name references (or env:NAME), never the key itself.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Storage {
    pub max: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub kind: String,
    pub environment: String,
    pub receive: Receive,
    pub signals: Signals,
    pub collector: Collector,
    pub forward: Forward,
    pub storage: Storage,
}

impl Settings {
    pub fn new(env: &str, http: u16, grpc: u16) -> Self {
        let s = |keep: &str| Signal { on: true, keep: keep.into() };
        Settings {
            kind: "telemetry".into(),
            environment: env.into(),
            receive: Receive { http, grpc, any_api_key: true },
            signals: Signals { traces: s("24h"), logs: s("24h"), metrics: s("7d") },
            collector: Collector { answer: "normal".into(), only_for: "all".into(), since: None, until: None, set_by: String::new() },
            forward: Forward { enabled: false, endpoint: String::new(), headers: BTreeMap::from([("api-key".to_string(), "secret://apm-license-key".to_string())]) },
            storage: Storage { max: "1GB".into() },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        for (n, s) in [("traces", &self.signals.traces), ("logs", &self.signals.logs), ("metrics", &self.signals.metrics)] {
            if util::parse_ms(&s.keep).is_none() {
                return Err(format!("{n}.keep: a duration like 24h or 7d"));
            }
        }
        if !ANSWERS.iter().any(|a| a.0 == self.collector.answer) {
            return Err(format!("collector.answer: {}", ANSWERS.iter().map(|a| a.0).collect::<Vec<_>>().join(" | ")));
        }
        if self.forward.enabled || !self.forward.endpoint.is_empty() {
            let e = &self.forward.endpoint;
            let ok = (e.starts_with("https://") || e.starts_with("http://")) && e.split("://").nth(1).map(|h| h.contains(':')).unwrap_or(false);
            if !ok {
                return Err("Endpoint: start with https:// and include the port, for example https://otlp.example-apm.com:4318".into());
            }
        }
        if self.storage_bytes() == 0 {
            return Err("storage.max: a size like 1GB or 500MB".into());
        }
        Ok(())
    }

    pub fn storage_bytes(&self) -> u64 {
        let s = self.storage.max.trim().to_uppercase();
        let n: f64 = s.trim_end_matches(|c: char| c.is_alphabetic() || c == ' ').trim().parse().unwrap_or(0.0);
        let f: u64 = if s.ends_with("GB") { 1 << 30 } else if s.ends_with("MB") { 1 << 20 } else { 1 };
        (n * f as f64) as u64
    }

    /// The provoked answer, if one is on now for this service.
    pub fn provoked_for(&self, service: &str) -> Option<&str> {
        let c = &self.collector;
        if c.answer == "normal" || c.until.map(|u| u < util::now_ms()).unwrap_or(false) {
            return None;
        }
        (c.only_for == "all" || c.only_for == service).then_some(c.answer.as_str())
    }
}

pub fn file(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path(&format!("telemetry/{}.yaml", ctx.env))
}

pub fn load(ctx: &Ctx, http: u16, grpc: u16) -> Result<Settings, ApiError> {
    let s: Option<Settings> = yamlfile::load(&file(ctx))?;
    match s {
        Some(s) => Ok(s),
        None => {
            let s = Settings::new(&ctx.env, http, grpc);
            yamlfile::save(&file(ctx), &s)?;
            Ok(s)
        }
    }
}

pub fn get(ctx: &Ctx) -> Result<Settings, ApiError> {
    load(ctx, crate::ports().0, crate::ports().1)
}

/// Who changed the settings last, for "changed by claude-code via MCP at 14:43".
pub fn last_change() -> &'static std::sync::Mutex<Option<(String, i64)>> {
    static L: std::sync::OnceLock<std::sync::Mutex<Option<(String, i64)>>> = std::sync::OnceLock::new();
    L.get_or_init(Default::default)
}

pub fn save_by(ctx: &Ctx, s: &Settings, by: &str) -> Result<(), ApiError> {
    save(ctx, s)?;
    *last_change().lock().unwrap() = Some((by.to_string(), util::now_ms()));
    Ok(())
}

pub fn save(ctx: &Ctx, s: &Settings) -> Result<(), ApiError> {
    s.validate().map_err(ApiError::bad)?;
    yamlfile::save(&file(ctx), s)?;
    ctx.live.send("telemetry.settings", &ctx.env, "updated");
    Ok(())
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn defaults_and_validation() {
        let mut s = Settings::new("stg-a", 4318, 4317);
        assert!(s.validate().is_ok());
        assert_eq!(s.storage_bytes(), 1 << 30);
        s.forward.endpoint = "otlp.example-apm.com".into();
        assert!(s.validate().unwrap_err().contains("https://"));
        s.forward.endpoint = "https://otlp.example-apm.com:4318".into();
        s.collector.answer = "503".into();
        assert_eq!(s.provoked_for("acme-api"), Some("503"));
        s.collector.only_for = "acme-worker".into();
        assert_eq!(s.provoked_for("acme-api"), None);
        let back: Settings = serde_yaml::from_str(&serde_yaml::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
