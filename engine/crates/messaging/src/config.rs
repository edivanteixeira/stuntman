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
    /// RabbitMQ: users of the management API (rabbitmqadmin) allowed on this vhost, with their
    /// configure / write / read patterns. A user declared there and missing here is refused.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub permissions: BTreeMap<String, Permission>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Permission {
    #[serde(default)]
    pub configure: String,
    #[serde(default)]
    pub write: String,
    #[serde(default)]
    pub read: String,
}

/// A RabbitMQ policy (management API `/api/policies/<vhost>/<name>`): queues or exchanges whose
/// name matches `pattern` take `definition` (max-length, overflow, delivery-limit,
/// dead-letter-exchange…); the highest priority wins.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct RabbitPolicy {
    pub pattern: String,
    /// all | queues | classic_queues | quorum_queues | streams | exchanges
    #[serde(rename = "apply-to", default = "all")]
    pub apply_to: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub definition: BTreeMap<String, serde_json::Value>,
}
fn all() -> String {
    "all".into()
}
pub const APPLY_TO: [&str; 6] = ["all", "queues", "classic_queues", "quorum_queues", "streams", "exchanges"];
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
    /// RabbitMQ: the x-arguments it was declared with (x-queue-type, x-delivery-limit,
    /// x-max-length, x-overflow, x-dead-letter-routing-key…).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub arguments: BTreeMap<String, serde_json::Value>,
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
    /// RabbitMQ: more binding keys between the same exchange and destination.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_by: String,
}

impl Subscription {
    /// Every binding key: the filter and the others.
    pub fn all_keys(&self) -> Vec<String> {
        std::iter::once(self.filter.clone()).chain(self.keys.iter().cloned()).collect()
    }
    /// Adds a binding key; false when it was there.
    pub fn add_key(&mut self, k: &str) -> bool {
        if self.all_keys().iter().any(|x| x == k) {
            return false;
        }
        self.keys.push(k.into());
        true
    }
    /// Removes a binding key; true when none is left.
    pub fn remove_key(&mut self, k: &str) -> bool {
        if self.filter == k {
            if self.keys.is_empty() {
                return true;
            }
            self.filter = self.keys.remove(0);
        } else {
            self.keys.retain(|x| x != k);
        }
        false
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Topic {
    /// RabbitMQ exchange type: topic | direct | fanout | headers. Empty for Service Bus.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Service Bus: subscriptions by name. RabbitMQ: bindings by destination queue.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub subscriptions: BTreeMap<String, Subscription>,
    /// RabbitMQ: exchange-to-exchange bindings by destination exchange.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exchanges: BTreeMap<String, Subscription>,
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
    /// RabbitMQ: the vhost's default queue type (empty: classic).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_queue_type: String,
    /// RabbitMQ: policies by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub policies: BTreeMap<String, RabbitPolicy>,
}

pub const REACH: [&str; 7] = ["ok", "refuse", "timeout", "401", "busy", "down", "close"];

/// Modes where the broker is gone: new connections are refused and open ones end
/// (`down`: the TCP connection just drops, as a killed broker; `close`: a forced close first).
pub fn is_down(mode: &str) -> bool {
    matches!(mode, "down" | "close")
}

type Watches = std::sync::Mutex<std::collections::HashMap<String, tokio::sync::watch::Sender<String>>>;

fn watches() -> &'static Watches {
    static W: std::sync::OnceLock<Watches> = std::sync::OnceLock::new();
    W.get_or_init(Default::default)
}

/// Open connections watch their namespace's reachability, so `down` reaches them at once.
pub fn reach_watch(ctx: &Ctx, n: &Namespace) -> tokio::sync::watch::Receiver<String> {
    watches().lock().unwrap().entry(format!("{}|{}", ctx.env, n.namespace)).or_insert_with(|| tokio::sync::watch::channel(n.behaviour.reachability.clone()).0).subscribe()
}

fn reach_publish(ctx: &Ctx, n: &Namespace) {
    if let Some(tx) = watches().lock().unwrap().get(&format!("{}|{}", ctx.env, n.namespace)) {
        tx.send_if_modified(|m| if *m != n.behaviour.reachability { *m = n.behaviour.reachability.clone(); true } else { false });
    }
}

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
            access: Access { any_key: true, policies, managed_identity: false, permissions: BTreeMap::new() },
            entities: BTreeMap::new(),
            behaviour: Behaviour::default(),
            seed: format!("seed/messaging/{name}"),
            default_queue_type: String::new(),
            policies: BTreeMap::new(),
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
                // RabbitMQ keeps queues and exchanges apart: one name can be both (MassTransit does it).
                (Some(_), Some(_)) if self.kind != RABBIT => return Err(format!("entities.{n}: either queue or topic")),
                (None, None) => return Err(format!("entities.{n}: either queue or topic")),
                _ => {}
            }
            if let Some(q) = &e.queue {
                util::parse_ms(&q.lock).ok_or(format!("entities.{n}.queue.lock: a duration like 30s"))?;
                if q.max_delivery == 0 {
                    return Err(format!("entities.{n}.queue.max_delivery: 1 or more"));
                }
                if !q.ttl.is_empty() && q.ttl != "never" && util::parse_ms(&q.ttl).is_none() {
                    return Err(format!("entities.{n}.queue.ttl: never or a duration like 14d"));
                }
            }
            if let Some(t) = &e.topic {
                if self.kind == RABBIT && !["topic", "direct", "fanout", "headers", ""].contains(&t.kind.as_str()) {
                    return Err(format!("entities.{n}.topic.kind: topic | direct | fanout | headers"));
                }
                for (s, sub) in &t.subscriptions {
                    valid_entity(s)?;
                    util::parse_ms(&sub.lock).ok_or(format!("entities.{n}.topic.subscriptions.{s}.lock: a duration like 30s"))?;
                }
                for x in t.exchanges.keys() {
                    valid_entity(x)?;
                }
            }
        }
        if !["", "classic", "quorum", "stream"].contains(&self.default_queue_type.as_str()) {
            return Err("default_queue_type: classic | quorum | stream".into());
        }
        for (name, p) in &self.policies {
            if !APPLY_TO.contains(&p.apply_to.as_str()) {
                return Err(format!("policies.{name}.apply-to: {}", APPLY_TO.join(" | ")));
            }
            regex::Regex::new(&p.pattern).map_err(|e| format!("policies.{name}.pattern: {e}"))?;
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

/// What a RabbitMQ queue does: its type, the policy that applies to it and the limits its
/// x-arguments and that policy give (the lower of both, as RabbitMQ does).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RabbitQueue {
    /// classic | quorum | stream
    pub kind: String,
    /// The policy that applies; empty: none.
    pub policy: String,
    pub definition: BTreeMap<String, serde_json::Value>,
    /// Quorum queues: returns (not nacks) before the message is dropped or dead-lettered.
    pub delivery_limit: Option<i64>,
    pub max_length: Option<i64>,
    /// drop-head | reject-publish | reject-publish-dlx
    pub overflow: String,
    pub dead_letter_exchange: Option<String>,
    pub dead_letter_routing_key: Option<String>,
}

/// RabbitMQ 4: a quorum queue without a delivery limit gets this one.
pub const QUORUM_DELIVERY_LIMIT: i64 = 20;

fn num(v: Option<&serde_json::Value>) -> Option<i64> {
    v.and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}

fn min_of(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

impl Namespace {
    /// The policy of a queue or an exchange (`queue`: which kind of object the name is).
    pub fn policy_for(&self, name: &str, queue_kind: Option<&str>) -> Option<(&String, &RabbitPolicy)> {
        self.policies
            .iter()
            .filter(|(_, p)| {
                let fits = match (p.apply_to.as_str(), queue_kind) {
                    ("all", _) => true,
                    ("exchanges", None) => true,
                    ("queues", Some(_)) => true,
                    ("classic_queues", Some("classic")) | ("quorum_queues", Some("quorum")) | ("streams", Some("stream")) => true,
                    _ => false,
                };
                fits && regex::Regex::new(&p.pattern).map(|r| r.is_match(name)).unwrap_or(false)
            })
            .max_by(|a, b| a.1.priority.cmp(&b.1.priority).then(b.0.cmp(a.0)))
    }

    pub fn rabbit_queue(&self, name: &str) -> Option<RabbitQueue> {
        let q = self.entities.get(name)?.queue.as_ref()?;
        let arg = |k: &str| q.arguments.get(k);
        let kind = arg("x-queue-type").and_then(|v| v.as_str()).unwrap_or("classic").to_string();
        let (policy, definition) = self.policy_for(name, Some(&kind)).map(|(n, p)| (n.clone(), p.definition.clone())).unwrap_or_default();
        let def = |k: &str| definition.get(k);
        let s = |v: Option<&serde_json::Value>| v.and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from);
        let delivery_limit = if kind == "quorum" {
            min_of(num(arg("x-delivery-limit")), num(def("delivery-limit"))).or(Some(QUORUM_DELIVERY_LIMIT)).filter(|n| *n >= 0)
        } else {
            None
        };
        Some(RabbitQueue {
            delivery_limit,
            max_length: min_of(num(arg("x-max-length")), num(def("max-length"))),
            overflow: s(arg("x-overflow")).or(s(def("overflow"))).unwrap_or_else(|| "drop-head".into()),
            dead_letter_exchange: Some(q.dead_letter_exchange.clone()).filter(|s| !s.is_empty()).or(s(arg("x-dead-letter-exchange"))).or(s(def("dead-letter-exchange"))),
            dead_letter_routing_key: s(arg("x-dead-letter-routing-key")).or(s(def("dead-letter-routing-key"))),
            kind,
            policy,
            definition,
        })
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
    reach_publish(ctx, n);
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
    reach_publish(ctx, &n);
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
        n.behaviour.reachability = "dead".into();
        assert!(n.validate().is_err());
        let y = serde_yaml::to_string(&Namespace::new(RABBIT, "acme-rabbit-stg")).unwrap();
        assert!(!y.contains("\npolicies:") && !y.contains("default_queue_type"));
        assert!(y.contains("kind: rabbitmq"));
        assert!(serde_yaml::from_str::<Namespace>(&y).unwrap().validate().is_ok());
    }

    #[test]
    fn rabbit_queue_takes_the_policy_and_the_lower_limit() {
        let mut n = Namespace::new(RABBIT, "acme-rabbit-stg");
        let q = |args: serde_json::Value| Entity { queue: Some(Queue { lock: "30s".into(), max_delivery: 10, arguments: serde_json::from_value(args).unwrap(), ..Default::default() }), topic: Some(Topic { kind: "fanout".into(), ..Default::default() }) };
        n.entities.insert("orders".into(), q(serde_json::json!({"x-queue-type": "quorum"})));
        n.entities.insert("orders-capped".into(), q(serde_json::json!({"x-queue-type": "quorum", "x-max-length": 2, "x-delivery-limit": 50})));
        n.entities.insert("audit".into(), q(serde_json::json!({})));
        n.entities.insert("globex-reports".into(), q(serde_json::json!({"x-queue-type": "quorum"})));
        let def = |m: serde_json::Value| serde_json::from_value(m).unwrap();
        n.policies.insert("acme-defaults".into(), RabbitPolicy { pattern: ".*".into(), apply_to: "quorum_queues".into(), priority: 10, definition: def(serde_json::json!({"max-length": 5000, "overflow": "drop-head", "delivery-limit": 3})) });
        n.policies.insert("globex-reports".into(), RabbitPolicy { pattern: "^globex-reports$".into(), apply_to: "quorum_queues".into(), priority: 20, definition: def(serde_json::json!({"max-length": 5000, "delivery-limit": 5, "consumer-timeout": 3600000})) });
        assert!(n.validate().is_ok(), "{:?}", n.validate());
        let o = n.rabbit_queue("orders").unwrap();
        assert_eq!((o.kind.as_str(), o.policy.as_str(), o.delivery_limit, o.max_length, o.overflow.as_str()), ("quorum", "acme-defaults", Some(3), Some(5000), "drop-head"));
        let c = n.rabbit_queue("orders-capped").unwrap();
        assert_eq!((c.delivery_limit, c.max_length), (Some(3), Some(2)));
        assert_eq!(n.rabbit_queue("globex-reports").unwrap().policy, "globex-reports");
        // a classic queue: the policy is for quorum queues, and classic queues have no delivery limit
        let a = n.rabbit_queue("audit").unwrap();
        assert_eq!((a.kind.as_str(), a.policy.as_str(), a.delivery_limit, a.max_length), ("classic", "", None, None));
        n.policies.clear();
        assert_eq!(n.rabbit_queue("orders").unwrap().delivery_limit, Some(QUORUM_DELIVERY_LIMIT));
        n.policies.insert("bad".into(), RabbitPolicy { pattern: "(".into(), apply_to: "all".into(), ..Default::default() });
        assert!(n.validate().is_err());
        n.policies.insert("bad".into(), RabbitPolicy { pattern: ".*".into(), apply_to: "nope".into(), ..Default::default() });
        assert!(n.validate().is_err());
        // Service Bus still keeps one name for one thing
        let mut sb = Namespace::new(SB, "acme-bus-stg");
        sb.entities.insert("x".into(), q(serde_json::json!({})));
        assert!(sb.validate().is_err());
    }
}
