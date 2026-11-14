//! Webhook inboxes (H2): how each one answers. Kept inboxes live in webhooks/inboxes.yaml (the
//! source of truth, same keys as the screen and MCP); ephemeral ones, made by tests and agents,
//! live in SQLite and delete themselves.

use stuntman_core::{util, yamlfile, ApiError, Ctx};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    /// always_200 | accept_202 | error | fail_then | fail_share | hang | reset | empty
    pub mode: String,
    /// The error status for error, fail_then and fail_share.
    #[serde(default = "s500")]
    pub status: u16,
    /// fail_then: how many failures before succeeding.
    #[serde(default = "three")]
    pub times: u32,
    /// fail_then: count per event id (each event fails N times) or per inbox.
    #[serde(default = "event_id")]
    pub per: String,
    /// fail_share: percent of deliveries that fail.
    #[serde(default = "fifty")]
    pub share: u32,
    /// The success status after failures.
    #[serde(default = "s200", rename = "then")]
    pub then: u16,
}
fn s500() -> u16 {
    500
}
fn s200() -> u16 {
    200
}
fn three() -> u32 {
    3
}
fn fifty() -> u32 {
    50
}
fn event_id() -> String {
    "event_id".into()
}

impl Default for Answer {
    fn default() -> Self {
        Self { mode: "always_200".into(), status: 500, times: 3, per: "event_id".into(), share: 50, then: 200 }
    }
}

pub const MODES: [(&str, &str); 8] = [
    ("always_200", "Always 200"),
    ("accept_202", "Accept with 202"),
    ("error", "Always an error…"),
    ("fail_then", "Fail N times, then succeed"),
    ("fail_share", "Fail a share of deliveries"),
    ("hang", "Hang (no answer)"),
    ("reset", "Reset the connection"),
    ("empty", "Empty reply"),
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Timing {
    /// "none" or a duration added before answering.
    #[serde(default = "none")]
    pub delay: String,
    /// "never" or how long to hold the request before answering (hang mode uses it, default 35s).
    #[serde(default = "never")]
    pub hang: String,
}
fn none() -> String {
    "none".into()
}
fn never() -> String {
    "never".into()
}

impl Default for Timing {
    fn default() -> Self {
        Self { delay: none(), hang: never() }
    }
}

pub const TRAPS: [&str; 3] = ["redirect_private", "huge", "slow"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Signature {
    /// The shared secret, or "env:NAME" to read it from the environment. Empty: no check.
    #[serde(default)]
    pub secret: String,
    /// t-v1-hmac-sha256 (t=…,v1=… over "t.body") | hmac-sha256-hex | hmac-sha256-base64 | none
    #[serde(default = "tv1")]
    pub scheme: String,
    /// Header that carries the signature; empty: the first header whose name contains "signature".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    /// The secret before rotation: a match shows "signed with the old secret".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub previous_secret: String,
    #[serde(default = "five_min")]
    pub tolerance: String,
    /// Answer 401 to bad signatures (a strict customer).
    #[serde(default)]
    pub reject_bad: bool,
}
fn tv1() -> String {
    "t-v1-hmac-sha256".into()
}
fn five_min() -> String {
    "5m".into()
}

impl Default for Signature {
    fn default() -> Self {
        Self { secret: String::new(), scheme: tv1(), header: String::new(), previous_secret: String::new(), tolerance: five_min(), reject_bad: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Inbox {
    pub name: String,
    #[serde(default)]
    pub kept: bool,
    #[serde(default)]
    pub answer: Answer,
    #[serde(default)]
    pub timing: Timing,
    #[serde(default)]
    pub traps: Vec<String>,
    /// Where the private-redirect trap points. Empty: a loopback address Stuntman serves, so it can see
    /// whether the app followed. Any other URL (e.g. http://10.0.0.5/admin) is sent as is, unobserved.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub redirect_to: String,
    #[serde(default)]
    pub signature: Signature,
    /// Ephemeral only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
}

impl Inbox {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            kept: false,
            answer: Answer::default(),
            timing: Timing::default(),
            traps: vec![],
            redirect_to: String::new(),
            signature: Signature::default(),
            expires_at: None,
            created_by: String::new(),
            created_at: None,
        }
    }

    pub fn expired(&self) -> bool {
        !self.kept && self.expires_at.map(|e| e < util::now_ms()).unwrap_or(false)
    }

    /// "answers 200", "fails 3 times, then 200", "times out after 35 s"…
    pub fn summary(&self) -> String {
        let a = &self.answer;
        let mut s = match a.mode.as_str() {
            "accept_202" => "answers 202".to_string(),
            "error" => format!("always answers {}", a.status),
            "fail_then" => format!("fails {} time{}, then {}", a.times, if a.times == 1 { "" } else { "s" }, a.then),
            "fail_share" => format!("fails {} % with {}", a.share, a.status),
            "hang" => format!("times out after {}", human(&self.hang_ms())),
            "reset" => "resets the connection".to_string(),
            "empty" => "closes without answering".to_string(),
            _ => "answers 200".to_string(),
        };
        if self.delay_ms() > 0 && a.mode != "hang" {
            s += &format!(" after {}", human(&self.delay_ms()));
        }
        if !self.traps.is_empty() {
            s += &format!(" · {} trap{}", self.traps.len(), if self.traps.len() == 1 { "" } else { "s" });
        }
        if self.signature.reject_bad {
            s += " · strict signatures";
        }
        s
    }

    pub fn delay_ms(&self) -> u64 {
        dur(&self.timing.delay)
    }

    pub fn hang_ms(&self) -> u64 {
        match dur(&self.timing.hang) {
            0 => 35_000,
            n => n,
        }
    }

    pub fn tolerance_ms(&self) -> u64 {
        dur(&self.signature.tolerance).max(1000)
    }

    /// The secret itself: literal, "env:NAME", or "secret://name" (resolved by the data folder's
    /// secrets.yaml until the Secrets service exists; then the same reference resolves there).
    pub fn secret_in(&self, data_dir: &std::path::Path) -> String {
        resolve(&self.signature.secret, data_dir)
    }

    pub fn validate(&self) -> Result<(), String> {
        valid_name(&self.name)?;
        if !MODES.iter().any(|m| m.0 == self.answer.mode) {
            return Err(format!("mode: {}", MODES.iter().map(|m| m.0).collect::<Vec<_>>().join(" | ")));
        }
        for st in [self.answer.status, self.answer.then] {
            if !(100..=599).contains(&st) {
                return Err(format!("{st} is not an HTTP status"));
            }
        }
        if self.answer.mode == "fail_then" && !(1..=50).contains(&self.answer.times) {
            return Err("times: from 1 to 50".into());
        }
        if !["event_id", "inbox"].contains(&self.answer.per.as_str()) {
            return Err("per: event_id | inbox".into());
        }
        if self.answer.share > 100 {
            return Err("share: a percent, 0 to 100".into());
        }
        for (k, v) in [("delay", &self.timing.delay), ("hang", &self.timing.hang)] {
            if !["none", "never", ""].contains(&v.as_str()) && util::parse_ms(v).is_none() {
                return Err(format!("{k}: none, or a duration like 2s or 35s"));
            }
        }
        if dur(&self.timing.delay) > 300_000 || dur(&self.timing.hang) > 300_000 {
            return Err("Delay and hang: at most 5 minutes".into());
        }
        for t in &self.traps {
            if !TRAPS.contains(&t.as_str()) {
                return Err(format!("trap “{t}”: {}", TRAPS.join(" | ")));
            }
        }
        if !["t-v1-hmac-sha256", "hmac-sha256-hex", "hmac-sha256-base64", "none"].contains(&self.signature.scheme.as_str()) {
            return Err("scheme: t-v1-hmac-sha256 | hmac-sha256-hex | hmac-sha256-base64 | none".into());
        }
        if util::parse_ms(&self.signature.tolerance).is_none() {
            return Err("tolerance: a duration like 5m".into());
        }
        Ok(())
    }
}

pub fn resolve(v: &str, data_dir: &std::path::Path) -> String {
    stuntman_core::util::resolve_secret(v, data_dir)
}

fn dur(s: &str) -> u64 {
    match s.trim() {
        "" | "none" | "never" => 0,
        o => util::parse_ms(o).unwrap_or(0),
    }
}

pub fn human(ms: &u64) -> String {
    if ms % 1000 == 0 { format!("{} s", ms / 1000) } else { format!("{:.1} s", *ms as f64 / 1000.0) }
}

pub fn valid_name(n: &str) -> Result<(), String> {
    if n.is_empty() || n.len() > 48 || !n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("Name: lowercase letters, digits and dashes (it is part of the URL)".into());
    }
    Ok(())
}

// ---------------------------------------------------------------- storage

fn file(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("webhooks/inboxes.yaml")
}

pub fn kept(ctx: &Ctx) -> Result<Vec<Inbox>, ApiError> {
    let mut v: Vec<Inbox> = yamlfile::load(&file(ctx))?;
    for i in &mut v {
        i.kept = true;
    }
    Ok(v)
}

pub fn save_kept(ctx: &Ctx, v: &[Inbox]) -> Result<(), ApiError> {
    let clean: Vec<Inbox> = v.iter().map(|i| Inbox { kept: true, expires_at: None, created_by: String::new(), created_at: None, ..i.clone() }).collect();
    yamlfile::save(&file(ctx), &clean)
}

pub fn yaml_text(ctx: &Ctx) -> String {
    std::fs::read_to_string(file(ctx)).unwrap_or_else(|_| "[]\n".into())
}

pub async fn ephemeral(ctx: &Ctx) -> Result<Vec<Inbox>, ApiError> {
    ctx.db
        .call(|c| {
            let mut st = c.prepare("SELECT data FROM webhook_ephemeral ORDER BY created_at DESC")?;
            let v = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v.into_iter().filter_map(|s| serde_json::from_str(&s).ok()).collect())
        })
        .await
}

pub async fn all(ctx: &Ctx) -> Result<Vec<Inbox>, ApiError> {
    let mut v = kept(ctx)?;
    v.extend(ephemeral(ctx).await?);
    Ok(v)
}

pub async fn get(ctx: &Ctx, name: &str) -> Result<Option<Inbox>, ApiError> {
    if let Some(i) = kept(ctx)?.into_iter().find(|i| i.name == name) {
        return Ok(Some(i));
    }
    let n = name.to_string();
    let s: Option<String> = ctx.db.call(move |c| c.query_row("SELECT data FROM webhook_ephemeral WHERE name = ?1", [n], |r| r.get(0)).optional()).await?;
    Ok(s.and_then(|s| serde_json::from_str(&s).ok()))
}

pub async fn need(ctx: &Ctx, name: &str) -> Result<Inbox, ApiError> {
    get(ctx, name).await?.ok_or_else(|| ApiError::not_found(format!("No webhook inbox “{name}”")))
}

/// Saves an inbox where it belongs; moving between kept and ephemeral is allowed.
pub async fn put(ctx: &Ctx, i: &Inbox, old_name: Option<&str>) -> Result<(), ApiError> {
    i.validate().map_err(ApiError::bad)?;
    let old = old_name.unwrap_or(&i.name).to_string();
    let mut k = kept(ctx)?;
    // Keep the inbox where it was in the file; new ones go last.
    let pos = k.iter().position(|x| x.name == old);
    k.retain(|x| x.name != old && x.name != i.name);
    let (o2, n2) = (old.clone(), i.name.clone());
    ctx.db.call(move |c| c.execute("DELETE FROM webhook_ephemeral WHERE name = ?1 OR name = ?2", params![o2, n2])).await?;
    if i.kept {
        k.insert(pos.unwrap_or(k.len()).min(k.len()), i.clone());
        save_kept(ctx, &k)?;
    } else {
        save_kept(ctx, &k)?;
        let (n, data, at, exp) = (i.name.clone(), serde_json::to_string(i).unwrap(), i.created_at.unwrap_or(util::now_ms()), i.expires_at);
        ctx.db
            .call(move |c| c.execute("INSERT INTO webhook_ephemeral(name, data, created_at, expires_at) VALUES(?1,?2,?3,?4)", params![n, data, at, exp]))
            .await?;
    }
    ctx.live.send("webhook.inbox", &i.name, "updated");
    Ok(())
}

pub async fn remove(ctx: &Ctx, name: &str) -> Result<bool, ApiError> {
    let mut k = kept(ctx)?;
    let before = k.len();
    k.retain(|x| x.name != name);
    let mut found = k.len() != before;
    if found {
        save_kept(ctx, &k)?;
    }
    let n = name.to_string();
    found |= ctx.db.call(move |c| c.execute("DELETE FROM webhook_ephemeral WHERE name = ?1", [n])).await? > 0;
    // Remembered so later deliveries get 410 Gone instead of 404.
    let n = name.to_string();
    ctx.db.call(move |c| c.execute("INSERT OR REPLACE INTO webhook_gone(name, at) VALUES(?1, ?2)", params![n, util::now_ms()])).await?;
    ctx.live.send("webhook.inbox", name, "deleted");
    Ok(found)
}

pub async fn gone(ctx: &Ctx, name: &str) -> Result<bool, ApiError> {
    let n = name.to_string();
    let cut = util::now_ms() - 86_400_000;
    ctx.db
        .call(move |c| c.query_row("SELECT COUNT(*) FROM webhook_gone WHERE name = ?1 AND at > ?2", params![n, cut], |r| r.get::<_, i64>(0)))
        .await
        .map(|n| n > 0)
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn yaml_matches_the_screen() {
        let y = r#"
- name: initech
  kept: true
  answer: { mode: fail_then, status: 500, times: 3, per: event_id, then: 200 }
  timing: { delay: none, hang: never }
  traps: []
  signature: { secret: whsec_test, scheme: t-v1-hmac-sha256, tolerance: 5m, rejectBad: false }
"#;
        let v: Vec<Inbox> = serde_yaml::from_str(y).unwrap();
        assert_eq!(v[0].summary(), "fails 3 times, then 200");
        assert!(v[0].validate().is_ok());
        let mut h = Inbox::new("umbrella");
        h.answer.mode = "hang".into();
        assert_eq!(h.summary(), "times out after 35 s");
        h.answer.mode = "nope".into();
        assert!(h.validate().is_err());
    }
}
