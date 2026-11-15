//! Topics, subscriptions, messages and each subscription's deliveries (pending, outstanding with an
//! ack deadline, acked, dead-lettered). SQLite of the environment. Names are the full resource
//! names: projects/<p>/topics/<t>, projects/<p>/subscriptions/<s>.

use std::collections::{BTreeMap, HashSet};

use rand::Rng;
use rusqlite::{params, OptionalExtension, Transaction};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx};

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
";

/// What a subscription's topic becomes when the topic is deleted (as Pub/Sub does).
pub const DELETED_TOPIC: &str = "_deleted-topic_";

pub type Labels = BTreeMap<String, String>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sub {
    pub name: String,
    pub topic: String,
    pub ack_deadline: i32,
    pub ordering: bool,
    pub dead_letter: String,
    pub max_attempts: i32,
    pub labels: Labels,
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
    Ok(Sub { name: r.get(0)?, topic: r.get(1)?, ack_deadline: r.get(2)?, ordering: r.get::<_, i64>(3)? != 0, dead_letter: r.get(4)?, max_attempts: r.get(5)?, labels: labels(r.get(6)?) })
}

const SUB_COLS: &str = "name, topic, ack_deadline, ordering, dead_letter, max_attempts, labels";

pub async fn create_topic(ctx: &Ctx, name: &str, l: &Labels) -> Result<bool, ApiError> {
    let (n, l) = (name.to_string(), json!(l).to_string());
    let made = ctx.db.call(move |cn| cn.execute("INSERT OR IGNORE INTO pubsub_topics(name, labels, created) VALUES(?1,?2,?3)", params![n, l, util::now_ms()]).map(|x| x > 0)).await?;
    if made { ctx.live.send("pubsub.topics", name, "created"); }
    Ok(made)
}

pub async fn topic(ctx: &Ctx, name: &str) -> Result<Option<Labels>, ApiError> {
    let n = name.to_string();
    Ok(ctx.db.call(move |cn| cn.query_row("SELECT labels FROM pubsub_topics WHERE name=?1", [n], |r| r.get::<_, String>(0)).optional()).await?.map(labels))
}

/// Topics whose name starts with `prefix` ("projects/acme-stg/topics/", or "" for all).
pub async fn topics(ctx: &Ctx, prefix: &str) -> Result<Vec<(String, Labels)>, ApiError> {
    let p = format!("{prefix}%");
    ctx.db.call(move |cn| {
        let mut st = cn.prepare("SELECT name, labels FROM pubsub_topics WHERE name LIKE ?1 ORDER BY name")?;
        let v = st.query_map([p], |r| Ok((r.get(0)?, labels(r.get(1)?))))?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

pub async fn set_topic_labels(ctx: &Ctx, name: &str, l: &Labels) -> Result<(), ApiError> {
    let (n, l) = (name.to_string(), json!(l).to_string());
    ctx.db.call(move |cn| cn.execute("UPDATE pubsub_topics SET labels=?2 WHERE name=?1", params![n, l]).map(|_| ())).await
}

/// Deletes a topic; its subscriptions stay, detached (their topic reads _deleted-topic_).
pub async fn delete_topic(ctx: &Ctx, name: &str) -> Result<bool, ApiError> {
    let n = name.to_string();
    let gone = ctx.db.call(move |cn| {
        let k = cn.execute("DELETE FROM pubsub_topics WHERE name=?1", [&n])?;
        cn.execute("UPDATE pubsub_subscriptions SET topic=?2 WHERE topic=?1", params![n, DELETED_TOPIC])?;
        Ok(k > 0)
    }).await?;
    if gone { ctx.live.send("pubsub.topics", name, "deleted"); }
    Ok(gone)
}

pub async fn create_sub(ctx: &Ctx, s: &Sub) -> Result<bool, ApiError> {
    let s2 = s.clone();
    let made = ctx.db.call(move |cn| cn.execute("INSERT OR IGNORE INTO pubsub_subscriptions(name, topic, ack_deadline, ordering, dead_letter, max_attempts, labels, created) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![s2.name, s2.topic, s2.ack_deadline, s2.ordering as i64, s2.dead_letter, s2.max_attempts, json!(s2.labels).to_string(), util::now_ms()]).map(|x| x > 0)).await?;
    if made { ctx.live.send("pubsub.subscriptions", &s.name, "created"); }
    Ok(made)
}

pub async fn sub(ctx: &Ctx, name: &str) -> Result<Option<Sub>, ApiError> {
    let n = name.to_string();
    ctx.db.call(move |cn| cn.query_row(&format!("SELECT {SUB_COLS} FROM pubsub_subscriptions WHERE name=?1"), [n], sub_row).optional()).await
}

/// Subscriptions by name prefix, or of one topic.
pub async fn subs(ctx: &Ctx, prefix: &str, topic: Option<&str>) -> Result<Vec<Sub>, ApiError> {
    let (p, t) = (format!("{prefix}%"), topic.map(String::from));
    ctx.db.call(move |cn| {
        let mut st = cn.prepare(&format!("SELECT {SUB_COLS} FROM pubsub_subscriptions WHERE name LIKE ?1 AND (?2 IS NULL OR topic=?2) ORDER BY name"))?;
        let v = st.query_map(params![p, t], sub_row)?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

pub async fn delete_sub(ctx: &Ctx, name: &str) -> Result<bool, ApiError> {
    let n = name.to_string();
    let gone = ctx.db.call(move |cn| {
        let k = cn.execute("DELETE FROM pubsub_subscriptions WHERE name=?1", [&n])?;
        cn.execute("DELETE FROM pubsub_deliveries WHERE sub=?1", [&n])?;
        Ok(k > 0)
    }).await?;
    if gone { ctx.live.send("pubsub.subscriptions", name, "deleted"); }
    Ok(gone)
}

/// One message onto a topic and a pending delivery for each of its subscriptions (only those that
/// exist now: Pub/Sub does not give a new subscription older messages).
fn insert(tx: &Transaction, topic: &str, data: &[u8], attrs: &Labels, key: &str, publisher: &str) -> rusqlite::Result<i64> {
    tx.execute("INSERT INTO pubsub_messages(topic, data, attributes, ordering_key, published, publisher) VALUES(?1,?2,?3,?4,?5,?6)",
        params![topic, data, json!(attrs).to_string(), key, util::now_ms(), publisher])?;
    let id = tx.last_insert_rowid();
    tx.execute("INSERT INTO pubsub_deliveries(sub, msg) SELECT name, ?2 FROM pubsub_subscriptions WHERE topic=?1", params![topic, id])?;
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

/// Hands out up to `max` messages: pending ones and outstanding ones whose deadline passed. With
/// message ordering, a key with a message still out waits. A message delivered max_attempts times
/// already goes to the dead-letter topic instead.
pub async fn pull(ctx: &Ctx, sub: &Sub, max: usize, secs: i32) -> Result<Vec<Got>, ApiError> {
    let s = sub.clone();
    let now = util::now_ms();
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
            let mut st = tx.prepare("SELECT d.msg, d.attempts, m.data, m.attributes, m.ordering_key, m.published FROM pubsub_deliveries d JOIN pubsub_messages m ON m.id=d.msg
                WHERE d.sub=?1 AND (d.state='pending' OR (d.state='outstanding' AND d.deadline<?2)) ORDER BY d.msg LIMIT 1000")?;
            let rows = st.query_map(params![s.name, now], |r| Ok((Msg { id: r.get(0)?, data: r.get(2)?, attributes: labels(r.get(3)?), ordering_key: r.get(4)?, published: r.get(5)? }, r.get::<_, i32>(1)?)))?;
            for r in rows {
                let (m, attempts) = r?;
                if got.len() >= max { break }
                if s.ordering && blocked.contains(&m.ordering_key) { continue }
                if s.max_attempts > 0 && attempts >= s.max_attempts { dead.push(m); continue }
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

/// Acks; with `duplicate`, a first delivery's ack does not stick and the message comes again.
pub async fn ack(ctx: &Ctx, sub: &str, ids: Vec<String>, duplicate: bool) -> Result<usize, ApiError> {
    if ids.is_empty() { return Ok(0) }
    let s = sub.to_string();
    let n = ctx.db.call(move |cn| {
        let tx = cn.transaction()?;
        let mut n = 0;
        for id in &ids {
            n += if duplicate {
                tx.execute("UPDATE pubsub_deliveries SET state=CASE WHEN attempts<=1 THEN 'pending' ELSE 'acked' END, acked=CASE WHEN attempts<=1 THEN NULL ELSE ?3 END, ack_id='' WHERE sub=?1 AND ack_id=?2 AND state='outstanding'", params![s, id, util::now_ms()])?
            } else {
                tx.execute("UPDATE pubsub_deliveries SET state='acked', acked=?3, ack_id='' WHERE sub=?1 AND ack_id=?2 AND state='outstanding'", params![s, id, util::now_ms()])?
            };
        }
        tx.commit()?;
        Ok(n)
    }).await?;
    if n > 0 { ctx.live.send("pubsub.deliveries", sub, "updated"); }
    if duplicate { crate::ring(); }
    Ok(n)
}

/// Moves the ack deadline of outstanding messages; 0 hands them back now (a nack).
pub async fn modack(ctx: &Ctx, sub: &str, ids: Vec<(String, i32)>) -> Result<(), ApiError> {
    if ids.is_empty() { return Ok(()) }
    let s = sub.to_string();
    let nacked = ctx.db.call(move |cn| {
        let tx = cn.transaction()?;
        let now = util::now_ms();
        let mut nacked = false;
        for (id, secs) in &ids {
            if *secs <= 0 {
                nacked |= tx.execute("UPDATE pubsub_deliveries SET state='pending', ack_id='' WHERE sub=?1 AND ack_id=?2 AND state='outstanding'", params![s, id])? > 0;
            } else {
                tx.execute("UPDATE pubsub_deliveries SET deadline=?3 WHERE sub=?1 AND ack_id=?2 AND state='outstanding'", params![s, id, now + (*secs).min(600) as i64 * 1000])?;
            }
        }
        tx.commit()?;
        Ok(nacked)
    }).await?;
    if nacked { ctx.live.send("pubsub.deliveries", sub, "updated"); crate::ring(); }
    Ok(())
}

/// Seek to a time: messages published from then on are unacked again, older ones acked.
pub async fn seek(ctx: &Ctx, sub: &str, ms: i64) -> Result<(), ApiError> {
    let s = sub.to_string();
    ctx.db.call(move |cn| cn.execute("UPDATE pubsub_deliveries SET ack_id='', state=CASE WHEN (SELECT published FROM pubsub_messages WHERE id=msg)>=?2 THEN 'pending' ELSE 'acked' END WHERE sub=?1 AND state!='dead'", params![s, ms]).map(|_| ())).await?;
    ctx.live.send("pubsub.deliveries", sub, "updated");
    crate::ring();
    Ok(())
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
