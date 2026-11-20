//! The broker both protocols share: messages live in "holders" (a queue, or a Service Bus
//! subscription "<topic>/subscriptions/<name>"), are claimed with a lock, then completed,
//! abandoned, dead-lettered or released. Topics and RabbitMQ exchanges route into holders.
//! Every step is written to the message's history: what the screen shows as "Delivery".

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use rusqlite::{params, OptionalExtension, Row};
use serde_json::{json, Map, Value};
use stuntman_core::{util, ApiError, Ctx};
use tokio::sync::Notify;

use crate::{config, rules};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS msg_messages(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, ns TEXT NOT NULL, holder TEXT NOT NULL,
  message_id TEXT NOT NULL DEFAULT '', subject TEXT NOT NULL DEFAULT '', content_type TEXT NOT NULL DEFAULT '',
  body BLOB NOT NULL, props TEXT NOT NULL DEFAULT '{}', sys TEXT NOT NULL DEFAULT '{}', size INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL, delivery_count INTEGER NOT NULL DEFAULT 0, enqueued_at INTEGER NOT NULL, available_at INTEGER NOT NULL,
  expires_at INTEGER, locked_until INTEGER NOT NULL DEFAULT 0, locked_by TEXT NOT NULL DEFAULT '', lock_token TEXT NOT NULL DEFAULT '',
  settled_at INTEGER, dl_reason TEXT NOT NULL DEFAULT '', dl_description TEXT NOT NULL DEFAULT '',
  sender TEXT NOT NULL DEFAULT '', via TEXT NOT NULL DEFAULT '', trace_id TEXT, flags TEXT NOT NULL DEFAULT '', origin INTEGER);
-- The next message of a queue in seq order: with available_at before seq, every claim scanned and sorted the
-- whole queue (a long queue delivered a few messages a second).
DROP INDEX IF EXISTS msg_holder;
CREATE INDEX IF NOT EXISTS msg_queue ON msg_messages(ns, holder, state, seq);
-- The sweep every second (expired locks, due scheduled messages, time to live) without a full scan.
CREATE INDEX IF NOT EXISTS msg_sweep ON msg_messages(state, locked_until, available_at);
CREATE INDEX IF NOT EXISTS msg_ttl ON msg_messages(expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS msg_token ON msg_messages(lock_token);
CREATE TABLE IF NOT EXISTS msg_history(id INTEGER PRIMARY KEY AUTOINCREMENT, seq INTEGER NOT NULL, at INTEGER NOT NULL,
  what TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '', tone TEXT NOT NULL DEFAULT '', kind TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS msg_history_seq ON msg_history(seq);
CREATE TABLE IF NOT EXISTS msg_next(ns TEXT NOT NULL, holder TEXT NOT NULL, behavior TEXT NOT NULL, times INTEGER NOT NULL, by_who TEXT NOT NULL DEFAULT '',
  PRIMARY KEY(ns, holder));
CREATE TABLE IF NOT EXISTS msg_consumers(id TEXT PRIMARY KEY, ns TEXT NOT NULL, holder TEXT NOT NULL, client TEXT NOT NULL, link TEXT NOT NULL DEFAULT '',
  mode TEXT NOT NULL DEFAULT '', prefetch INTEGER NOT NULL DEFAULT 0, since INTEGER NOT NULL, until INTEGER, reason TEXT NOT NULL DEFAULT '',
  done INTEGER NOT NULL DEFAULT 0, remote TEXT NOT NULL DEFAULT '', protocol TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS msg_sessions(ns TEXT NOT NULL, holder TEXT NOT NULL, sid TEXT NOT NULL, state BLOB, owner TEXT NOT NULL DEFAULT '',
  locked_until INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(ns, holder, sid));
CREATE TABLE IF NOT EXISTS msg_rule_counts(service TEXT NOT NULL, name TEXT NOT NULL, n INTEGER NOT NULL, PRIMARY KEY(service, name));
CREATE TABLE IF NOT EXISTS msg_rule_hits(service TEXT NOT NULL, name TEXT NOT NULL, at INTEGER NOT NULL);
";

pub const STATES: [&str; 5] = ["active", "scheduled", "locked", "dead_letter", "completed"];

/// What an app (or a person, or an agent) sends.
#[derive(Clone, Debug, Default)]
pub struct Outgoing {
    pub message_id: String,
    pub subject: String,
    pub content_type: String,
    pub body: Vec<u8>,
    /// Application properties (Service Bus) / headers (RabbitMQ).
    pub props: Map<String, Value>,
    /// The rest: correlationId, sessionId, replyTo, timeToLive, exchange, routingKey, AMQP 0-9-1 properties…
    pub sys: Map<String, Value>,
    pub scheduled_at: Option<i64>,
    pub sender: String,
    /// amqp | api | mcp | you
    pub via: String,
}

/// A message claimed for one consumer.
#[derive(Clone, Debug)]
pub struct Claimed {
    pub seq: i64,
    pub holder: String,
    pub lock_token: String,
    pub locked_until: i64,
    pub delivery_count: i64,
    pub enqueued_at: i64,
    pub message_id: String,
    pub subject: String,
    pub content_type: String,
    pub body: Vec<u8>,
    pub props: Map<String, Value>,
    pub sys: Map<String, Value>,
    /// The next settle of this delivery must fail as if the lock was lost.
    pub lose_lock: bool,
}

/// Why a send or a settle failed, in protocol-neutral words.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    NotFound(String),
    Busy(String),
    Quota(String),
    Unauthorized(String),
    LockLost(String),
}

impl Refusal {
    pub fn text(&self) -> &str {
        match self {
            Refusal::NotFound(t) | Refusal::Busy(t) | Refusal::Quota(t) | Refusal::Unauthorized(t) | Refusal::LockLost(t) => t,
        }
    }
}

// ---------------------------------------------------------------- wake-ups

/// One Notify per (environment, namespace, holder): consumers wait on it, sends wake it.
fn bells() -> &'static Mutex<HashMap<String, Arc<Notify>>> {
    static B: OnceLock<Mutex<HashMap<String, Arc<Notify>>>> = OnceLock::new();
    B.get_or_init(Default::default)
}

pub fn bell(ctx: &Ctx, ns: &str, holder: &str) -> Arc<Notify> {
    bells().lock().unwrap().entry(format!("{}|{ns}|{holder}", ctx.env)).or_insert_with(|| Arc::new(Notify::new())).clone()
}

fn ring(ctx: &Ctx, ns: &str, holder: &str) {
    bell(ctx, ns, holder).notify_waiters();
    ctx.live.send("messaging.message", &format!("{ns}/{holder}"), "changed");
}

// ---------------------------------------------------------------- timeline

/// One Timeline row in the service's words; completions fold per consumer, entity and minute.
async fn tl(ctx: &Ctx, id: &str, ns: &str, label: &str, outcome: &str, detail: &str, trace: Option<&str>, actor: &str, link: &str) {
    let _ = stuntman_core::trips::record(ctx, stuntman_core::trips::Trip { id, module: "messaging", label, status: outcome, trace_id: trace, link, host: ns, actor, outcome, detail }).await;
}

fn link_of(ns: &str, holder: &str, seq: i64) -> String {
    format!("/queues?ns={ns}&entity={}&seq={seq}", holder.replace('/', "%2F"))
}

async fn trace_of_seq(ctx: &Ctx, seq: i64) -> Option<String> {
    ctx.db.call(move |c| c.query_row("SELECT trace_id FROM msg_messages WHERE seq=?1", [seq], |r| r.get::<_, Option<String>>(0)).optional()).await.ok().flatten().flatten()
}

// ---------------------------------------------------------------- history

pub async fn note(ctx: &Ctx, seq: i64, what: &str, detail: &str, tone: &str) {
    let (w, d, t) = (what.to_string(), detail.to_string(), tone.to_string());
    let _ = ctx.db.call(move |c| c.execute("INSERT INTO msg_history(seq, at, what, detail, tone) VALUES(?1,?2,?3,?4,?5)", params![seq, util::now_ms(), w, d, t])).await;
}

// ---------------------------------------------------------------- routing

/// Holders a message published to `entity` lands in (a queue: itself; a topic: its matching
/// subscriptions or bound queues). Err when the entity does not exist and may not be created.
pub fn route(n: &config::Namespace, entity: &str, o: &Outgoing) -> Result<Vec<String>, Refusal> {
    // RabbitMQ publishes say which exchange: "" is the default one (straight to the queue named
    // by the routing key); queues and exchanges may share a name.
    if n.kind == config::RABBIT {
        if let Some(ex) = o.sys.get("exchange").and_then(|v| v.as_str()) {
            let key = o.sys.get("routingKey").and_then(|v| v.as_str()).unwrap_or("");
            if ex.is_empty() {
                return Ok(if n.entities.get(entity).and_then(|e| e.queue.as_ref()).is_some() { vec![entity.to_string()] } else { vec![] });
            }
            if n.entities.get(ex).and_then(|e| e.topic.as_ref()).is_none() {
                return Err(Refusal::NotFound(format!("no exchange '{ex}' in vhost '{}'", n.namespace)));
            }
            return Ok(rabbit_route(n, ex, key, &o.props));
        }
    }
    // Service Bus clients may address a subscription's dead-letter or a subscription directly.
    if n.holders().iter().any(|h| h == entity) {
        return Ok(vec![entity.to_string()]);
    }
    let Some(e) = n.entities.get(entity) else {
        return Err(Refusal::NotFound(format!("The messaging entity '{}/{}' could not be found.", n.namespace, entity)));
    };
    if e.queue.is_some() {
        return Ok(vec![entity.to_string()]);
    }
    if n.kind == config::RABBIT {
        let key = o.sys.get("routingKey").and_then(|v| v.as_str()).unwrap_or("");
        return Ok(rabbit_route(n, entity, key, &o.props));
    }
    let t = e.topic.as_ref().unwrap();
    let mut out = vec![];
    for (name, s) in &t.subscriptions {
        if sb_filter(&s.filter, o) {
            out.push(format!("{entity}/subscriptions/{name}"));
        }
    }
    Ok(out)
}

/// Does a binding of an exchange of this type take the routing key (or the headers)?
fn binds(kind: &str, s: &config::Subscription, key: &str, headers: &Map<String, Value>) -> bool {
    match kind {
        "fanout" => true,
        "direct" => s.all_keys().iter().any(|k| k == key),
        // Headers bindings keep their arguments in the filter as "k=v, k2=v2" (x-match all).
        "headers" => s.filter.split(',').map(str::trim).filter(|p| !p.is_empty() && !p.starts_with("x-match")).all(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            headers.get(k.trim()).map(|h| h.as_str().map(String::from).unwrap_or(h.to_string()) == v.trim()).unwrap_or(false)
        }),
        _ => s.all_keys().iter().any(|k| topic_match(k, key)),
    }
}

/// RabbitMQ routing from one exchange: its queue bindings, then the exchanges it is bound to
/// (exchange-to-exchange, as MassTransit builds its topology), each queue once.
pub fn rabbit_route(n: &config::Namespace, exchange: &str, key: &str, headers: &Map<String, Value>) -> Vec<String> {
    rabbit_trace(n, exchange, key, headers, None)
}

/// rabbit_route that also writes down each binding it looked at: source, destination, its type,
/// its keys and whether the message took it.
pub fn rabbit_trace(n: &config::Namespace, exchange: &str, key: &str, headers: &Map<String, Value>, mut hops: Option<&mut Vec<Value>>) -> Vec<String> {
    let (mut out, mut seen, mut todo) = (Vec::<String>::new(), std::collections::HashSet::new(), vec![exchange.to_string()]);
    while let Some(x) = todo.pop() {
        if !seen.insert(x.clone()) {
            continue;
        }
        let Some(t) = n.entities.get(&x).and_then(|e| e.topic.as_ref()) else { continue };
        for (dtype, map) in [("queue", &t.subscriptions), ("exchange", &t.exchanges)] {
            for (d, s) in map {
                let took = binds(&t.kind, s, key, headers);
                if let Some(h) = hops.as_deref_mut() {
                    let keys = if t.kind == "headers" { vec![s.filter.clone()] } else { s.all_keys() };
                    h.push(json!({"source": x, "sourceType": t.kind, "destination": d, "destinationType": dtype, "bindingKeys": keys, "matches": took}));
                }
                if !took {
                    continue;
                }
                if dtype == "exchange" {
                    todo.push(d.clone());
                } else if !out.contains(d) && n.entities.get(d).and_then(|e| e.queue.as_ref()).is_some() {
                    out.push(d.clone());
                }
            }
        }
    }
    out
}

type Unrouted = HashMap<(String, String, String, String), (u64, i64, String)>;

/// Messages published that reached no queue: (environment, namespace, exchange, routing key) → (count,
/// last time, last sender). Kept in memory; the warning goes when a binding now takes that key.
fn unrouted() -> &'static Mutex<Unrouted> {
    static U: OnceLock<Mutex<Unrouted>> = OnceLock::new();
    U.get_or_init(Default::default)
}

pub fn note_unrouted(ctx: &Ctx, ns: &str, exchange: &str, key: &str, sender: &str) {
    let mut u = unrouted().lock().unwrap();
    // ponytail: at most 500 distinct (exchange, key) pairs per process; a flood of random keys stops being counted.
    if u.len() >= 500 && !u.contains_key(&(ctx.env.to_string(), ns.into(), exchange.into(), key.into())) {
        return;
    }
    let e = u.entry((ctx.env.to_string(), ns.into(), exchange.into(), key.into())).or_insert((0, 0, String::new()));
    *e = (e.0 + 1, util::now_ms(), sender.to_string());
}

/// (exchange, routing key, count, last time, last sender) of this namespace.
pub fn unrouted_of(ctx: &Ctx, ns: &str) -> Vec<(String, String, u64, i64, String)> {
    let u = unrouted().lock().unwrap();
    let mut v: Vec<_> = u.iter().filter(|(k, _)| k.0 == *ctx.env && k.1 == ns).map(|(k, v)| (k.2.clone(), k.3.clone(), v.0, v.1, v.2.clone())).collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.3));
    v
}

pub fn forget_unrouted(ctx: &Ctx) {
    unrouted().lock().unwrap().retain(|k, _| k.0 != *ctx.env);
}

/// AMQP 0-9-1 topic exchange matching: words split by ".", "*" one word, "#" zero or more.
pub fn topic_match(pattern: &str, key: &str) -> bool {
    fn go(p: &[&str], k: &[&str]) -> bool {
        match (p.first(), k.first()) {
            (None, None) => true,
            (Some(&"#"), _) => go(&p[1..], k) || (!k.is_empty() && go(p, &k[1..])),
            (Some(&"*"), Some(_)) => go(&p[1..], &k[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &k[1..]),
            _ => false,
        }
    }
    let p: Vec<&str> = if pattern.is_empty() { vec![] } else { pattern.split('.').collect() };
    let k: Vec<&str> = if key.is_empty() { vec![] } else { key.split('.').collect() };
    go(&p, &k)
}

/// Service Bus subscription filters Stuntman understands: empty (all), "subject = X",
/// "<property> = <value>" (quotes optional), "1=1" and "1=0".
pub fn sb_filter(f: &str, o: &Outgoing) -> bool {
    let f = f.trim();
    if f.is_empty() || f == "1=1" || f.eq_ignore_ascii_case("true") {
        return true;
    }
    if f == "1=0" || f.eq_ignore_ascii_case("false") {
        return false;
    }
    let Some((k, v)) = f.split_once('=') else { return true };
    let (k, v) = (k.trim().trim_start_matches("sys.").trim_start_matches("user."), v.trim().trim_matches('\'').trim_matches('"'));
    let have = match k.to_lowercase().as_str() {
        "subject" | "label" => o.subject.clone(),
        "correlationid" => o.sys.get("correlationId").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        "messageid" => o.message_id.clone(),
        _ => o.props.get(k).map(|x| x.as_str().map(String::from).unwrap_or(x.to_string())).unwrap_or_default(),
    };
    have == v
}

fn trace_of(o: &Outgoing) -> Option<String> {
    let tp = o.props.get("traceparent").or(o.props.get("Diagnostic-Id")).and_then(|v| v.as_str())?;
    tp.split('-').nth(1).filter(|t| t.len() == 32).map(str::to_lowercase)
}

// ---------------------------------------------------------------- send

/// Enqueues a message (or its copies, for a topic). Applies the rules and the "next delivery"
/// setting of each holder. Returns the sequence numbers created.
pub async fn publish(ctx: &Ctx, ns: &str, entity: &str, o: Outgoing) -> Result<Vec<i64>, Refusal> {
    let n = config::get(ctx, ns).map_err(|e| Refusal::NotFound(e.message))?;
    if n.behaviour.reachability == "busy" || n.throughput() == Some(Some(0)) {
        return Err(Refusal::Busy(format!("The request was terminated because the namespace {ns} is being throttled. Retry after 1 s.")));
    }
    if let Some(Some(per_s)) = n.throughput() {
        if over_rate(ctx, ns, per_s) {
            return Err(Refusal::Busy(format!("The request was terminated because the namespace {ns} is over {per_s} messages a second.")));
        }
    }
    let seen = rules::Seen { op: if o.scheduled_at.is_some() { "schedule".into() } else { "send".into() }, namespace: ns.into(), entity: entity.into(), subject: o.subject.clone(), props: o.props.clone(),
        session: o.sys.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").into(), size: o.body.len() as u64, sender: o.sender.clone(), delivery_count: 0 };
    let rule = rules::decide(ctx, &n.kind, &seen).await.ok().flatten();
    if let Some(r) = &rule {
        if let Some(why) = refusal_of(&r.then.refuse, ns, entity) {
            return Err(why);
        }
    }
    let holders = route(&n, entity, &o)?;
    if holders.is_empty() && n.kind == config::RABBIT {
        let ex = o.sys.get("exchange").and_then(|v| v.as_str()).unwrap_or(entity);
        note_unrouted(ctx, ns, ex, o.sys.get("routingKey").and_then(|v| v.as_str()).unwrap_or(""), &o.sender);
    }
    let now = util::now_ms();
    let trace = trace_of(&o);
    let mut seqs = vec![];
    let mut rejected = None;
    for h in holders {
        // RabbitMQ max-length: reject-publish refuses the new message (the publisher gets a nack).
        let capped = n.rabbit_queue(&h).and_then(|q| q.max_length.map(|m| (m, q.overflow)));
        if let Some((max, overflow)) = &capped {
            if overflow.starts_with("reject-publish") && ready(ctx, ns, &h).await >= *max {
                rejected = Some(h.clone());
                continue;
            }
        }
        let lim = n.limits(&h);
        let ttl = o.sys.get("timeToLive").and_then(|v| v.as_i64()).or(lim.ttl_ms);
        // What this send will do: a rule first, else the holder's "next delivery".
        let behavior = match &rule {
            Some(r) if !r.then.delivery.is_empty() => Some((r.then.delivery.clone(), format!("rule “{}”", r.name))),
            _ => take_next(ctx, ns, &h).await,
        };
        let (b, why) = behavior.clone().unwrap_or_default();
        let hold = if b == "hold" { rule.as_ref().and_then(|r| r.then.seconds).unwrap_or(30) as i64 * 1000 } else if b == "out_of_order" || b == "reorder" { 3_000 } else { 0 };
        let available = o.scheduled_at.unwrap_or(now).max(now) + hold;
        let state = match b.as_str() {
            "dead_letter" => "dead_letter",
            "drop" => "completed",
            _ if available > now => "scheduled",
            _ => "active",
        };
        let copies = if b == "duplicate" { 2 } else { 1 };
        let mut first = None;
        for i in 0..copies {
            let (ns2, h2, mid, sub, ct, body) = (ns.to_string(), h.clone(), if o.message_id.is_empty() { util::id("msg") } else { o.message_id.clone() }, o.subject.clone(), o.content_type.clone(), o.body.clone());
            let (props, sys) = (Value::Object(o.props.clone()).to_string(), Value::Object(o.sys.clone()).to_string());
            let (sender, via, tr) = (o.sender.clone(), o.via.clone(), trace.clone());
            let st = state.to_string();
            let flags = if b == "lose_lock" { "lose_lock".to_string() } else { String::new() };
            let (dlr, dld) = if state == "dead_letter" { ("DeadLetteredBy".to_string(), format!("dead-lettered on send by {why}")) } else { (String::new(), String::new()) };
            let settled = if state == "completed" { Some(now) } else { None };
            let origin = first;
            let seq: i64 = ctx.db.call(move |c| {
                c.execute("INSERT INTO msg_messages(ns, holder, message_id, subject, content_type, body, props, sys, size, state, enqueued_at, available_at, expires_at, sender, via, trace_id, flags, dl_reason, dl_description, settled_at, origin)
                           VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
                    params![ns2, h2, mid, sub, ct, body.clone(), props, sys, body.len() as i64, st, now, available, ttl.map(|t| available + t), sender, via, tr, flags, dlr, dld, settled, origin])?;
                Ok(c.last_insert_rowid())
            }).await.map_err(|e| Refusal::Busy(e.message))?;
            first.get_or_insert(seq);
            let by = if o.via == "amqp" { format!("Published by {}", o.sender) } else { format!("Published by {} (not the app)", if o.sender.is_empty() { "you" } else { &o.sender }) };
            note(ctx, seq, &by, &format!("{}  ·  {}", if o.scheduled_at.is_some() { "SCHEDULE" } else { "SEND" }, if o.via == "amqp" { "AMQP".to_string() } else { o.via.to_uppercase() }), "").await;
            if !b.is_empty() {
                let (what, tone) = match b.as_str() {
                    "duplicate" if i == 1 => (format!("Duplicate of #{}", first.unwrap()), "attn"),
                    "duplicate" => ("Will be delivered twice".to_string(), "attn"),
                    "hold" => (format!("Held {} s, as you set", hold / 1000), "attn"),
                    "out_of_order" | "reorder" => ("Held back so the next message overtakes it".to_string(), "attn"),
                    "drop" => ("Dropped silently: never delivered".to_string(), "danger"),
                    "dead_letter" => ("Dead-lettered on arrival".to_string(), "danger"),
                    "lose_lock" => ("Its first delivery will lose the lock".to_string(), "attn"),
                    x => (x.to_string(), "attn"),
                };
                note(ctx, seq, &what, &why, tone).await;
            }
            seqs.push(seq);
        }
        ring(ctx, ns, &h);
        // drop-head: past the limit, the oldest ready messages go (dead-lettered when it can).
        if let Some((max, overflow)) = capped.filter(|(_, o)| o == "drop-head") {
            let over = ready(ctx, ns, &h).await - max;
            if over > 0 {
                let (n2, h2) = (ns.to_string(), h.clone());
                let old: Vec<i64> = ctx.db.call(move |c| { let mut s = c.prepare("SELECT seq FROM msg_messages WHERE ns=?1 AND holder=?2 AND state='active' ORDER BY seq LIMIT ?3")?; let v = s.query_map(params![n2, h2, over], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>(); v }).await.unwrap_or_default();
                for seq in old {
                    rabbit_drop(ctx, ns, seq, "maxlen", &format!("the queue holds at most {max} (overflow {overflow}): the oldest goes")).await;
                }
            }
        }
    }
    if let Some(h) = rejected {
        return Err(Refusal::Quota(format!("queue '{h}' in vhost '{ns}' is full (max-length, overflow reject-publish)")));
    }
    if let (Some(r), false) = (&rule, seqs.is_empty()) {
        rules::hit(ctx, &n.kind, &r.name).await;
    }
    // People and agents show up as their own actions; the app's sends are rows here.
    if o.via == "amqp" {
        if let Some(first) = seqs.first() {
            let copies = seqs.len();
            let provoked = rule.is_some();
            let what = if o.subject.is_empty() { "a message".to_string() } else { o.subject.clone() };
            let detail = format!("{}{ns}  ·  {}", if copies > 1 { format!("{copies} copies  ·  ") } else { String::new() }, if o.scheduled_at.is_some() { "scheduled" } else { "sent" });
            tl(ctx, &format!("mq:{first}"), ns, &format!("{} published {what} to {entity}", o.sender), if provoked { "provoked" } else { "ok" }, &detail, trace.as_deref(), &o.sender, &link_of(ns, entity, *first)).await;
        }
    }
    Ok(seqs)
}

/// Ready messages of a queue (what RabbitMQ's max-length counts).
async fn ready(ctx: &Ctx, ns: &str, holder: &str) -> i64 {
    let (n2, h2) = (ns.to_string(), holder.to_string());
    ctx.db.call(move |c| c.query_row("SELECT count(*) FROM msg_messages WHERE ns=?1 AND holder=?2 AND state='active'", params![n2, h2], |r| r.get(0))).await.unwrap_or(0)
}

// ---------------------------------------------------------------- RabbitMQ dead-lettering

/// A RabbitMQ queue lets a message go (max-length, delivery limit): it leaves the queue (the
/// screen keeps it as dead-lettered) and goes to the queue's dead-letter exchange, if any.
pub async fn rabbit_drop(ctx: &Ctx, ns: &str, seq: i64, reason: &str, why: &str) {
    let (r2, now) = (reason.to_string(), util::now_ms());
    let _ = ctx.db.call(move |c| c.execute("UPDATE msg_messages SET state='dead_letter', lock_token='', locked_by='', locked_until=0, dl_reason=?2, dl_description='', settled_at=?3 WHERE seq=?1", params![seq, r2, now])).await;
    note(ctx, seq, &format!("Dropped by RabbitMQ ({reason})"), why, "danger").await;
    rabbit_forward(ctx, ns, seq, reason).await;
}

/// Publishes a copy of a dead-lettered message to its queue's dead-letter exchange, with the
/// x-death headers RabbitMQ adds. No dead-letter exchange: nothing (RabbitMQ discards it).
pub fn rabbit_forward<'a>(ctx: &'a Ctx, ns: &'a str, seq: i64, reason: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let Ok(n) = config::get(ctx, ns) else { return };
        let row: Option<(String, String, String, String, Vec<u8>, String, String)> = ctx.db.call(move |c| c.query_row("SELECT holder, message_id, subject, content_type, body, props, sys FROM msg_messages WHERE seq=?1", [seq], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))).optional()).await.ok().flatten();
        let Some((holder, message_id, subject, content_type, body, props, sys)) = row else { return };
        let Some(q) = n.rabbit_queue(&holder) else { return };
        let Some(dlx) = q.dead_letter_exchange else { return };
        let mut props: Map<String, Value> = serde_json::from_str(&props).unwrap_or_default();
        let mut sys: Map<String, Value> = serde_json::from_str(&sys).unwrap_or_default();
        let (ex, key) = (sys.get("exchange").and_then(|v| v.as_str()).unwrap_or("").to_string(), sys.get("routingKey").and_then(|v| v.as_str()).unwrap_or("").to_string());
        let mut deaths: Vec<Value> = props.get("x-death").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        match deaths.iter_mut().find(|d| d["queue"] == json!(holder) && d["reason"] == json!(reason)) {
            Some(d) => d["count"] = json!(d["count"].as_i64().unwrap_or(0) + 1),
            None => deaths.insert(0, json!({"count": 1, "reason": reason, "queue": holder, "time": util::now_ms() / 1000, "exchange": ex, "routing-keys": [key]})),
        }
        props.insert("x-death".into(), Value::Array(deaths));
        for (k, v) in [("reason", json!(reason)), ("queue", json!(holder)), ("exchange", json!(ex))] {
            props.entry(format!("x-first-death-{k}")).or_insert(v.clone());
            props.insert(format!("x-last-death-{k}"), v);
        }
        sys.insert("exchange".into(), json!(dlx));
        sys.insert("routingKey".into(), json!(q.dead_letter_routing_key.unwrap_or(key)));
        let o = Outgoing { message_id, subject, content_type, body, props, sys, scheduled_at: None, sender: format!("RabbitMQ (dead-lettered from {holder})"), via: "amqp".into() };
        match publish(ctx, ns, &dlx, o).await {
            Ok(seqs) => note(ctx, seq, &format!("Dead-lettered to exchange {dlx}"), &format!("{reason}  ·  {} cop{}", seqs.len(), if seqs.len() == 1 { "y" } else { "ies" }), "attn").await,
            Err(e) => note(ctx, seq, &format!("Dead-letter exchange {dlx} refused it"), e.text(), "danger").await,
        }
    })
}

/// A RabbitMQ unacked message goes back to its queue (the channel closed). Quorum queues count
/// the return in x-delivery-count and, past their delivery limit, drop or dead-letter it.
pub async fn rabbit_return(ctx: &Ctx, ns: &str, token: &str, why: &str, limit: Option<i64>) {
    let t2 = token.to_string();
    let row: Option<(i64, String, String, String)> = ctx.db.call(move |c| c.query_row("SELECT seq, holder, state, props FROM msg_messages WHERE lock_token=?1", [t2], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()).await.ok().flatten();
    let Some((seq, holder, state, props)) = row else { return };
    if state != "locked" {
        return;
    }
    let mut props: Map<String, Value> = serde_json::from_str(&props).unwrap_or_default();
    let returns = props.get("x-delivery-count").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
    if let Some(l) = limit {
        props.insert("x-delivery-count".into(), json!(returns));
        let p = Value::Object(props).to_string();
        let _ = ctx.db.call(move |c| c.execute("UPDATE msg_messages SET props=?2 WHERE seq=?1", params![seq, p])).await;
        if returns > l {
            rabbit_drop(ctx, ns, seq, "delivery_limit", &format!("returned {returns} times; the queue's delivery limit is {l}")).await;
            tl(ctx, &format!("mq:dl:{seq}"), ns, &format!("#{seq} dropped by {holder}: delivery limit"), "failed", &format!("returned {returns} times, limit {l}"), trace_of_seq(ctx, seq).await.as_deref(), "", &link_of(ns, &holder, seq)).await;
            ring(ctx, ns, &holder);
            return;
        }
    }
    let _ = ctx.db.call(move |c| c.execute("UPDATE msg_messages SET state='active', lock_token='', locked_by='', locked_until=0 WHERE seq=?1", [seq])).await;
    note(ctx, seq, "Back to the queue", why, "attn").await;
    ring(ctx, ns, &holder);
}

fn refusal_of(refuse: &str, ns: &str, entity: &str) -> Option<Refusal> {
    match refuse {
        "server_busy" => Some(Refusal::Busy(format!("The request was terminated because the entity {entity} is being throttled (rule). Retry after 1 s."))),
        "quota_exceeded" => Some(Refusal::Quota(format!("The maximum entity size has been reached or exceeded for {ns}/{entity} (rule)."))),
        "entity_not_found" => Some(Refusal::NotFound(format!("The messaging entity '{ns}/{entity}' could not be found (rule)."))),
        "unauthorized" => Some(Refusal::Unauthorized(format!("Unauthorized access. 'Send' claim(s) are required to perform this operation on {ns}/{entity} (rule)."))),
        _ => None,
    }
}

/// A sliding one-second window per namespace for the throughput limit.
fn over_rate(ctx: &Ctx, ns: &str, per_s: u32) -> bool {
    static W: OnceLock<Mutex<HashMap<String, (i64, u32)>>> = OnceLock::new();
    let mut m = W.get_or_init(Default::default).lock().unwrap();
    let now = util::now_ms() / 1000;
    let e = m.entry(format!("{}|{ns}", ctx.env)).or_insert((now, 0));
    if e.0 != now {
        *e = (now, 0);
    }
    e.1 += 1;
    e.1 > per_s
}

/// The holder's "next delivery" setting, used once (times counts down; 0 = every time).
async fn take_next(ctx: &Ctx, ns: &str, holder: &str) -> Option<(String, String)> {
    let (n2, h2) = (ns.to_string(), holder.to_string());
    let row: Option<(String, i64, String)> = ctx.db.call(move |c| c.query_row("SELECT behavior, times, by_who FROM msg_next WHERE ns=?1 AND holder=?2", params![n2, h2], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()).await.ok().flatten();
    let (b, times, by) = row?;
    let (n2, h2) = (ns.to_string(), holder.to_string());
    if times == 1 {
        let _ = ctx.db.call(move |c| c.execute("DELETE FROM msg_next WHERE ns=?1 AND holder=?2", params![n2, h2])).await;
    } else if times > 1 {
        let _ = ctx.db.call(move |c| c.execute("UPDATE msg_next SET times=times-1 WHERE ns=?1 AND holder=?2", params![n2, h2])).await;
    }
    ctx.live.send("messaging.next", &format!("{ns}/{holder}"), "used");
    Some((b, if by.is_empty() { "you".into() } else { format!("set by {by}") }))
}

pub const BEHAVIORS: [(&str, &str); 7] = [("normal", "Normal"), ("lose_lock", "Lose the lock (redeliver)"), ("duplicate", "Deliver twice (duplicate)"), ("hold", "Hold 30 s, then deliver"),
    ("drop", "Drop it silently"), ("dead_letter", "Dead-letter it now"), ("out_of_order", "Deliver out of order (sessions)")];

pub async fn set_next(ctx: &Ctx, ns: &str, holder: &str, behavior: &str, times: i64, by: &str) -> Result<(), ApiError> {
    let b = if behavior.starts_with("hold") { "hold" } else { behavior };
    if !BEHAVIORS.iter().any(|x| x.0 == b) {
        return Err(ApiError::bad(format!("behavior: {}", BEHAVIORS.iter().map(|x| x.0).collect::<Vec<_>>().join(" | "))));
    }
    let (n2, h2, b2, by2) = (ns.to_string(), holder.to_string(), b.to_string(), by.to_string());
    ctx.db.call(move |c| {
        if b2 == "normal" {
            c.execute("DELETE FROM msg_next WHERE ns=?1 AND holder=?2", params![n2, h2])
        } else {
            c.execute("INSERT INTO msg_next VALUES(?1,?2,?3,?4,?5) ON CONFLICT(ns, holder) DO UPDATE SET behavior=?3, times=?4, by_who=?5", params![n2, h2, b2, times, by2])
        }
    }).await?;
    ctx.live.send("messaging.next", &format!("{ns}/{holder}"), "set");
    Ok(())
}

pub async fn next_of(ctx: &Ctx, ns: &str, holder: &str) -> Value {
    let (n2, h2) = (ns.to_string(), holder.to_string());
    ctx.db.call(move |c| c.query_row("SELECT behavior, times, by_who FROM msg_next WHERE ns=?1 AND holder=?2", params![n2, h2], |r| Ok(json!({"behavior": r.get::<_, String>(0)?, "times": r.get::<_, i64>(1)?, "by": r.get::<_, String>(2)?}))).optional())
        .await.ok().flatten().unwrap_or(json!({"behavior": "normal", "times": 0}))
}

// ---------------------------------------------------------------- receive

/// Claims the next available message of a holder for a consumer: locks it and counts the delivery.
/// `lock_ms` None: no expiry (RabbitMQ unacked messages stay with their channel).
pub async fn claim(ctx: &Ctx, ns: &str, holder: &str, consumer: &str, consumer_name: &str, lock_ms: Option<i64>) -> Option<Claimed> {
    claim_in(ctx, ns, holder, consumer, consumer_name, lock_ms, Scope::default()).await
}

/// Where a receiver takes from: one session only, or the dead-letter queue.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    pub session: Option<String>,
    pub dead_letter: bool,
}

pub async fn claim_in(ctx: &Ctx, ns: &str, holder: &str, consumer: &str, consumer_name: &str, lock_ms: Option<i64>, scope: Scope) -> Option<Claimed> {
    // Dead-lettered messages are handed over as they are: no rules on a dead-letter receiver.
    if scope.dead_letter {
        return claim_one(ctx, ns, holder, consumer, consumer_name, lock_ms, &scope).await;
    }
    let kind = config::get(ctx, ns).map(|n| n.kind).unwrap_or_default();
    for _ in 0..50 {
        let mut c = claim_one(ctx, ns, holder, consumer, consumer_name, lock_ms, &scope).await?;
        let seen = rules::Seen { op: "receive".into(), namespace: ns.into(), entity: holder.into(), subject: c.subject.clone(), props: c.props.clone(),
            session: c.sys.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").into(), size: c.body.len() as u64, sender: consumer_name.into(), delivery_count: c.delivery_count };
        let Some(r) = rules::decide(ctx, &kind, &seen).await.ok().flatten() else { return Some(c.locked(ctx, lock_ms).await) };
        if r.then.delivery.is_empty() {
            return Some(c.locked(ctx, lock_ms).await);
        }
        rules::hit(ctx, &kind, &r.name).await;
        let why = format!("rule “{}”", r.name);
        let seq = c.seq;
        let now = util::now_ms();
        match r.then.delivery.as_str() {
            "lose_lock" => {
                c.lose_lock = true;
                note(ctx, seq, "Lock lost, as the rule says", &why, "attn").await;
                return Some(c.locked(ctx, lock_ms).await);
            }
            "duplicate" => {
                let _ = ctx.db.call(move |cn| cn.execute("INSERT INTO msg_messages(ns, holder, message_id, subject, content_type, body, props, sys, size, state, enqueued_at, available_at, expires_at, sender, via, trace_id, origin)
                    SELECT ns, holder, message_id, subject, content_type, body, props, sys, size, 'active', enqueued_at, ?2, expires_at, sender, via, trace_id, seq FROM msg_messages WHERE seq=?1", params![seq, now])).await;
                note(ctx, seq, "Will be delivered twice, as the rule says", &why, "attn").await;
                ring(ctx, ns, holder);
                return Some(c.locked(ctx, lock_ms).await);
            }
            other => {
                let (st, avail, what, tone) = match other {
                    "dead_letter" => ("dead_letter", now, "Dead-lettered instead of delivered", "danger"),
                    "drop" => ("completed", now, "Dropped instead of delivered", "danger"),
                    "hold" => ("scheduled", now + r.then.seconds.unwrap_or(30) as i64 * 1000, "Held back instead of delivered", "attn"),
                    _ => ("active", now + 3_000, "Held back so the next message overtakes it", "attn"),
                };
                let (st2, desc) = (st.to_string(), format!("dead-lettered on receive by {why}"));
                let _ = ctx.db.call(move |cn| cn.execute("UPDATE msg_messages SET state=?2, available_at=?3, lock_token='', locked_by='', locked_until=0, delivery_count=max(delivery_count-1,0),
                    dl_reason=CASE WHEN ?2='dead_letter' THEN 'DeadLetteredBy' ELSE dl_reason END, dl_description=CASE WHEN ?2='dead_letter' THEN ?4 ELSE dl_description END,
                    settled_at=CASE WHEN ?2 IN ('dead_letter','completed') THEN ?3 ELSE settled_at END WHERE seq=?1", params![seq, st2, avail, desc])).await;
                note(ctx, seq, what, &why, tone).await;
                ring(ctx, ns, holder);
            }
        }
    }
    None
}

impl Claimed {
    /// A lost lock is real for Service Bus (the lock expires now, so the settle fails and the
    /// message comes back); RabbitMQ has no locks, so its side requeues on the ack instead.
    async fn locked(mut self, ctx: &Ctx, lock_ms: Option<i64>) -> Claimed {
        if self.lose_lock && lock_ms.is_some() {
            let seq = self.seq;
            let until = util::now_ms() + 200;
            let _ = ctx.db.call(move |c| c.execute("UPDATE msg_messages SET locked_until=?2, flags=flags || ' provoked' WHERE seq=?1", params![seq, until])).await;
            // The message says so too: SDKs that check locked-until before settling see it expire.
            self.locked_until = until;
        }
        self
    }
}

async fn claim_one(ctx: &Ctx, ns: &str, holder: &str, consumer: &str, consumer_name: &str, lock_ms: Option<i64>, scope: &Scope) -> Option<Claimed> {
    let (sess, dlq) = (scope.session.clone().unwrap_or_default(), scope.dead_letter);
    let (n2, h2, who) = (ns.to_string(), holder.to_string(), consumer.to_string());
    let now = util::now_ms();
    let token = uuid4();
    let t2 = token.clone();
    let until = lock_ms.map(|l| now + l).unwrap_or(i64::MAX);
    let row = ctx.db.call(move |c| {
        let tx = c.unchecked_transaction()?;
        let seq: Option<i64> = if dlq {
            tx.query_row("SELECT seq FROM msg_messages WHERE ns=?1 AND holder=?2 AND state='dead_letter' ORDER BY seq LIMIT 1", params![n2, h2], |r| r.get(0)).optional()?
        } else if !sess.is_empty() {
            tx.query_row("SELECT seq FROM msg_messages WHERE ns=?1 AND holder=?2 AND state='active' AND available_at<=?3 AND json_extract(sys, '$.sessionId')=?4 ORDER BY seq LIMIT 1", params![n2, h2, now, sess], |r| r.get(0)).optional()?
        } else {
            tx.query_row("SELECT seq FROM msg_messages WHERE ns=?1 AND holder=?2 AND state='active' AND available_at<=?3 ORDER BY seq LIMIT 1", params![n2, h2, now], |r| r.get(0)).optional()?
        };
        let Some(seq) = seq else { return Ok(None) };
        // Taken from dead-letter: an abandon puts it back there, not on the queue.
        let flag = if dlq { " fromdlq" } else { "" };
        tx.execute("UPDATE msg_messages SET state='locked', locked_until=?2, locked_by=?3, lock_token=?4, delivery_count=delivery_count+1, flags=flags || ?5 WHERE seq=?1", params![seq, until, who, t2, flag])?;
        let r = tx.query_row("SELECT seq, holder, lock_token, locked_until, delivery_count, enqueued_at, message_id, subject, content_type, body, props, sys, flags, dl_reason, dl_description FROM msg_messages WHERE seq=?1", [seq], |r| {
            let mut props: serde_json::Map<String, Value> = serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or_default();
            // Dead-lettered messages carry why, as Service Bus puts it in their application properties.
            if dlq {
                props.insert("DeadLetterReason".into(), json!(r.get::<_, String>(13)?));
                props.insert("DeadLetterErrorDescription".into(), json!(r.get::<_, String>(14)?));
            }
            Ok(Claimed {
                seq: r.get(0)?, holder: r.get(1)?, lock_token: r.get(2)?, locked_until: r.get(3)?, delivery_count: r.get(4)?, enqueued_at: r.get(5)?,
                message_id: r.get(6)?, subject: r.get(7)?, content_type: r.get(8)?, body: r.get(9)?,
                props, sys: serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or_default(),
                lose_lock: r.get::<_, String>(12)?.contains("lose_lock"),
            })
        })?;
        if r.lose_lock {
            tx.execute("UPDATE msg_messages SET flags=replace(flags, 'lose_lock', '') WHERE seq=?1", [seq])?;
        }
        tx.commit()?;
        Ok(Some(r))
    }).await.ok().flatten()?;
    let lock = lock_ms.map(|l| format!("lock {} s", l / 1000)).unwrap_or_else(|| "unacked until the app answers".into());
    note(ctx, row.seq, &format!("Delivered to {consumer_name}"), &if row.delivery_count > 1 { format!("attempt {}  ·  {lock}", row.delivery_count) } else { lock }, "").await;
    ring(ctx, ns, holder);
    Some(row)
}

/// complete | abandon | dead_letter | release (RabbitMQ requeue) | defer
pub async fn settle(ctx: &Ctx, ns: &str, token: &str, outcome: &str, reason: &str, description: &str, consumer_name: &str) -> Result<i64, Refusal> {
    let t2 = token.to_string();
    let row: Option<(i64, String, String, i64, i64, String, String)> = ctx.db.call(move |c| c.query_row("SELECT seq, holder, state, locked_until, delivery_count, locked_by, flags FROM msg_messages WHERE lock_token=?1", [t2], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))).optional()).await.ok().flatten();
    let Some((seq, holder, state, until, count, consumer_id, flags)) = row else {
        return Err(Refusal::LockLost("The lock supplied is invalid. Either the lock expired, or the message has already been removed from the queue.".into()));
    };
    if state != "locked" || until < util::now_ms() {
        return Err(Refusal::LockLost(format!("The lock supplied is invalid. Either the lock expired, or the message has already been removed from the queue. Reference:#{seq}")));
    }
    let n = config::get(ctx, ns).ok();
    let max = n.as_ref().map(|n| n.limits(&holder).max_delivery as i64).unwrap_or(10);
    let now = util::now_ms();
    let from_dlq = flags.contains("fromdlq");
    if from_dlq {
        let _ = ctx.db.call(move |c| c.execute("UPDATE msg_messages SET flags=replace(flags, ' fromdlq', '') WHERE seq=?1", [seq])).await;
    }
    let outcome = if from_dlq && matches!(outcome, "abandon" | "release" | "defer") { "back_to_dlq" } else { outcome };
    let (new_state, what, detail, tone): (&str, String, String, &str) = match outcome {
        "back_to_dlq" => ("dead_letter", format!("Abandoned by {consumer_name}"), "back to the dead-letter queue".into(), "attn"),
        "complete" => ("completed", "Completed".into(), "removed from the queue".into(), "ok"),
        "abandon" | "release" if outcome == "abandon" && count >= max && n.as_ref().map(|n| n.kind == config::SB).unwrap_or(false) =>
            ("dead_letter", "Moved to dead-letter".into(), format!("after the {count}th attempt"), "danger"),
        "abandon" => ("active", format!("Abandoned by {consumer_name}"), if reason.is_empty() { "back to the queue".into() } else { reason.to_string() }, "attn"),
        "release" => ("active", format!("Requeued by {consumer_name}"), if reason.is_empty() { "redelivered".into() } else { reason.to_string() }, "attn"),
        "dead_letter" => ("dead_letter", format!("Dead-lettered by {consumer_name}"), if reason.is_empty() { "the app moved it".into() } else { reason.to_string() }, "danger"),
        "defer" => ("scheduled", format!("Deferred by {consumer_name}"), "kept aside until the app asks for it".into(), "attn"),
        _ => return Err(Refusal::NotFound(format!("unknown outcome {outcome}"))),
    };
    let (dl_r, dl_d) = match (new_state, outcome) {
        ("dead_letter", "dead_letter") => (if reason.is_empty() { "DeadLetteredByReceiver".to_string() } else { reason.to_string() }, description.to_string()),
        ("dead_letter", _) => ("MaxDeliveryCountExceeded".to_string(), format!("Message could not be consumed after {max} delivery attempts.")),
        _ => (String::new(), String::new()),
    };
    let ns2 = new_state.to_string();
    ctx.db.call(move |c| c.execute("UPDATE msg_messages SET state=?2, lock_token='', locked_by='', locked_until=0, settled_at=CASE WHEN ?2 IN ('completed','dead_letter') THEN ?3 ELSE settled_at END,
        available_at=CASE WHEN ?2='scheduled' THEN 9223372036854775807 ELSE available_at END,
        dl_reason=CASE WHEN ?4<>'' THEN ?4 ELSE dl_reason END, dl_description=CASE WHEN ?5<>'' THEN ?5 ELSE dl_description END WHERE seq=?1", params![seq, ns2, now, dl_r, dl_d])).await.map_err(|e| Refusal::Busy(e.message))?;
    note(ctx, seq, &what, &detail, tone).await;
    let trace = trace_of_seq(ctx, seq).await;
    if new_state == "completed" {
        bump_done(ctx, &consumer_id).await;
        let minute = now / 60_000;
        let n_done = done_this_minute(ctx, ns, &holder, minute);
        let label = if n_done <= 1 { format!("{consumer_name} completed #{seq} from {holder}") } else { format!("{consumer_name} completed {n_done} messages from {holder}") };
        tl(ctx, &format!("mq:done:{ns}:{holder}:{consumer_name}:{minute}"), ns, &label, "ok", &if n_done <= 1 { ns.to_string() } else { format!("folded  ·  {n_done} in 1 min") }, trace.as_deref(), consumer_name, &link_of(ns, &holder, seq)).await;
    } else if new_state == "dead_letter" {
        tl(ctx, &format!("mq:dl:{seq}"), ns, &format!("#{seq} moved to dead-letter of {holder}"), "failed", &format!("{}  ·  {consumer_name}", if outcome == "dead_letter" { "dead-lettered by the app".to_string() } else { format!("after {count} attempts") }), trace.as_deref(), consumer_name, &link_of(ns, &holder, seq)).await;
    } else if outcome == "abandon" {
        tl(ctx, &format!("mq:ab:{seq}:{count}"), ns, &format!("{consumer_name} abandoned #{seq} on {holder}"), "failed", &format!("attempt {count} of {max}"), trace.as_deref(), consumer_name, &link_of(ns, &holder, seq)).await;
    }
    ring(ctx, ns, &holder);
    Ok(seq)
}

/// Extends a lock (Service Bus renew-lock): the new locked-until.
pub async fn renew(ctx: &Ctx, ns: &str, token: &str) -> Result<i64, Refusal> {
    let t2 = token.to_string();
    let row: Option<(i64, String, String, i64)> = ctx.db.call(move |c| c.query_row("SELECT seq, holder, state, locked_until FROM msg_messages WHERE lock_token=?1", [t2], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()).await.ok().flatten();
    let Some((seq, holder, state, until)) = row else { return Err(Refusal::LockLost("The lock supplied is invalid.".into())) };
    if state != "locked" || until < util::now_ms() {
        return Err(Refusal::LockLost(format!("The lock supplied is invalid. Reference:#{seq}")));
    }
    let lock = config::get(ctx, ns).map(|n| n.limits(&holder).lock_ms).unwrap_or(30_000);
    let nu = util::now_ms() + lock;
    let _ = ctx.db.call(move |c| c.execute("UPDATE msg_messages SET locked_until=?2 WHERE seq=?1", params![seq, nu])).await;
    Ok(nu)
}

/// Expires the locks a consumer holds now ("Expire its locks").
pub async fn expire_locks(ctx: &Ctx, consumer: &str) -> usize {
    let c2 = consumer.to_string();
    let now = util::now_ms() - 1;
    ctx.db.call(move |c| c.execute("UPDATE msg_messages SET locked_until=?2 WHERE locked_by=?1 AND state='locked'", params![c2, now])).await.unwrap_or(0)
}

/// Messages completed from a holder in this minute (the timeline folds them into one row). Counted in
/// memory: a count over msg_history on every ack grew with the history and slowed a long queue down.
fn done_this_minute(ctx: &Ctx, ns: &str, holder: &str, minute: i64) -> i64 {
    static DONE: OnceLock<Mutex<HashMap<String, (i64, i64)>>> = OnceLock::new();
    let mut m = DONE.get_or_init(Default::default).lock().unwrap();
    m.retain(|_, (min, _)| *min >= minute - 1);
    let e = m.entry(format!("{}|{ns}|{holder}", ctx.env)).or_insert((minute, 0));
    if e.0 != minute {
        *e = (minute, 0);
    }
    e.1 += 1;
    e.1
}

async fn bump_done(ctx: &Ctx, consumer_id: &str) {
    let c2 = consumer_id.to_string();
    let _ = ctx.db.call(move |c| c.execute("UPDATE msg_consumers SET done=done+1 WHERE id=?1", [c2])).await;
}

// ---------------------------------------------------------------- the sweep

/// Every second: expired locks back to the queue (or to dead-letter at max delivery), scheduled
/// messages become active, time-to-live, and completed messages older than keep_completed go.
pub async fn sweep(ctx: &Ctx) {
    let now = util::now_ms();
    let nss = config::list(ctx);
    // Locks that expired.
    let lost: Vec<(i64, String, String, i64, String, String)> = ctx.db.call(move |c| { let mut s = c.prepare("SELECT seq, ns, holder, delivery_count, locked_by, flags FROM msg_messages WHERE state='locked' AND locked_until<?1")?; let v = s.query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<rusqlite::Result<Vec<_>>>(); v }).await.unwrap_or_default();
    for (seq, ns, holder, count, by, flags) in lost {
        // Taken from dead-letter and not settled: it goes back there.
        if flags.contains("fromdlq") {
            let _ = ctx.db.call(move |c| c.execute("UPDATE msg_messages SET state='dead_letter', lock_token='', locked_by='', locked_until=0, flags=replace(flags, ' fromdlq', '') WHERE seq=?1", [seq])).await;
            note(ctx, seq, "Lock lost", "back to the dead-letter queue", "attn").await;
            ring(ctx, &ns, &holder);
            continue;
        }
        let provoked = flags.contains("provoked");
        if provoked {
            let _ = ctx.db.call(move |c| c.execute("UPDATE msg_messages SET flags=replace(flags, ' provoked', '') WHERE seq=?1", [seq])).await;
        }
        let max = nss.iter().find(|n| n.namespace == ns).map(|n| n.limits(&holder).max_delivery as i64).unwrap_or(10);
        let dead = count >= max;
        let (st, r, d) = if dead { ("dead_letter", "MaxDeliveryCountExceeded".to_string(), format!("Message could not be consumed after {max} delivery attempts.")) } else { ("active", String::new(), String::new()) };
        let st2 = st.to_string();
        let _ = ctx.db.call(move |c| c.execute("UPDATE msg_messages SET state=?2, lock_token='', locked_by='', locked_until=0, dl_reason=?3, dl_description=?4, settled_at=CASE WHEN ?2='dead_letter' THEN ?5 ELSE settled_at END WHERE seq=?1", params![seq, st2, r, d, now])).await;
        note(ctx, seq, if dead { "Moved to dead-letter" } else { "Lock lost" }, &if dead { format!("after the {count}th attempt") } else { format!("{by} did not settle it in time; back to the queue") }, if dead { "danger" } else { "attn" }).await;
        let tr = trace_of_seq(ctx, seq).await;
        tl(ctx, &format!("mq:lost:{seq}:{count}"), &ns, &if dead { format!("#{seq} moved to dead-letter of {holder}") } else { format!("Lock on #{seq} lost: back to {holder}") }, if provoked { "provoked" } else { "failed" }, &if dead { format!("MaxDeliveryCountExceeded after {count} attempts") } else if provoked { "a rule made the lock expire".to_string() } else { "not settled in time".to_string() }, tr.as_deref(), "", &link_of(&ns, &holder, seq)).await;
        ring(ctx, &ns, &holder);
    }
    // Scheduled → active.
    let due: Vec<(i64, String, String)> = ctx.db.call(move |c| { let mut s = c.prepare("SELECT seq, ns, holder FROM msg_messages WHERE state='scheduled' AND available_at<=?1")?; let v = s.query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>(); v }).await.unwrap_or_default();
    for (seq, ns, holder) in due {
        let _ = ctx.db.call(move |c| c.execute("UPDATE msg_messages SET state='active' WHERE seq=?1", [seq])).await;
        ring(ctx, &ns, &holder);
    }
    // Time to live.
    let expired: Vec<(i64, String, String)> = ctx.db.call(move |c| { let mut s = c.prepare("SELECT seq, ns, holder FROM msg_messages WHERE state IN ('active','scheduled') AND expires_at IS NOT NULL AND expires_at<?1")?; let v = s.query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>(); v }).await.unwrap_or_default();
    for (seq, ns, holder) in expired {
        let dle = nss.iter().find(|n| n.namespace == ns).map(|n| n.limits(&holder).dead_letter_on_expiration).unwrap_or(false);
        let st = if dle { "dead_letter" } else { "completed" }.to_string();
        let _ = ctx.db.call(move |c| c.execute("UPDATE msg_messages SET state=?2, dl_reason=CASE WHEN ?2='dead_letter' THEN 'TTLExpiredException' ELSE dl_reason END, dl_description=CASE WHEN ?2='dead_letter' THEN 'The message expired and was dead lettered.' ELSE dl_description END, settled_at=?3 WHERE seq=?1", params![seq, st, now])).await;
        note(ctx, seq, if dle { "Expired: moved to dead-letter" } else { "Expired: removed" }, "nobody received it within its time to live", if dle { "danger" } else { "attn" }).await;
        ring(ctx, &ns, &holder);
    }
    // Keep completed for a while, then delete.
    for n in &nss {
        let keep = util::parse_ms(&n.behaviour.keep_completed).unwrap_or(86_400_000) as i64;
        let (ns, cut) = (n.namespace.clone(), now - keep);
        let _ = ctx.db.call(move |c| {
            c.execute("DELETE FROM msg_history WHERE seq IN (SELECT seq FROM msg_messages WHERE ns=?1 AND state='completed' AND settled_at<?2)", params![ns, cut])?;
            c.execute("DELETE FROM msg_messages WHERE ns=?1 AND state='completed' AND settled_at<?2", params![ns, cut])
        }).await;
    }
}

// ---------------------------------------------------------------- reading

pub fn uuid4() -> String {
    let b: [u8; 16] = rand::random();
    format!("{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}", u32::from_be_bytes([b[0], b[1], b[2], b[3]]), u16::from_be_bytes([b[4], b[5]]), u16::from_be_bytes([b[6], b[7]]) & 0x0fff,
        (u16::from_be_bytes([b[8], b[9]]) & 0x3fff) | 0x8000, u64::from_be_bytes([0, 0, b[10], b[11], b[12], b[13], b[14], b[15]]))
}

const COLS: &str = "seq, ns, holder, message_id, subject, content_type, size, state, delivery_count, enqueued_at, available_at, expires_at, locked_until, COALESCE((SELECT client FROM msg_consumers c WHERE c.id=locked_by), locked_by), settled_at, dl_reason, dl_description, sender, via, trace_id, props, sys, origin";

fn row(r: &Row) -> rusqlite::Result<Value> {
    let props: Value = serde_json::from_str(&r.get::<_, String>(20)?).unwrap_or(json!({}));
    let sys: Value = serde_json::from_str(&r.get::<_, String>(21)?).unwrap_or(json!({}));
    let locked_until: i64 = r.get(12)?;
    Ok(json!({"seq": r.get::<_, i64>(0)?, "namespace": r.get::<_, String>(1)?, "entity": r.get::<_, String>(2)?, "messageId": r.get::<_, String>(3)?, "subject": r.get::<_, String>(4)?,
        "contentType": r.get::<_, String>(5)?, "size": r.get::<_, i64>(6)?, "state": r.get::<_, String>(7)?, "deliveryCount": r.get::<_, i64>(8)?, "enqueuedAt": r.get::<_, i64>(9)?,
        "availableAt": r.get::<_, i64>(10)?, "expiresAt": r.get::<_, Option<i64>>(11)?, "lockedUntil": if locked_until == i64::MAX { Value::Null } else { json!(locked_until) }, "lockedBy": r.get::<_, String>(13)?,
        "settledAt": r.get::<_, Option<i64>>(14)?, "deadLetterReason": r.get::<_, String>(15)?, "deadLetterDescription": r.get::<_, String>(16)?, "sender": r.get::<_, String>(17)?,
        "via": r.get::<_, String>(18)?, "traceId": r.get::<_, Option<String>>(19)?, "properties": props, "system": sys, "origin": r.get::<_, Option<i64>>(22)?}))
}

pub async fn list(ctx: &Ctx, ns: &str, holder: &str, state: Option<String>, q: Option<String>, last: u32) -> Result<Vec<Value>, ApiError> {
    let (n2, h2) = (ns.to_string(), holder.to_string());
    ctx.db.call(move |c| {
        let mut w = vec!["ns = ?1".to_string()];
        let mut v: Vec<rusqlite::types::Value> = vec![n2.into()];
        if !h2.is_empty() {
            w.push(format!("(holder = ?{0} OR holder LIKE ?{1})", v.len() + 1, v.len() + 2));
            v.push(h2.clone().into());
            v.push(format!("{h2}/subscriptions/%").into());
        }
        if let Some(s) = state.filter(|s| !s.is_empty() && s != "all") {
            w.push(format!("state = ?{}", v.len() + 1));
            v.push(s.into());
        }
        if let Some(q) = q.filter(|q| !q.is_empty()) {
            w.push(format!("(message_id LIKE ?{0} OR subject LIKE ?{0} OR CAST(seq AS TEXT) = ?{1} OR CAST(body AS TEXT) LIKE ?{0})", v.len() + 1, v.len() + 2));
            v.push(format!("%{q}%").into());
            v.push(q.trim_start_matches('#').to_string().into());
        }
        let sql = format!("SELECT {COLS} FROM msg_messages WHERE {} ORDER BY seq DESC LIMIT {}", w.join(" AND "), last.clamp(1, 1000));
        let mut s = c.prepare(&sql)?;
        let r = s.query_map(rusqlite::params_from_iter(v), row)?.collect::<rusqlite::Result<Vec<_>>>();
        r
    }).await
}

/// Counters per state for a holder (or the whole namespace when holder is empty); a topic sums its subscriptions.
pub async fn counts(ctx: &Ctx, ns: &str, holder: &str) -> Value {
    let (n2, h2) = (ns.to_string(), holder.to_string());
    ctx.db.call(move |c| {
        let mut s = c.prepare("SELECT state, count(*) FROM msg_messages WHERE ns=?1 AND (?2='' OR holder=?2 OR holder LIKE ?2 || '/subscriptions/%') GROUP BY state")?;
        let mut m = Map::new();
        for st in STATES {
            m.insert(st.into(), json!(0));
        }
        for r in s.query_map(params![n2, h2], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (k, v) = r?;
            m.insert(k, json!(v));
        }
        Ok(Value::Object(m))
    }).await.unwrap_or(json!({}))
}

pub async fn get(ctx: &Ctx, ns: &str, seq: i64) -> Result<Value, ApiError> {
    let n2 = ns.to_string();
    let v = ctx.db.call(move |c| c.query_row(&format!("SELECT {COLS} FROM msg_messages WHERE ns=?1 AND seq=?2"), params![n2, seq], row).optional()).await?
        .ok_or_else(|| ApiError::not_found(format!("No message #{seq} in {ns}")))?;
    let (body, hist): (Vec<u8>, Vec<Value>) = ctx.db.call(move |c| {
        let b: Vec<u8> = c.query_row("SELECT body FROM msg_messages WHERE seq=?1", [seq], |r| r.get(0))?;
        let mut s = c.prepare("SELECT at, what, detail, tone FROM msg_history WHERE seq=?1 ORDER BY id")?;
        let h = s.query_map([seq], |r| Ok(json!({"at": r.get::<_, i64>(0)?, "what": r.get::<_, String>(1)?, "detail": r.get::<_, String>(2)?, "tone": r.get::<_, String>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((b, h))
    }).await?;
    let mut v = v;
    let text = String::from_utf8(body.clone()).ok();
    v["body"] = match &text {
        Some(t) => serde_json::from_str::<Value>(t).map(|j| json!(serde_json::to_string_pretty(&j).unwrap_or(t.clone()))).unwrap_or(json!(t)),
        None => json!(format!("{} bytes (binary)", body.len())),
    };
    v["bodyIsJson"] = json!(text.as_deref().map(|t| serde_json::from_str::<Value>(t).is_ok()).unwrap_or(false));
    v["history"] = json!(hist);
    Ok(v)
}

pub async fn raw_body(ctx: &Ctx, seq: i64) -> Option<Vec<u8>> {
    ctx.db.call(move |c| c.query_row("SELECT body FROM msg_messages WHERE seq=?1", [seq], |r| r.get(0)).optional()).await.ok().flatten()
}

/// Resubmit from dead-letter (or anywhere): back to active, with a note.
pub async fn resubmit(ctx: &Ctx, ns: &str, seqs: Vec<i64>, by: &str) -> Result<usize, ApiError> {
    let mut n = 0;
    for seq in seqs {
        let n2 = ns.to_string();
        let h: Option<String> = ctx.db.call(move |c| c.query_row("SELECT holder FROM msg_messages WHERE ns=?1 AND seq=?2 AND state='dead_letter'", params![n2, seq], |r| r.get(0)).optional()).await?;
        let Some(holder) = h else { continue };
        let now = util::now_ms();
        ctx.db.call(move |c| c.execute("UPDATE msg_messages SET state='active', delivery_count=0, available_at=?2, settled_at=NULL WHERE seq=?1", params![seq, now])).await?;
        note(ctx, seq, &format!("Resubmitted by {by}"), "back on the queue, delivery count reset", "ok").await;
        ring(ctx, ns, &holder);
        n += 1;
    }
    Ok(n)
}

/// Move to dead-letter by hand ("dead-lettered by you").
pub async fn dead_letter(ctx: &Ctx, ns: &str, seq: i64, by: &str) -> Result<(), ApiError> {
    let n2 = ns.to_string();
    let h: Option<String> = ctx.db.call(move |c| c.query_row("SELECT holder FROM msg_messages WHERE ns=?1 AND seq=?2 AND state IN ('active','scheduled','locked')", params![n2, seq], |r| r.get(0)).optional()).await?;
    let holder = h.ok_or_else(|| ApiError::new(409, format!("#{seq} is not waiting in a queue")))?;
    let (now, b2) = (util::now_ms(), format!("dead-lettered by {by}"));
    ctx.db.call(move |c| c.execute("UPDATE msg_messages SET state='dead_letter', lock_token='', locked_by='', dl_reason=?3, dl_description='', settled_at=?2 WHERE seq=?1", params![seq, now, b2])).await?;
    note(ctx, seq, &format!("Moved to dead-letter by {by}"), "", "danger").await;
    ring(ctx, ns, &holder);
    Ok(())
}

pub async fn purge(ctx: &Ctx, ns: &str, holder: &str, state: Option<String>) -> Result<usize, ApiError> {
    let (n2, h2) = (ns.to_string(), holder.to_string());
    let n = ctx.db.call(move |c| {
        let st = state.unwrap_or_default();
        c.execute("DELETE FROM msg_history WHERE seq IN (SELECT seq FROM msg_messages WHERE ns=?1 AND (?2='' OR holder=?2 OR holder LIKE ?2 || '/subscriptions/%') AND (?3='' OR state=?3))", params![n2, h2, st])?;
        c.execute("DELETE FROM msg_messages WHERE ns=?1 AND (?2='' OR holder=?2 OR holder LIKE ?2 || '/subscriptions/%') AND (?3='' OR state=?3)", params![n2, h2, st])
    }).await?;
    ring(ctx, ns, holder);
    Ok(n)
}

pub async fn delete(ctx: &Ctx, ns: &str, seq: i64) -> Result<(), ApiError> {
    let n2 = ns.to_string();
    let n = ctx.db.call(move |c| { c.execute("DELETE FROM msg_history WHERE seq=?1", [seq])?; c.execute("DELETE FROM msg_messages WHERE ns=?2 AND seq=?1", params![seq, n2]) }).await?;
    if n == 0 {
        return Err(ApiError::not_found(format!("No message #{seq}")));
    }
    ctx.live.send("messaging.message", ns, "deleted");
    Ok(())
}

// ---------------------------------------------------------------- consumers

pub async fn consumer_open(ctx: &Ctx, id: &str, ns: &str, holder: &str, client: &str, link: &str, mode: &str, prefetch: i64, remote: &str, protocol: &str) {
    let (i, n, h, c2, l, m, r, p) = (id.to_string(), ns.to_string(), holder.to_string(), client.to_string(), link.to_string(), mode.to_string(), remote.to_string(), protocol.to_string());
    let _ = ctx.db.call(move |c| c.execute("INSERT OR REPLACE INTO msg_consumers(id, ns, holder, client, link, mode, prefetch, since, remote, protocol) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![i, n, h, c2, l, m, prefetch, util::now_ms(), r, p])).await;
    ctx.live.send("messaging.consumer", id, "open");
    ctx.live.seen(client.split(" :").next().unwrap_or(client), if protocol == "amqp1" { "Service Bus" } else { "RabbitMQ" });
}

pub async fn consumer_close(ctx: &Ctx, id: &str, reason: &str) {
    let (i, r) = (id.to_string(), reason.to_string());
    let _ = ctx.db.call(move |c| c.execute("UPDATE msg_consumers SET until=?2, reason=?3 WHERE id=?1 AND until IS NULL", params![i, util::now_ms(), r])).await;
    ctx.live.send("messaging.consumer", id, "closed");
}

/// Holders more than one connection receives from now: (holder, the clients). Each gets part of the
/// messages, so a forgotten second app (an old build, another worktree) handles some with old code.
pub async fn shared_receivers(ctx: &Ctx, ns: &str) -> Vec<(String, Vec<String>)> {
    let n2 = ns.to_string();
    let rows: Vec<(String, String)> = ctx.db.call(move |c| {
        let mut s = c.prepare("SELECT DISTINCT holder, client || CASE WHEN remote<>'' THEN ' from ' || remote ELSE '' END FROM msg_consumers WHERE ns=?1 AND until IS NULL ORDER BY holder")?;
        let v = s.query_map([n2], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>();
        v
    }).await.unwrap_or_default();
    let mut out: Vec<(String, Vec<String>)> = vec![];
    for (h, c) in rows {
        match out.last_mut() {
            Some((lh, cs)) if *lh == h => cs.push(c),
            _ => out.push((h, vec![c])),
        }
    }
    out.retain(|(_, cs)| cs.len() > 1);
    out
}

pub async fn consumers(ctx: &Ctx, ns: &str, holder: &str) -> Result<Vec<Value>, ApiError> {
    let (n2, h2) = (ns.to_string(), holder.to_string());
    let day = util::now_ms() - 86_400_000;
    ctx.db.call(move |c| {
        let mut s = c.prepare("SELECT c.id, c.client, c.link, c.mode, c.prefetch, c.since, c.until, c.reason, c.done, c.remote, c.protocol, c.holder,
            (SELECT count(*) FROM msg_messages m WHERE m.locked_by=c.id AND m.state='locked')
            FROM msg_consumers c WHERE c.ns=?1 AND (?2='' OR c.holder=?2 OR c.holder LIKE ?2 || '/subscriptions/%') AND (c.until IS NULL OR c.until>?3) ORDER BY c.until IS NOT NULL, c.client")?;
        let v = s.query_map(params![n2, h2, day], |r| Ok(json!({"id": r.get::<_, String>(0)?, "client": r.get::<_, String>(1)?, "link": r.get::<_, String>(2)?, "mode": r.get::<_, String>(3)?,
            "prefetch": r.get::<_, i64>(4)?, "since": r.get::<_, i64>(5)?, "until": r.get::<_, Option<i64>>(6)?, "reason": r.get::<_, String>(7)?, "doneToday": r.get::<_, i64>(8)?,
            "remote": r.get::<_, String>(9)?, "protocol": r.get::<_, String>(10)?, "entity": r.get::<_, String>(11)?, "locks": r.get::<_, i64>(12)?,
            "state": if r.get::<_, Option<i64>>(6)?.is_some() { "disconnected" } else { "receiving" }})))?.collect::<rusqlite::Result<Vec<_>>>();
        v
    }).await
}

pub async fn consumer_locks(ctx: &Ctx, id: &str) -> Vec<Value> {
    let i = id.to_string();
    ctx.db.call(move |c| { let mut s = c.prepare("SELECT seq, subject, locked_until FROM msg_messages WHERE locked_by=?1 AND state='locked' ORDER BY seq")?; let v = s.query_map([i], |r| { let u: i64 = r.get(2)?; Ok(json!({"seq": r.get::<_, i64>(0)?, "subject": r.get::<_, String>(1)?, "lockedUntil": if u == i64::MAX { Value::Null } else { json!(u) }})) })?.collect::<rusqlite::Result<Vec<_>>>(); v }).await.unwrap_or_default()
}

pub async fn consumer_last(ctx: &Ctx, client: &str) -> Vec<Value> {
    let c2 = format!("% by {client}");
    let c3 = format!("Delivered to {client}");
    ctx.db.call(move |c| { let mut s = c.prepare("SELECT h.at, h.what, h.detail, h.tone, h.seq FROM msg_history h WHERE h.what LIKE ?1 OR h.what = ?2 OR (h.what='Completed' AND h.seq IN (SELECT seq FROM msg_history WHERE what=?2)) ORDER BY h.id DESC LIMIT 12")?;
        let v = s.query_map(params![c2, c3], |r| Ok(json!({"at": r.get::<_, i64>(0)?, "what": r.get::<_, String>(1)?, "detail": r.get::<_, String>(2)?, "tone": r.get::<_, String>(3)?, "seq": r.get::<_, i64>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>(); v }).await.unwrap_or_default()
}

/// Live consumers can be told to go away ("Disconnect it"): the protocol side listens here.
pub fn kills() -> &'static Mutex<HashMap<String, Arc<Notify>>> {
    static K: OnceLock<Mutex<HashMap<String, Arc<Notify>>>> = OnceLock::new();
    K.get_or_init(Default::default)
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn topic_patterns() {
        assert!(topic_match("#", "a.b.c"));
        assert!(topic_match("", ""));
        assert!(topic_match("orders.*", "orders.created"));
        assert!(!topic_match("orders.*", "orders.created.eu"));
        assert!(topic_match("orders.#", "orders.created.eu"));
        assert!(topic_match("*.created", "orders.created"));
        assert!(!topic_match("a", ""));
    }

    #[test]
    fn sb_filters() {
        let mut o = Outgoing { subject: "OrderCreated".into(), ..Default::default() };
        o.props.insert("tenant".into(), json!("globex"));
        assert!(sb_filter("", &o));
        assert!(sb_filter("subject = 'OrderCreated'", &o));
        assert!(!sb_filter("sys.Label = 'X'", &o));
        assert!(sb_filter("tenant = globex", &o));
        assert!(!sb_filter("1=0", &o));
    }
}

// ---------------------------------------------------------------- sessions (Service Bus)

/// Locks a session for a receiver: the one asked for, or the next one with messages nobody holds.
/// Some((session id, locked until)).
pub async fn accept_session(ctx: &Ctx, ns: &str, holder: &str, want: Option<String>, owner: &str, lock_ms: i64) -> Option<(String, i64)> {
    let (n2, h2, o2) = (ns.to_string(), holder.to_string(), owner.to_string());
    let now = util::now_ms();
    let until = now + lock_ms;
    ctx.db.call(move |c| {
        let tx = c.unchecked_transaction()?;
        let free = |sid: &str| -> rusqlite::Result<bool> {
            let held: Option<(String, i64)> = tx.query_row("SELECT owner, locked_until FROM msg_sessions WHERE ns=?1 AND holder=?2 AND sid=?3", params![n2, h2, sid], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            Ok(held.map(|(o, u)| o == o2 || u < now || o.is_empty()).unwrap_or(true))
        };
        let sid = match want {
            Some(s) => if free(&s)? { Some(s) } else { None },
            None => {
                let mut st = tx.prepare("SELECT DISTINCT json_extract(sys, '$.sessionId') FROM msg_messages WHERE ns=?1 AND holder=?2 AND state='active' AND available_at<=?3 AND json_extract(sys, '$.sessionId') IS NOT NULL ORDER BY seq")?;
                let all: Vec<String> = st.query_map(params![n2, h2, now], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                drop(st);
                let mut pick = None;
                for s in all { if free(&s)? { pick = Some(s); break; } }
                pick
            }
        };
        let Some(sid) = sid else { return Ok(None) };
        tx.execute("INSERT INTO msg_sessions(ns, holder, sid, owner, locked_until) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(ns, holder, sid) DO UPDATE SET owner=?4, locked_until=?5", params![n2, h2, sid, o2, until])?;
        tx.commit()?;
        Ok(Some((sid, until)))
    }).await.ok().flatten()
}

pub async fn renew_session(ctx: &Ctx, ns: &str, holder: &str, sid: &str, lock_ms: i64) -> i64 {
    let (n2, h2, s2) = (ns.to_string(), holder.to_string(), sid.to_string());
    let until = util::now_ms() + lock_ms;
    let _ = ctx.db.call(move |c| c.execute("UPDATE msg_sessions SET locked_until=?4 WHERE ns=?1 AND holder=?2 AND sid=?3", params![n2, h2, s2, until])).await;
    until
}

pub async fn release_session(ctx: &Ctx, ns: &str, holder: &str, sid: &str) {
    let (n2, h2, s2) = (ns.to_string(), holder.to_string(), sid.to_string());
    let _ = ctx.db.call(move |c| c.execute("UPDATE msg_sessions SET owner='', locked_until=0 WHERE ns=?1 AND holder=?2 AND sid=?3", params![n2, h2, s2])).await;
}

pub async fn session_state(ctx: &Ctx, ns: &str, holder: &str, sid: &str) -> Option<Vec<u8>> {
    let (n2, h2, s2) = (ns.to_string(), holder.to_string(), sid.to_string());
    ctx.db.call(move |c| c.query_row("SELECT state FROM msg_sessions WHERE ns=?1 AND holder=?2 AND sid=?3", params![n2, h2, s2], |r| r.get::<_, Option<Vec<u8>>>(0)).optional()).await.ok().flatten().flatten()
}

pub async fn set_session_state(ctx: &Ctx, ns: &str, holder: &str, sid: &str, state: Option<Vec<u8>>) {
    let (n2, h2, s2) = (ns.to_string(), holder.to_string(), sid.to_string());
    let _ = ctx.db.call(move |c| c.execute("INSERT INTO msg_sessions(ns, holder, sid, state) VALUES(?1,?2,?3,?4) ON CONFLICT(ns, holder, sid) DO UPDATE SET state=?4", params![n2, h2, s2, state])).await;
}

/// Deferred messages fetched by sequence number (Service Bus receive-by-sequence-number): locked
/// for the receiver and returned.
pub async fn claim_deferred(ctx: &Ctx, ns: &str, holder: &str, seqs: Vec<i64>, consumer: &str, consumer_name: &str, lock_ms: i64) -> Vec<Claimed> {
    let mut out = vec![];
    for seq in seqs {
        let (n2, h2, who) = (ns.to_string(), holder.to_string(), consumer.to_string());
        let token = uuid4();
        let t2 = token.clone();
        let until = util::now_ms() + lock_ms;
        let row = ctx.db.call(move |c| {
            let n = c.execute("UPDATE msg_messages SET state='locked', locked_until=?4, locked_by=?5, lock_token=?6, delivery_count=delivery_count+1
                WHERE ns=?1 AND holder=?2 AND seq=?3 AND state='scheduled' AND available_at=9223372036854775807", params![n2, h2, seq, until, who, t2])?;
            if n == 0 { return Ok(None); }
            c.query_row("SELECT seq, holder, lock_token, locked_until, delivery_count, enqueued_at, message_id, subject, content_type, body, props, sys FROM msg_messages WHERE seq=?1", [seq], |r| Ok(Claimed {
                seq: r.get(0)?, holder: r.get(1)?, lock_token: r.get(2)?, locked_until: r.get(3)?, delivery_count: r.get(4)?, enqueued_at: r.get(5)?, message_id: r.get(6)?, subject: r.get(7)?,
                content_type: r.get(8)?, body: r.get(9)?, props: serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or_default(), sys: serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or_default(), lose_lock: false })).map(Some)
        }).await.ok().flatten();
        if let Some(c) = row {
            note(ctx, seq, &format!("Received by sequence number by {consumer_name}"), "it was deferred", "").await;
            out.push(c);
        }
    }
    out
}
