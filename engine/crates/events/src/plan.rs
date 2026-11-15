//! events/plans.yaml: what a flow must send. Each plan lists events and the properties each needs.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use stuntman_core::{yamlfile, ApiError, Ctx};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Plans {
    pub plans: Vec<Plan>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Plan {
    pub name: String,
    pub events: Vec<Expected>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Expected {
    pub event: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<String>,
    /// At least this many (default 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<u64>,
}

pub fn path(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("events/plans.yaml")
}

pub fn load(ctx: &Ctx) -> Plans {
    yamlfile::load(&path(ctx)).unwrap_or_default()
}

pub fn save(ctx: &Ctx, p: &Plans) -> Result<(), ApiError> {
    yamlfile::save(&path(ctx), p)
}

fn has(props: &Value, key: &str) -> bool {
    key.split('.').try_fold(props, |acc, k| acc.get(k)).map(|v| !v.is_null()).unwrap_or(false)
}

/// Each expected event against what arrived: how many, and the properties some of them lacked.
pub fn check(expected: &[Expected], arrived: &[Value]) -> Value {
    let mut rows = vec![];
    let mut ok = true;
    for e in expected {
        let got: Vec<&Value> = arrived.iter().filter(|a| a["name"] == json!(e.event)).collect();
        let missing: Vec<&String> = e.props.iter().filter(|p| got.iter().any(|g| !has(&g["props"], p))).collect();
        let min = e.min.unwrap_or(1);
        let state = if (got.len() as u64) < min { "missing" } else if !missing.is_empty() { "props_missing" } else { "ok" };
        ok &= state == "ok";
        rows.push(json!({"event": e.event, "arrived": got.len(), "min": min, "state": state, "missingProps": missing}));
    }
    json!({"ok": ok, "events": rows, "missing": rows.iter().filter(|r| r["state"] != "ok").map(|r| r["event"].clone()).collect::<Vec<_>>()})
}
