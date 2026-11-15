//! Azure Event Hubs: namespaces of event hubs with partitions and consumer groups, on the AMQP port
//! Service Bus uses (the SDKs in emulator mode, UseDevelopmentEmulator=true, and the TLS port).
//! The links, $cbs and $management live in amqp1/eventhubs.rs; here: the namespaces (YAML under
//! eventhubs/<ns>.yaml), the events (SQLite, per partition: sequence number, byte offset, enqueued
//! time on the environment clock, retention), partition-key hashing as the service does it,
//! receivers with their owner level (epoch), checkpoints the app keeps in Blob Storage, the
//! provocations and the tools. The Kafka endpoint ($ConnectionString over SASL PLAIN) reads and
//! writes the same partitions through Streams' Kafka port.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use stuntman_core::{clock, env::Envs, tools::Args, util, ApiError, Ctx, Tool};
use tokio::sync::Notify;

// ================================================================ namespaces

fn four() -> u32 {
    4
}
fn default_groups() -> Vec<String> {
    vec!["$Default".into()]
}
fn day() -> String {
    "24h".into()
}
fn yes() -> bool {
    true
}
fn root() -> Vec<String> {
    vec!["RootManageSharedAccessKey".into()]
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Hub {
    #[serde(default = "four")]
    pub partitions: u32,
    #[serde(default = "default_groups")]
    pub consumer_groups: Vec<String>,
    /// How long events stay (a duration like 24h or 7d), on the environment clock.
    #[serde(default = "day")]
    pub retention: String,
    #[serde(default)]
    pub created_at: i64,
}

/// What set_eventhubs changes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Provoke {
    /// The next N sends are refused with ServerBusy (com.microsoft:server-busy); the SDKs retry.
    pub server_busy: u32,
    /// The next N sends are refused with QuotaExceeded (amqp:resource-limit-exceeded).
    pub quota_exceeded: u32,
    /// Wait before accepting each send.
    pub slow_sends_ms: u64,
    /// Hubs that answer "not found" (links and management) while listed.
    pub not_found: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Namespace {
    pub namespace: String,
    /// Any SAS token is accepted; off: only tokens signed with a policy key below.
    #[serde(default = "yes")]
    pub any_key: bool,
    /// Shared access policies; their keys are secrets named eventhubs.<namespace>.<policy>.
    #[serde(default = "root")]
    pub policies: Vec<String>,
    #[serde(default)]
    pub hubs: BTreeMap<String, Hub>,
    #[serde(default)]
    pub provoke: Provoke,
}

impl Namespace {
    pub fn hub(&self, name: &str) -> Option<&Hub> {
        self.hubs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, h)| h).filter(|_| !self.provoke.not_found.iter().any(|x| x.eq_ignore_ascii_case(name)))
    }
    /// The hub's name as configured (the SDKs may change its case).
    pub fn hub_name(&self, name: &str) -> Option<String> {
        self.hubs.keys().find(|k| k.eq_ignore_ascii_case(name)).cloned()
    }
}

fn valid_hub(n: &str) -> Result<(), ApiError> {
    if !n.is_empty() && n.len() <= 256 && n.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c)) {
        Ok(())
    } else {
        Err(ApiError::bad(format!("event hub “{n}”: letters, numbers, dots, dashes or underscores")))
    }
}

fn file(ctx: &Ctx, ns: &str) -> std::path::PathBuf {
    ctx.path("eventhubs").join(format!("{ns}.yaml"))
}

pub fn rel_file(ns: &str) -> String {
    format!("eventhubs/{ns}.yaml")
}

pub fn list(ctx: &Ctx) -> Vec<Namespace> {
    let mut out: Vec<Namespace> = std::fs::read_dir(ctx.path("eventhubs")).into_iter().flatten().flatten()
        .filter(|e| e.path().extension().map(|x| x == "yaml").unwrap_or(false))
        .filter_map(|e| serde_yaml::from_str(&std::fs::read_to_string(e.path()).ok()?).ok())
        .collect();
    out.sort_by(|a, b| a.namespace.cmp(&b.namespace));
    out
}

pub fn get(ctx: &Ctx, ns: &str) -> Option<Namespace> {
    serde_yaml::from_str(&std::fs::read_to_string(file(ctx, ns)).ok()?).ok()
}

static WRITE: Mutex<()> = Mutex::new(());

fn write(ctx: &Ctx, n: &Namespace) -> Result<(), ApiError> {
    let t = serde_yaml::to_string(n).map_err(|e| ApiError::internal(e.to_string()))?;
    std::fs::create_dir_all(ctx.path("eventhubs")).map_err(|e| ApiError::internal(e.to_string()))?;
    let f = file(ctx, &n.namespace);
    let tmp = f.with_extension("yaml.tmp");
    std::fs::write(&tmp, t).and_then(|_| std::fs::rename(&tmp, &f)).map_err(|e| ApiError::internal(e.to_string()))?;
    ctx.live.send("eventhubs.namespace", &n.namespace, "updated");
    Ok(())
}

/// Reads, changes and saves a namespace under the writer lock; creates it (with a fresh key) when missing.
pub fn change<F: FnOnce(&mut Namespace) -> Result<(), ApiError>>(ctx: &Ctx, ns: &str, create: bool, f: F) -> Result<Namespace, ApiError> {
    let _g = WRITE.lock().unwrap();
    let mut n = match get(ctx, ns) {
        Some(n) => n,
        None if create => {
            crate::config::valid_name(ns).map_err(ApiError::bad)?;
            let n = Namespace { namespace: ns.into(), any_key: true, policies: root(), hubs: BTreeMap::new(), provoke: Provoke::default() };
            for p in &n.policies {
                new_key(ctx, ns, p)?;
            }
            n
        }
        None => return Err(ApiError::not_found(format!("No Event Hubs namespace “{ns}”"))),
    };
    f(&mut n)?;
    write(ctx, &n)?;
    Ok(n)
}

fn secret_name(ns: &str, policy: &str) -> String {
    format!("eventhubs.{ns}.{policy}")
}

pub fn key_value(ctx: &Ctx, ns: &str, policy: &str) -> String {
    util::resolve_secret(&format!("secret://{}", secret_name(ns, policy)), &ctx.data_dir)
}

fn new_key(ctx: &Ctx, ns: &str, policy: &str) -> Result<(), ApiError> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    util::set_secret(&ctx.data_dir, &secret_name(ns, policy), &B64.encode(b)).map(|_| ()).map_err(|e| ApiError::internal(e.to_string()))
}

/// A SAS token signed with one of the namespace's policy keys (any token when any_key is on).
pub fn token_ok(ctx: &Ctx, n: &Namespace, token: &str) -> bool {
    n.any_key || crate::amqp1::sas_valid(token, |skn| n.policies.iter().any(|p| p == skn).then(|| key_value(ctx, &n.namespace, skn)))
}

fn host_of(addr: &str) -> String {
    let rest = addr.split_once("://").map(|(_, r)| r).unwrap_or(addr);
    rest.split(['/', ':']).next().unwrap_or("").to_lowercase()
}

/// The namespace an address names: "<ns>.localhost[:port]/…" or "<ns>.<env>.localhost/…". For
/// "localhost" (the official emulator's connection string) or an IP, the namespace holding the hub
/// the path names. Never creates one: create_eventhub does.
pub fn resolve(envs: &Envs, addr: &str) -> Option<(Ctx, Namespace)> {
    let host = host_of(addr);
    let (first, rest) = host.split_once('.').unwrap_or((host.as_str(), ""));
    if first == "localhost" || first.parse::<u8>().is_ok() || first.is_empty() || first == "[" {
        let path = addr.split_once("://").map(|(_, r)| r.split_once('/').map(|(_, p)| p).unwrap_or("")).unwrap_or("");
        let hub = path.trim_matches('/').split('/').next().filter(|h| !h.is_empty() && !h.starts_with('$'))?;
        let ctx = envs.default_env()?;
        let n = list(&ctx).into_iter().find(|n| n.hub_name(hub).is_some())?;
        return Some((ctx, n));
    }
    let ctx = envs.by_host(Some(rest)).or_else(|| envs.default_env())?;
    get(&ctx, first).map(|n| (ctx, n))
}

/// The connection string an app uses: the namespace's host on Stuntman's AMQP port, in emulator mode.
pub fn connection(ctx: &Ctx, n: &Namespace, hub: Option<&str>) -> String {
    let host = if ctx.prefix().is_empty() { format!("{}.localhost", n.namespace) } else { format!("{}.{}.localhost", n.namespace, ctx.env) };
    let policy = n.policies.first().cloned().unwrap_or_default();
    let key = key_value(ctx, &n.namespace, &policy);
    let entity = hub.map(|h| format!(";EntityPath={h}")).unwrap_or_default();
    format!("Endpoint=sb://{host}:{}/;SharedAccessKeyName={policy};SharedAccessKey={key}{entity};UseDevelopmentEmulator=true", crate::port())
}

// ================================================================ partition keys

fn mix(a: &mut u32, b: &mut u32, c: &mut u32) {
    *a = a.wrapping_sub(*c); *a ^= c.rotate_left(4); *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a); *b ^= a.rotate_left(6); *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b); *c ^= b.rotate_left(8); *b = b.wrapping_add(*a);
    *a = a.wrapping_sub(*c); *a ^= c.rotate_left(16); *c = c.wrapping_add(*b);
    *b = b.wrapping_sub(*a); *b ^= a.rotate_left(19); *a = a.wrapping_add(*c);
    *c = c.wrapping_sub(*b); *c ^= b.rotate_left(4); *b = b.wrapping_add(*a);
}

fn fin(a: &mut u32, b: &mut u32, c: &mut u32) {
    *c ^= *b; *c = c.wrapping_sub(b.rotate_left(14));
    *a ^= *c; *a = a.wrapping_sub(c.rotate_left(11));
    *b ^= *a; *b = b.wrapping_sub(a.rotate_left(25));
    *c ^= *b; *c = c.wrapping_sub(b.rotate_left(16));
    *a ^= *c; *a = a.wrapping_sub(c.rotate_left(4));
    *b ^= *a; *b = b.wrapping_sub(a.rotate_left(14));
    *c ^= *b; *c = c.wrapping_sub(b.rotate_left(24));
}

/// Bob Jenkins' lookup3 hashlittle2 (seeds 0, 0): (c, b).
fn lookup3(d: &[u8]) -> (u32, u32) {
    let mut a = 0xdeadbeefu32.wrapping_add(d.len() as u32);
    let (mut b, mut c) = (a, a);
    let word = |s: &[u8], i: usize| -> u32 { (0..4).map(|j| (s.get(i + j).copied().unwrap_or(0) as u32) << (8 * j)).fold(0, |x, y| x | y) };
    let mut p = 0;
    let mut left = d.len();
    while left > 12 {
        a = a.wrapping_add(word(d, p));
        b = b.wrapping_add(word(d, p + 4));
        c = c.wrapping_add(word(d, p + 8));
        mix(&mut a, &mut b, &mut c);
        p += 12;
        left -= 12;
    }
    if left == 0 {
        return (c, b);
    }
    let tail = &d[p..];
    a = a.wrapping_add(word(tail, 0));
    if left > 4 { b = b.wrapping_add(word(tail, 4)); }
    if left > 8 { c = c.wrapping_add(word(tail, 8)); }
    fin(&mut a, &mut b, &mut c);
    (c, b)
}

/// The partition a key goes to, as the service and the .NET SDK's PartitionResolver pick it:
/// lookup3 of the UTF-8 key, folded to a 16-bit signed value, |hash % count|.
pub fn partition_for_key(key: &str, count: u32) -> u32 {
    if key.is_empty() || count == 0 {
        return 0;
    }
    let (c, b) = lookup3(key.as_bytes());
    let h = ((c ^ b) & 0xffff) as u16 as i16 as i32;
    (h % count as i32).unsigned_abs()
}

fn round_robin(count: u32) -> u32 {
    static N: AtomicUsize = AtomicUsize::new(0);
    (N.fetch_add(1, Ordering::Relaxed) % count.max(1) as usize) as u32
}

pub fn pick_partition(hub: &Hub, key: &str) -> u32 {
    if key.is_empty() { round_robin(hub.partitions) } else { partition_for_key(key, hub.partitions) }
}

// ================================================================ events

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS eh_events(id INTEGER PRIMARY KEY AUTOINCREMENT, ns TEXT NOT NULL, hub TEXT NOT NULL, part INTEGER NOT NULL, seq INTEGER NOT NULL,
  off INTEGER NOT NULL, at INTEGER NOT NULL, pkey TEXT NOT NULL DEFAULT '', raw BLOB NOT NULL, body BLOB NOT NULL, props TEXT NOT NULL DEFAULT '{}',
  sys TEXT NOT NULL DEFAULT '{}', sender TEXT NOT NULL DEFAULT '', via TEXT NOT NULL DEFAULT '');
CREATE UNIQUE INDEX IF NOT EXISTS eh_pos ON eh_events(ns, hub, part, seq);
CREATE TABLE IF NOT EXISTS eh_marks(ns TEXT NOT NULL, hub TEXT NOT NULL, part INTEGER NOT NULL, next_seq INTEGER NOT NULL, next_off INTEGER NOT NULL,
  last_at INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(ns, hub, part));
";

/// An event to append. `raw` is the AMQP message as the app sent it (re-sent with the x-opt
/// annotations added); body, properties and system properties are what the tools show.
#[derive(Clone, Debug, Default)]
pub struct New {
    pub raw: Vec<u8>,
    pub body: Vec<u8>,
    pub props: Map<String, Value>,
    pub sys: Map<String, Value>,
    pub pkey: String,
    pub sender: String,
    pub via: String,
}

#[derive(Clone, Debug, Default)]
pub struct Event {
    pub partition: u32,
    pub seq: i64,
    pub offset: i64,
    pub at: i64,
    pub pkey: String,
    pub raw: Vec<u8>,
    pub body: Vec<u8>,
    pub props: Map<String, Value>,
    pub sys: Map<String, Value>,
    pub sender: String,
    pub via: String,
}

impl Event {
    pub fn view(&self) -> Value {
        let text = std::str::from_utf8(&self.body).ok().map(String::from);
        let mut v = json!({"partition": self.partition.to_string(), "sequenceNumber": self.seq, "offset": self.offset.to_string(), "enqueuedAt": self.at,
            "partitionKey": if self.pkey.is_empty() { Value::Null } else { json!(self.pkey) }, "properties": self.props, "system": self.sys, "sender": self.sender, "via": self.via});
        match text {
            Some(t) => v["body"] = json!(t),
            None => {
                use base64::{engine::general_purpose::STANDARD as B64, Engine};
                v["bodyBase64"] = json!(B64.encode(&self.body));
            }
        }
        v
    }
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Event> {
    Ok(Event {
        partition: r.get::<_, i64>(0)? as u32,
        seq: r.get(1)?,
        offset: r.get(2)?,
        at: r.get(3)?,
        pkey: r.get(4)?,
        raw: r.get(5)?,
        body: r.get(6)?,
        props: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
        sys: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
        sender: r.get(9)?,
        via: r.get(10)?,
    })
}
const COLS: &str = "part, seq, off, at, pkey, raw, body, props, sys, sender, via";

fn bell(env: &str) -> Arc<Notify> {
    static B: OnceLock<Mutex<HashMap<String, Arc<Notify>>>> = OnceLock::new();
    B.get_or_init(Default::default).lock().unwrap().entry(env.to_string()).or_default().clone()
}

pub fn wake(ctx: &Ctx) -> Arc<Notify> {
    bell(&ctx.env)
}

/// Appends events to one partition; their (sequence number, offset).
pub async fn append(ctx: &Ctx, ns: &str, hub: &str, part: u32, evs: Vec<New>) -> Result<Vec<(i64, i64)>, ApiError> {
    let (n, h, at) = (ns.to_string(), hub.to_string(), clock::now(ctx));
    let out = ctx.db.call(move |c| {
        let tx = c.transaction()?;
        let (mut seq, mut off): (i64, i64) = tx.query_row("SELECT next_seq, next_off FROM eh_marks WHERE ns=?1 AND hub=?2 AND part=?3", params![n, h, part], |r| Ok((r.get(0)?, r.get(1)?))).optional()?.unwrap_or((0, 0));
        let mut out = vec![];
        for e in evs {
            tx.execute(&format!("INSERT INTO eh_events(ns, hub, {COLS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)"),
                params![n, h, part, seq, off, at, e.pkey, e.raw, e.body, Value::Object(e.props).to_string(), Value::Object(e.sys).to_string(), e.sender, e.via])?;
            out.push((seq, off));
            seq += 1;
            off += e.raw.len().max(1) as i64;
        }
        tx.execute("INSERT OR REPLACE INTO eh_marks(ns, hub, part, next_seq, next_off, last_at) VALUES(?1,?2,?3,?4,?5,?6)", params![n, h, part, seq, off, at])?;
        tx.commit()?;
        Ok(out)
    }).await?;
    bell(&ctx.env).notify_waiters();
    ctx.live.send("eventhubs.event", hub, "changed");
    Ok(out)
}

/// Events of a partition from a sequence number on.
pub async fn read(ctx: &Ctx, ns: &str, hub: &str, part: u32, from_seq: i64, max: usize) -> Result<Vec<Event>, ApiError> {
    let (n, h) = (ns.to_string(), hub.to_string());
    ctx.db.call(move |c| {
        let mut st = c.prepare(&format!("SELECT {COLS} FROM eh_events WHERE ns=?1 AND hub=?2 AND part=?3 AND seq>=?4 ORDER BY seq LIMIT ?5"))?;
        let v = st.query_map(params![n, h, part, from_seq, max as i64], row)?.collect::<Result<Vec<_>, _>>();
        v
    }).await
}

/// Newest events first, of one partition or every one.
pub async fn latest(ctx: &Ctx, ns: &str, hub: &str, part: Option<u32>, last: u32) -> Result<Vec<Event>, ApiError> {
    let (n, h) = (ns.to_string(), hub.to_string());
    ctx.db.call(move |c| {
        let mut st = c.prepare(&format!("SELECT {COLS} FROM eh_events WHERE ns=?1 AND hub=?2 AND (?3 < 0 OR part=?3) ORDER BY at DESC, id DESC LIMIT ?4"))?;
        let v = st.query_map(params![n, h, part.map(|p| p as i64).unwrap_or(-1), last], row)?.collect::<Result<Vec<_>, _>>();
        v
    }).await
}

/// What a partition holds: begin and last sequence numbers, last offset and time, count.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    pub id: String,
    pub begin_sequence_number: i64,
    pub last_enqueued_sequence_number: i64,
    pub last_enqueued_offset: String,
    pub last_enqueued_time_utc: i64,
    pub is_empty: bool,
    pub count: i64,
}

pub async fn part(ctx: &Ctx, ns: &str, hub: &str, p: u32) -> Result<Part, ApiError> {
    let (n, h) = (ns.to_string(), hub.to_string());
    ctx.db.call(move |c| {
        let (next, next_off, last_at): (i64, i64, i64) = c.query_row("SELECT next_seq, next_off, last_at FROM eh_marks WHERE ns=?1 AND hub=?2 AND part=?3", params![n, h, p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?.unwrap_or((0, 0, 0));
        let (min, cnt): (Option<i64>, i64) = c.query_row("SELECT MIN(seq), COUNT(*) FROM eh_events WHERE ns=?1 AND hub=?2 AND part=?3", params![n, h, p], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let last_off: Option<i64> = c.query_row("SELECT off FROM eh_events WHERE ns=?1 AND hub=?2 AND part=?3 ORDER BY seq DESC LIMIT 1", params![n, h, p], |r| r.get(0)).optional()?;
        Ok(Part {
            id: p.to_string(),
            begin_sequence_number: min.unwrap_or(next),
            last_enqueued_sequence_number: next - 1,
            last_enqueued_offset: last_off.map(|o| o.to_string()).unwrap_or_else(|| if next == 0 { "-1".into() } else { (next_off - 1).to_string() }),
            last_enqueued_time_utc: last_at,
            is_empty: cnt == 0,
            count: cnt,
        })
    }).await
}

/// Where a receiver starts, from its selector filter: offset, sequence number or enqueued time
/// (`>` or `>=`), "-1" (the start) or "@latest".
#[derive(Clone, Debug, PartialEq)]
pub enum From {
    Start,
    Latest,
    Seq(i64, bool),
    Offset(i64, bool),
    Time(i64, bool),
}

/// "amqp.annotation.x-opt-offset > '-1'" → From.
pub fn parse_selector(s: &str) -> Option<From> {
    let s = s.trim();
    let rest = s.strip_prefix("amqp.annotation.")?;
    let (field, rest) = rest.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    let (incl, val) = if let Some(v) = rest.strip_prefix(">=") { (true, v) } else if let Some(v) = rest.strip_prefix('>') { (false, v) } else if let Some(v) = rest.strip_prefix('=') { (true, v) } else { return None };
    let val = val.trim().trim_matches('\'').trim_matches('"');
    match field {
        "x-opt-offset" if val == "-1" => Some(From::Start),
        "x-opt-offset" if val == "@latest" => Some(From::Latest),
        "x-opt-offset" => val.parse().ok().map(|v| From::Offset(v, incl)),
        "x-opt-sequence-number" => val.parse().ok().map(|v: i64| if v < 0 && !incl { From::Start } else { From::Seq(v, incl) }),
        "x-opt-enqueued-time" => val.parse().ok().map(|v| From::Time(v, incl)),
        _ => None,
    }
}

/// The first sequence number a receiver reads.
pub async fn start_seq(ctx: &Ctx, ns: &str, hub: &str, p: u32, from: &From) -> Result<i64, ApiError> {
    let (n, h, f) = (ns.to_string(), hub.to_string(), from.clone());
    ctx.db.call(move |c| {
        let next: i64 = c.query_row("SELECT next_seq FROM eh_marks WHERE ns=?1 AND hub=?2 AND part=?3", params![n, h, p], |r| r.get(0)).optional()?.unwrap_or(0);
        let first = |col: &str, v: i64, incl: bool| -> rusqlite::Result<i64> {
            let q = format!("SELECT MIN(seq) FROM eh_events WHERE ns=?1 AND hub=?2 AND part=?3 AND {col} {} ?4", if incl { ">=" } else { ">" });
            Ok(c.query_row(&q, params![n, h, p, v], |r| r.get::<_, Option<i64>>(0))?.unwrap_or(next))
        };
        match f {
            From::Start => Ok(0),
            From::Latest => Ok(next),
            From::Seq(v, incl) => Ok(if incl { v } else { v + 1 }.max(0)),
            From::Offset(v, incl) => first("off", v, incl),
            From::Time(v, incl) => first("at", v, incl),
        }
    }).await
}

/// Retention on the environment clock: events older than their hub's retention go.
pub async fn sweep(ctx: &Ctx) {
    let now = clock::now(ctx);
    for n in list(ctx) {
        for (h, hub) in &n.hubs {
            let Some(keep) = util::parse_ms(&hub.retention) else { continue };
            let (ns, hn, before) = (n.namespace.clone(), h.clone(), now - keep as i64);
            // ponytail: a delete per hub every pass; an index on `at` if hubs get large.
            let gone = ctx.db.call(move |c| c.execute("DELETE FROM eh_events WHERE ns=?1 AND hub=?2 AND at < ?3", params![ns, hn, before])).await.unwrap_or(0);
            if gone > 0 {
                ctx.live.send("eventhubs.event", h, "expired");
            }
        }
    }
}

/// Checkpoints the app keeps with BlobCheckpointStore: blobs "<fqns>/<hub>/<group>/checkpoint/<p>"
/// with offset and sequencenumber metadata, in any storage account of the environment.
async fn checkpoints(ctx: &Ctx, ns: &str, hub: &str) -> Vec<Value> {
    let like = format!("{}%/{}/%/checkpoint/%", ns.to_lowercase(), hub.to_lowercase());
    ctx.db.call(move |c| {
        let Ok(mut st) = c.prepare("SELECT account, container, path, meta, at FROM storage_blobs WHERE current=1 AND deleted_at IS NULL AND lower(path) LIKE ?1") else { return Ok(vec![]) };
        let v = st.query_map([like], |r| {
            let path: String = r.get(2)?;
            let meta: Map<String, Value> = serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default();
            let seg: Vec<&str> = path.split('/').collect();
            let m = |k: &str| meta.iter().find(|(x, _)| x.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone()).unwrap_or(Value::Null);
            Ok(json!({"consumerGroup": seg.get(seg.len().saturating_sub(3)).copied().unwrap_or(""), "partition": seg.last().copied().unwrap_or(""),
                "offset": m("offset"), "sequenceNumber": m("sequencenumber"), "account": r.get::<_, String>(0)?, "container": r.get::<_, String>(1)?, "at": r.get::<_, i64>(4)?}))
        })?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await.unwrap_or_default()
}

// ================================================================ receivers and connections

/// A receiver reading a partition of a consumer group now.
#[derive(Clone)]
pub struct Reader {
    pub id: u64,
    pub key: String,
    pub epoch: Option<i64>,
    pub client: String,
    pub remote: String,
    pub since: i64,
    /// The last sequence number it was sent (-1: none yet).
    pub pos: Arc<AtomicI64>,
    /// Set when it must go: (condition, description).
    pub kick: Arc<Mutex<Option<(String, String)>>>,
    pub wake: Arc<Notify>,
}

fn readers() -> &'static Mutex<Vec<Reader>> {
    static R: OnceLock<Mutex<Vec<Reader>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

pub fn reader_key(ctx: &Ctx, ns: &str, hub: &str, group: &str, p: u32) -> String {
    format!("{}|{ns}|{}|{}|{p}", ctx.env, hub.to_lowercase(), group.to_lowercase())
}

fn kick(r: &Reader, cond: &str, text: String) {
    *r.kick.lock().unwrap() = Some((cond.into(), text));
    r.wake.notify_waiters();
    r.wake.notify_one();
}

/// A receiver joins its partition, Event Hubs' way: a higher owner level (epoch) disconnects the
/// receivers below it; a lower one, or one without, is refused while a higher one reads; at most 5
/// receivers per partition and consumer group.
pub fn join(key: &str, epoch: Option<i64>, client: &str, remote: &str, wake: Arc<Notify>) -> Result<Reader, (String, String)> {
    static ID: AtomicU64 = AtomicU64::new(1);
    let mut all = readers().lock().unwrap();
    let here: Vec<Reader> = all.iter().filter(|r| r.key == key).cloned().collect();
    let top = here.iter().filter_map(|r| r.epoch).max();
    match (epoch, top) {
        (Some(e), Some(t)) if e < t => return Err(("amqp:link:stolen".into(), format!("Receiver 'nil' with higher epoch of '{t}' already exists. Receiver 'nil' with epoch {e} cannot be created. Make sure you are creating receiver with increasing epoch value to ensure connectivity, or ensure all old epoch receivers are closed or disconnected."))),
        (None, Some(t)) => return Err(("amqp:link:stolen".into(), format!("Receiver 'nil' with higher epoch of '{t}' already exists. Receiver 'nil' with epoch 0 cannot be created. Make sure you are creating receiver with increasing epoch value to ensure connectivity, or ensure all old epoch receivers are closed or disconnected."))),
        (None, None) if here.len() >= 5 => return Err(("amqp:resource-limit-exceeded".into(), "Exceeded the maximum number of allowed receivers per partition in a consumer group which is 5.".into())),
        (Some(e), _) => {
            for r in &here {
                if r.epoch.map(|x| x <= e).unwrap_or(true) {
                    kick(r, "amqp:link:stolen", format!("New receiver 'nil' with higher epoch of '{e}' is created hence current receiver 'nil' with epoch '{}' is getting disconnected. If you are recreating the receiver, make sure a higher epoch is used.", r.epoch.unwrap_or(0)));
                }
            }
            all.retain(|r| !(r.key == key && r.epoch.map(|x| x <= e).unwrap_or(true)));
        }
        _ => {}
    }
    let r = Reader { id: ID.fetch_add(1, Ordering::Relaxed), key: key.into(), epoch, client: client.into(), remote: remote.into(), since: util::now_ms(), pos: Arc::new(AtomicI64::new(-1)), kick: Arc::new(Mutex::new(None)), wake };
    all.push(r.clone());
    Ok(r)
}

pub fn leave(id: u64) {
    readers().lock().unwrap().retain(|r| r.id != id);
}

/// "Partition owner stolen": the receivers of a hub (every hub when None) go as if a higher epoch came.
fn steal(ctx: &Ctx, ns: &str, hub: Option<&str>) -> usize {
    let prefix = format!("{}|{ns}|{}", ctx.env, hub.map(|h| format!("{}|", h.to_lowercase())).unwrap_or_default());
    let mut all = readers().lock().unwrap();
    let mut n = 0;
    for r in all.iter().filter(|r| r.key.starts_with(&prefix)) {
        kick(r, "amqp:link:stolen", format!("New receiver 'nil' with higher epoch of '{}' is created hence current receiver 'nil' with epoch '{}' is getting disconnected. If you are recreating the receiver, make sure a higher epoch is used. (provoked in Stuntman: set_eventhubs steal_owner)", r.epoch.unwrap_or(0) + 1, r.epoch.unwrap_or(0)));
        n += 1;
    }
    all.retain(|r| !r.key.starts_with(&prefix));
    n
}

/// Connections in Event Hubs mode: (env|ns, remote, kill).
fn conns() -> &'static Mutex<Vec<(String, String, Arc<Notify>)>> {
    static C: OnceLock<Mutex<Vec<(String, String, Arc<Notify>)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

pub fn conn_open(ctx: &Ctx, ns: &str, remote: &str, kill: Arc<Notify>) {
    let mut c = conns().lock().unwrap();
    c.retain(|x| x.1 != remote);
    c.push((format!("{}|{ns}", ctx.env), remote.into(), kill));
}

pub fn conn_closed(remote: &str) {
    conns().lock().unwrap().retain(|x| x.1 != remote);
}

fn drop_conns(ctx: &Ctx, ns: &str) -> usize {
    let k = format!("{}|{ns}", ctx.env);
    let c = conns().lock().unwrap();
    let mine: Vec<&(String, String, Arc<Notify>)> = c.iter().filter(|x| x.0 == k).collect();
    for x in &mine {
        x.2.notify_one();
    }
    mine.len()
}

/// Spends one provoked refusal for a send (busy_only: for a management request, which only
/// ServerBusy refuses): Some((condition, text)).
pub fn send_refusal(ctx: &Ctx, ns: &str, busy_only: bool) -> Option<(&'static str, String)> {
    let n = get(ctx, ns)?;
    if n.provoke.server_busy > 0 {
        let _ = change(ctx, ns, false, |n| { n.provoke.server_busy = n.provoke.server_busy.saturating_sub(1); Ok(()) });
        return Some(("com.microsoft:server-busy", "The request was terminated because the entity is being throttled. Error code : 50002. Sub error : 102. Please wait 4 seconds and try again. (provoked in Stuntman: set_eventhubs server_busy)".into()));
    }
    if n.provoke.quota_exceeded > 0 && !busy_only {
        let _ = change(ctx, ns, false, |n| { n.provoke.quota_exceeded = n.provoke.quota_exceeded.saturating_sub(1); Ok(()) });
        return Some(("amqp:resource-limit-exceeded", "QuotaExceeded: The namespace's throughput units are used up; ingress is refused. (provoked in Stuntman: set_eventhubs quota_exceeded)".into()));
    }
    None
}

pub fn not_found_text(ns: &str, hub: &str) -> String {
    format!("The messaging entity 'sb://{ns}.servicebus.windows.net/{hub}' could not be found. To know more visit https://aka.ms/sbResourceMgrExceptions.")
}

// ================================================================ Kafka endpoint

/// Kafka clients configured for Event Hubs (SASL PLAIN, user "$ConnectionString", the connection
/// string as password) reach the namespace's hubs as topics through Streams' Kafka port: a hub's
/// partitions are the topic's, its sequence numbers the Kafka offsets.
pub struct Kafka;

fn conn_str_ns(envs: &Envs, cs: &str) -> Option<(Ctx, Namespace)> {
    let endpoint = cs.split(';').find_map(|kv| kv.trim().split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case("Endpoint")).map(|(_, v)| v.to_string()))?;
    resolve(envs, &endpoint)
}

impl stuntman_streams::foreign::Foreign for Kafka {
    fn login(&self, ctx: &Ctx, user: &str, password: &str) -> Option<String> {
        if user != "$ConnectionString" {
            return None;
        }
        let (c, n) = conn_str_ns(&ctx.envs, password)?;
        let key = password.split(';').find_map(|kv| kv.trim().split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case("SharedAccessKey")).map(|(_, v)| v.to_string())).unwrap_or_default();
        let policy = password.split(';').find_map(|kv| kv.trim().split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case("SharedAccessKeyName")).map(|(_, v)| v.to_string())).unwrap_or_default();
        let ok = n.any_key || (n.policies.contains(&policy) && key_value(&c, &n.namespace, &policy) == key);
        ok.then_some(n.namespace)
    }

    fn partitions(&self, ctx: &Ctx, ns: &str, topic: &str) -> Option<i32> {
        get(ctx, ns)?.hub(topic).map(|h| h.partitions as i32)
    }

    fn topics(&self, ctx: &Ctx, ns: &str) -> Vec<(String, i32)> {
        get(ctx, ns).map(|n| n.hubs.iter().map(|(k, h)| (k.clone(), h.partitions as i32)).collect()).unwrap_or_default()
    }

    fn append(&self, ctx: Ctx, ns: String, topic: String, part: i32, recs: Vec<stuntman_streams::wire::Rec>, producer: String) -> stuntman_streams::foreign::Fut<Result<i64, String>> {
        Box::pin(async move {
            let n = get(&ctx, &ns).ok_or("no namespace")?;
            let hub = n.hub_name(&topic).ok_or("unknown topic")?;
            if let Some((_, text)) = send_refusal(&ctx, &ns, false) {
                return Err(text);
            }
            let evs = recs.into_iter().map(|r| {
                let props: Map<String, Value> = r.headers.iter().map(|(k, v)| (k.clone(), json!(v.as_ref().map(|x| String::from_utf8_lossy(x).to_string()).unwrap_or_default()))).collect();
                let pkey = r.key.as_ref().map(|k| String::from_utf8_lossy(k).to_string()).unwrap_or_default();
                let body = r.value.clone().unwrap_or_default();
                New { raw: crate::amqp1::kafka_message(&body, &props), body, props, sys: Map::new(), pkey, sender: producer.clone(), via: "kafka".into() }
            }).collect();
            append(&ctx, &ns, &hub, part as u32, evs).await.map(|v| v.first().map(|x| x.0).unwrap_or(0)).map_err(|e| e.message)
        })
    }

    fn marks(&self, ctx: Ctx, ns: String, topic: String, part: i32) -> stuntman_streams::foreign::Fut<(i64, i64)> {
        Box::pin(async move {
            let hub = get(&ctx, &ns).and_then(|n| n.hub_name(&topic)).unwrap_or(topic);
            let p = crate::eventhubs::part(&ctx, &ns, &hub, part as u32).await.unwrap_or_default();
            (p.begin_sequence_number, p.last_enqueued_sequence_number + 1)
        })
    }

    fn read(&self, ctx: Ctx, ns: String, topic: String, part: i32, from: i64, max_bytes: usize) -> stuntman_streams::foreign::Fut<Vec<stuntman_streams::wire::Rec>> {
        Box::pin(async move {
            let hub = get(&ctx, &ns).and_then(|n| n.hub_name(&topic)).unwrap_or(topic);
            let mut out = vec![];
            let mut size = 0;
            for e in crate::eventhubs::read(&ctx, &ns, &hub, part as u32, from, 500).await.unwrap_or_default() {
                size += e.body.len() + 64;
                if size > max_bytes && !out.is_empty() {
                    break;
                }
                out.push(stuntman_streams::wire::Rec {
                    offset: e.seq,
                    ts: e.at,
                    key: (!e.pkey.is_empty()).then(|| e.pkey.clone().into_bytes()),
                    value: Some(e.body.clone()),
                    headers: e.props.iter().map(|(k, v)| (k.clone(), Some(v.as_str().map(String::from).unwrap_or(v.to_string()).into_bytes()))).collect(),
                });
            }
            out
        })
    }
}

// ================================================================ tools

fn ns_of(a: &Args) -> Result<String, ApiError> {
    a.str("namespace").ok_or_else(|| ApiError::bad("“namespace” is required"))
}

fn hub_of(ctx: &Ctx, a: &Args) -> Result<(Namespace, String), ApiError> {
    let ns = ns_of(a)?;
    let n = get(ctx, &ns).ok_or_else(|| ApiError::not_found(format!("No Event Hubs namespace “{ns}”")))?;
    let want = a.str("hub").or(a.str("event_hub")).or(a.str("eventhub")).ok_or_else(|| ApiError::bad("“hub” is required"))?;
    let h = n.hub_name(&want).ok_or_else(|| ApiError::not_found(format!("No event hub “{want}” in {ns}")))?;
    Ok((n, h))
}

fn who(a: &Args) -> (String, String) {
    match a.str("_by").as_deref() {
        Some(b) if b.starts_with("mcp:") => (b.trim_start_matches("mcp:").to_string(), "mcp".into()),
        Some(b) if b.starts_with("api:") => (b.trim_start_matches("api:").to_string(), "api".into()),
        _ => ("you".into(), "you".into()),
    }
}

fn readers_of(ctx: &Ctx, ns: &str, hub: &str, group: &str) -> Vec<Value> {
    let prefix = format!("{}|{ns}|{}|{}|", ctx.env, hub.to_lowercase(), group.to_lowercase());
    readers().lock().unwrap().iter().filter(|r| r.key.starts_with(&prefix)).map(|r| json!({"partition": r.key.rsplit('|').next().unwrap_or(""), "epoch": r.epoch,
        "client": r.client, "remote": r.remote, "since": r.since, "position": r.pos.load(Ordering::Relaxed)})).collect()
}

pub async fn ns_view(ctx: &Ctx, n: &Namespace) -> Value {
    let mut hubs = vec![];
    for (name, h) in &n.hubs {
        let mut parts = vec![];
        let mut total = 0;
        for p in 0..h.partitions {
            let x = part(ctx, &n.namespace, name, p).await.unwrap_or_default();
            total += x.count;
            parts.push(json!(x));
        }
        let cps = checkpoints(ctx, &n.namespace, name).await;
        let groups: Vec<Value> = h.consumer_groups.iter().map(|g| json!({"name": g, "readers": readers_of(ctx, &n.namespace, name, g),
            "checkpoints": cps.iter().filter(|c| c["consumerGroup"].as_str().map(|x| x.eq_ignore_ascii_case(g)).unwrap_or(false)).cloned().collect::<Vec<_>>()})).collect();
        hubs.push(json!({"name": name, "partitionCount": h.partitions, "partitions": parts, "consumerGroups": groups, "retention": h.retention, "createdAt": h.created_at,
            "events": total, "notFound": n.provoke.not_found.iter().any(|x| x.eq_ignore_ascii_case(name)), "connection": connection(ctx, n, Some(name))}));
    }
    let keys: Vec<Value> = n.policies.iter().map(|p| json!({"name": p, "masked": crate::config::mask(&key_value(ctx, &n.namespace, p))})).collect();
    json!({"namespace": n.namespace, "kind": "azure-event-hubs", "label": "Azure Event Hubs", "hubs": hubs, "connection": connection(ctx, n, None), "keys": keys, "anyKey": n.any_key,
        "provoke": n.provoke, "file": rel_file(&n.namespace), "port": crate::port(), "kafka": format!("localhost:{} · SASL PLAIN · user $ConnectionString · password the connection string", stuntman_streams::kafka_port())})
}

fn props_arg(a: &Args) -> Result<Map<String, Value>, ApiError> {
    match a.get("properties") {
        None => Ok(Map::new()),
        Some(Value::Object(o)) => Ok(o.clone()),
        Some(Value::String(s)) => serde_json::from_str::<Map<String, Value>>(s).map_err(|_| ApiError::bad("properties: a JSON object like {\"region\": \"us-east\"}")),
        Some(_) => Err(ApiError::bad("properties: a JSON object")),
    }
}

fn matches(e: &Event, wh: &Map<String, Value>, text: Option<&str>) -> bool {
    let body = String::from_utf8_lossy(&e.body);
    let bj: Value = serde_json::from_slice(&e.body).unwrap_or(Value::Null);
    text.map(|t| body.contains(t)).unwrap_or(true) && wh.iter().all(|(k, v)| {
        let want = v.as_str().map(String::from).unwrap_or(v.to_string());
        let got = if k == "partitionKey" || k == "partition_key" { Some(json!(e.pkey)) } else { e.props.get(k).cloned().or_else(|| bj.get(k).cloned()) };
        got.map(|g| g.as_str().map(String::from).unwrap_or(g.to_string()) == want).unwrap_or(false)
    })
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("list_eventhubs", "Azure Event Hubs namespaces (AMQP port, the SDKs in emulator mode): each event hub with its partitions (begin and last sequence number, last offset, last enqueued time, how many events it keeps), its consumer groups with the receivers reading each partition now (owner level/epoch, position) and the checkpoints the app wrote with BlobCheckpointStore, retention, the connection string (EntityPath per hub), the Kafka endpoint and what set_eventhubs provokes.", |ctx, _a| async move {
            let mut out = vec![];
            for n in list(&ctx) {
                out.push(ns_view(&ctx, &n).await);
            }
            Ok(json!({"namespaces": out, "port": crate::port(), "bindError": crate::amqp::BIND_ERROR.lock().unwrap().clone()}))
        })
        .route("GET", "/eventhubs"),
        Tool::new("create_eventhub", "Creates an event hub (and its namespace when new) with partitions (default 4, can only grow), consumer groups ($Default is always there) and retention (default 24h, on the environment clock). Again on an existing hub: adds partitions and consumer groups, changes retention.", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let hub = a.need("hub")?;
            valid_hub(&hub)?;
            let parts = a.u64("partitions").map(|p| p as u32);
            if parts.map(|p| p == 0 || p > 32).unwrap_or(false) {
                return Err(ApiError::bad("partitions: 1 to 32"));
            }
            let retention = a.str("retention");
            if let Some(r) = &retention {
                util::parse_ms(r).ok_or_else(|| ApiError::bad("retention: a duration like 24h or 7d"))?;
            }
            let groups = a.list("consumer_groups");
            let n = change(&ctx, &ns, true, |n| {
                let name = n.hub_name(&hub).unwrap_or(hub.clone());
                let h = n.hubs.entry(name).or_insert(Hub { partitions: parts.unwrap_or(4), consumer_groups: default_groups(), retention: day(), created_at: util::now_ms() });
                if let Some(p) = parts {
                    if p < h.partitions {
                        return Err(ApiError::bad(format!("partitions: {} now; Event Hubs only lets them grow", h.partitions)));
                    }
                    h.partitions = p;
                }
                for g in groups {
                    valid_hub(g.trim_start_matches('$'))?;
                    if !h.consumer_groups.iter().any(|x| x.eq_ignore_ascii_case(&g)) {
                        h.consumer_groups.push(g);
                    }
                }
                if let Some(r) = retention {
                    h.retention = r;
                }
                Ok(())
            })?;
            Ok(ns_view(&ctx, &n).await)
        })
        .req("namespace", "namespace, e.g. acme-hubs-stg").req("hub", "event hub name").param("partitions", "1 to 32, default 4").param("consumer_groups", "list, e.g. [\"billing\", \"audit\"]").param("retention", "e.g. 24h, 7d")
        .route("POST", "/eventhubs/{namespace}/hubs"),
        Tool::new("list_eventhub_events", "Events of an event hub, newest first, with partition, sequence number, offset, enqueued time (environment clock), partition key, properties, body and who sent it (the app over AMQP or Kafka, you, an agent).", |ctx, a| async move {
            let (n, hub) = hub_of(&ctx, &a)?;
            let p = a.u64("partition").map(|p| p as u32);
            let evs = latest(&ctx, &n.namespace, &hub, p, a.u64("last").unwrap_or(50) as u32).await?;
            let mut parts = vec![];
            for i in 0..n.hubs[&hub].partitions {
                parts.push(json!(part(&ctx, &n.namespace, &hub, i).await?));
            }
            Ok(json!({"namespace": n.namespace, "hub": hub, "partitions": parts, "events": evs.iter().map(|e| e.view()).collect::<Vec<_>>()}))
        })
        .req("namespace", "namespace").req("hub", "event hub").param("partition", "only this partition").param("last", "default 50")
        .route("GET", "/eventhubs/{namespace}/{hub}/events"),
        Tool::new("eventhub_send", "Sends events to an event hub as another producer would; the app's receivers get them. One event (body) or a batch (events: [{body, properties, partition_key}]). partition_key is hashed to a partition as the service does; partition picks one.", |ctx, a| async move {
            let (n, hub) = hub_of(&ctx, &a)?;
            let h = n.hubs[&hub].clone();
            let (by, via) = who(&a);
            let items: Vec<Value> = match a.get("events") {
                Some(Value::Array(v)) => v.clone(),
                Some(Value::String(s)) => serde_json::from_str(s).map_err(|_| ApiError::bad("events: a JSON array"))?,
                _ => vec![json!({"body": a.get("body").or(a.get("text")).cloned().ok_or_else(|| ApiError::bad("“body” (or events) is required"))?, "properties": props_arg(&a)?})],
            };
            let key = a.str("partition_key").or_else(|| items.first().and_then(|x| x["partition_key"].as_str().or(x["partitionKey"].as_str()).map(String::from))).unwrap_or_default();
            let p = match a.u64("partition") {
                Some(p) if p as u32 >= h.partitions => return Err(ApiError::bad(format!("partition: 0 to {}", h.partitions - 1))),
                Some(p) => p as u32,
                None => pick_partition(&h, &key),
            };
            let evs = items.iter().map(|x| {
                let body = match &x["body"] { Value::String(s) => s.clone().into_bytes(), Value::Null => vec![], v => v.to_string().into_bytes() };
                let props = x["properties"].as_object().cloned().unwrap_or_default();
                New { raw: crate::amqp1::kafka_message(&body, &props), body, props, sys: Map::new(), pkey: key.clone(), sender: by.clone(), via: via.clone() }
            }).collect();
            let got = append(&ctx, &n.namespace, &hub, p, evs).await?;
            Ok(json!({"namespace": n.namespace, "hub": hub, "partition": p.to_string(), "sequenceNumbers": got.iter().map(|x| x.0).collect::<Vec<_>>(), "offsets": got.iter().map(|x| x.1.to_string()).collect::<Vec<_>>()}))
        })
        .req("namespace", "namespace").req("hub", "event hub").param("body", "the event body (text or JSON)").param("properties", "application properties, a JSON object")
        .param("partition_key", "hashed to a partition").param("partition", "a partition id instead").param("events", "a batch: [{body, properties}]")
        .route("POST", "/eventhubs/{namespace}/{hub}/events"),
        Tool::new("set_eventhubs", "Event Hubs provocations for a namespace: server_busy (the next N sends get ServerBusy, which the SDKs retry), quota_exceeded (the next N sends get QuotaExceeded), slow_sends (wait before each send is accepted), not_found (hubs that answer 'entity not found' while listed), steal_owner (the receivers of a hub, or all with true, are disconnected as by a higher epoch: OwnershipLost / link stolen), drop_connections (closes every AMQP connection of the namespace now; the SDKs reconnect), any_key. Only what you pass changes; reset=true goes back to normal.", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let slow = a.ms("slow_sends")?;
            let nf = a.get("not_found").map(|_| a.list("not_found"));
            let n = change(&ctx, &ns, false, |n| {
                if a.bool("reset").unwrap_or(false) {
                    n.provoke = Provoke::default();
                }
                if let Some(x) = a.u64("server_busy") { n.provoke.server_busy = x as u32; }
                if let Some(x) = a.u64("quota_exceeded") { n.provoke.quota_exceeded = x as u32; }
                if let Some(x) = slow { n.provoke.slow_sends_ms = x; }
                if let Some(x) = nf { n.provoke.not_found = x; }
                if let Some(x) = a.bool("any_key") { n.any_key = x; }
                Ok(())
            })?;
            let mut done = json!({});
            match a.get("steal_owner") {
                Some(Value::Bool(true)) => done["stolen"] = json!(steal(&ctx, &ns, None)),
                Some(Value::String(h)) if h == "true" => done["stolen"] = json!(steal(&ctx, &ns, None)),
                Some(Value::String(h)) if h != "false" => done["stolen"] = json!(steal(&ctx, &ns, Some(h))),
                _ => {}
            }
            if a.bool("drop_connections").unwrap_or(false) {
                done["dropped"] = json!(drop_conns(&ctx, &ns));
            }
            let mut v = ns_view(&ctx, &n).await;
            v["done"] = done;
            Ok(v)
        })
        .req("namespace", "namespace").param("server_busy", "the next N sends get ServerBusy").param("quota_exceeded", "the next N sends get QuotaExceeded").param("slow_sends", "e.g. 2s; 0 for none")
        .param("not_found", "hubs answering not found, e.g. [\"orders\"]; [] for none").param("steal_owner", "a hub name, or true for every hub").param("drop_connections", "true").param("any_key", "false: only policy keys").param("reset", "true: back to normal")
        .route("PUT", "/eventhubs/{namespace}/settings"),
        Tool::new("wait_for_eventhub_event", "Waits for the first event the app sent (over AMQP or Kafka, not you or an agent) to an event hub that matches: properties or JSON body fields in `where`, text in the body, a partition. 408 after the timeout.", |ctx, a| async move {
            let (n, hub) = hub_of(&ctx, &a)?;
            let since = a.u64("since").map(|x| x as i64).unwrap_or_else(|| clock::now(&ctx) - 2_000);
            let wh: Map<String, Value> = match a.get("where") {
                Some(Value::Object(o)) => o.clone(),
                Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_default(),
                _ => Map::new(),
            };
            let text = a.str("text");
            let p = a.u64("partition").map(|p| p as u32);
            let end = util::now_ms() + a.ms("timeout")?.unwrap_or(30_000) as i64;
            let bell = bell(&ctx.env);
            loop {
                let evs = latest(&ctx, &n.namespace, &hub, p, 500).await?;
                if let Some(e) = evs.iter().rev().find(|e| e.at >= since && (e.via == "amqp" || e.via == "kafka") && matches(e, &wh, text.as_deref())) {
                    return Ok(json!({"event": e.view()}));
                }
                if util::now_ms() > end {
                    return Err(ApiError::new(408, format!("No event from the app on {hub} matched in time")).with(json!({"seen": evs.iter().take(5).map(|e| e.view()).collect::<Vec<_>>()})));
                }
                tokio::select! { _ = bell.notified() => {} _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {} }
            }
        })
        .req("namespace", "namespace").req("hub", "event hub").param("where", "{\"orderId\": \"ORD-1042\"}: properties or JSON body fields").param("text", "text in the body")
        .param("partition", "only this partition").param("since", "ms, default 2 s ago (environment clock)").param("timeout", "default 30s")
        .route("GET", "/eventhubs/{namespace}/{hub}/wait"),
    ]
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn hashing_selectors_and_names() {
        // Values from the Python SDK's lookup3 (compute_hash) for the same keys.
        assert_eq!(lookup3(b""), (0xdeadbeef, 0xdeadbeef));
        assert_eq!(lookup3(b"Four score and seven years ago"), (0x17770551, 0xce7226e6));
        assert_eq!(partition_for_key("", 4), 0);
        assert_eq!(lookup3(b"ORD-1042"), (0x580a86ee, 0x92286262));
        assert_eq!(lookup3(b"twelve chars"), (0xe7c30d24, 0x12af3393));
        assert_eq!(lookup3(b"thirteen char"), (0x7dfefb6b, 0xbfd77e27));
        // Hash codes -14161 and 16055: |code % 4| as the .NET SDK and the service pick.
        assert_eq!((partition_for_key("abc", 4), partition_for_key("twelve chars", 4), partition_for_key("ORD-1042", 4)), (1, 3, 0));
        assert_eq!(parse_selector("amqp.annotation.x-opt-offset > '-1'"), Some(From::Start));
        assert_eq!(parse_selector("amqp.annotation.x-opt-offset > '@latest'"), Some(From::Latest));
        assert_eq!(parse_selector("amqp.annotation.x-opt-offset >= '1024'"), Some(From::Offset(1024, true)));
        assert_eq!(parse_selector("amqp.annotation.x-opt-sequence-number > '5'"), Some(From::Seq(5, false)));
        assert_eq!(parse_selector("amqp.annotation.x-opt-enqueued-time > '1700000000000'"), Some(From::Time(1_700_000_000_000, false)));
        assert_eq!(parse_selector("x > 1"), None);
        let n = Namespace { namespace: "acme-hubs-stg".into(), any_key: true, policies: root(), hubs: [("orders".to_string(), Hub { partitions: 4, consumer_groups: default_groups(), retention: day(), created_at: 0 })].into(), provoke: Provoke::default() };
        assert_eq!(n.hub_name("ORDERS").as_deref(), Some("orders"));
        assert!(n.hub("orders").is_some());
        let y = serde_yaml::to_string(&n).unwrap();
        assert_eq!(serde_yaml::from_str::<Namespace>(&y).unwrap(), n);
        assert_eq!(host_of("sb://acme-hubs-stg.localhost:5673/orders"), "acme-hubs-stg.localhost");
    }
}
