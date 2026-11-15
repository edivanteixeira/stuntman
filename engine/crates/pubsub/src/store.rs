//! Topics, subscriptions, messages and each subscription's deliveries (pending, outstanding with an
//! ack deadline, acked, dead-lettered); schemas with their revisions, snapshots, and what BigQuery and
//! Cloud Storage subscriptions wrote. SQLite of the environment. Names are the full resource names:
//! projects/<p>/topics/<t>, projects/<p>/subscriptions/<s>. The settings beyond the columns (schema,
//! retention, push, filter...) are the Topic and Subscription protos as sent, kept beside each row.

use std::collections::{BTreeMap, HashSet};

use prost::Message;
use rand::Rng;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx};

use crate::{filter, v1::{Subscription, Topic}};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS pubsub_topics(name TEXT PRIMARY KEY, labels TEXT NOT NULL DEFAULT '{}', created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pubsub_subscriptions(name TEXT PRIMARY KEY, topic TEXT NOT NULL, ack_deadline INTEGER NOT NULL, ordering INTEGER NOT NULL DEFAULT 0,
  dead_letter TEXT NOT NULL DEFAULT '', max_attempts INTEGER NOT NULL DEFAULT 0, labels TEXT NOT NULL DEFAULT '{}', created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pubsub_messages(id INTEGER PRIMARY KEY AUTOINCREMENT, topic TEXT NOT NULL, data BLOB NOT NULL, attributes TEXT NOT NULL DEFAULT '{}',
  ordering_key TEXT NOT NULL DEFAULT '', published INTEGER NOT NULL, publisher TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS pubsub_messages_topic ON pubsub_messages(topic, id);
CREATE TABLE IF NOT EXISTS pubsub_deliveries(sub TEXT NOT NULL, msg INTEGER NOT NULL, state TEXT NOT NULL DEFAULT 'pending', attempts INTEGER NOT NULL DEFAULT 0,
  deadline INTEGER NOT NULL DEFAULT 0, ack_id TEXT NOT NULL DEFAULT '', acked INTEGER, PRIMARY KEY(sub, msg));
CREATE INDEX IF NOT EXISTS pubsub_deliveries_ack ON pubsub_deliveries(ack_id);
CREATE TABLE IF NOT EXISTS pubsub_topic_conf(name TEXT PRIMARY KEY, pb BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS pubsub_sub_conf(name TEXT PRIMARY KEY, pb BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS pubsub_schemas(seq INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, revision TEXT NOT NULL, kind INTEGER NOT NULL, definition TEXT NOT NULL, created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pubsub_snapshots(name TEXT PRIMARY KEY, topic TEXT NOT NULL, sub TEXT NOT NULL, labels TEXT NOT NULL DEFAULT '{}', created INTEGER NOT NULL,
  expire INTEGER NOT NULL, last_msg INTEGER NOT NULL, msgs TEXT NOT NULL DEFAULT '[]');
CREATE TABLE IF NOT EXISTS pubsub_bq_rows(id INTEGER PRIMARY KEY AUTOINCREMENT, sub TEXT NOT NULL, tbl TEXT NOT NULL, row TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pubsub_gcs_files(id INTEGER PRIMARY KEY AUTOINCREMENT, sub TEXT NOT NULL, bucket TEXT NOT NULL, object TEXT NOT NULL, data BLOB NOT NULL,
  messages INTEGER NOT NULL, format TEXT NOT NULL, stored TEXT NOT NULL DEFAULT '', at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pubsub_bt_rows(id INTEGER PRIMARY KEY AUTOINCREMENT, sub TEXT NOT NULL, tbl TEXT NOT NULL, row_key TEXT NOT NULL, cells TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pubsub_bq_tables(tbl TEXT PRIMARY KEY, schema TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS pubsub_sub_seen(name TEXT PRIMARY KEY, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pubsub_sub_errors(name TEXT PRIMARY KEY, error TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pubsub_ingest(topic TEXT NOT NULL, k TEXT NOT NULL, v INTEGER NOT NULL, PRIMARY KEY(topic, k));
";

/// What a subscription's topic becomes when the topic is deleted (as Pub/Sub does).
pub const DELETED_TOPIC: &str = "_deleted-topic_";
/// What a topic's schema becomes when the schema is deleted.
pub const DELETED_SCHEMA: &str = "_deleted-schema_";
pub const DAY_MS: i64 = 86_400_000;

pub type Labels = BTreeMap<String, String>;

pub fn dur_ms(d: &Option<prost_types::Duration>) -> Option<i64> {
    d.as_ref().map(|d| d.seconds * 1000 + d.nanos as i64 / 1_000_000).filter(|x| *x > 0)
}

pub fn dur(ms: i64) -> prost_types::Duration {
    prost_types::Duration { seconds: ms / 1000, nanos: (ms % 1000) as i32 * 1_000_000 }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sub {
    pub name: String,
    pub topic: String,
    pub ack_deadline: i32,
    pub ordering: bool,
    pub dead_letter: String,
    pub max_attempts: i32,
    pub labels: Labels,
    /// The whole subscription as last created or updated (push, BigQuery, filter, retention...).
    pub pb: Subscription,
}

impl Sub {
    /// The row a subscription proto makes, with Pub/Sub's defaults and bounds.
    pub fn from_pb(mut pb: Subscription) -> Sub {
        pb.ack_deadline_seconds = if pb.ack_deadline_seconds == 0 { 10 } else { pb.ack_deadline_seconds.clamp(10, 600) };
        if let Some(d) = pb.dead_letter_policy.as_mut() {
            d.max_delivery_attempts = if d.max_delivery_attempts == 0 { 5 } else { d.max_delivery_attempts.clamp(5, 100) };
        }
        if pb.dead_letter_policy.as_ref().is_some_and(|d| d.dead_letter_topic.is_empty()) { pb.dead_letter_policy = None }
        if pb.push_config.as_ref().is_some_and(|p| p.push_endpoint.is_empty()) { pb.push_config = None }
        let (dl, max) = pb.dead_letter_policy.as_ref().map(|d| (d.dead_letter_topic.clone(), d.max_delivery_attempts)).unwrap_or_default();
        Sub { name: pb.name.clone(), topic: pb.topic.clone(), ack_deadline: pb.ack_deadline_seconds, ordering: pb.enable_message_ordering, dead_letter: dl, max_attempts: max,
            labels: pb.labels.clone().into_iter().collect(), pb }
    }

    /// pull | push | bigquery | cloud_storage
    pub fn kind(&self) -> &'static str {
        if self.pb.bigquery_config.is_some() { "bigquery" } else if self.pb.bigtable_config.is_some() { "bigtable" } else if self.pb.cloud_storage_config.is_some() { "cloud_storage" } else if self.pb.push_config.is_some() { "push" } else { "pull" }
    }

    /// How long unacked (and, retained, acked) messages stay: 7 days unless set.
    pub fn retention_ms(&self) -> i64 {
        dur_ms(&self.pb.message_retention_duration).unwrap_or(7 * DAY_MS)
    }

    pub fn exactly_once(&self) -> bool {
        self.pb.enable_exactly_once_delivery
    }

    /// How long a message waits before it comes again after a nack, an expired deadline or a failed
    /// push/write: the retry policy's backoff (minimum doubling to maximum), else none for pull and
    /// Pub/Sub's push backoff (100 ms doubling to 60 s) for the rest.
    pub fn backoff_ms(&self, attempt: i32) -> i64 {
        let (lo, hi) = match &self.pb.retry_policy {
            Some(r) => (dur_ms(&r.minimum_backoff).unwrap_or(10_000), dur_ms(&r.maximum_backoff).unwrap_or(600_000)),
            None if self.kind() == "pull" => return 0,
            None => (100, 60_000),
        };
        lo.saturating_mul(1 << (attempt - 1).clamp(0, 20)).min(hi)
    }

    /// Idle time after which the subscription is deleted (None: never). Unset policy: 31 days.
    pub fn ttl_ms(&self) -> Option<i64> {
        match &self.pb.expiration_policy { None => Some(31 * DAY_MS), Some(p) => dur_ms(&p.ttl) }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Msg {
    pub id: i64,
    pub data: Vec<u8>,
    pub attributes: Labels,
    pub ordering_key: String,
    pub published: i64,
}

#[derive(Clone, Debug)]
pub struct Got {
    pub msg: Msg,
    pub ack_id: String,
    pub attempt: i32,
}

fn labels(s: String) -> Labels {
    serde_json::from_str(&s).unwrap_or_default()
}

fn sub_row(r: &rusqlite::Row) -> rusqlite::Result<Sub> {
    let mut pb = Subscription::decode(r.get::<_, Vec<u8>>(7)?.as_slice()).unwrap_or_default();
    let labels = labels(r.get(6)?);
    pb.name = r.get(0)?;
    pb.topic = r.get(1)?;
    pb.labels = labels.clone().into_iter().collect();
    Ok(Sub { name: pb.name.clone(), topic: pb.topic.clone(), ack_deadline: r.get(2)?, ordering: r.get::<_, i64>(3)? != 0, dead_letter: r.get(4)?, max_attempts: r.get(5)?, labels, pb })
}

const SUB_COLS: &str = "s.name, s.topic, s.ack_deadline, s.ordering, s.dead_letter, s.max_attempts, s.labels, COALESCE(c.pb, x'') FROM pubsub_subscriptions s LEFT JOIN pubsub_sub_conf c ON c.name=s.name";

/// A topic with only a name (tools make them so).
pub fn bare(name: &str) -> Topic {
    Topic { name: name.into(), ..Default::default() }
}

fn topic_row(name: String, l: String, pb: Vec<u8>) -> Topic {
    let mut t = Topic::decode(pb.as_slice()).unwrap_or_default();
    t.name = name;
    t.labels = labels(l).into_iter().collect();
    t
}

pub async fn create_topic(ctx: &Ctx, t: &Topic) -> Result<bool, ApiError> {
    let (n, l, pb) = (t.name.clone(), json!(t.labels.iter().collect::<BTreeMap<_, _>>()).to_string(), t.encode_to_vec());
    let made = ctx.db.call(move |cn| {
        let made = cn.execute("INSERT OR IGNORE INTO pubsub_topics(name, labels, created) VALUES(?1,?2,?3)", params![n, l, util::now_ms()])? > 0;
        if made { cn.execute("INSERT OR REPLACE INTO pubsub_topic_conf(name, pb) VALUES(?1,?2)", params![n, pb])?; }
        Ok(made)
    }).await?;
    if made { ctx.live.send("pubsub.topics", &t.name, "created"); }
    Ok(made)
}

pub async fn topic(ctx: &Ctx, name: &str) -> Result<Option<Topic>, ApiError> {
    let n = name.to_string();
    ctx.db.call(move |cn| cn.query_row("SELECT t.name, t.labels, COALESCE(c.pb, x'') FROM pubsub_topics t LEFT JOIN pubsub_topic_conf c ON c.name=t.name WHERE t.name=?1", [n],
        |r| Ok(topic_row(r.get(0)?, r.get(1)?, r.get(2)?))).optional()).await
}

/// Topics whose name starts with `prefix` ("projects/acme-stg/topics/", or "" for all).
pub async fn topics(ctx: &Ctx, prefix: &str) -> Result<Vec<Topic>, ApiError> {
    let p = format!("{prefix}%");
    ctx.db.call(move |cn| {
        let mut st = cn.prepare("SELECT t.name, t.labels, COALESCE(c.pb, x'') FROM pubsub_topics t LEFT JOIN pubsub_topic_conf c ON c.name=t.name WHERE t.name LIKE ?1 ORDER BY t.name")?;
        let v = st.query_map([p], |r| Ok(topic_row(r.get(0)?, r.get(1)?, r.get(2)?)))?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

pub async fn save_topic(ctx: &Ctx, t: &Topic) -> Result<(), ApiError> {
    let (n, l, pb) = (t.name.clone(), json!(t.labels.iter().collect::<BTreeMap<_, _>>()).to_string(), t.encode_to_vec());
    ctx.db.call(move |cn| {
        cn.execute("UPDATE pubsub_topics SET labels=?2 WHERE name=?1", params![n, l])?;
        cn.execute("INSERT OR REPLACE INTO pubsub_topic_conf(name, pb) VALUES(?1,?2)", params![n, pb]).map(|_| ())
    }).await?;
    ctx.live.send("pubsub.topics", &t.name, "updated");
    Ok(())
}

/// Deletes a topic; its subscriptions stay, detached (their topic reads _deleted-topic_).
pub async fn delete_topic(ctx: &Ctx, name: &str) -> Result<bool, ApiError> {
    let n = name.to_string();
    let gone = ctx.db.call(move |cn| {
        let k = cn.execute("DELETE FROM pubsub_topics WHERE name=?1", [&n])?;
        cn.execute("DELETE FROM pubsub_topic_conf WHERE name=?1", [&n])?;
        cn.execute("UPDATE pubsub_subscriptions SET topic=?2 WHERE topic=?1", params![n, DELETED_TOPIC])?;
        Ok(k > 0)
    }).await?;
    if gone { ctx.live.send("pubsub.topics", name, "deleted"); }
    Ok(gone)
}

pub async fn create_sub(ctx: &Ctx, s: &Sub) -> Result<bool, ApiError> {
    let s2 = s.clone();
    let made = ctx.db.call(move |cn| {
        let made = cn.execute("INSERT OR IGNORE INTO pubsub_subscriptions(name, topic, ack_deadline, ordering, dead_letter, max_attempts, labels, created) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![s2.name, s2.topic, s2.ack_deadline, s2.ordering as i64, s2.dead_letter, s2.max_attempts, json!(s2.labels).to_string(), util::now_ms()])? > 0;
        if made { cn.execute("INSERT OR REPLACE INTO pubsub_sub_conf(name, pb) VALUES(?1,?2)", params![s2.name, s2.pb.encode_to_vec()])?; }
        Ok(made)
    }).await?;
    if made { ctx.live.send("pubsub.subscriptions", &s.name, "created"); }
    Ok(made)
}

/// Writes an updated subscription (UpdateSubscription, ModifyPushConfig, DetachSubscription).
pub async fn save_sub(ctx: &Ctx, s: &Sub) -> Result<(), ApiError> {
    let s2 = s.clone();
    ctx.db.call(move |cn| {
        cn.execute("UPDATE pubsub_subscriptions SET ack_deadline=?2, ordering=?3, dead_letter=?4, max_attempts=?5, labels=?6 WHERE name=?1",
            params![s2.name, s2.ack_deadline, s2.ordering as i64, s2.dead_letter, s2.max_attempts, json!(s2.labels).to_string()])?;
        cn.execute("INSERT OR REPLACE INTO pubsub_sub_conf(name, pb) VALUES(?1,?2)", params![s2.name, s2.pb.encode_to_vec()]).map(|_| ())
    }).await?;
    ctx.live.send("pubsub.subscriptions", &s.name, "updated");
    crate::ring();
    Ok(())
}

pub async fn sub(ctx: &Ctx, name: &str) -> Result<Option<Sub>, ApiError> {
    let n = name.to_string();
    ctx.db.call(move |cn| cn.query_row(&format!("SELECT {SUB_COLS} WHERE s.name=?1"), [n], sub_row).optional()).await
}

/// Subscriptions by name prefix, or of one topic.
pub async fn subs(ctx: &Ctx, prefix: &str, topic: Option<&str>) -> Result<Vec<Sub>, ApiError> {
    let (p, t) = (format!("{prefix}%"), topic.map(String::from));
    ctx.db.call(move |cn| {
        let mut st = cn.prepare(&format!("SELECT {SUB_COLS} WHERE s.name LIKE ?1 AND (?2 IS NULL OR s.topic=?2) ORDER BY s.name"))?;
        let v = st.query_map(params![p, t], sub_row)?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

pub async fn delete_sub(ctx: &Ctx, name: &str) -> Result<bool, ApiError> {
    let n = name.to_string();
    let gone = ctx.db.call(move |cn| {
        let k = cn.execute("DELETE FROM pubsub_subscriptions WHERE name=?1", [&n])?;
        cn.execute("DELETE FROM pubsub_sub_conf WHERE name=?1", [&n])?;
        cn.execute("DELETE FROM pubsub_deliveries WHERE sub=?1", [&n])?;
        Ok(k > 0)
    }).await?;
    if gone { ctx.live.send("pubsub.subscriptions", name, "deleted"); }
    Ok(gone)
}

/// Drops what a detached subscription held.
pub async fn drop_deliveries(ctx: &Ctx, sub: &str) -> Result<(), ApiError> {
    let n = sub.to_string();
    ctx.db.call(move |cn| cn.execute("DELETE FROM pubsub_deliveries WHERE sub=?1", [n]).map(|_| ())).await
}

/// The subscriptions of a topic a message goes to: not detached, and its filter matches.
fn takers(tx: &Transaction, topic: &str) -> rusqlite::Result<Vec<(String, String)>> {
    let mut st = tx.prepare("SELECT s.name, COALESCE(c.pb, x'') FROM pubsub_subscriptions s LEFT JOIN pubsub_sub_conf c ON c.name=s.name WHERE s.topic=?1")?;
    let v = st.query_map([topic], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))?.filter_map(|x| x.ok())
        .filter_map(|(n, pb)| { let pb = Subscription::decode(pb.as_slice()).unwrap_or_default(); (!pb.detached).then_some((n, pb.filter)) }).collect();
    Ok(v)
}

/// One message onto a topic and a pending delivery for each subscription that takes it (only those
/// that exist now: Pub/Sub does not give a new subscription older messages).
fn insert(tx: &Transaction, topic: &str, data: &[u8], attrs: &Labels, key: &str, publisher: &str) -> rusqlite::Result<i64> {
    tx.execute("INSERT INTO pubsub_messages(topic, data, attributes, ordering_key, published, publisher) VALUES(?1,?2,?3,?4,?5,?6)",
        params![topic, data, json!(attrs).to_string(), key, util::now_ms(), publisher])?;
    let id = tx.last_insert_rowid();
    for (sub, f) in takers(tx, topic)? {
        if filter::matches(&f, attrs) {
            tx.execute("INSERT INTO pubsub_deliveries(sub, msg) VALUES(?1,?2)", params![sub, id])?;
        }
    }
    Ok(id)
}

/// Publishes (data, attributes, ordering key) messages; returns their ids.
pub async fn publish(ctx: &Ctx, topic: &str, msgs: Vec<(Vec<u8>, Labels, String)>, publisher: &str) -> Result<Vec<i64>, ApiError> {
    let (t, p) = (topic.to_string(), publisher.to_string());
    let ids = ctx.db.call(move |cn| {
        let tx = cn.transaction()?;
        let ids = msgs.iter().map(|(d, a, k)| insert(&tx, &t, d, a, k, &p)).collect::<rusqlite::Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(ids)
    }).await?;
    ctx.live.send("pubsub.messages", topic, "created");
    crate::ring();
    Ok(ids)
}

/// Hands out up to `max` messages: pending ones and outstanding ones whose deadline passed, still in
/// the retention window. With message ordering, a key with a message still out waits. A message
/// delivered max_attempts times already goes to the dead-letter topic instead.
pub async fn pull(ctx: &Ctx, sub: &Sub, max: usize, secs: i32) -> Result<Vec<Got>, ApiError> {
    pull_bytes(ctx, sub, max, secs, 0).await
}

/// pull, also stopping before `max_bytes` of data (0: no limit; at least one message goes; negative:
/// nothing past -max_bytes). A
/// message whose retry backoff has not passed waits.
pub async fn pull_bytes(ctx: &Ctx, sub: &Sub, max: usize, secs: i32, max_bytes: i64) -> Result<Vec<Got>, ApiError> {
    let s = sub.clone();
    let now = util::now_ms();
    let cutoff = now - sub.retention_ms();
    let (got, dead) = ctx.db.call(move |cn| {
        let tx = cn.transaction()?;
        let mut got = vec![];
        let mut dead = vec![];
        {
            let blocked: HashSet<String> = if s.ordering {
                let mut st = tx.prepare("SELECT DISTINCT m.ordering_key FROM pubsub_deliveries d JOIN pubsub_messages m ON m.id=d.msg WHERE d.sub=?1 AND d.state='outstanding' AND d.deadline>=?2 AND m.ordering_key!=''")?;
                let v = st.query_map(params![s.name, now], |r| r.get(0))?.filter_map(|x| x.ok()).collect();
                v
            } else {
                HashSet::new()
            };
            let mut st = tx.prepare("SELECT d.msg, d.attempts, m.data, m.attributes, m.ordering_key, m.published, d.state, d.deadline FROM pubsub_deliveries d JOIN pubsub_messages m ON m.id=d.msg
                WHERE d.sub=?1 AND (d.state='pending' OR (d.state='outstanding' AND d.deadline<?2)) AND m.published>=?3 ORDER BY d.msg LIMIT 1000")?;
            let rows = st.query_map(params![s.name, now, cutoff], |r| Ok((Msg { id: r.get(0)?, data: r.get(2)?, attributes: labels(r.get(3)?), ordering_key: r.get(4)?, published: r.get(5)? }, r.get::<_, i32>(1)?,
                r.get::<_, String>(6)?, r.get::<_, i64>(7)?)))?;
            let mut seen_keys = HashSet::new();
            let mut bytes = 0i64;
            for r in rows {
                let (m, attempts, state, deadline) = r?;
                if got.len() >= max { break }
                if max_bytes != 0 && (max_bytes < 0 || !got.is_empty()) && bytes + m.data.len() as i64 > max_bytes.abs() { break }
                if s.kind() == "pull" && state == "outstanding" && attempts > 0 && deadline + s.backoff_ms(attempts) > now { continue }
                if s.ordering && !m.ordering_key.is_empty() && (blocked.contains(&m.ordering_key) || (s.kind() == "push" && !seen_keys.insert(m.ordering_key.clone()))) { continue }
                if s.max_attempts > 0 && attempts >= s.max_attempts { dead.push(m); continue }
                bytes += m.data.len() as i64;
                let ack_id = format!("{}-{}-{:08x}", m.id, attempts + 1, rand::thread_rng().gen::<u32>());
                got.push(Got { msg: m, ack_id, attempt: attempts + 1 });
            }
        }
        for g in &got {
            tx.execute("UPDATE pubsub_deliveries SET state='outstanding', attempts=?3, deadline=?4, ack_id=?5 WHERE sub=?1 AND msg=?2", params![s.name, g.msg.id, g.attempt, now + secs as i64 * 1000, g.ack_id])?;
        }
        let dl_exists = !s.dead_letter.is_empty() && tx.query_row("SELECT 1 FROM pubsub_topics WHERE name=?1", [&s.dead_letter], |_| Ok(())).optional()?.is_some();
        for m in &dead {
            tx.execute("UPDATE pubsub_deliveries SET state='dead', ack_id='' WHERE sub=?1 AND msg=?2", params![s.name, m.id])?;
            if dl_exists {
                let mut a = m.attributes.clone();
                a.insert("CloudPubSubDeadLetterSourceSubscription".into(), s.name.rsplit('/').next().unwrap_or("").into());
                a.insert("CloudPubSubDeadLetterSourceDeliveryCount".into(), s.max_attempts.to_string());
                insert(&tx, &s.dead_letter, &m.data, &a, &m.ordering_key, "dead-letter")?;
            }
        }
        tx.commit()?;
        Ok((got, dead.len()))
    }).await?;
    if !got.is_empty() || dead > 0 { ctx.live.send("pubsub.deliveries", &sub.name, "updated"); }
    if dead > 0 { crate::ring(); }
    Ok(got)
}

/// Messages of a subscription out now (not acked, deadline not passed): flow control.
pub async fn outstanding(ctx: &Ctx, sub: &str) -> Result<i64, ApiError> {
    let s = sub.to_string();
    ctx.db.call(move |cn| cn.query_row("SELECT COUNT(*) FROM pubsub_deliveries WHERE sub=?1 AND state='outstanding' AND deadline>=?2", params![s, util::now_ms()], |r| r.get(0))).await
}

/// (messages, bytes, oldest publish time) waiting for a subscription: what a Cloud Storage
/// subscription batches.
pub async fn backlog(ctx: &Ctx, sub: &Sub) -> Result<(i64, i64, Option<i64>), ApiError> {
    let (s, now, cutoff) = (sub.name.clone(), util::now_ms(), util::now_ms() - sub.retention_ms());
    ctx.db.call(move |cn| cn.query_row("SELECT COUNT(*), COALESCE(SUM(LENGTH(m.data)),0), MIN(m.published) FROM pubsub_deliveries d JOIN pubsub_messages m ON m.id=d.msg
        WHERE d.sub=?1 AND (d.state='pending' OR (d.state='outstanding' AND d.deadline<?2)) AND m.published>=?3", params![s, now, cutoff], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))).await
}

/// Acks; answers the ack ids that matched nothing out (expired or unknown). With `duplicate`, a
/// first delivery's ack does not stick and the message comes again. `strict` (exactly-once): an
/// ack after the deadline does not count.
pub async fn ack(ctx: &Ctx, sub: &str, ids: Vec<String>, duplicate: bool, strict: bool) -> Result<Vec<String>, ApiError> {
    if ids.is_empty() { return Ok(vec![]) }
    let s = sub.to_string();
    let (n, bad) = ctx.db.call(move |cn| {
        let tx = cn.transaction()?;
        let now = util::now_ms();
        let floor = if strict { now } else { i64::MIN };
        let (mut n, mut bad) = (0, vec![]);
        for id in &ids {
            let k = if duplicate {
                tx.execute("UPDATE pubsub_deliveries SET state=CASE WHEN attempts<=1 THEN 'pending' ELSE 'acked' END, acked=CASE WHEN attempts<=1 THEN NULL ELSE ?3 END, ack_id='' WHERE sub=?1 AND ack_id=?2 AND state='outstanding' AND deadline>=?4", params![s, id, now, floor])?
            } else {
                tx.execute("UPDATE pubsub_deliveries SET state='acked', acked=?3, ack_id='' WHERE sub=?1 AND ack_id=?2 AND state='outstanding' AND deadline>=?4", params![s, id, now, floor])?
            };
            n += k;
            if k == 0 { bad.push(id.clone()) }
        }
        tx.commit()?;
        Ok((n, bad))
    }).await?;
    if n > 0 { ctx.live.send("pubsub.deliveries", sub, "updated"); }
    if duplicate { crate::ring(); }
    Ok(bad)
}

/// Moves the ack deadline of outstanding messages; 0 hands them back now (a nack). Answers the ack
/// ids that matched nothing out.
pub async fn modack(ctx: &Ctx, sub: &Sub, ids: Vec<(String, i32)>) -> Result<Vec<String>, ApiError> {
    if ids.is_empty() { return Ok(vec![]) }
    let (s, strict) = (sub.name.clone(), sub.exactly_once());
    // With a retry policy a nack keeps the message out (no ack id) until its backoff passes.
    let nack = if sub.pb.retry_policy.is_some() { "UPDATE pubsub_deliveries SET deadline=?4, ack_id='' WHERE sub=?1 AND ack_id=?2 AND state='outstanding' AND deadline>=?3" }
        else { "UPDATE pubsub_deliveries SET state='pending', ack_id='' WHERE sub=?1 AND ack_id=?2 AND state='outstanding' AND deadline>=?3 AND ?4 IS NOT NULL" };
    let (nacked, bad) = ctx.db.call(move |cn| {
        let tx = cn.transaction()?;
        let now = util::now_ms();
        let floor = if strict { now } else { i64::MIN };
        let (mut nacked, mut bad) = (false, vec![]);
        for (id, secs) in &ids {
            let k = if *secs <= 0 {
                let k = tx.execute(nack, params![s, id, floor, now])?;
                nacked |= k > 0;
                k
            } else {
                tx.execute("UPDATE pubsub_deliveries SET deadline=?3 WHERE sub=?1 AND ack_id=?2 AND state='outstanding' AND deadline>=?4", params![s, id, now + (*secs).min(600) as i64 * 1000, floor])?
            };
            if k == 0 { bad.push(id.clone()) }
        }
        tx.commit()?;
        Ok((nacked, bad))
    }).await?;
    if nacked { ctx.live.send("pubsub.deliveries", &sub.name, "updated"); crate::ring(); }
    Ok(bad)
}

/// A push that failed: the message stays out and comes again after `ms` (the push backoff).
pub async fn retry_after(ctx: &Ctx, sub: &str, ack_id: &str, ms: i64) -> Result<(), ApiError> {
    let (s, a) = (sub.to_string(), ack_id.to_string());
    ctx.db.call(move |cn| cn.execute("UPDATE pubsub_deliveries SET deadline=?3 WHERE sub=?1 AND ack_id=?2 AND state='outstanding'", params![s, a, util::now_ms() + ms]).map(|_| ())).await?;
    ctx.live.send("pubsub.deliveries", sub, "updated");
    Ok(())
}

/// Gives a subscription deliveries (acked, for a seek to decide) of its topic's messages it lacks:
/// those of a snapshot, or still in the topic's retention.
fn backfill(tx: &Transaction, sub: &str, topic: &str, filter: &str, cond: &str, p: i64) -> rusqlite::Result<()> {
    let mut st = tx.prepare(&format!("SELECT id, attributes FROM pubsub_messages m WHERE topic=?1 AND {cond} AND NOT EXISTS (SELECT 1 FROM pubsub_deliveries d WHERE d.sub=?2 AND d.msg=m.id)"))?;
    let rows: Vec<(i64, String)> = st.query_map(params![topic, sub, p], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(|x| x.ok()).collect();
    for (id, a) in rows {
        if filter::matches(filter, &labels(a)) {
            tx.execute("INSERT OR IGNORE INTO pubsub_deliveries(sub, msg, state) VALUES(?1,?2,'acked')", params![sub, id])?;
        }
    }
    Ok(())
}

/// Seek to a time: messages published from then on are unacked again, older ones acked. Only what
/// is retained replays: acked messages need retain_acked_messages or topic retention; nothing older
/// than the retention window comes back. Topic retention also reaches messages published before the
/// subscription existed.
pub async fn seek(ctx: &Ctx, sub: &Sub, ms: i64, topic_retention: Option<i64>) -> Result<(), ApiError> {
    let s = sub.clone();
    let now = util::now_ms();
    let window = topic_retention.unwrap_or(0).max(if s.pb.retain_acked_messages { s.retention_ms() } else { 0 });
    let replay_acked = window > 0;
    let cutoff = now - window.max(s.retention_ms());
    ctx.db.call(move |cn| {
        let tx = cn.transaction()?;
        if let Some(r) = topic_retention { backfill(&tx, &s.name, &s.topic, &s.pb.filter, "published>=?3", ms.max(now - r))?; }
        tx.execute("UPDATE pubsub_deliveries SET ack_id='', state=CASE
              WHEN (SELECT published FROM pubsub_messages WHERE id=msg)<?2 THEN 'acked'
              WHEN (SELECT published FROM pubsub_messages WHERE id=msg)>=?3 AND (state!='acked' OR ?4) THEN 'pending'
              ELSE state END,
            acked=CASE WHEN (SELECT published FROM pubsub_messages WHERE id=msg)<?2 THEN COALESCE(acked, ?5) ELSE acked END
            WHERE sub=?1 AND state!='dead'", params![s.name, ms, cutoff, replay_acked, now])?;
        tx.commit()
    }).await?;
    ctx.live.send("pubsub.deliveries", &sub.name, "updated");
    crate::ring();
    Ok(())
}

// ---------------------------------------------------------------- snapshots

#[derive(Clone, Debug, Default)]
pub struct Snap {
    pub name: String,
    pub topic: String,
    pub sub: String,
    pub labels: Labels,
    pub created: i64,
    pub expire: i64,
    /// The newest message of the topic when it was taken: later ones are unacked in it.
    pub last_msg: i64,
    /// The messages unacked in the subscription when it was taken.
    pub msgs: Vec<i64>,
}

fn snap_row(r: &rusqlite::Row) -> rusqlite::Result<Snap> {
    Ok(Snap { name: r.get(0)?, topic: r.get(1)?, sub: r.get(2)?, labels: labels(r.get(3)?), created: r.get(4)?, expire: r.get(5)?, last_msg: r.get(6)?,
        msgs: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default() })
}

const SNAP_COLS: &str = "name, topic, sub, labels, created, expire, last_msg, msgs";

/// Takes a snapshot of a subscription's acks: None when the name is taken. It expires 7 days after
/// its oldest unacked message was published.
pub async fn create_snapshot(ctx: &Ctx, name: &str, sub: &Sub, l: &Labels) -> Result<Option<Snap>, ApiError> {
    let (n, s, l) = (name.to_string(), sub.clone(), l.clone());
    let now = util::now_ms();
    let snap = ctx.db.call(move |cn| {
        let tx = cn.transaction()?;
        if tx.query_row("SELECT 1 FROM pubsub_snapshots WHERE name=?1 AND expire>?2", params![n, now], |_| Ok(())).optional()?.is_some() { return Ok(None) }
        let msgs: Vec<(i64, i64)> = {
            let mut st = tx.prepare("SELECT d.msg, m.published FROM pubsub_deliveries d JOIN pubsub_messages m ON m.id=d.msg WHERE d.sub=?1 AND d.state IN ('pending','outstanding') ORDER BY d.msg")?;
            let v = st.query_map([&s.name], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(|x| x.ok()).collect();
            v
        };
        let last: i64 = tx.query_row("SELECT COALESCE(MAX(id),0) FROM pubsub_messages WHERE topic=?1", [&s.topic], |r| r.get(0))?;
        let oldest = msgs.iter().map(|x| x.1).min().unwrap_or(now);
        let snap = Snap { name: n.clone(), topic: s.topic.clone(), sub: s.name.clone(), labels: l, created: now, expire: oldest + 7 * DAY_MS, last_msg: last, msgs: msgs.iter().map(|x| x.0).collect() };
        tx.execute(&format!("INSERT OR REPLACE INTO pubsub_snapshots({SNAP_COLS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)"),
            params![snap.name, snap.topic, snap.sub, json!(snap.labels).to_string(), snap.created, snap.expire, snap.last_msg, json!(snap.msgs).to_string()])?;
        tx.commit()?;
        Ok(Some(snap))
    }).await?;
    if snap.is_some() { ctx.live.send("pubsub.snapshots", name, "created"); }
    Ok(snap)
}

/// A snapshot that has not expired.
pub async fn snapshot(ctx: &Ctx, name: &str) -> Result<Option<Snap>, ApiError> {
    let n = name.to_string();
    ctx.db.call(move |cn| cn.query_row(&format!("SELECT {SNAP_COLS} FROM pubsub_snapshots WHERE name=?1 AND expire>?2"), params![n, util::now_ms()], snap_row).optional()).await
}

/// Snapshots by name prefix, or of one topic, that have not expired.
pub async fn snapshots(ctx: &Ctx, prefix: &str, topic: Option<&str>) -> Result<Vec<Snap>, ApiError> {
    let (p, t) = (format!("{prefix}%"), topic.map(String::from));
    ctx.db.call(move |cn| {
        let mut st = cn.prepare(&format!("SELECT {SNAP_COLS} FROM pubsub_snapshots WHERE name LIKE ?1 AND (?2 IS NULL OR topic=?2) AND expire>?3 ORDER BY name"))?;
        let v = st.query_map(params![p, t, util::now_ms()], snap_row)?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

pub async fn update_snapshot(ctx: &Ctx, s: &Snap) -> Result<(), ApiError> {
    let (n, l, e) = (s.name.clone(), json!(s.labels).to_string(), s.expire);
    ctx.db.call(move |cn| cn.execute("UPDATE pubsub_snapshots SET labels=?2, expire=?3 WHERE name=?1", params![n, l, e]).map(|_| ())).await?;
    ctx.live.send("pubsub.snapshots", &s.name, "updated");
    Ok(())
}

pub async fn delete_snapshot(ctx: &Ctx, name: &str) -> Result<bool, ApiError> {
    let n = name.to_string();
    let gone = ctx.db.call(move |cn| cn.execute("DELETE FROM pubsub_snapshots WHERE name=?1", [n]).map(|k| k > 0)).await?;
    if gone { ctx.live.send("pubsub.snapshots", name, "deleted"); }
    Ok(gone)
}

/// Seek to a snapshot: what was unacked when it was taken, and everything the topic got since, is
/// unacked again; the rest acked.
pub async fn seek_snapshot(ctx: &Ctx, sub: &Sub, snap: &Snap) -> Result<(), ApiError> {
    let (s, snap) = (sub.clone(), snap.clone());
    let now = util::now_ms();
    ctx.db.call(move |cn| {
        let tx = cn.transaction()?;
        let set = json!(snap.msgs).to_string();
        backfill(&tx, &s.name, &s.topic, &s.pb.filter, &format!("(id>?3 OR id IN (SELECT value FROM json_each('{set}')))"), snap.last_msg)?;
        tx.execute("UPDATE pubsub_deliveries SET ack_id='', state=CASE WHEN msg>?2 OR msg IN (SELECT value FROM json_each(?3)) THEN 'pending' ELSE 'acked' END,
            acked=CASE WHEN msg>?2 OR msg IN (SELECT value FROM json_each(?3)) THEN NULL ELSE COALESCE(acked, ?4) END WHERE sub=?1 AND state!='dead'", params![s.name, snap.last_msg, set, now])?;
        tx.commit()
    }).await?;
    ctx.live.send("pubsub.deliveries", &sub.name, "updated");
    crate::ring();
    Ok(())
}

// ---------------------------------------------------------------- schemas

#[derive(Clone, Debug, Default)]
pub struct Rev {
    pub name: String,
    pub revision: String,
    pub kind: i32,
    pub definition: String,
    pub created: i64,
}

fn rev_row(r: &rusqlite::Row) -> rusqlite::Result<Rev> {
    Ok(Rev { name: r.get(0)?, revision: r.get(1)?, kind: r.get(2)?, definition: r.get(3)?, created: r.get(4)? })
}

/// A new revision of a schema (its first makes the schema).
pub async fn add_rev(ctx: &Ctx, name: &str, kind: i32, def: &str) -> Result<Rev, ApiError> {
    let r = Rev { name: name.into(), revision: format!("{:08x}", rand::thread_rng().gen::<u32>()), kind, definition: def.into(), created: util::now_ms() };
    let r2 = r.clone();
    ctx.db.call(move |cn| cn.execute("INSERT INTO pubsub_schemas(name, revision, kind, definition, created) VALUES(?1,?2,?3,?4,?5)", params![r2.name, r2.revision, r2.kind, r2.definition, r2.created]).map(|_| ())).await?;
    ctx.live.send("pubsub.schemas", name, "updated");
    Ok(r)
}

/// A schema's revisions, newest first (empty: no such schema).
pub async fn revs(ctx: &Ctx, name: &str) -> Result<Vec<Rev>, ApiError> {
    let n = name.to_string();
    ctx.db.call(move |cn| {
        let mut st = cn.prepare("SELECT name, revision, kind, definition, created FROM pubsub_schemas WHERE name=?1 ORDER BY seq DESC")?;
        let v = st.query_map([n], rev_row)?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

/// The newest revision of each schema under a prefix.
pub async fn schemas(ctx: &Ctx, prefix: &str) -> Result<Vec<Rev>, ApiError> {
    let p = format!("{prefix}%");
    ctx.db.call(move |cn| {
        let mut st = cn.prepare("SELECT name, revision, kind, definition, created FROM pubsub_schemas s WHERE name LIKE ?1 AND seq=(SELECT MAX(seq) FROM pubsub_schemas x WHERE x.name=s.name) ORDER BY name")?;
        let v = st.query_map([p], rev_row)?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

pub async fn delete_rev(ctx: &Ctx, name: &str, rev: &str) -> Result<bool, ApiError> {
    let (n, r) = (name.to_string(), rev.to_string());
    let gone = ctx.db.call(move |cn| cn.execute("DELETE FROM pubsub_schemas WHERE name=?1 AND revision=?2", params![n, r]).map(|k| k > 0)).await?;
    ctx.live.send("pubsub.schemas", name, "updated");
    Ok(gone)
}

pub async fn delete_schema(ctx: &Ctx, name: &str) -> Result<bool, ApiError> {
    let n = name.to_string();
    let gone = ctx.db.call(move |cn| cn.execute("DELETE FROM pubsub_schemas WHERE name=?1", [n]).map(|k| k > 0)).await?;
    if gone { ctx.live.send("pubsub.schemas", name, "deleted"); }
    Ok(gone)
}

// ---------------------------------------------------------------- BigQuery and Cloud Storage subscriptions

pub async fn add_bq_row(ctx: &Ctx, sub: &str, table: &str, row: &Value) -> Result<(), ApiError> {
    let (s, t, r) = (sub.to_string(), table.to_string(), row.to_string());
    ctx.db.call(move |cn| cn.execute("INSERT INTO pubsub_bq_rows(sub, tbl, row, at) VALUES(?1,?2,?3,?4)", params![s, t, r, util::now_ms()]).map(|_| ())).await?;
    ctx.live.send("pubsub.bigquery", table, "created");
    Ok(())
}

/// Rows written to a table (or by a subscription), newest first.
pub async fn bq_rows(ctx: &Ctx, table: Option<String>, sub: Option<String>, limit: i64) -> Result<Vec<Value>, ApiError> {
    ctx.db.call(move |cn| {
        let mut st = cn.prepare("SELECT sub, tbl, row, at FROM pubsub_bq_rows WHERE (?1 IS NULL OR tbl=?1) AND (?2 IS NULL OR sub=?2) ORDER BY id DESC LIMIT ?3")?;
        let v = st.query_map(params![table, sub, limit], |r| Ok(json!({"subscription": r.get::<_, String>(0)?, "table": r.get::<_, String>(1)?,
            "row": serde_json::from_str::<Value>(&r.get::<_, String>(2)?).unwrap_or(Value::Null), "writtenAt": r.get::<_, i64>(3)?})))?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

#[allow(clippy::too_many_arguments)]
pub async fn add_file(ctx: &Ctx, sub: &str, bucket: &str, object: &str, data: Vec<u8>, messages: i64, format: &str, stored: &str) -> Result<(), ApiError> {
    let (s, b, o, f, st) = (sub.to_string(), bucket.to_string(), object.to_string(), format.to_string(), stored.to_string());
    ctx.db.call(move |cn| cn.execute("INSERT INTO pubsub_gcs_files(sub, bucket, object, data, messages, format, stored, at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![s, b, o, data, messages, f, st, util::now_ms()]).map(|_| ())).await?;
    ctx.live.send("pubsub.storage", bucket, "created");
    Ok(())
}

/// Files a Cloud Storage subscription wrote (in a bucket, or by a subscription), newest first, with
/// their content (text as text, Avro as base64).
pub async fn files(ctx: &Ctx, bucket: Option<String>, sub: Option<String>, limit: i64) -> Result<Vec<Value>, ApiError> {
    use base64::Engine;
    ctx.db.call(move |cn| {
        let mut st = cn.prepare("SELECT sub, bucket, object, data, messages, format, stored, at FROM pubsub_gcs_files WHERE (?1 IS NULL OR bucket=?1) AND (?2 IS NULL OR sub=?2) ORDER BY id DESC LIMIT ?3")?;
        let v = st.query_map(params![bucket, sub, limit], |r| {
            let (data, format): (Vec<u8>, String) = (r.get(3)?, r.get(5)?);
            let content = if format == "text" { json!(String::from_utf8_lossy(&data)) } else { json!(base64::engine::general_purpose::STANDARD.encode(&data)) };
            Ok(json!({"subscription": r.get::<_, String>(0)?, "bucket": r.get::<_, String>(1)?, "object": r.get::<_, String>(2)?, "bytes": data.len(), "messages": r.get::<_, i64>(4)?,
                "format": format, "storedIn": Some(r.get::<_, String>(6)?).filter(|x| !x.is_empty()), "writtenAt": r.get::<_, i64>(7)?, "content": content}))
        })?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

/// (pending, outstanding, acked, dead) of a subscription; an expired outstanding one counts as pending.
pub async fn counts(ctx: &Ctx, sub: &str) -> Result<[i64; 4], ApiError> {
    let s = sub.to_string();
    ctx.db.call(move |cn| cn.query_row("SELECT COALESCE(SUM(state='pending' OR (state='outstanding' AND deadline<?2)),0), COALESCE(SUM(state='outstanding' AND deadline>=?2),0),
        COALESCE(SUM(state='acked'),0), COALESCE(SUM(state='dead'),0) FROM pubsub_deliveries WHERE sub=?1", params![s, util::now_ms()], |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?]))).await
}

pub async fn message_count(ctx: &Ctx, topic: &str) -> Result<i64, ApiError> {
    let t = topic.to_string();
    ctx.db.call(move |cn| cn.query_row("SELECT COUNT(*) FROM pubsub_messages WHERE topic=?1", [t], |r| r.get(0))).await
}

/// The newest messages of a topic (since a time), each with its state in every subscription.
pub async fn messages(ctx: &Ctx, topic: &str, since: i64, limit: i64) -> Result<Vec<Value>, ApiError> {
    let t = topic.to_string();
    let now = util::now_ms();
    ctx.db.call(move |cn| {
        let mut st = cn.prepare("SELECT id, data, attributes, ordering_key, published, publisher FROM pubsub_messages WHERE topic=?1 AND published>=?2 ORDER BY id DESC LIMIT ?3")?;
        let mut ds = cn.prepare("SELECT sub, state, attempts, deadline, acked FROM pubsub_deliveries WHERE msg=?1 ORDER BY sub")?;
        let rows: Vec<(i64, Vec<u8>, String, String, i64, String)> = st.query_map(params![t, since, limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.filter_map(|x| x.ok()).collect();
        let mut out = vec![];
        for (id, data, attrs, key, at, by) in rows {
            let subs: Vec<Value> = ds.query_map([id], |r| Ok(delivery(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, now)))?.filter_map(|x| x.ok()).collect();
            out.push(json!({"messageId": id.to_string(), "publishedAt": at, "publisher": by, "orderingKey": key, "attributes": serde_json::from_str::<Value>(&attrs).unwrap_or(json!({})),
                "data": show(&data), "subscriptions": subs}));
        }
        Ok(out)
    }).await
}

/// A subscription's deliveries, newest message first.
pub async fn deliveries(ctx: &Ctx, sub: &str, limit: i64) -> Result<Vec<Value>, ApiError> {
    let s = sub.to_string();
    let now = util::now_ms();
    ctx.db.call(move |cn| {
        let mut st = cn.prepare("SELECT d.msg, m.data, m.attributes, m.ordering_key, m.published, m.publisher, d.state, d.attempts, d.deadline, d.acked FROM pubsub_deliveries d
            JOIN pubsub_messages m ON m.id=d.msg WHERE d.sub=?1 ORDER BY d.msg DESC LIMIT ?2")?;
        let v = st.query_map(params![s, limit], |r| {
            let mut v = delivery(s.clone(), r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, now);
            let o = v.as_object_mut().unwrap();
            o.remove("subscription");
            o.insert("messageId".into(), json!(r.get::<_, i64>(0)?.to_string()));
            o.insert("data".into(), show(&r.get::<_, Vec<u8>>(1)?));
            o.insert("attributes".into(), serde_json::from_str::<Value>(&r.get::<_, String>(2)?).unwrap_or(json!({})));
            o.insert("orderingKey".into(), json!(r.get::<_, String>(3)?));
            o.insert("publishedAt".into(), json!(r.get::<_, i64>(4)?));
            o.insert("publisher".into(), json!(r.get::<_, String>(5)?));
            Ok(v)
        })?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

fn delivery(sub: String, state: String, attempts: i64, deadline: i64, acked: Option<i64>, now: i64) -> Value {
    // An outstanding message whose deadline passed is waiting to be delivered again.
    let st = if state == "outstanding" && deadline < now { "expired" } else { &state };
    json!({"subscription": sub, "state": st, "deliveryAttempts": attempts, "ackDeadline": (state == "outstanding").then_some(deadline), "ackedAt": acked})
}

/// Data as people read it: JSON, text, else base64.
pub fn show(b: &[u8]) -> Value {
    use base64::Engine;
    match serde_json::from_slice::<Value>(b) {
        Ok(v) if v.is_object() || v.is_array() => json!({"format": "json", "value": v}),
        _ => match std::str::from_utf8(b) {
            Ok(t) => json!({"format": "text", "value": t}),
            Err(_) => json!({"format": "base64", "value": base64::engine::general_purpose::STANDARD.encode(b)}),
        },
    }
}

// ---------------------------------------------------------------- Bigtable rows, BigQuery tables, activity, errors, ingestion

pub async fn add_bt_row(ctx: &Ctx, sub: &str, table: &str, key: &str, cells: &Value) -> Result<(), ApiError> {
    let (s, t, k, c) = (sub.to_string(), table.to_string(), key.to_string(), cells.to_string());
    ctx.db.call(move |cn| cn.execute("INSERT INTO pubsub_bt_rows(sub, tbl, row_key, cells, at) VALUES(?1,?2,?3,?4,?5)", params![s, t, k, c, util::now_ms()]).map(|_| ())).await?;
    ctx.live.send("pubsub.bigtable", table, "created");
    Ok(())
}

pub async fn bt_rows(ctx: &Ctx, table: Option<String>, sub: Option<String>, limit: i64) -> Result<Vec<Value>, ApiError> {
    ctx.db.call(move |cn| {
        let mut st = cn.prepare("SELECT sub, tbl, row_key, cells, at FROM pubsub_bt_rows WHERE (?1 IS NULL OR tbl=?1) AND (?2 IS NULL OR sub=?2) ORDER BY id DESC LIMIT ?3")?;
        let v = st.query_map(params![table, sub, limit], |r| Ok(json!({"subscription": r.get::<_, String>(0)?, "table": r.get::<_, String>(1)?, "rowKey": r.get::<_, String>(2)?,
            "cells": serde_json::from_str::<Value>(&r.get::<_, String>(3)?).unwrap_or(Value::Null), "writtenAt": r.get::<_, i64>(4)?})))?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

/// A BigQuery table's schema as set_pubsub_bigquery_table gave it: [{name, type, mode}].
pub async fn set_bq_table(ctx: &Ctx, table: &str, schema: &Value) -> Result<(), ApiError> {
    let (t, s) = (table.to_string(), schema.to_string());
    ctx.db.call(move |cn| cn.execute("INSERT OR REPLACE INTO pubsub_bq_tables(tbl, schema) VALUES(?1,?2)", params![t, s]).map(|_| ())).await
}

pub async fn bq_table(ctx: &Ctx, table: &str) -> Result<Option<Value>, ApiError> {
    let t = table.to_string();
    Ok(ctx.db.call(move |cn| cn.query_row("SELECT schema FROM pubsub_bq_tables WHERE tbl=?1", [t], |r| r.get::<_, String>(0)).optional()).await?.and_then(|s| serde_json::from_str(&s).ok()))
}

pub async fn bq_tables(ctx: &Ctx) -> Result<Vec<(String, Value)>, ApiError> {
    ctx.db.call(|cn| {
        let mut st = cn.prepare("SELECT tbl, schema FROM pubsub_bq_tables ORDER BY tbl")?;
        let v = st.query_map([], |r| Ok((r.get::<_, String>(0)?, serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or(Value::Null))))?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

/// A subscriber was active (environment clock): what the expiration policy measures from.
pub async fn seen(ctx: &Ctx, sub: &str) -> Result<(), ApiError> {
    let (s, at) = (sub.to_string(), stuntman_core::clock::now(ctx));
    ctx.db.call(move |cn| cn.execute("INSERT OR REPLACE INTO pubsub_sub_seen(name, at) VALUES(?1,?2)", params![s, at]).map(|_| ())).await
}

/// When a subscription was last active (environment clock); a subscription never seen counts from now.
pub async fn last_seen(ctx: &Ctx, sub: &str) -> Result<i64, ApiError> {
    let s = sub.to_string();
    match ctx.db.call(move |cn| cn.query_row("SELECT at FROM pubsub_sub_seen WHERE name=?1", [s], |r| r.get::<_, i64>(0)).optional()).await? {
        Some(t) => Ok(t),
        None => { seen(ctx, sub).await?; Ok(stuntman_core::clock::now(ctx)) }
    }
}

/// Why the last delivery of a push, BigQuery, Cloud Storage or Bigtable subscription failed; "" clears it.
pub async fn set_error(ctx: &Ctx, sub: &str, e: &str) -> Result<(), ApiError> {
    let (s, e2) = (sub.to_string(), e.to_string());
    ctx.db.call(move |cn| if e2.is_empty() { cn.execute("DELETE FROM pubsub_sub_errors WHERE name=?1", [s]).map(|_| ()) }
        else { cn.execute("INSERT OR REPLACE INTO pubsub_sub_errors(name, error, at) VALUES(?1,?2,?3)", params![s, e2, util::now_ms()]).map(|_| ()) }).await
}

pub async fn error_of(ctx: &Ctx, sub: &str) -> Result<Option<(String, i64)>, ApiError> {
    let s = sub.to_string();
    ctx.db.call(move |cn| cn.query_row("SELECT error, at FROM pubsub_sub_errors WHERE name=?1", [s], |r| Ok((r.get(0)?, r.get(1)?))).optional()).await
}

/// Ingestion progress of a topic: a Kafka partition's next offset, an object already read.
pub async fn ingest_mark(ctx: &Ctx, topic: &str, k: &str) -> Result<Option<i64>, ApiError> {
    let (t, k) = (topic.to_string(), k.to_string());
    ctx.db.call(move |cn| cn.query_row("SELECT v FROM pubsub_ingest WHERE topic=?1 AND k=?2", params![t, k], |r| r.get(0)).optional()).await
}

pub async fn set_ingest_mark(ctx: &Ctx, topic: &str, k: &str, v: i64) -> Result<(), ApiError> {
    let (t, k) = (topic.to_string(), k.to_string());
    ctx.db.call(move |cn| cn.execute("INSERT OR REPLACE INTO pubsub_ingest(topic, k, v) VALUES(?1,?2,?3)", params![t, k, v]).map(|_| ())).await
}

/// Ids for messages a topic transform filtered out: Pub/Sub answers the publisher with an id but
/// keeps nothing (the ids are taken from the same sequence, never reused).
pub async fn reserve_ids(ctx: &Ctx, topic: &str, n: usize) -> Result<Vec<i64>, ApiError> {
    if n == 0 { return Ok(vec![]) }
    let t = topic.to_string();
    ctx.db.call(move |cn| {
        let tx = cn.transaction()?;
        let mut ids = vec![];
        for _ in 0..n {
            tx.execute("INSERT INTO pubsub_messages(topic, data, published) VALUES(?1, x'', 0)", [&t])?;
            ids.push(tx.last_insert_rowid());
        }
        tx.execute("DELETE FROM pubsub_messages WHERE topic=?1 AND published=0", [&t])?;
        tx.commit()?;
        Ok(ids)
    }).await
}

/// Bytes of data out now in a subscription (StreamingPull's max_outstanding_bytes).
pub async fn outstanding_bytes(ctx: &Ctx, sub: &str) -> Result<i64, ApiError> {
    let s = sub.to_string();
    ctx.db.call(move |cn| cn.query_row("SELECT COALESCE(SUM(LENGTH(m.data)),0) FROM pubsub_deliveries d JOIN pubsub_messages m ON m.id=d.msg WHERE d.sub=?1 AND d.state='outstanding' AND d.deadline>=?2 AND d.ack_id!=''",
        params![s, util::now_ms()], |r| r.get(0))).await
}
