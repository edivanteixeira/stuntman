//! M3 · Namespaces: one YAML file per namespace under messaging/<name>.yaml (Azure Service Bus or
//! RabbitMQ). The form, the API and agents read and write the same file; key values live in
//! secrets.yaml. RabbitMQ uses the same words: a vhost is a namespace, an exchange is a topic and
//! its bindings are its subscriptions.

use std::{collections::BTreeMap, path::PathBuf};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use stuntman_core::{util, ApiError, Ctx};

pub const SB: &str = "azure-service-bus";
pub const RABBIT: &str = "rabbitmq";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Policy {
    /// manage | send | listen
    #[serde(default)]
    pub rights: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Access {
    /// Any key (any SAS token, any RabbitMQ user and password) is accepted.
    #[serde(default = "yes")]
    pub any_key: bool,
    /// Service Bus: shared access policies; RabbitMQ: users. Values are secrets named
    /// messaging.<namespace>.<policy>.
    #[serde(default)]
    pub policies: BTreeMap<String, Policy>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub managed_identity: bool,
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Queue {
    /// Lock duration (Service Bus peek-lock): 30s by default.
    #[serde(default = "thirty", skip_serializing_if = "is_thirty")]
    pub lock: String,
    #[serde(default = "ten", skip_serializing_if = "is_ten")]
    pub max_delivery: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sessions: bool,
    /// Default time to live (14d); "never" keeps messages.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ttl: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dead_letter_on_expiration: bool,
    /// RabbitMQ: x-dead-letter-exchange (empty: a rejected message is discarded).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dead_letter_exchange: String,
    /// The app created it (auto create on): who and when; "Keep it in the YAML" clears it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_by: String,
}
fn thirty() -> String {
    "30s".into()
}
fn is_thirty(s: &String) -> bool {
    s == "30s"
}
fn ten() -> u32 {
    10
}
fn is_ten(n: &u32) -> bool {
    *n == 10
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Subscription {
    #[serde(default = "thirty", skip_serializing_if = "is_thirty")]
    pub lock: String,
    #[serde(default = "ten", skip_serializing_if = "is_ten")]
    pub max_delivery: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ttl: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dead_letter_on_expiration: bool,
    /// Service Bus: "subject = X" or a property filter "key = value"; empty: every message.
    /// RabbitMQ: the binding key (topic pattern, direct key or empty for fanout).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub filter: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_by: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Topic {
    /// RabbitMQ exchange type: topic | direct | fanout | headers. Empty for Service Bus.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Service Bus: subscriptions by name. RabbitMQ: bindings by destination queue.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub subscriptions: BTreeMap<String, Subscription>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_by: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Entity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<Queue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<Topic>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Behaviour {
    /// none | <n>ms
    #[serde(default = "none")]
    pub latency: String,
    /// none | <n> (messages a second; over it sends get ServerBusy) | throttle_now
    #[serde(default = "none")]
    pub throughput: String,
    /// Create the queues and topics the app asks for.
    #[serde(default = "yes")]
    pub auto_create: bool,
    #[serde(default = "day")]
    pub keep_completed: String,
    /// ok | refuse | timeout | 401 | busy: the whole namespace.
    #[serde(default = "okv", skip_serializing_if = "is_ok")]
    pub reachability: String,
    #[serde(default = "yes")]
    pub seed_on_reset: bool,
}
fn none() -> String {
    "none".into()
}
fn day() -> String {
    "24h".into()
}
fn okv() -> String {
    "ok".into()
}
fn is_ok(s: &String) -> bool {
    s == "ok"
}

impl Default for Behaviour {
    fn default() -> Self {
        Self { latency: none(), throughput: none(), auto_create: true, keep_completed: day(), reachability: okv(), seed_on_reset: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Namespace {
    /// azure-service-bus | rabbitmq
    pub kind: String,
    pub namespace: String,
    pub access: Access,
    #[serde(default)]
    pub entities: BTreeMap<String, Entity>,
    #[serde(default)]
    pub behaviour: Behaviour,
    #[serde(default)]
    pub seed: String,
}

pub const REACH: [&str; 5] = ["ok", "refuse", "timeout", "401", "busy"];

impl Namespace {
    pub fn new(kind: &str, name: &str) -> Self {
        let mut policies = BTreeMap::new();
        if kind == SB {
            policies.insert("RootManageSharedAccessKey".into(), Policy { rights: vec!["manage".into(), "send".into(), "listen".into()] });
        } else {
            policies.insert("guest".into(), Policy { rights: vec!["manage".into()] });
        }
        Namespace {
            kind: kind.into(),
            namespace: name.into(),
            access: Access { any_key: true, policies, managed_identity: false },
            entities: BTreeMap::new(),
            behaviour: Behaviour::default(),
            seed: format!("seed/messaging/{name}"),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.kind != SB && self.kind != RABBIT {
            return Err(format!("kind: {SB} or {RABBIT}"));
        }
        valid_name(&self.namespace)?;
        for (n, e) in &self.entities {
            valid_entity(n)?;
            match (&e.queue, &e.topic) {
                (Some(q), None) => {
                    util::parse_ms(&q.lock).ok_or(format!("entities.{n}.queue.lock: a duration like 30s"))?;
                    if q.max_delivery == 0 {
                        return Err(format!("entities.{n}.queue.max_delivery: 1 or more"));
                    }
                    if !q.ttl.is_empty() && q.ttl != "never" && util::parse_ms(&q.ttl).is_none() {
                        return Err(format!("entities.{n}.queue.ttl: never or a duration like 14d"));
                    }
                }
                (None, Some(t)) => {
                    if self.kind == RABBIT && !["topic", "direct", "fanout", "headers", ""].contains(&t.kind.as_str()) {
                        return Err(format!("entities.{n}.topic.kind: topic | direct | fanout | headers"));
                    }
                    for (s, sub) in &t.subscriptions {
                        valid_entity(s)?;
                        util::parse_ms(&sub.lock).ok_or(format!("entities.{n}.topic.subscriptions.{s}.lock: a duration like 30s"))?;
                    }
                }
                _ => return Err(format!("entities.{n}: either queue or topic")),
            }
        }
        if latency_ms(&self.behaviour.latency).is_none() {
            return Err("behaviour.latency: none or a number of ms like 200ms".into());
        }
        if self.throughput().is_none() {
            return Err("behaviour.throughput: none, throttle_now or messages a second like 100".into());
        }
        if !REACH.contains(&self.behaviour.reachability.as_str()) {
            return Err(format!("behaviour.reachability: {}", REACH.join(" | ")));
        }
        util::parse_ms(&self.behaviour.keep_completed).ok_or("behaviour.keep_completed: a duration like 24h")?;
        Ok(())
    }

    /// Some(None): no limit. Some(Some(0)): throttled now. Some(Some(n)): n a second.
    pub fn throughput(&self) -> Option<Option<u32>> {
        match self.behaviour.throughput.as_str() {
            "none" | "" => Some(None),
            "throttle_now" => Some(Some(0)),
            t => t.trim_end_matches("/s").trim().parse().ok().map(Some),
        }
    }

    pub fn queues(&self) -> usize {
        self.entities.values().filter(|e| e.queue.is_some()).count()
    }
    pub fn topics(&self) -> usize {
        self.entities.values().filter(|e| e.topic.is_some()).count()
    }

    /// Where messages live: every queue, and every subscription as "<topic>/subscriptions/<name>"
    /// (Service Bus). RabbitMQ bindings point at queues, so only queues hold messages there.
    pub fn holders(&self) -> Vec<String> {
        let mut v = vec![];
        for (n, e) in &self.entities {
            if e.queue.is_some() {
                v.push(n.clone());
            }
            if let (Some(t), true) = (&e.topic, self.kind == SB) {
                for s in t.subscriptions.keys() {
                    v.push(format!("{n}/subscriptions/{s}"));
                }
            }
        }
        v
    }

    /// Lock, max delivery and expiry of a holder ("orders-created", "events/subscriptions/audit").
    pub fn limits(&self, holder: &str) -> Limits {
        let (lock, max, ttl, dle, dlx) = match holder.split_once("/subscriptions/") {
            Some((t, s)) => match self.entities.get(t).and_then(|e| e.topic.as_ref()).and_then(|t| t.subscriptions.get(s)) {
                Some(s) => (s.lock.clone(), s.max_delivery, s.ttl.clone(), s.dead_letter_on_expiration, String::new()),
                None => (thirty(), 10, String::new(), false, String::new()),
            },
            None => match self.entities.get(holder).and_then(|e| e.queue.as_ref()) {
                Some(q) => (q.lock.clone(), q.max_delivery, q.ttl.clone(), q.dead_letter_on_expiration, q.dead_letter_exchange.clone()),
                None => (thirty(), 10, String::new(), false, String::new()),
            },
        };
        Limits {
            lock_ms: util::parse_ms(&lock).unwrap_or(30_000) as i64,
            max_delivery: max,
            ttl_ms: if ttl.is_empty() || ttl == "never" { None } else { util::parse_ms(&ttl).map(|x| x as i64) },
            dead_letter_on_expiration: dle,
            dead_letter_exchange: dlx,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Limits {
    pub lock_ms: i64,
    pub max_delivery: u32,
    pub ttl_ms: Option<i64>,
    pub dead_letter_on_expiration: bool,
    pub dead_letter_exchange: String,
}

/// "none" → (0, 0); "200ms" → (200, 0).
pub fn latency_ms(l: &str) -> Option<u64> {
    match l {
        "none" | "" => Some(0),
        x => x.trim_end_matches("ms").trim().parse().ok(),
    }
}

pub fn valid_name(n: &str) -> Result<(), String> {
    let ok = n.len() >= 3 && n.len() <= 50 && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && n.chars().next().map(|c| c.is_ascii_lowercase()).unwrap_or(false);
    if ok { Ok(()) } else { Err(format!("namespace “{n}”: 3 to 50 lowercase letters, numbers or dashes, starting with a letter")) }
}

/// Queue, topic and subscription names: letters, numbers, . - _ (RabbitMQ allows more; these cover both).
pub fn valid_entity(n: &str) -> Result<(), String> {
    let ok = !n.is_empty() && n.len() <= 260 && n.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:".contains(c));
    if ok { Ok(()) } else { Err(format!("“{n}”: letters, numbers, dots, dashes, underscores or colons")) }
}

fn dir(ctx: &Ctx) -> PathBuf {
    ctx.path("messaging")
}

pub fn file(ctx: &Ctx, name: &str) -> PathBuf {
    dir(ctx).join(format!("{name}.yaml"))
}

pub fn rel_file(name: &str) -> String {
    format!("messaging/{name}.yaml")
}

pub fn list(ctx: &Ctx) -> Vec<Namespace> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir(ctx)).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().map(|x| x == "yaml").unwrap_or(false) && p.file_stem().map(|s| s != "rules").unwrap_or(false) {
            if let Some(n) = std::fs::read_to_string(&p).ok().and_then(|t| serde_yaml::from_str::<Namespace>(&t).ok()) {
                out.push(n);
            }
        }
    }
    out.sort_by(|a, b| b.kind.cmp(&a.kind).then(a.namespace.cmp(&b.namespace)));
    out
}

pub fn get(ctx: &Ctx, name: &str) -> Result<Namespace, ApiError> {
    let t = std::fs::read_to_string(file(ctx, name)).map_err(|_| ApiError::not_found(format!("No namespace “{name}”")))?;
    serde_yaml::from_str(&t).map_err(|e| ApiError::bad(format!("{}: {e}", rel_file(name))))
}

pub fn text(ctx: &Ctx, name: &str) -> String {
    std::fs::read_to_string(file(ctx, name)).unwrap_or_default()
}

/// One writer at a time: the AMQP side (auto create) and the API both change these files.
static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn save(ctx: &Ctx, n: &Namespace) -> Result<(), ApiError> {
    n.validate().map_err(ApiError::bad)?;
    let _g = WRITE.lock().unwrap();
    let t = serde_yaml::to_string(n).map_err(|e| ApiError::internal(e.to_string()))?;
    std::fs::create_dir_all(dir(ctx)).map_err(|e| ApiError::internal(e.to_string()))?;
    let f = file(ctx, &n.namespace);
    let tmp = f.with_extension("yaml.tmp");
    std::fs::write(&tmp, t).and_then(|_| std::fs::rename(&tmp, &f)).map_err(|e| ApiError::internal(e.to_string()))?;
    ctx.live.send("messaging.namespace", &n.namespace, "updated");
    Ok(())
}

/// Reads, changes and saves a namespace under the writer lock (no lost update between two writers).
pub fn change<F: FnOnce(&mut Namespace) -> Result<(), ApiError>>(ctx: &Ctx, name: &str, f: F) -> Result<Namespace, ApiError> {
    let _g = WRITE.lock().unwrap();
    let mut n = get(ctx, name)?;
    f(&mut n)?;
    n.validate().map_err(ApiError::bad)?;
    let t = serde_yaml::to_string(&n).map_err(|e| ApiError::internal(e.to_string()))?;
    let path = file(ctx, name);
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, t).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| ApiError::internal(e.to_string()))?;
    ctx.live.send("messaging.namespace", name, "updated");
    Ok(n)
}

pub fn secret_name(ns: &str, policy: &str) -> String {
    format!("messaging.{ns}.{policy}")
}

pub fn key_value(ctx: &Ctx, ns: &str, policy: &str) -> String {
    util::resolve_secret(&format!("secret://{}", secret_name(ns, policy)), &ctx.data_dir)
}

/// A fresh key (Service Bus: 32 bytes base64, like Azure; RabbitMQ: a password).
pub fn new_key(ctx: &Ctx, ns: &str, policy: &str) -> Result<String, ApiError> {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    let v = B64.encode(b);
    util::set_secret(&ctx.data_dir, &secret_name(ns, policy), &v).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(v)
}

pub fn mask(v: &str) -> String {
    if v.len() < 12 {
        return "…".into();
    }
    format!("{}…{}", &v[..6], &v[v.len() - 4..])
}

pub fn create(ctx: &Ctx, kind: &str, name: &str) -> Result<Namespace, ApiError> {
    if file(ctx, name).exists() {
        return Err(ApiError::new(409, format!("A namespace “{name}” already exists")));
    }
    let n = Namespace::new(kind, name);
    n.validate().map_err(ApiError::bad)?;
    for p in n.access.policies.keys() {
        new_key(ctx, name, p)?;
    }
    save(ctx, &n)?;
    Ok(n)
}

pub fn schema(kind: &str) -> serde_json::Value {
    let sb = kind == SB;
    serde_json::json!({
        "service": kind,
        "fields": [
            {"key": "namespace", "type": "text", "label": "Namespace", "help": "used in URLs and keys", "rule": "3 to 50 lowercase letters, numbers or dashes"},
            {"key": "access.any_key", "type": "bool", "label": "Accept any key", "help": if sb { "off: only the policies below; others get 401" } else { "off: only the users below; others get ACCESS_REFUSED" }},
            {"key": "access.policies", "type": "keys", "label": if sb { "Shared access policies" } else { "Users" }},
            {"key": "entities", "type": "entities", "label": if sb { "Queues and topics" } else { "Queues and exchanges" }},
            {"key": "behaviour.latency", "type": "select", "label": "Latency", "help": "added to every answer", "options": [["none", "None"], ["50ms", "50 ms"], ["200ms", "200 ms"], ["1000ms", "1 s"]]},
            {"key": "behaviour.throughput", "type": "select", "label": "Throughput", "help": "over it, sends get ServerBusy",
             "options": [["none", "No limit"], ["100", "100 messages a second"], ["10", "10 messages a second"], ["throttle_now", "Throttle now · every send ServerBusy"]]},
            {"key": "behaviour.auto_create", "type": "bool", "label": "Create queues the app asks for"},
            {"key": "behaviour.keep_completed", "type": "text", "label": "Keep completed messages", "help": "a duration like 24h"},
            {"key": "behaviour.seed_on_reset", "type": "bool", "label": "Load the seed folder on reset"},
        ]
    })
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn holders_limits_and_validation() {
        let mut n = Namespace::new(SB, "acme-bus-stg");
        n.entities.insert("orders-created".into(), Entity { queue: Some(Queue { lock: "30s".into(), max_delivery: 3, ..Default::default() }), topic: None });
        let mut t = Topic::default();
        t.subscriptions.insert("audit".into(), Subscription { lock: "2m".into(), max_delivery: 10, ..Default::default() });
        n.entities.insert("document-events".into(), Entity { queue: None, topic: Some(t) });
        assert!(n.validate().is_ok());
        assert_eq!(n.holders(), vec!["document-events/subscriptions/audit".to_string(), "orders-created".to_string()]);
        assert_eq!(n.limits("orders-created").max_delivery, 3);
        assert_eq!(n.limits("document-events/subscriptions/audit").lock_ms, 120_000);
        n.behaviour.throughput = "throttle_now".into();
        assert_eq!(n.throughput(), Some(Some(0)));
        n.behaviour.reachability = "down".into();
        assert!(n.validate().is_err());
        let y = serde_yaml::to_string(&Namespace::new(RABBIT, "acme-rabbit-stg")).unwrap();
        assert!(y.contains("kind: rabbitmq"));
        assert!(serde_yaml::from_str::<Namespace>(&y).unwrap().validate().is_ok());
    }
}
