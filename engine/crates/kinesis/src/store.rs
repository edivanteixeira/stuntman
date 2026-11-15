//! Streams, shards (hash key ranges, parents after a reshard), records and consumers in the
//! environment's SQLite, and the Kinesis operations on them. Times come from the environment clock
//! (retention and iterator expiry move with advance_clock).

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::limits::{Limiter, MIB};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS kinesis_streams(name TEXT PRIMARY KEY, arn TEXT NOT NULL, mode TEXT NOT NULL, retention INTEGER NOT NULL DEFAULT 24, created INTEGER NOT NULL,
  tags TEXT NOT NULL DEFAULT '{}', next_seq INTEGER NOT NULL DEFAULT 1, next_shard INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS kinesis_shards(stream TEXT NOT NULL, id TEXT NOT NULL, parent TEXT, adjacent TEXT, start_hash TEXT NOT NULL, end_hash TEXT NOT NULL,
  start_seq INTEGER NOT NULL, end_seq INTEGER, PRIMARY KEY(stream, id));
CREATE TABLE IF NOT EXISTS kinesis_records(stream TEXT NOT NULL, shard TEXT NOT NULL, seq INTEGER NOT NULL, pkey TEXT NOT NULL, data BLOB NOT NULL, at INTEGER NOT NULL,
  producer TEXT NOT NULL DEFAULT '', PRIMARY KEY(stream, seq));
CREATE INDEX IF NOT EXISTS kinesis_records_shard ON kinesis_records(stream, shard, seq);
CREATE TABLE IF NOT EXISTS kinesis_consumers(stream TEXT NOT NULL, name TEXT NOT NULL, arn TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY(stream, name));
CREATE TABLE IF NOT EXISTS kinesis_stream_state(name TEXT PRIMARY KEY, status TEXT NOT NULL DEFAULT 'ACTIVE', ready_at INTEGER NOT NULL DEFAULT 0,
  encryption TEXT NOT NULL DEFAULT 'NONE', key_id TEXT, metrics TEXT NOT NULL DEFAULT '[]');
";

pub const ACCOUNT: &str = "000000000000";
/// An iterator lives 5 minutes, as Kinesis says.
pub const ITERATOR_TTL: i64 = 300_000;
const MAX: u128 = u128::MAX;

/// An AWS error: its __type and message.
#[derive(Debug)]
pub struct KErr {
    pub code: &'static str,
    pub msg: String,
}

pub fn kerr(code: &'static str, msg: impl Into<String>) -> KErr {
    KErr { code, msg: msg.into() }
}

impl From<rusqlite::Error> for KErr {
    fn from(e: rusqlite::Error) -> Self {
        kerr("InternalFailure", e.to_string())
    }
}

pub type R<T> = Result<T, KErr>;

#[derive(Clone, Debug)]
pub struct Shard {
    pub id: String,
    pub parent: Option<String>,
    pub adjacent: Option<String>,
    pub start: u128,
    pub end: u128,
    pub start_seq: i64,
    pub end_seq: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct Stream {
    pub name: String,
    pub arn: String,
    pub mode: String,
    pub retention: i64,
    pub created: i64,
    pub tags: Map<String, Value>,
    pub next_seq: i64,
    pub next_shard: i64,
    pub shards: Vec<Shard>,
    /// CREATING | UPDATING | ACTIVE | DELETING; the transition ends at `ready_at` (wall clock, ms).
    pub status: String,
    pub ready_at: i64,
    /// NONE | KMS (records are not really encrypted).
    pub encryption: String,
    pub key_id: Option<String>,
    pub metrics: Vec<String>,
}

pub fn seq_str(n: i64) -> String {
    format!("49{n:054}")
}

pub fn seq_parse(s: &str) -> R<i64> {
    s.strip_prefix("49").filter(|_| s.len() == 56).and_then(|x| x.parse().ok()).ok_or_else(|| kerr("InvalidArgumentException", format!("StartingSequenceNumber {s} is invalid because it did not come from this stream.")))
}

/// The 128-bit hash Kinesis maps a partition key to: MD5, big-endian.
pub fn hash(pkey: &str) -> u128 {
    u128::from_be_bytes(md5::compute(pkey.as_bytes()).0)
}

/// n shards covering the whole hash key space, evenly.
pub fn uniform(n: u128) -> Vec<(u128, u128)> {
    if n <= 1 { return vec![(0, MAX)] }
    let step = MAX / n + if MAX % n == n - 1 { 1 } else { 0 };
    (0..n).map(|i| (step * i, if i == n - 1 { MAX } else { step * (i + 1) - 1 })).collect()
}

/// "shipments" from a name or arn:aws:kinesis:…:stream/shipments[/consumer/…].
pub fn stream_name(s: &str) -> String {
    let s = s.split_once(":stream/").map(|x| x.1).unwrap_or(s);
    s.split('/').next().unwrap_or(s).to_string()
}

fn now_s(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

impl Shard {
    pub fn open(&self) -> bool {
        self.end_seq.is_none()
    }
    pub fn json(&self) -> Value {
        let mut v = json!({"ShardId": self.id, "HashKeyRange": {"StartingHashKey": self.start.to_string(), "EndingHashKey": self.end.to_string()},
            "SequenceNumberRange": {"StartingSequenceNumber": seq_str(self.start_seq)}});
        if let Some(e) = self.end_seq { v["SequenceNumberRange"]["EndingSequenceNumber"] = json!(seq_str(e)); }
        if let Some(p) = &self.parent { v["ParentShardId"] = json!(p); }
        if let Some(p) = &self.adjacent { v["AdjacentParentShardId"] = json!(p); }
        v
    }
}

impl Stream {
    pub fn shard(&self, id: &str) -> R<&Shard> {
        self.shards.iter().find(|s| s.id == id).ok_or_else(|| kerr("ResourceNotFoundException", format!("Shard {id} in stream {} under account {ACCOUNT} does not exist", self.name)))
    }
    pub fn mode_json(&self) -> Value {
        json!({"StreamMode": self.mode})
    }
    pub fn children(&self, id: &str) -> Vec<Value> {
        self.shards.iter().filter(|c| c.parent.as_deref() == Some(id) || c.adjacent.as_deref() == Some(id)).map(|c| {
            let parents: Vec<String> = [c.parent.clone(), c.adjacent.clone()].into_iter().flatten().collect();
            json!({"ShardId": c.id, "ParentShards": parents,
                "HashKeyRange": {"StartingHashKey": c.start.to_string(), "EndingHashKey": c.end.to_string()}})
        }).collect()
    }
}

fn wall() -> i64 {
    stuntman_core::util::now_ms()
}

/// Ends the transitions that are due: CREATING and UPDATING become ACTIVE, DELETING streams go.
pub fn settle(cn: &Connection, now: i64) -> R<()> {
    cn.execute("UPDATE kinesis_stream_state SET status='ACTIVE' WHERE status IN ('CREATING','UPDATING') AND ready_at<=?1", [now])?;
    let gone: Vec<String> = cn.prepare("SELECT name FROM kinesis_stream_state WHERE status='DELETING' AND ready_at<=?1")?.query_map([now], |r| r.get(0))?.collect::<Result<_, _>>()?;
    for n in gone { purge(cn, &n)? }
    Ok(())
}

fn purge(cn: &Connection, name: &str) -> R<()> {
    for t in ["kinesis_streams WHERE name", "kinesis_shards WHERE stream", "kinesis_records WHERE stream", "kinesis_consumers WHERE stream", "kinesis_stream_state WHERE name"] {
        cn.execute(&format!("DELETE FROM {t}=?1"), [name])?;
    }
    Ok(())
}

/// Starts a transition (UPDATING after a reshard, mode or encryption change) that ends at `ready`.
pub fn transition(cn: &Connection, name: &str, status: &str, ready: i64) -> R<()> {
    cn.execute("INSERT INTO kinesis_stream_state(name, status, ready_at) VALUES(?1,?2,?3) ON CONFLICT(name) DO UPDATE SET status=?2, ready_at=?3", params![name, status, ready])?;
    Ok(())
}

/// What a call may do in the stream's state: control-plane changes need ACTIVE
/// (ResourceInUseException); reads and writes work while UPDATING, not while CREATING or DELETING.
pub fn gate(cn: &Connection, op: &str, name: &str) -> R<()> {
    let s = load(cn, name)?;
    let data = matches!(op, "PutRecord" | "PutRecords" | "GetShardIterator" | "GetRecords" | "SubscribeToShard");
    let control = matches!(op, "DeleteStream" | "SplitShard" | "MergeShards" | "UpdateShardCount" | "UpdateStreamMode" | "StartStreamEncryption" | "StopStreamEncryption"
        | "IncreaseStreamRetentionPeriod" | "DecreaseStreamRetentionPeriod" | "AddTagsToStream" | "RemoveTagsFromStream" | "TagResource" | "UntagResource"
        | "RegisterStreamConsumer" | "EnableEnhancedMonitoring" | "DisableEnhancedMonitoring");
    if data && (s.status == "CREATING" || s.status == "DELETING") {
        return Err(kerr("ResourceNotFoundException", format!("Stream {name} under account {ACCOUNT} not found.")));
    }
    if control && s.status != "ACTIVE" {
        return Err(kerr("ResourceInUseException", format!("Stream {name} under account {ACCOUNT} not ACTIVE, instead in state {}", s.status)));
    }
    Ok(())
}

pub fn load(cn: &Connection, name: &str) -> R<Stream> {
    settle(cn, wall())?;
    let s = cn.query_row("SELECT s.name, arn, mode, retention, created, tags, next_seq, next_shard, coalesce(x.status,'ACTIVE'), coalesce(x.ready_at,0), coalesce(x.encryption,'NONE'), x.key_id, coalesce(x.metrics,'[]')
        FROM kinesis_streams s LEFT JOIN kinesis_stream_state x ON x.name=s.name WHERE s.name=?1", [name], |r| Ok(Stream {
        name: r.get(0)?, arn: r.get(1)?, mode: r.get(2)?, retention: r.get(3)?, created: r.get(4)?,
        tags: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(), next_seq: r.get(6)?, next_shard: r.get(7)?, shards: vec![],
        status: r.get(8)?, ready_at: r.get(9)?, encryption: r.get(10)?, key_id: r.get(11)?, metrics: serde_json::from_str(&r.get::<_, String>(12)?).unwrap_or_default(),
    })).optional()?;
    let mut s = s.ok_or_else(|| kerr("ResourceNotFoundException", format!("Stream {name} under account {ACCOUNT} not found.")))?;
    let mut st = cn.prepare("SELECT id, parent, adjacent, start_hash, end_hash, start_seq, end_seq FROM kinesis_shards WHERE stream=?1 ORDER BY id")?;
    s.shards = st.query_map([name], |r| Ok(Shard {
        id: r.get(0)?, parent: r.get(1)?, adjacent: r.get(2)?, start: r.get::<_, String>(3)?.parse().unwrap_or(0), end: r.get::<_, String>(4)?.parse().unwrap_or(0),
        start_seq: r.get(5)?, end_seq: r.get(6)?,
    }))?.collect::<Result<_, _>>()?;
    Ok(s)
}

pub fn names(cn: &Connection) -> R<Vec<String>> {
    settle(cn, wall())?;
    let mut st = cn.prepare("SELECT name FROM kinesis_streams ORDER BY name")?;
    let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    Ok(v)
}

fn add_shard(cn: &Connection, s: &mut Stream, parent: Option<&str>, adjacent: Option<&str>, range: (u128, u128)) -> R<String> {
    let id = format!("shardId-{:012}", s.next_shard);
    s.next_shard += 1;
    cn.execute("INSERT INTO kinesis_shards VALUES(?1,?2,?3,?4,?5,?6,?7,NULL)", params![s.name, id, parent, adjacent, range.0.to_string(), range.1.to_string(), s.next_seq])?;
    cn.execute("UPDATE kinesis_streams SET next_shard=?2 WHERE name=?1", params![s.name, s.next_shard])?;
    s.shards.push(Shard { id: id.clone(), parent: parent.map(String::from), adjacent: adjacent.map(String::from), start: range.0, end: range.1, start_seq: s.next_seq, end_seq: None });
    Ok(id)
}

/// Closes shards at the stream's last sequence number; their children start after it.
fn close(cn: &Connection, s: &mut Stream, ids: &[String]) -> R<()> {
    let end = s.next_seq;
    for id in ids {
        cn.execute("UPDATE kinesis_shards SET end_seq=?3 WHERE stream=?1 AND id=?2", params![s.name, id, end])?;
        if let Some(x) = s.shards.iter_mut().find(|x| &x.id == id) { x.end_seq = Some(end) }
    }
    s.next_seq += 1;
    cn.execute("UPDATE kinesis_streams SET next_seq=?2 WHERE name=?1", params![s.name, s.next_seq])?;
    Ok(())
}

fn name_of(b: &Value) -> R<String> {
    match (b["StreamName"].as_str(), b["StreamARN"].as_str().or(b["ResourceARN"].as_str())) {
        (Some(n), _) if !n.is_empty() => Ok(n.to_string()),
        (_, Some(a)) if !a.is_empty() => Ok(stream_name(a)),
        _ => Err(kerr("ValidationException", "1 validation error detected: Either StreamName or StreamARN should be provided.")),
    }
}

/// The stream a request names (also through its iterator or consumer), for provocations.
pub fn named(op: &str, b: &Value) -> Option<String> {
    if op == "GetRecords" {
        return b["ShardIterator"].as_str().and_then(|i| parse_iterator(i).ok()).map(|x| x.0);
    }
    name_of(b).ok().or_else(|| b["ConsumerARN"].as_str().map(stream_name))
}

fn shard_count(b: &Value, key: &str) -> R<i64> {
    let n = b[key].as_i64().ok_or_else(|| kerr("ValidationException", format!("{key} is required")))?;
    if !(1..=10_000).contains(&n) { return Err(kerr("InvalidArgumentException", format!("{key} must be between 1 and 10000"))) }
    Ok(n)
}

/// The stream is CREATING until `ready` (wall clock), then ACTIVE.
pub fn create(cn: &Connection, b: &Value, region: &str, now: i64, ready: i64) -> R<Value> {
    let name = b["StreamName"].as_str().filter(|n| !n.is_empty()).ok_or_else(|| kerr("ValidationException", "StreamName is required"))?.to_string();
    if name.len() > 128 || !name.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
        return Err(kerr("ValidationException", format!("1 validation error detected: Value '{name}' at 'streamName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9_.-]+")));
    }
    let mode = b["StreamModeDetails"]["StreamMode"].as_str().unwrap_or(if b["ShardCount"].is_null() { "ON_DEMAND" } else { "PROVISIONED" }).to_string();
    let n = if mode == "ON_DEMAND" { 4 } else { shard_count(b, "ShardCount")? };
    if cn.query_row("SELECT 1 FROM kinesis_streams WHERE name=?1", [&name], |_| Ok(())).optional()?.is_some() {
        return Err(kerr("ResourceInUseException", format!("Stream {name} under account {ACCOUNT} already exists.")));
    }
    let arn = format!("arn:aws:kinesis:{region}:{ACCOUNT}:stream/{name}");
    let tags = b["Tags"].as_object().cloned().unwrap_or_default();
    cn.execute("INSERT INTO kinesis_streams(name, arn, mode, created, tags) VALUES(?1,?2,?3,?4,?5)", params![name, arn, mode, now, Value::Object(tags).to_string()])?;
    transition(cn, &name, "CREATING", ready)?;
    let mut s = load(cn, &name)?;
    for r in uniform(n as u128) { add_shard(cn, &mut s, None, None, r)?; }
    Ok(json!({}))
}

/// The stream is DELETING until `ready` (wall clock), then gone.
pub fn delete(cn: &Connection, b: &Value, ready: i64) -> R<Value> {
    let s = load(cn, &name_of(b)?)?;
    let consumers: i64 = cn.query_row("SELECT COUNT(*) FROM kinesis_consumers WHERE stream=?1", [&s.name], |r| r.get(0))?;
    if consumers > 0 && !b["EnforceConsumerDeletion"].as_bool().unwrap_or(false) {
        return Err(kerr("ResourceInUseException", format!("Stream {} has {consumers} registered consumer(s); deregister them or set EnforceConsumerDeletion.", s.name)));
    }
    transition(cn, &s.name, "DELETING", ready)?;
    settle(cn, wall())?;
    Ok(json!({}))
}

fn open_count(s: &Stream) -> usize {
    s.shards.iter().filter(|x| x.open()).count()
}

pub fn describe(cn: &Connection, b: &Value, summary: bool) -> R<Value> {
    let s = load(cn, &name_of(b)?)?;
    let mut d = json!({"StreamName": s.name, "StreamARN": s.arn, "StreamStatus": s.status, "StreamModeDetails": s.mode_json(), "RetentionPeriodHours": s.retention,
        "StreamCreationTimestamp": now_s(s.created), "EnhancedMonitoring": [{"ShardLevelMetrics": s.metrics}], "EncryptionType": s.encryption});
    if let Some(k) = &s.key_id { d["KeyId"] = json!(k); }
    if summary {
        let consumers: i64 = cn.query_row("SELECT COUNT(*) FROM kinesis_consumers WHERE stream=?1", [&s.name], |r| r.get(0))?;
        d["OpenShardCount"] = json!(open_count(&s));
        d["ConsumerCount"] = json!(consumers);
        return Ok(json!({"StreamDescriptionSummary": d}));
    }
    let after = b["ExclusiveStartShardId"].as_str().unwrap_or("");
    let limit = b["Limit"].as_u64().unwrap_or(100).clamp(1, 10_000) as usize;
    let rest: Vec<&Shard> = s.shards.iter().filter(|x| after.is_empty() || x.id.as_str() > after).collect();
    d["Shards"] = json!(rest.iter().take(limit).map(|x| x.json()).collect::<Vec<_>>());
    d["HasMoreShards"] = json!(rest.len() > limit);
    Ok(json!({"StreamDescription": d}))
}

pub fn list_streams(cn: &Connection, b: &Value) -> R<Value> {
    let after = b["ExclusiveStartStreamName"].as_str().map(String::from).or_else(|| b["NextToken"].as_str().map(|t| String::from_utf8(B64.decode(t).unwrap_or_default()).unwrap_or_default())).unwrap_or_default();
    let limit = b["Limit"].as_u64().unwrap_or(100).clamp(1, 10_000) as usize;
    let all: Vec<String> = names(cn)?.into_iter().filter(|n| after.is_empty() || *n > after).collect();
    let page: Vec<&String> = all.iter().take(limit).collect();
    let mut sums = vec![];
    for n in &page {
        let s = load(cn, n)?;
        sums.push(json!({"StreamName": s.name, "StreamARN": s.arn, "StreamStatus": s.status, "StreamModeDetails": s.mode_json(), "StreamCreationTimestamp": now_s(s.created)}));
    }
    let more = all.len() > limit;
    let mut v = json!({"StreamNames": page, "HasMoreStreams": more, "StreamSummaries": sums});
    if more { v["NextToken"] = json!(B64.encode(page.last().unwrap().as_bytes())); }
    Ok(v)
}

pub fn list_shards(cn: &Connection, b: &Value, now: i64) -> R<Value> {
    let (name, after) = match b["NextToken"].as_str() {
        Some(t) => {
            let raw = String::from_utf8(B64.decode(t).unwrap_or_default()).unwrap_or_default();
            let (n, a) = raw.split_once('\n').ok_or_else(|| kerr("InvalidArgumentException", "NextToken is not valid"))?;
            (n.to_string(), a.to_string())
        }
        None => (name_of(b)?, b["ExclusiveStartShardId"].as_str().unwrap_or("").to_string()),
    };
    let s = load(cn, &name)?;
    let max = b["MaxResults"].as_u64().unwrap_or(1000).clamp(1, 10_000) as usize;
    let f = &b["ShardFilter"];
    let keep = |x: &Shard| match f["Type"].as_str().unwrap_or("") {
        "AT_LATEST" => x.open(),
        "AFTER_SHARD_ID" => f["ShardId"].as_str().is_none_or(|id| x.id.as_str() > id),
        "AT_TIMESTAMP" | "FROM_TIMESTAMP" => { let ts = (f["Timestamp"].as_f64().unwrap_or(0.0) * 1000.0) as i64; x.open() || x.end_seq.is_some_and(|_| ts <= now) }
        _ => true,
    };
    let rest: Vec<&Shard> = s.shards.iter().filter(|x| (after.is_empty() || x.id > after) && keep(x)).collect();
    let page: Vec<&&Shard> = rest.iter().take(max).collect();
    let mut v = json!({"Shards": page.iter().map(|x| x.json()).collect::<Vec<_>>()});
    if rest.len() > max { v["NextToken"] = json!(B64.encode(format!("{}\n{}", s.name, page.last().unwrap().id))); }
    Ok(v)
}

// ---------------------------------------------------------------- records

/// One record as the API returns it.
fn rec_json(seq: i64, pkey: &str, data: &[u8], at: i64, kms: bool) -> Value {
    let mut v = json!({"SequenceNumber": seq_str(seq), "ApproximateArrivalTimestamp": now_s(at), "Data": B64.encode(data), "PartitionKey": pkey});
    if kms { v["EncryptionType"] = json!("KMS") }
    v
}

fn throttled(s: &Stream, shard: &str) -> KErr {
    kerr("ProvisionedThroughputExceededException", format!("Rate exceeded for shard {shard} in stream {} under account {ACCOUNT}.", s.name))
}

fn put_one(cn: &Connection, s: &mut Stream, r: &Value, now: i64, producer: &str, lim: &Limiter) -> R<(String, i64)> {
    let pkey = r["PartitionKey"].as_str().unwrap_or("");
    if pkey.is_empty() || pkey.chars().count() > 256 {
        return Err(kerr("ValidationException", "1 validation error detected: Value at 'partitionKey' failed to satisfy constraint: Member must have length between 1 and 256"));
    }
    let data = B64.decode(r["Data"].as_str().unwrap_or("")).map_err(|_| kerr("SerializationException", "Data is not valid base64"))?;
    if data.len() > 1 << 20 {
        return Err(kerr("ValidationException", "1 validation error detected: Value at 'data' failed to satisfy constraint: Member must have length less than or equal to 1048576"));
    }
    let h = match r["ExplicitHashKey"].as_str() {
        Some(e) => e.parse::<u128>().map_err(|_| kerr("InvalidArgumentException", format!("Invalid ExplicitHashKey. ExplicitHashKey must be in the range: [0, 2^128-1]. Specified value was {e}")))?,
        None => hash(pkey),
    };
    let shard = s.shards.iter().find(|x| x.open() && x.start <= h && h <= x.end).map(|x| x.id.clone()).ok_or_else(|| kerr("InternalFailure", "no open shard covers the hash key"))?;
    if !lim.write(s, &shard, (data.len() + pkey.len()) as u64) { return Err(throttled(s, &shard)) }
    let seq = s.next_seq;
    s.next_seq += 1;
    cn.execute("INSERT INTO kinesis_records VALUES(?1,?2,?3,?4,?5,?6,?7)", params![s.name, shard, seq, pkey, data, now, producer])?;
    Ok((shard, seq))
}

fn prune(cn: &Connection, s: &Stream, now: i64) -> R<()> {
    cn.execute("DELETE FROM kinesis_records WHERE stream=?1 AND at<?2", params![s.name, now - s.retention * 3_600_000])?;
    Ok(())
}

pub fn put_record(cn: &mut Connection, b: &Value, now: i64, producer: &str, lim: &Limiter) -> R<Value> {
    let tx = cn.transaction()?;
    let mut s = load(&tx, &name_of(b)?)?;
    let (shard, seq) = put_one(&tx, &mut s, b, now, producer, lim)?;
    tx.execute("UPDATE kinesis_streams SET next_seq=?2 WHERE name=?1", params![s.name, s.next_seq])?;
    prune(&tx, &s, now)?;
    tx.commit()?;
    Ok(json!({"ShardId": shard, "SequenceNumber": seq_str(seq), "EncryptionType": s.encryption}))
}

/// `fail(i)`: the provoked partial failure of record i. A record over the shard's limit fails alone.
pub fn put_records(cn: &mut Connection, b: &Value, now: i64, producer: &str, fail: impl Fn(usize) -> bool, lim: &Limiter) -> R<Value> {
    let recs = b["Records"].as_array().cloned().unwrap_or_default();
    if recs.is_empty() || recs.len() > 500 {
        return Err(kerr("ValidationException", "1 validation error detected: Value at 'records' failed to satisfy constraint: Member must have length between 1 and 500"));
    }
    let size: usize = recs.iter().map(|r| r["Data"].as_str().map_or(0, |d| d.len() / 4 * 3) + r["PartitionKey"].as_str().map_or(0, str::len)).sum();
    if size as u64 > 5 * MIB {
        return Err(kerr("InvalidArgumentException", "Records size exceeds 5 MB limit"));
    }
    let tx = cn.transaction()?;
    let mut s = load(&tx, &name_of(b)?)?;
    let (mut out, mut failed) = (vec![], 0);
    for (i, r) in recs.iter().enumerate() {
        if fail(i) {
            failed += 1;
            let h = r["ExplicitHashKey"].as_str().and_then(|e| e.parse().ok()).unwrap_or_else(|| hash(r["PartitionKey"].as_str().unwrap_or("")));
            let sh = s.shards.iter().find(|x| x.open() && x.start <= h && h <= x.end).map(|x| x.id.clone()).unwrap_or_default();
            out.push(json!({"ErrorCode": "ProvisionedThroughputExceededException", "ErrorMessage": format!("Rate exceeded for shard {sh} in stream {} under account {ACCOUNT}.", s.name)}));
            continue;
        }
        match put_one(&tx, &mut s, r, now, producer, lim) {
            Ok((shard, seq)) => out.push(json!({"ShardId": shard, "SequenceNumber": seq_str(seq)})),
            Err(e) if e.code == "ProvisionedThroughputExceededException" => { failed += 1; out.push(json!({"ErrorCode": e.code, "ErrorMessage": e.msg})) }
            Err(e) => return Err(e),
        }
    }
    tx.execute("UPDATE kinesis_streams SET next_seq=?2 WHERE name=?1", params![s.name, s.next_seq])?;
    prune(&tx, &s, now)?;
    tx.commit()?;
    Ok(json!({"FailedRecordCount": failed, "Records": out, "EncryptionType": s.encryption}))
}

pub fn make_iterator(stream: &str, shard: &str, from: i64, now: i64) -> String {
    B64.encode(format!("{stream}\n{shard}\n{from}\n{now}"))
}

pub fn parse_iterator(it: &str) -> R<(String, String, i64, i64)> {
    let bad = || kerr("InvalidArgumentException", format!("Invalid ShardIterator: {it}"));
    let raw = String::from_utf8(B64.decode(it).map_err(|_| bad())?).map_err(|_| bad())?;
    let p: Vec<&str> = raw.split('\n').collect();
    if p.len() != 4 { return Err(bad()) }
    Ok((p[0].into(), p[1].into(), p[2].parse().map_err(|_| bad())?, p[3].parse().map_err(|_| bad())?))
}

/// Where a reader starts: the first sequence number it gets.
pub fn start_at(cn: &Connection, s: &Stream, shard: &Shard, kind: &str, seq: Option<&str>, ts: Option<f64>) -> R<i64> {
    Ok(match kind {
        "TRIM_HORIZON" => shard.start_seq,
        "LATEST" => s.next_seq,
        "AT_SEQUENCE_NUMBER" | "AFTER_SEQUENCE_NUMBER" => {
            let n = seq_parse(seq.ok_or_else(|| kerr("InvalidArgumentException", format!("Must specify a StartingSequenceNumber for {kind}")))?)?;
            if n > s.next_seq { return Err(kerr("InvalidArgumentException", format!("StartingSequenceNumber {} used in GetShardIterator on shard {} in stream {} under account {ACCOUNT} is invalid.", seq_str(n), shard.id, s.name))) }
            if kind == "AT_SEQUENCE_NUMBER" { n } else { n + 1 }
        }
        "AT_TIMESTAMP" => {
            let ms = (ts.ok_or_else(|| kerr("InvalidArgumentException", "Must specify a Timestamp for AT_TIMESTAMP"))? * 1000.0) as i64;
            cn.query_row("SELECT MIN(seq) FROM kinesis_records WHERE stream=?1 AND shard=?2 AND at>=?3", params![s.name, shard.id, ms], |r| r.get::<_, Option<i64>>(0))?.unwrap_or(s.next_seq)
        }
        k => return Err(kerr("ValidationException", format!("1 validation error detected: Value '{k}' at 'shardIteratorType' failed to satisfy constraint: Member must satisfy enum value set: [AFTER_SEQUENCE_NUMBER, LATEST, AT_TIMESTAMP, TRIM_HORIZON, AT_SEQUENCE_NUMBER]"))),
    })
}

pub fn get_shard_iterator(cn: &Connection, b: &Value, now: i64) -> R<Value> {
    let s = load(cn, &name_of(b)?)?;
    let shard = s.shard(b["ShardId"].as_str().unwrap_or(""))?;
    let from = start_at(cn, &s, shard, b["ShardIteratorType"].as_str().unwrap_or(""), b["StartingSequenceNumber"].as_str(), b["Timestamp"].as_f64())?;
    Ok(json!({"ShardIterator": make_iterator(&s.name, &shard.id, from, now)}))
}

/// What a reader at `from` gets: records, where it goes next (None: the shard is closed and read
/// to its end), how far behind the tip it is, the children of a closed shard.
pub struct Batch {
    pub records: Vec<Value>,
    pub last: Option<i64>,
    pub next: Option<i64>,
    pub behind: i64,
    pub children: Vec<Value>,
}

pub fn read(cn: &Connection, s: &Stream, shard: &Shard, from: i64, limit: usize, now: i64) -> R<Batch> {
    let mut st = cn.prepare("SELECT seq, pkey, data, at FROM kinesis_records WHERE stream=?1 AND shard=?2 AND seq>=?3 AND at>=?4 ORDER BY seq LIMIT ?5")?;
    let rows: Vec<(i64, String, Vec<u8>, i64)> = st.query_map(params![s.name, shard.id, from, now - s.retention * 3_600_000, limit as i64 + 1], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
    // At most `limit` records and 10 MiB (always one record).
    let (mut n, mut bytes) = (0, 0);
    while n < rows.len().min(limit) && (n == 0 || bytes + rows[n].2.len() <= 10 * MIB as usize) { bytes += rows[n].2.len(); n += 1 }
    let more = n < rows.len();
    let behind = if more { (now - rows[n].3).max(0) } else { 0 };
    let rows = &rows[..n];
    let last = rows.last().map(|r| r.0);
    let next_from = last.map(|l| l + 1).unwrap_or(from);
    let done = !more && shard.end_seq.is_some();
    Ok(Batch {
        records: rows.iter().map(|(q, k, d, a)| rec_json(*q, k, d, *a, s.encryption == "KMS")).collect(),
        last,
        next: (!done).then_some(next_from),
        behind,
        children: if done { s.children(&shard.id) } else { vec![] },
    })
}

pub fn get_records(cn: &Connection, b: &Value, now: i64, lim: &Limiter) -> R<Value> {
    let it = b["ShardIterator"].as_str().unwrap_or("");
    let (name, shard, from, issued) = parse_iterator(it)?;
    if now - issued > ITERATOR_TTL {
        return Err(kerr("ExpiredIteratorException", format!("Iterator expired. The iterator was created at time {} while right now it is {} which is further in the future than the tolerated delay of {ITERATOR_TTL} milliseconds.", issued, now)));
    }
    let s = load(cn, &name)?;
    let sh = s.shard(&shard)?;
    let limit = b["Limit"].as_u64().unwrap_or(10_000).clamp(1, 10_000) as usize;
    if !lim.read_call(&s, &shard) { return Err(throttled(&s, &shard)) }
    let r = read(cn, &s, sh, from, limit, now)?;
    lim.read_bytes(&s, &shard, r.records.iter().map(|x| x["Data"].as_str().map_or(0, |d| d.len() / 4 * 3) as u64).sum());
    let mut v = json!({"Records": r.records, "MillisBehindLatest": r.behind});
    if let Some(n) = r.next { v["NextShardIterator"] = json!(make_iterator(&name, &shard, n, now)); }
    if !r.children.is_empty() { v["ChildShards"] = json!(r.children); }
    Ok(v)
}

// ---------------------------------------------------------------- resharding

pub fn split(cn: &mut Connection, b: &Value) -> R<Value> {
    let tx = cn.transaction()?;
    let mut s = load(&tx, &name_of(b)?)?;
    let sh = s.shard(b["ShardToSplit"].as_str().unwrap_or(""))?.clone();
    if !sh.open() { return Err(kerr("ResourceInUseException", format!("Shard {} in stream {} under account {ACCOUNT} has already been merged or split, and thus is not eligible for merging or splitting.", sh.id, s.name))) }
    let k: u128 = b["NewStartingHashKey"].as_str().and_then(|x| x.parse().ok()).ok_or_else(|| kerr("InvalidArgumentException", "NewStartingHashKey must be a 128-bit decimal"))?;
    if k <= sh.start || k > sh.end {
        return Err(kerr("InvalidArgumentException", format!("NewStartingHashKey {k} used in SplitShard() on shard {} in stream {} under account {ACCOUNT} is not both greater than one plus the shard's StartingHashKey {} and less than the shard's EndingHashKey {}.", sh.id, s.name, sh.start, sh.end)));
    }
    close(&tx, &mut s, &[sh.id.clone()])?;
    add_shard(&tx, &mut s, Some(&sh.id), None, (sh.start, k - 1))?;
    add_shard(&tx, &mut s, Some(&sh.id), None, (k, sh.end))?;
    tx.commit()?;
    Ok(json!({}))
}

pub fn merge(cn: &mut Connection, b: &Value) -> R<Value> {
    let tx = cn.transaction()?;
    let mut s = load(&tx, &name_of(b)?)?;
    let a = s.shard(b["ShardToMerge"].as_str().unwrap_or(""))?.clone();
    let c = s.shard(b["AdjacentShardToMerge"].as_str().unwrap_or(""))?.clone();
    if !a.open() || !c.open() { return Err(kerr("ResourceInUseException", "Shards that were merged or split are not eligible for merging.")) }
    if a.end.checked_add(1) != Some(c.start) && c.end.checked_add(1) != Some(a.start) {
        return Err(kerr("InvalidArgumentException", format!("Shards {} and {} in stream {} under account {ACCOUNT} are not an adjacent pair of shards eligible for merging", a.id, c.id, s.name)));
    }
    close(&tx, &mut s, &[a.id.clone(), c.id.clone()])?;
    add_shard(&tx, &mut s, Some(&a.id), Some(&c.id), (a.start.min(c.start), a.end.max(c.end)))?;
    tx.commit()?;
    Ok(json!({}))
}

/// UNIFORM_SCALING: every open shard closes; the new ones split the key space evenly, each a child
/// of the old shards it overlaps (the first two).
pub fn update_shard_count(cn: &mut Connection, b: &Value) -> R<Value> {
    let tx = cn.transaction()?;
    let mut s = load(&tx, &name_of(b)?)?;
    let n = shard_count(b, "TargetShardCount")?;
    if s.mode == "ON_DEMAND" { return Err(kerr("ValidationException", "UpdateShardCount is not supported for streams in ON_DEMAND mode.")) }
    let old: Vec<Shard> = s.shards.iter().filter(|x| x.open()).cloned().collect();
    close(&tx, &mut s, &old.iter().map(|x| x.id.clone()).collect::<Vec<_>>())?;
    for r in uniform(n as u128) {
        let over: Vec<&Shard> = old.iter().filter(|o| o.start <= r.1 && r.0 <= o.end).collect();
        add_shard(&tx, &mut s, over.first().map(|x| x.id.as_str()), over.get(1).map(|x| x.id.as_str()), r)?;
    }
    tx.commit()?;
    Ok(json!({"StreamName": s.name, "StreamARN": s.arn, "CurrentShardCount": old.len(), "TargetShardCount": n}))
}

/// The provoked resharding: the widest open shard is split at its middle.
pub fn reshard(cn: &mut Connection, name: &str, ready: i64) -> R<Value> {
    gate(cn, "SplitShard", name)?;
    let s = load(cn, name)?;
    let sh = s.shards.iter().filter(|x| x.open()).max_by_key(|x| x.end - x.start).ok_or_else(|| kerr("ResourceInUseException", "no open shard"))?.clone();
    let mid = sh.start + (sh.end - sh.start) / 2 + 1;
    split(cn, &json!({"StreamName": name, "ShardToSplit": sh.id, "NewStartingHashKey": mid.to_string()}))?;
    transition(cn, name, "UPDATING", ready)?;
    Ok(json!({"stream": name, "split": sh.id}))
}

// ---------------------------------------------------------------- mode, encryption, monitoring

/// UpdateStreamMode: PROVISIONED ↔ ON_DEMAND; the shards stay as they are.
pub fn update_mode(cn: &Connection, b: &Value) -> R<Value> {
    let s = load(cn, &name_of(b)?)?;
    let m = b["StreamModeDetails"]["StreamMode"].as_str().unwrap_or("");
    if m != "PROVISIONED" && m != "ON_DEMAND" {
        return Err(kerr("ValidationException", format!("1 validation error detected: Value '{m}' at 'streamModeDetails.streamMode' failed to satisfy constraint: Member must satisfy enum value set: [PROVISIONED, ON_DEMAND]")));
    }
    if m == s.mode { return Err(kerr("InvalidArgumentException", format!("Stream {} under account {ACCOUNT} is already in {m} mode.", s.name))) }
    cn.execute("UPDATE kinesis_streams SET mode=?2 WHERE name=?1", params![s.name, m])?;
    Ok(json!({}))
}

/// A KMS key as Kinesis accepts it: key id, key ARN, alias name or alias ARN (alias/aws/kinesis too).
pub fn kms_key_ok(k: &str) -> bool {
    let uuid = |x: &str| x.len() == 36 && x.chars().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_hexdigit() });
    let mrk = |x: &str| x.strip_prefix("mrk-").is_some_and(|h| h.len() == 32 && h.chars().all(|c| c.is_ascii_hexdigit()));
    let alias = |x: &str| x.strip_prefix("alias/").is_some_and(|a| !a.is_empty() && a.len() <= 250 && a.chars().all(|c| c.is_ascii_alphanumeric() || "/_-".contains(c)));
    if let Some(rest) = k.strip_prefix("arn:aws:kms:") {
        let p: Vec<&str> = rest.splitn(3, ':').collect();
        return p.len() == 3 && !p[0].is_empty() && p[1].len() == 12 && p[1].chars().all(|c| c.is_ascii_digit())
            && (p[2].strip_prefix("key/").is_some_and(|x| uuid(x) || mrk(x)) || alias(p[2]));
    }
    uuid(k) || mrk(k) || alias(k)
}

/// Start/StopStreamEncryption: EncryptionType KMS and a well-formed KeyId. Nothing is encrypted.
pub fn encryption(cn: &Connection, b: &Value, start: bool) -> R<Value> {
    let s = load(cn, &name_of(b)?)?;
    let t = b["EncryptionType"].as_str().unwrap_or("");
    if t != "KMS" { return Err(kerr("InvalidArgumentException", format!("EncryptionType {t} is not valid: it must be KMS."))) }
    let k = b["KeyId"].as_str().unwrap_or("");
    if !kms_key_ok(k) {
        return Err(kerr("InvalidArgumentException", format!("KeyId {k} is not valid: give a key ID, key ARN, alias name (alias/…) or alias ARN.")));
    }
    let (enc, key) = if start { ("KMS", Some(k)) } else { ("NONE", None) };
    cn.execute("INSERT INTO kinesis_stream_state(name, encryption, key_id) VALUES(?1,?2,?3) ON CONFLICT(name) DO UPDATE SET encryption=?2, key_id=?3", params![s.name, enc, key])?;
    Ok(json!({}))
}

pub const METRICS: [&str; 7] = ["IncomingBytes", "IncomingRecords", "OutgoingBytes", "OutgoingRecords", "WriteProvisionedThroughputExceeded", "ReadProvisionedThroughputExceeded", "IteratorAgeMilliseconds"];

/// Enable/DisableEnhancedMonitoring: the shard-level metrics are kept and returned (none are emitted).
pub fn monitoring(cn: &Connection, b: &Value, enable: bool) -> R<Value> {
    let s = load(cn, &name_of(b)?)?;
    let asked: Vec<String> = b["ShardLevelMetrics"].as_array().cloned().unwrap_or_default().iter().filter_map(|v| v.as_str().map(String::from)).collect();
    if asked.is_empty() { return Err(kerr("ValidationException", "1 validation error detected: Value null at 'shardLevelMetrics' failed to satisfy constraint: Member must not be null")) }
    let mut want: Vec<String> = vec![];
    for m in &asked {
        if m == "ALL" { want = METRICS.iter().map(|x| x.to_string()).collect(); continue }
        if !METRICS.contains(&m.as_str()) {
            return Err(kerr("ValidationException", format!("1 validation error detected: Value '[{m}]' at 'shardLevelMetrics' failed to satisfy constraint: Member must satisfy enum value set: [ALL, {}]", METRICS.join(", "))));
        }
        if !want.contains(m) { want.push(m.clone()) }
    }
    let desired: Vec<String> = if enable {
        METRICS.iter().map(|x| x.to_string()).filter(|x| s.metrics.contains(x) || want.contains(x)).collect()
    } else {
        s.metrics.iter().filter(|x| !want.contains(x)).cloned().collect()
    };
    cn.execute("INSERT INTO kinesis_stream_state(name, metrics) VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET metrics=?2", params![s.name, json!(desired).to_string()])?;
    Ok(json!({"StreamName": s.name, "StreamARN": s.arn, "CurrentShardLevelMetrics": s.metrics, "DesiredShardLevelMetrics": desired}))
}

pub fn encrypted(cn: &Connection, name: &str) -> bool {
    load(cn, name).is_ok_and(|s| s.encryption == "KMS")
}

// ---------------------------------------------------------------- retention, tags, consumers

pub fn retention(cn: &Connection, b: &Value, increase: bool) -> R<Value> {
    let s = load(cn, &name_of(b)?)?;
    let h = b["RetentionPeriodHours"].as_i64().unwrap_or(0);
    if !(24..=8760).contains(&h) { return Err(kerr("InvalidArgumentException", format!("Minimum allowed retention period is 24 hours, maximum is 8760 hours. Requested retention period ({h} hours) is outside this range."))) }
    if increase && h < s.retention { return Err(kerr("InvalidArgumentException", format!("Requested retention period ({h} hours) for stream {} can not be shorter than existing retention period ({} hours). Use DecreaseRetentionPeriod API.", s.name, s.retention))) }
    if !increase && h > s.retention { return Err(kerr("InvalidArgumentException", format!("Requested retention period ({h} hours) for stream {} can not be longer than existing retention period ({} hours). Use IncreaseRetentionPeriod API.", s.name, s.retention))) }
    cn.execute("UPDATE kinesis_streams SET retention=?2 WHERE name=?1", params![s.name, h])?;
    Ok(json!({}))
}

pub fn tags(cn: &Connection, op: &str, b: &Value) -> R<Value> {
    let mut s = load(cn, &name_of(b)?)?;
    match op {
        "AddTagsToStream" | "TagResource" => {
            for (k, v) in b["Tags"].as_object().cloned().unwrap_or_default() { s.tags.insert(k, v); }
            if s.tags.len() > 50 { return Err(kerr("InvalidArgumentException", "A stream can have at most 50 tags")) }
        }
        "RemoveTagsFromStream" | "UntagResource" => {
            for k in b["TagKeys"].as_array().cloned().unwrap_or_default() { s.tags.remove(k.as_str().unwrap_or("")); }
        }
        _ => {
            let list: Vec<Value> = s.tags.iter().map(|(k, v)| json!({"Key": k, "Value": v})).collect();
            return Ok(if op == "ListTagsForResource" { json!({"Tags": list}) } else { json!({"Tags": list, "HasMoreTags": false}) });
        }
    }
    cn.execute("UPDATE kinesis_streams SET tags=?2 WHERE name=?1", params![s.name, Value::Object(s.tags).to_string()])?;
    Ok(json!({}))
}

fn consumer_json(arn: &str, name: &str, created: i64, stream_arn: Option<&str>) -> Value {
    let mut v = json!({"ConsumerName": name, "ConsumerARN": arn, "ConsumerStatus": "ACTIVE", "ConsumerCreationTimestamp": now_s(created)});
    if let Some(s) = stream_arn { v["StreamARN"] = json!(s); }
    v
}

/// (stream, name, arn, created) of the consumer a request names (ConsumerARN, or StreamARN + ConsumerName).
pub fn consumer(cn: &Connection, b: &Value) -> R<(Stream, String, String, i64)> {
    let (stream, name) = match b["ConsumerARN"].as_str() {
        Some(a) => (stream_name(a), a.split("/consumer/").nth(1).and_then(|x| x.split(':').next()).unwrap_or("").to_string()),
        None => (name_of(b)?, b["ConsumerName"].as_str().unwrap_or("").to_string()),
    };
    let s = load(cn, &stream)?;
    let r = cn.query_row("SELECT arn, created FROM kinesis_consumers WHERE stream=?1 AND name=?2", params![s.name, name], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).optional()?;
    let (arn, created) = r.ok_or_else(|| kerr("ResourceNotFoundException", format!("Consumer {name}, account {ACCOUNT} not found.")))?;
    Ok((s, name, arn, created))
}

pub fn consumers(cn: &Connection, op: &str, b: &Value, now: i64) -> R<Value> {
    match op {
        "RegisterStreamConsumer" => {
            let s = load(cn, &name_of(b)?)?;
            let name = b["ConsumerName"].as_str().filter(|x| !x.is_empty()).ok_or_else(|| kerr("ValidationException", "ConsumerName is required"))?;
            let arn = format!("{}/consumer/{name}:{}", s.arn, now / 1000);
            if cn.execute("INSERT OR IGNORE INTO kinesis_consumers VALUES(?1,?2,?3,?4)", params![s.name, name, arn, now])? == 0 {
                return Err(kerr("ResourceInUseException", format!("Consumer {name} under stream {} already exists for account {ACCOUNT}.", s.name)));
            }
            Ok(json!({"Consumer": consumer_json(&arn, name, now, None)}))
        }
        "ListStreamConsumers" => {
            let s = load(cn, &name_of(b)?)?;
            let mut st = cn.prepare("SELECT name, arn, created FROM kinesis_consumers WHERE stream=?1 ORDER BY name")?;
            let v: Vec<Value> = st.query_map([&s.name], |r| Ok(consumer_json(&r.get::<_, String>(1)?, &r.get::<_, String>(0)?, r.get(2)?, None)))?.collect::<Result<_, _>>()?;
            Ok(json!({"Consumers": v}))
        }
        "DescribeStreamConsumer" => {
            let (s, name, arn, created) = consumer(cn, b)?;
            Ok(json!({"ConsumerDescription": consumer_json(&arn, &name, created, Some(&s.arn))}))
        }
        _ => {
            let (s, name, _, _) = consumer(cn, b)?;
            cn.execute("DELETE FROM kinesis_consumers WHERE stream=?1 AND name=?2", params![s.name, name])?;
            Ok(json!({}))
        }
    }
}

pub fn limits(cn: &Connection) -> R<Value> {
    let (mut open, mut od) = (0, 0);
    for n in names(cn)? {
        let s = load(cn, &n)?;
        open += open_count(&s);
        if s.mode == "ON_DEMAND" { od += 1 }
    }
    Ok(json!({"ShardLimit": 500, "OpenShardCount": open, "OnDemandStreamCount": od, "OnDemandStreamCountLimit": 50}))
}

// ---------------------------------------------------------------- the screen and the tools

/// Every stream with its shards (record counts), consumers and tags.
pub fn overview(cn: &Connection) -> R<Vec<Value>> {
    let mut out = vec![];
    for n in names(cn)? {
        let s = load(cn, &n)?;
        let mut shards = vec![];
        let mut total = 0;
        for x in &s.shards {
            let c: i64 = cn.query_row("SELECT COUNT(*) FROM kinesis_records WHERE stream=?1 AND shard=?2", params![s.name, x.id], |r| r.get(0))?;
            total += c;
            shards.push(json!({"id": x.id, "parent": x.parent, "adjacent": x.adjacent, "startingHashKey": x.start.to_string(), "endingHashKey": x.end.to_string(),
                "share": ((x.end - x.start) as f64 / MAX as f64 * 100.0 * 10.0).round() / 10.0,
                "startingSequenceNumber": seq_str(x.start_seq), "endingSequenceNumber": x.end_seq.map(seq_str), "open": x.open(), "records": c}));
        }
        let mut st = cn.prepare("SELECT name, arn, created FROM kinesis_consumers WHERE stream=?1 ORDER BY name")?;
        let cons: Vec<Value> = st.query_map([&s.name], |r| Ok(json!({"name": r.get::<_, String>(0)?, "arn": r.get::<_, String>(1)?, "createdAt": r.get::<_, i64>(2)?})))?.collect::<Result<_, _>>()?;
        out.push(json!({"name": s.name, "arn": s.arn, "mode": s.mode, "status": s.status, "encryption": s.encryption, "keyId": s.key_id, "metrics": s.metrics, "retentionHours": s.retention, "createdAt": s.created, "tags": s.tags,
            "records": total, "openShards": open_count(&s), "shards": shards, "consumers": cons}));
    }
    Ok(out)
}

/// The newest records (of one shard or all), as people read them.
pub fn newest(cn: &Connection, stream: &str, shard: Option<&str>, since: i64, limit: i64) -> R<Vec<Value>> {
    load(cn, stream)?;
    let mut st = cn.prepare("SELECT shard, seq, pkey, data, at, producer FROM kinesis_records WHERE stream=?1 AND (?2 IS NULL OR shard=?2) AND at>=?3 ORDER BY seq DESC LIMIT ?4")?;
    let v = st.query_map(params![stream, shard, since, limit], |r| {
        let d: Vec<u8> = r.get(3)?;
        let shown = match serde_json::from_slice::<Value>(&d) { Ok(v) => json!({"format": "json", "value": v}), Err(_) => match String::from_utf8(d.clone()) { Ok(t) => json!({"format": "text", "value": t}), Err(_) => json!({"format": "binary", "base64": B64.encode(&d)}) } };
        Ok(json!({"shard": r.get::<_, String>(0)?, "sequenceNumber": seq_str(r.get(1)?), "partitionKey": r.get::<_, String>(2)?, "data": shown, "bytes": d.len(), "at": r.get::<_, i64>(4)?, "producer": r.get::<_, String>(5)?}))
    })?.collect::<Result<_, _>>()?;
    Ok(v)
}

/// What Pub/Sub ingestion reads: (seq, partition key, data) of one shard from a sequence number.
pub fn shard_records(cn: &Connection, stream: &str, shard: &str, from: i64, now: i64) -> R<Vec<(i64, String, Vec<u8>)>> {
    let s = load(cn, stream)?;
    let sh = s.shard(shard)?;
    let mut st = cn.prepare("SELECT seq, pkey, data FROM kinesis_records WHERE stream=?1 AND shard=?2 AND seq>=?3 AND at>=?4 ORDER BY seq LIMIT 1000")?;
    let v = st.query_map(params![s.name, sh.id, from, now - s.retention * 3_600_000], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
    Ok(v)
}

#[cfg(test)]
mod t {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SQL).unwrap();
        c
    }

    fn put(c: &mut Connection, pk: &str, data: &str) -> Value {
        put_record(c, &json!({"StreamName": "shipments", "PartitionKey": pk, "Data": B64.encode(data)}), 1_000, "app", &Limiter::off()).unwrap()
    }

    #[test]
    fn hash_ranges_cover_the_key_space() {
        assert_eq!(uniform(1), vec![(0, MAX)]);
        let two = uniform(2);
        assert_eq!(two[1].0, 1u128 << 127);
        assert_eq!(two[0].1 + 1, two[1].0);
        let four = uniform(4);
        assert_eq!(four[3].1, MAX);
        assert!(four.windows(2).all(|w| w[0].1 + 1 == w[1].0));
        // MD5 of the partition key, big-endian, as the AWS docs compute it.
        assert_eq!(hash("SHP-1042"), u128::from_str_radix(&format!("{:x}", md5::compute("SHP-1042")), 16).unwrap());
        assert_eq!(stream_name("arn:aws:kinesis:us-east-1:000000000000:stream/shipments/consumer/billing:1763190000"), "shipments");
        assert_eq!(seq_parse(&seq_str(42)).unwrap(), 42);
    }

    #[test]
    fn put_read_split_and_children() {
        let mut c = db();
        create(&c, &json!({"StreamName": "shipments", "ShardCount": 2}), "us-east-1", 0, 0).unwrap();
        assert_eq!(create(&c, &json!({"StreamName": "shipments", "ShardCount": 2}), "us-east-1", 0, 0).unwrap_err().code, "ResourceInUseException");
        for i in 0..6 { put(&mut c, &format!("SHP-{i}"), &format!("{{\"n\":{i}}}")); }
        let s = load(&c, "shipments").unwrap();
        let total: usize = s.shards.iter().map(|sh| read(&c, &s, sh, sh.start_seq, 100, 1_000).unwrap().records.len()).sum();
        assert_eq!(total, 6);
        // Same key, same shard.
        let a = put(&mut c, "SHP-9", "x"); let b = put(&mut c, "SHP-9", "y");
        assert_eq!(a["ShardId"], b["ShardId"]);
        // Iterators: TRIM_HORIZON reads all, AFTER the first skips it, an old one expired.
        let it = get_shard_iterator(&c, &json!({"StreamName": "shipments", "ShardId": a["ShardId"], "ShardIteratorType": "AFTER_SEQUENCE_NUMBER", "StartingSequenceNumber": a["SequenceNumber"]}), 1_000).unwrap();
        let r = get_records(&c, &json!({"ShardIterator": it["ShardIterator"], "Limit": 1}), 2_000, &Limiter::off()).unwrap();
        assert_eq!(r["Records"][0]["SequenceNumber"], b["SequenceNumber"]);
        assert_eq!(get_records(&c, &json!({"ShardIterator": it["ShardIterator"]}), 1_000 + ITERATOR_TTL + 1, &Limiter::off()).unwrap_err().code, "ExpiredIteratorException");
        // Split shard 0: it closes, its children start after; reading it to its end shows them.
        let s0 = s.shards[0].clone();
        split(&mut c, &json!({"StreamName": "shipments", "ShardToSplit": s0.id, "NewStartingHashKey": (s0.end / 2).to_string()})).unwrap();
        let s = load(&c, "shipments").unwrap();
        assert_eq!(s.shards.iter().filter(|x| x.open()).count(), 3);
        let r = read(&c, &s, s.shard(&s0.id).unwrap(), s0.start_seq, 100, 1_000).unwrap();
        assert!(r.next.is_none() && r.children.len() == 2);
        // Merge the two children back.
        let kids: Vec<String> = s.shards.iter().filter(|x| x.parent.as_deref() == Some(&s0.id)).map(|x| x.id.clone()).collect();
        merge(&mut c, &json!({"StreamName": "shipments", "ShardToMerge": kids[0], "AdjacentShardToMerge": kids[1]})).unwrap();
        let u = update_shard_count(&mut c, &json!({"StreamName": "shipments", "TargetShardCount": 4})).unwrap();
        assert_eq!((u["CurrentShardCount"].as_i64(), u["TargetShardCount"].as_i64()), (Some(2), Some(4)));
        // Partial failures: every second record.
        let p = put_records(&mut c, &json!({"StreamName": "shipments", "Records": [{"PartitionKey": "a", "Data": "eA=="}, {"PartitionKey": "b", "Data": "eA=="}, {"PartitionKey": "c", "Data": "eA=="}]}), 1_000, "app", |i| i % 2 == 1, &Limiter::off()).unwrap();
        assert_eq!(p["FailedRecordCount"], 1);
        assert_eq!(p["Records"][1]["ErrorCode"], "ProvisionedThroughputExceededException");
        // Retention on the clock: 25 h later nothing is read.
        let s = load(&c, "shipments").unwrap();
        let sh = s.shards.iter().find(|x| x.open()).unwrap();
        assert!(read(&c, &s, s.shard(&s.shards[1].id).unwrap(), 0, 100, 25 * 3_600_000).unwrap().records.is_empty());
        let _ = sh;
    }

    #[test]
    fn lifecycle_mode_encryption_monitoring() {
        let c = db();
        let far = wall() + 600_000;
        create(&c, &json!({"StreamName": "orders", "ShardCount": 1}), "us-east-1", 0, far).unwrap();
        assert_eq!(load(&c, "orders").unwrap().status, "CREATING");
        assert_eq!(gate(&c, "PutRecord", "orders").unwrap_err().code, "ResourceNotFoundException");
        assert_eq!(gate(&c, "SplitShard", "orders").unwrap_err().code, "ResourceInUseException");
        assert!(gate(&c, "DescribeStream", "orders").is_ok());
        settle(&c, far).unwrap();
        assert_eq!(load(&c, "orders").unwrap().status, "ACTIVE");
        transition(&c, "orders", "UPDATING", far + 600_000).unwrap();
        assert!(gate(&c, "PutRecord", "orders").is_ok());
        assert_eq!(gate(&c, "UpdateStreamMode", "orders").unwrap_err().code, "ResourceInUseException");
        transition(&c, "orders", "ACTIVE", 0).unwrap();
        // Mode.
        update_mode(&c, &json!({"StreamName": "orders", "StreamModeDetails": {"StreamMode": "ON_DEMAND"}})).unwrap();
        assert_eq!(load(&c, "orders").unwrap().mode, "ON_DEMAND");
        assert_eq!(update_mode(&c, &json!({"StreamName": "orders", "StreamModeDetails": {"StreamMode": "ON_DEMAND"}})).unwrap_err().code, "InvalidArgumentException");
        // Encryption: key formats.
        for k in ["alias/aws/kinesis", "1234abcd-12ab-34cd-56ef-1234567890ab", "arn:aws:kms:us-east-1:111122223333:key/1234abcd-12ab-34cd-56ef-1234567890ab",
                  "arn:aws:kms:us-east-1:111122223333:alias/orders-key", "mrk-1234abcd12ab34cd56ef1234567890ab"] { assert!(kms_key_ok(k), "{k}") }
        for k in ["", "orders-key", "arn:aws:kms:us-east-1:1:key/x", "1234abcd-12ab-34cd-56ef-1234567890aZ", "arn:aws:s3:::bucket"] { assert!(!kms_key_ok(k), "{k}") }
        assert_eq!(encryption(&c, &json!({"StreamName": "orders", "EncryptionType": "KMS", "KeyId": "nope"}), true).unwrap_err().code, "InvalidArgumentException");
        encryption(&c, &json!({"StreamName": "orders", "EncryptionType": "KMS", "KeyId": "alias/aws/kinesis"}), true).unwrap();
        let d = describe(&c, &json!({"StreamName": "orders"}), true).unwrap();
        assert_eq!((d["StreamDescriptionSummary"]["EncryptionType"].as_str(), d["StreamDescriptionSummary"]["KeyId"].as_str()), (Some("KMS"), Some("alias/aws/kinesis")));
        assert!(encrypted(&c, "orders"));
        // Monitoring.
        let m = monitoring(&c, &json!({"StreamName": "orders", "ShardLevelMetrics": ["IncomingBytes"]}), true).unwrap();
        assert_eq!(m["DesiredShardLevelMetrics"], json!(["IncomingBytes"]));
        let m = monitoring(&c, &json!({"StreamName": "orders", "ShardLevelMetrics": ["ALL"]}), true).unwrap();
        assert_eq!(m["DesiredShardLevelMetrics"].as_array().unwrap().len(), 7);
        let m = monitoring(&c, &json!({"StreamName": "orders", "ShardLevelMetrics": ["IncomingBytes"]}), false).unwrap();
        assert_eq!(m["DesiredShardLevelMetrics"].as_array().unwrap().len(), 6);
        // Delete: DELETING, then gone.
        delete(&c, &json!({"StreamName": "orders"}), far).unwrap();
        assert_eq!(load(&c, "orders").unwrap().status, "DELETING");
        settle(&c, far).unwrap();
        assert_eq!(load(&c, "orders").unwrap_err().code, "ResourceNotFoundException");
    }
}
