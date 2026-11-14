//! M2 · Messaging rules: when (operation + namespace + entity + subject + only when…) → instead
//! (a delivery behaviour, or a refusal) → how often. Same editor and API as S2; one file per
//! service: messaging/rules/<service>.yaml. The first enabled rule that matches answers.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use stuntman_core::{util, ApiError, Ctx};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Cond {
    /// property | session | size | sender | delivery_count
    pub field: String,
    /// property: the application property's name
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// property/sender: equals; session: equals | starts_with; size: over (KB); delivery_count: at_least
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub op: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct When {
    /// send | receive | complete | abandon | schedule | any
    #[serde(default = "any")]
    pub op: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub namespace: String,
    /// A queue, topic, or a pattern like payments-*.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entity: String,
    /// Exact text or a pattern like Order*.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subject: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only_when: Vec<Cond>,
}
fn any() -> String {
    "any".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Then {
    /// lose_lock | duplicate | hold | reorder | dead_letter | drop
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub delivery: String,
    /// server_busy | quota_exceeded | entity_not_found | unauthorized
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refuse: String,
    /// hold: seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct HowOften {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub random: Option<u32>,
    /// Every time (the default when neither first nor random is set).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub every: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Rule {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub when: When,
    #[serde(alias = "instead")]
    pub then: Then,
    #[serde(default)]
    pub how_often: HowOften,
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct File {
    #[serde(default)]
    pub rules: Vec<Rule>,
}

pub const OPS: [(&str, &str); 6] = [("send", "Send (publish)"), ("receive", "Receive"), ("complete", "Complete"), ("abandon", "Abandon"), ("schedule", "Schedule"), ("any", "Any operation")];
pub const DELIVERY: [(&str, &str); 6] = [("lose_lock", "Lose the lock"), ("duplicate", "Deliver twice"), ("hold", "Hold N seconds"), ("reorder", "Deliver out of order"), ("dead_letter", "Dead-letter it"), ("drop", "Drop it silently")];
pub const REFUSE: [(&str, &str); 4] = [("server_busy", "503 ServerBusy (throttled)"), ("quota_exceeded", "Quota exceeded"), ("entity_not_found", "Entity not found · 404"), ("unauthorized", "Unauthorized · 401")];
pub const ONLY_WHEN: [(&str, &str); 5] = [("property", "Property · application property equals"), ("session", "Session id · equals or starts with"), ("size", "Size · over N KB"), ("sender", "Sender · client id is"), ("delivery_count", "Delivery count · is at least")];

pub fn options(service: &str) -> Value {
    json!({
        "service": service,
        "operations": OPS.iter().map(|o| json!({"value": o.0, "label": o.1})).collect::<Vec<_>>(),
        "where": ["namespace", "entity", "subject"],
        "only_when": ONLY_WHEN.iter().map(|o| json!({"value": o.0, "label": o.1})).collect::<Vec<_>>(),
        "instead": {
            "delivery": DELIVERY.iter().map(|o| json!({"value": o.0, "label": o.1})).collect::<Vec<_>>(),
            "refuse": REFUSE.iter().map(|o| json!({"value": o.0, "label": o.1})).collect::<Vec<_>>(),
        },
        "how_often": ["first", "every", "random"],
    })
}

impl Rule {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("name: required".into());
        }
        if !OPS.iter().any(|o| o.0 == self.when.op) {
            return Err(format!("when.op: {}", OPS.iter().map(|o| o.0).collect::<Vec<_>>().join(" | ")));
        }
        match (self.then.delivery.is_empty(), self.then.refuse.is_empty()) {
            (true, true) => return Err("instead: a delivery (lose_lock, duplicate, hold, reorder, dead_letter, drop) or a refusal (server_busy, quota_exceeded, entity_not_found, unauthorized)".into()),
            (false, false) => return Err("instead: either delivery or refuse, not both".into()),
            _ => {}
        }
        if !self.then.delivery.is_empty() && !DELIVERY.iter().any(|d| d.0 == self.then.delivery) {
            return Err(format!("instead.delivery: {}", DELIVERY.iter().map(|o| o.0).collect::<Vec<_>>().join(" | ")));
        }
        if !self.then.refuse.is_empty() && !REFUSE.iter().any(|d| d.0 == self.then.refuse) {
            return Err(format!("instead.refuse: {}", REFUSE.iter().map(|o| o.0).collect::<Vec<_>>().join(" | ")));
        }
        if self.how_often.random.map(|p| p == 0 || p > 100).unwrap_or(false) {
            return Err("how_often.random: 1 to 100 (%)".into());
        }
        for c in &self.when.only_when {
            if !ONLY_WHEN.iter().any(|o| o.0 == c.field) {
                return Err(format!("only_when.field: {}", ONLY_WHEN.iter().map(|o| o.0).collect::<Vec<_>>().join(" | ")));
            }
            if c.field == "property" && c.key.is_empty() {
                return Err("only_when: a property needs its key".into());
            }
            if (c.field == "size" || c.field == "delivery_count") && c.value.trim().trim_end_matches("KB").trim().parse::<u64>().is_err() {
                return Err(format!("only_when.{}: a number", c.field));
            }
        }
        Ok(())
    }

    /// "orders-created → lose lock"
    pub fn meta(&self) -> String {
        let w = if !self.when.entity.is_empty() { self.when.entity.clone() } else if !self.when.subject.is_empty() { self.when.subject.clone() } else if !self.when.namespace.is_empty() { self.when.namespace.clone() } else { "any".into() };
        let then = if !self.then.delivery.is_empty() {
            match self.then.delivery.as_str() {
                "hold" => format!("+{} s", self.then.seconds.unwrap_or(30)),
                "dead_letter" => "DLQ".into(),
                d => d.replace('_', " "),
            }
        } else {
            self.then.refuse.replace('_', " ")
        };
        let often = match (self.how_often.first, self.how_often.random) {
            (Some(n), _) => format!(" ×{n}"),
            (_, Some(p)) => format!(" {p}%"),
            _ => String::new(),
        };
        format!("{w} → {then}{often}")
    }
}

/// What a rule looks at.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub op: String,
    pub namespace: String,
    pub entity: String,
    pub subject: String,
    pub props: Map<String, Value>,
    pub session: String,
    pub size: u64,
    pub sender: String,
    pub delivery_count: i64,
}

/// "*" anywhere in the pattern; empty matches everything.
pub fn glob(pat: &str, s: &str) -> bool {
    if pat.is_empty() || pat == "*" {
        return true;
    }
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut rest = s;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            if !rest.starts_with(p) {
                return false;
            }
            rest = &rest[p.len()..];
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else if let Some(k) = rest.find(p) {
            rest = &rest[k + p.len()..];
        } else {
            return false;
        }
    }
    true
}

pub fn matches(r: &Rule, s: &Seen) -> bool {
    let w = &r.when;
    let op_ok = w.op == "any" || w.op == s.op || (w.op == "send" && s.op == "schedule");
    // A subscription is reached through its topic: "events" matches "events/subscriptions/audit".
    let entity_ok = w.entity.is_empty() || glob(&w.entity, &s.entity) || s.entity.split("/subscriptions/").next().map(|t| glob(&w.entity, t)).unwrap_or(false);
    op_ok
        && (w.namespace.is_empty() || w.namespace == s.namespace)
        && entity_ok
        && glob(&w.subject, &s.subject)
        && w.only_when.iter().all(|c| match c.field.as_str() {
            "property" => s.props.get(&c.key).map(|v| v.as_str().map(String::from).unwrap_or(v.to_string()) == c.value).unwrap_or(false),
            "session" => if c.op == "starts_with" { s.session.starts_with(&c.value) } else { s.session == c.value },
            "size" => s.size > c.value.trim().trim_end_matches("KB").trim().parse::<u64>().unwrap_or(0) * 1024,
            "sender" => s.sender == c.value,
            "delivery_count" => s.delivery_count >= c.value.trim().parse::<i64>().unwrap_or(0),
            _ => true,
        })
}

pub fn file_path(ctx: &Ctx, service: &str) -> std::path::PathBuf {
    ctx.path(&format!("messaging/rules/{service}.yaml"))
}

pub fn load(ctx: &Ctx, service: &str) -> Result<File, ApiError> {
    stuntman_core::yamlfile::load(&file_path(ctx, service))
}

pub fn save(ctx: &Ctx, service: &str, f: &File) -> Result<(), ApiError> {
    let mut seen = std::collections::HashSet::new();
    for r in &f.rules {
        r.validate().map_err(|e| ApiError::bad(format!("“{}”: {e}", r.name)))?;
        if !seen.insert(r.name.clone()) {
            return Err(ApiError::new(409, format!("Two rules are called “{}”", r.name)));
        }
    }
    stuntman_core::yamlfile::save(&file_path(ctx, service), f)?;
    ctx.live.send("messaging.rules", service, "updated");
    Ok(())
}

pub async fn reset_count(ctx: &Ctx, service: &str, name: &str) -> Result<(), ApiError> {
    let (s, n) = (service.to_string(), name.to_string());
    ctx.db.call(move |c| c.execute("DELETE FROM msg_rule_counts WHERE service=?1 AND name=?2", params![s, n]).map(|_| ())).await
}

/// The rule that fires now for what was seen, if any.
pub async fn decide(ctx: &Ctx, service: &str, s: &Seen) -> Result<Option<Rule>, ApiError> {
    let f = load(ctx, service).unwrap_or_default();
    for r in f.rules.into_iter().filter(|r| r.enabled) {
        if !matches(&r, s) {
            continue;
        }
        let (svc, name) = (service.to_string(), r.name.clone());
        let n: i64 = ctx.db.call(move |c| {
            c.execute("INSERT INTO msg_rule_counts VALUES(?1,?2,1) ON CONFLICT(service,name) DO UPDATE SET n=n+1", params![svc, name])?;
            c.query_row("SELECT n FROM msg_rule_counts WHERE service=?1 AND name=?2", params![svc, name], |x| x.get(0))
        }).await?;
        let fire = match (r.how_often.first, r.how_often.random) {
            (Some(k), _) => n <= k as i64,
            (_, Some(p)) => rand::random::<u32>() % 100 < p,
            _ => true,
        };
        return Ok(fire.then_some(r));
    }
    Ok(None)
}

pub async fn hit(ctx: &Ctx, service: &str, name: &str) {
    let (s, n) = (service.to_string(), name.to_string());
    let _ = ctx.db.call(move |c| c.execute("INSERT INTO msg_rule_hits VALUES(?1,?2,?3)", params![s, n, util::now_ms()])).await;
}

pub async fn hits(ctx: &Ctx, service: &str) -> Result<std::collections::BTreeMap<String, (i64, Option<i64>)>, ApiError> {
    let s = service.to_string();
    let day = util::now_ms() - 86_400_000;
    ctx.db.call(move |c| {
        let mut q = c.prepare("SELECT name, sum(at > ?2), max(at) FROM msg_rule_hits WHERE service=?1 GROUP BY name")?;
        let v = q.query_map(params![s, day], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?))))?.collect::<Result<_, _>>();
        v
    }).await
}

fn effect(t: &Then, lock_s: i64) -> String {
    match (t.delivery.as_str(), t.refuse.as_str()) {
        ("lose_lock", _) => format!("lock lost after {lock_s} s  ·  redelivered"),
        ("duplicate", _) => "delivered twice".into(),
        ("hold", _) => format!("delivered {} s late", t.seconds.unwrap_or(30)),
        ("reorder", _) => "delivered after the next message".into(),
        ("dead_letter", _) => "moved to dead-letter, never delivered".into(),
        ("drop", _) => "never delivered".into(),
        (_, "server_busy") => "ServerBusyException, the app retries".into(),
        (_, "quota_exceeded") => "QuotaExceededException".into(),
        (_, "entity_not_found") => "MessagingEntityNotFoundException".into(),
        (_, "unauthorized") => "UnauthorizedAccessException".into(),
        _ => String::new(),
    }
}

/// "Try it": what the draft would match among the last deliveries and what the app sees per delivery.
pub fn test(rule: &Rule, others: &[Rule], last: &[Value], lock_s: i64) -> Value {
    let seen = |v: &Value| Seen {
        op: "receive".into(),
        namespace: v["namespace"].as_str().unwrap_or("").into(),
        entity: v["entity"].as_str().unwrap_or("").into(),
        subject: v["subject"].as_str().unwrap_or("").into(),
        props: v["properties"].as_object().cloned().unwrap_or_default(),
        session: v["system"]["sessionId"].as_str().unwrap_or("").into(),
        size: v["size"].as_u64().unwrap_or(0),
        sender: v["sender"].as_str().unwrap_or("").into(),
        delivery_count: v["deliveryCount"].as_i64().unwrap_or(0),
    };
    let mut probe = rule.clone();
    if probe.when.op != "send" && probe.when.op != "schedule" {
        probe.when.op = "any".into();
    } else {
        probe.when.op = "any".into();
    }
    let hits: Vec<&Value> = last.iter().filter(|v| matches(&probe, &seen(v))).collect();
    let e = effect(&rule.then, lock_s);
    let tries: Vec<Value> = match (rule.how_often.first, rule.how_often.random) {
        (Some(n), _) => (1..=n.min(4) + 1).map(|i| if i <= n { json!({"try": i, "answer": e, "provoked": true}) } else { json!({"try": i, "answer": "completed  ·  back to normal", "provoked": false}) }).collect(),
        (_, Some(p)) => vec![json!({"try": 0, "answer": format!("about 1 in {} deliveries: {e}", (100 / p.max(1)).max(1)), "provoked": true})],
        _ => (1..=2).map(|i| json!({"try": i, "answer": e, "provoked": true})).collect(),
    };
    let mut warnings = vec![];
    let me = others.iter().position(|r| r.name == rule.name).unwrap_or(others.len());
    for above in others.iter().take(me).filter(|r| r.enabled && r.name != rule.name) {
        let (a, w) = (&above.when, &rule.when);
        if (a.op == "any" || a.op == w.op) && (a.namespace.is_empty() || a.namespace == w.namespace) && (a.entity.is_empty() || a.entity == w.entity) && a.subject.is_empty() && a.only_when.is_empty() && above.how_often.first.is_none() && above.how_often.random.is_none() {
            warnings.push(json!({"kind": "shadowed", "by": above.name, "text": format!("“{}” is above it and matches the same messages, so it answers first.", above.name)}));
        }
    }
    json!({
        "wouldMatch": hits.iter().take(20).map(|v| json!({"seq": v["seq"], "subject": v["subject"], "entity": v["entity"], "at": v["enqueuedAt"], "consumer": v["lockedBy"], "sender": v["sender"]})).collect::<Vec<_>>(),
        "matched": hits.len(), "of": last.len(), "untouched": last.len() - hits.len(), "appWillSee": tries, "warnings": warnings,
    })
}

#[cfg(test)]
mod t {
    use super::*;

    fn rule() -> Rule {
        Rule { name: "Orders lose the lock".into(), enabled: true, when: When { op: "receive".into(), namespace: "acme-bus-stg".into(), entity: "orders-created".into(), subject: "Order*".into(), only_when: vec![] },
            then: Then { delivery: "lose_lock".into(), ..Default::default() }, how_often: HowOften { first: Some(1), ..Default::default() } }
    }

    #[test]
    fn matching_and_try_it() {
        let r = rule();
        assert!(r.validate().is_ok());
        let mut s = Seen { op: "receive".into(), namespace: "acme-bus-stg".into(), entity: "orders-created".into(), subject: "OrderCreated".into(), ..Default::default() };
        assert!(matches(&r, &s));
        s.subject = "Invoice".into();
        assert!(!matches(&r, &s));
        let mut r2 = r.clone();
        r2.when.only_when = vec![Cond { field: "property".into(), key: "tenant".into(), op: String::new(), value: "globex".into() }];
        s.subject = "OrderCreated".into();
        assert!(!matches(&r2, &s));
        s.props.insert("tenant".into(), json!("globex"));
        assert!(matches(&r2, &s));
        let last = vec![json!({"seq": 1042, "namespace": "acme-bus-stg", "entity": "orders-created", "subject": "OrderCreated"}), json!({"seq": 1, "namespace": "acme-bus-stg", "entity": "invoice-render", "subject": "X"})];
        let v = test(&r, &[], &last, 30);
        assert_eq!(v["matched"], 1);
        assert_eq!(v["appWillSee"][0]["answer"], "lock lost after 30 s  ·  redelivered");
        assert_eq!(v["appWillSee"][1]["answer"], "completed  ·  back to normal");
        assert_eq!(r.meta(), "orders-created → lose lock ×1");
        assert!(glob("payments-*", "payments-retry"));
        let mut bad = rule();
        bad.then.refuse = "server_busy".into();
        assert!(bad.validate().is_err());
    }
}
