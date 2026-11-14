//! Inboxes (E2): kept with the environment or temporary; what happens when a message arrives,
//! and the steps that simulate the recipient afterwards.

use stuntman_core::{util, ApiError, Ctx};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One simulated recipient action. Text form, used in YAML and over MCP:
/// "delivered +1s", "opened +30s ±5s from previous", "click:signLink +500ms".
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    #[serde(default = "on")]
    pub on: bool,
    /// delivered | opened | click | spamreport | unsubscribe
    pub action: String,
    /// For click: an extractor name, "any", "first", or "re:<regex>".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub wait_ms: u64,
    /// ms | sec | min | h — how the screen shows the wait.
    #[serde(default = "sec")]
    pub unit: String,
    /// arrival | previous | delivery | step:N (1-based)
    #[serde(default = "previous")]
    pub from: String,
    #[serde(default)]
    pub vary_ms: u64,
}
fn on() -> bool {
    true
}
fn sec() -> String {
    "sec".into()
}
fn previous() -> String {
    "previous".into()
}

pub const ACTIONS: [&str; 5] = ["delivered", "opened", "click", "spamreport", "unsubscribe"];

impl Step {
    pub fn parse(s: &str) -> Result<Step, String> {
        let mut parts = s.split_whitespace();
        let head = parts.next().ok_or("empty step")?;
        let (action, link) = match head.split_once(':') {
            Some((a, l)) => (a.to_string(), Some(l.to_string())),
            None => (head.to_string(), None),
        };
        let action = match action.as_str() {
            "open" => "opened".to_string(),
            "deliver" => "delivered".to_string(),
            a => a.to_string(),
        };
        if !ACTIONS.contains(&action.as_str()) {
            return Err(format!("Unknown step “{action}”: use {}", ACTIONS.join(", ")));
        }
        let mut st = Step { on: true, action, link, wait_ms: 0, unit: "sec".into(), from: "previous".into(), vary_ms: 0 };
        let mut expect_from = false;
        for p in parts {
            if expect_from {
                st.from = p.to_string();
                expect_from = false;
            } else if let Some(w) = p.strip_prefix('+') {
                st.wait_ms = util::parse_ms(w).ok_or(format!("Not a wait: {p}"))?;
                st.unit = unit_of(w);
            } else if let Some(v) = p.strip_prefix('±') {
                st.vary_ms = util::parse_ms(v).ok_or(format!("Not a variation: {p}"))?;
            } else if p == "from" {
                expect_from = true;
            } else if p == "off" {
                st.on = false;
            } else {
                return Err(format!("Did not understand “{p}” in “{s}”"));
            }
        }
        if st.action == "delivered" && !s.contains(" from ") {
            st.from = "arrival".into();
        }
        Ok(st)
    }

    pub fn text(&self) -> String {
        let mut s = self.action.clone();
        if let Some(l) = &self.link {
            s = format!("{s}:{l}");
        }
        s += &format!(" +{}", util::fmt_ms(self.wait_ms));
        if self.vary_ms > 0 {
            s += &format!(" ±{}", util::fmt_ms(self.vary_ms));
        }
        let default_from = if self.action == "delivered" { "arrival" } else { "previous" };
        if self.from != default_from {
            s += &format!(" from {}", self.from);
        }
        if !self.on {
            s += " off";
        }
        s
    }

    /// Steps arrive as text ("opened +30s") or as objects (from the screen).
    pub fn from_value(v: &Value) -> Result<Step, String> {
        match v {
            Value::String(s) => Step::parse(s),
            o => {
                let st: Step = serde_json::from_value(o.clone()).map_err(|e| e.to_string())?;
                if !ACTIONS.contains(&st.action.as_str()) {
                    return Err(format!("Unknown step “{}”", st.action));
                }
                if st.wait_ms > 86_400_000 {
                    return Err("Use a wait up to 24 hours".into());
                }
                Ok(st)
            }
        }
    }
}

fn unit_of(w: &str) -> String {
    let u: String = w.chars().filter(|c| c.is_alphabetic()).collect();
    match u.as_str() {
        "ms" => "ms",
        "m" | "min" => "min",
        "h" => "h",
        _ => "sec",
    }
    .into()
}

/// Checks the order rule: "A step never runs before the one it counts from."
pub fn check_steps(steps: &[Step]) -> Result<(), String> {
    for (i, s) in steps.iter().enumerate() {
        if let Some(n) = s.from.strip_prefix("step:") {
            let n: usize = n.parse().map_err(|_| format!("Step {}: “{}” is not a step number", i + 1, s.from))?;
            if n == 0 || n > i {
                return Err(format!("Step {}: it counts from step {n}, which runs after it. Steps never run before the one they count from.", i + 1));
            }
        } else if !["arrival", "previous", "delivery"].contains(&s.from.as_str()) {
            return Err(format!("Step {}: count from arrival, previous, delivery or step:N", i + 1));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Inbox {
    pub address: String,
    /// kept | temporary
    pub kind: String,
    pub created_by: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// accept | bounce | defer | hold | drop
    pub on_arrival: String,
    pub defer_times: u32,
    pub hold_for_ms: u64,
    /// hard | soft
    pub bounce_kind: String,
    /// None: the service's default steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<Step>>,
    /// every | first | first:N | subject:<re> | kind:<k> | has:<extractor> | from:<addr>
    pub apply_to: String,
    /// "seeded user", free text.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub messages: u64,
}


const COLS: &str = "address, kind, created_by, created_at, expires_at, on_arrival, defer_times, hold_for_ms, bounce_kind, steps, apply_to, label,
  (SELECT COUNT(*) FROM email_messages m WHERE m.rcpt = email_inboxes.address COLLATE NOCASE)";

fn row(r: &Row) -> rusqlite::Result<Inbox> {
    let steps: Option<String> = r.get(9)?;
    Ok(Inbox {
        address: r.get(0)?,
        kind: r.get(1)?,
        created_by: r.get(2)?,
        created_at: r.get(3)?,
        expires_at: r.get(4)?,
        on_arrival: r.get(5)?,
        defer_times: r.get(6)?,
        hold_for_ms: r.get(7)?,
        bounce_kind: r.get(8)?,
        steps: steps.and_then(|s| serde_json::from_str(&s).ok()),
        apply_to: r.get(10)?,
        label: r.get(11)?,
        messages: r.get(12)?,
    })
}

impl Inbox {
    pub fn expired(&self) -> bool {
        self.expires_at.map(|e| e < util::now_ms()).unwrap_or(false)
    }
}

pub async fn get(ctx: &Ctx, address: &str) -> Result<Option<Inbox>, ApiError> {
    let a = address.to_lowercase();
    ctx.db.call(move |c| c.query_row(&format!("SELECT {COLS} FROM email_inboxes WHERE address = ?1"), [a], row).optional()).await
}

pub async fn need(ctx: &Ctx, address: &str) -> Result<Inbox, ApiError> {
    get(ctx, address).await?.ok_or_else(|| ApiError::not_found(format!("No inbox {address}")))
}

pub async fn list(ctx: &Ctx, kind: Option<String>, q: Option<String>) -> Result<Vec<Inbox>, ApiError> {
    ctx.db
        .call(move |c| {
            let mut st = c.prepare(&format!(
                "SELECT {COLS} FROM email_inboxes WHERE (?1 IS NULL OR kind = ?1) AND (?2 IS NULL OR address LIKE '%' || ?2 || '%' OR label LIKE '%' || ?2 || '%')
                 ORDER BY kind, CASE WHEN kind='kept' THEN address END, created_at DESC"
            ))?;
            let v = st.query_map(params![kind, q], row)?.collect::<rusqlite::Result<Vec<_>>>();
            v
        })
        .await
}

pub async fn save(ctx: &Ctx, i: &Inbox) -> Result<(), ApiError> {
    if !["accept", "bounce", "defer", "hold", "drop"].contains(&i.on_arrival.as_str()) {
        return Err(ApiError::bad("When a message arrives: accept, bounce, defer, hold or drop"));
    }
    if !["hard", "soft"].contains(&i.bounce_kind.as_str()) {
        return Err(ApiError::bad("Bounce kind: hard or soft"));
    }
    if let Some(s) = &i.steps {
        check_steps(s).map_err(ApiError::bad)?;
    }
    check_apply_to(&i.apply_to).map_err(ApiError::bad)?;
    let i = i.clone();
    let steps = i.steps.as_ref().map(|s| serde_json::to_string(s).unwrap());
    ctx.db
        .call(move |c| {
            c.execute(
                "INSERT INTO email_inboxes(address, kind, created_by, created_at, expires_at, on_arrival, defer_times, hold_for_ms, bounce_kind, steps, apply_to, label)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
                 ON CONFLICT(address) DO UPDATE SET kind=?2, expires_at=?5, on_arrival=?6, defer_times=?7, hold_for_ms=?8, bounce_kind=?9, steps=?10, apply_to=?11, label=?12",
                params![i.address.to_lowercase(), i.kind, i.created_by, i.created_at, i.expires_at, i.on_arrival, i.defer_times, i.hold_for_ms, i.bounce_kind, steps, i.apply_to, i.label],
            )
        })
        .await?;
    Ok(())
}

pub fn check_apply_to(a: &str) -> Result<(), String> {
    let (k, v) = a.split_once(':').unwrap_or((a, ""));
    match k {
        "every" | "first" => Ok(()),
        "first_n" | "first-n" | "firstN" => v.parse::<u32>().map(|_| ()).map_err(|_| "Apply to the first N: N must be a number".into()),
        "subject" => regex::Regex::new(v).map(|_| ()).map_err(|e| format!("Apply to, subject: {e}")),
        "kind" | "has" | "from" if !v.is_empty() => Ok(()),
        _ => Err("Apply to: every, first, first_n:N, subject:<regex>, kind:<kind>, has:<extractor>, from:<address>".into()),
    }
}

pub async fn delete(ctx: &Ctx, address: &str, with_messages: bool) -> Result<u64, ApiError> {
    let a = address.to_lowercase();
    ctx.db
        .call(move |c| {
            let tx = c.transaction()?;
            let mut n = 0;
            if with_messages {
                n = tx.execute("DELETE FROM email_messages WHERE rcpt = ?1 COLLATE NOCASE", [&a])? as u64;
            }
            tx.execute("DELETE FROM email_inboxes WHERE address = ?1", [&a])?;
            tx.commit()?;
            Ok(n)
        })
        .await
}

/// A new temporary address: "signer-a83f@stg-a.inbox.stuntman.dev".
pub fn random_address(prefix: &str, domain: &str) -> String {
    let p = if prefix.is_empty() { "inbox" } else { prefix };
    format!("{p}-{:04x}@{domain}", rand::random::<u16>())
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn steps_text_round_trip() {
        for s in ["delivered +1s", "opened +30s ±5s", "click:signLink +500ms", "opened +2m from step:1", "spamreport +1h off"] {
            let st = Step::parse(s).unwrap();
            assert_eq!(st.text(), s, "{s}");
        }
        assert_eq!(Step::parse("delivered +1s").unwrap().from, "arrival");
        assert!(Step::parse("fly +1s").is_err());
    }

    #[test]
    fn order_rule() {
        let a = Step::parse("delivered +1s").unwrap();
        let mut b = Step::parse("opened +30s").unwrap();
        b.from = "step:2".into();
        assert!(check_steps(&[a.clone(), b.clone()]).is_err());
        b.from = "step:1".into();
        assert!(check_steps(&[a, b]).is_ok());
    }
}
