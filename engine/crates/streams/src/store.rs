//! Topics, their records, consumer-group offsets and the Schema Registry's schemas. SQLite of the
//! environment.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx};

use crate::wire::Rec;

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS kafka_topics(name TEXT PRIMARY KEY, partitions INTEGER NOT NULL, created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS kafka_records(topic TEXT NOT NULL, part INTEGER NOT NULL, off INTEGER NOT NULL, ts INTEGER NOT NULL, key BLOB, value BLOB,
  headers TEXT NOT NULL DEFAULT '[]', producer TEXT NOT NULL DEFAULT '', PRIMARY KEY(topic, part, off));
CREATE TABLE IF NOT EXISTS kafka_offsets(grp TEXT NOT NULL, topic TEXT NOT NULL, part INTEGER NOT NULL, off INTEGER NOT NULL, meta TEXT NOT NULL DEFAULT '', at INTEGER NOT NULL,
  PRIMARY KEY(grp, topic, part));
CREATE TABLE IF NOT EXISTS kafka_schemas(id INTEGER PRIMARY KEY AUTOINCREMENT, schema TEXT NOT NULL, type TEXT NOT NULL DEFAULT 'AVRO', refs TEXT NOT NULL DEFAULT '[]');
CREATE TABLE IF NOT EXISTS kafka_subjects(subject TEXT NOT NULL, version INTEGER NOT NULL, schema_id INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0, at INTEGER NOT NULL,
  PRIMARY KEY(subject, version));
CREATE TABLE IF NOT EXISTS kafka_config(subject TEXT PRIMARY KEY, compatibility TEXT NOT NULL);
";

pub async fn topics(ctx: &Ctx) -> Result<Vec<(String, i32)>, ApiError> {
    ctx.db.call(|cn| {
        let mut st = cn.prepare("SELECT name, partitions FROM kafka_topics ORDER BY name")?;
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

pub async fn topic(ctx: &Ctx, name: &str) -> Result<Option<i32>, ApiError> {
    let n = name.to_string();
    ctx.db.call(move |cn| cn.query_row("SELECT partitions FROM kafka_topics WHERE name=?1", [n], |r| r.get(0)).optional()).await
}

pub async fn create_topic(ctx: &Ctx, name: &str, partitions: i32) -> Result<bool, ApiError> {
    let n = name.to_string();
    let made = ctx.db.call(move |cn| cn.execute("INSERT OR IGNORE INTO kafka_topics(name, partitions, created) VALUES(?1,?2,?3)", params![n, partitions.max(1), util::now_ms()]).map(|x| x > 0)).await?;
    if made { ctx.live.send("streams.topics", name, "created"); }
    Ok(made)
}

pub async fn delete_topic(ctx: &Ctx, name: &str) -> Result<(), ApiError> {
    let n = name.to_string();
    ctx.db.call(move |cn| { cn.execute("DELETE FROM kafka_topics WHERE name=?1", [&n])?; cn.execute("DELETE FROM kafka_records WHERE topic=?1", [&n]).map(|_| ()) }).await
}

/// Appends records; returns the base offset.
pub async fn append(ctx: &Ctx, topic: &str, part: i32, recs: Vec<Rec>, producer: &str) -> Result<i64, ApiError> {
    let (t, p) = (topic.to_string(), producer.to_string());
    let base = ctx.db.call(move |cn| {
        let tx = cn.transaction()?;
        let next: i64 = tx.query_row("SELECT COALESCE(MAX(off)+1, 0) FROM kafka_records WHERE topic=?1 AND part=?2", params![t, part], |r| r.get(0))?;
        {
            let mut st = tx.prepare("INSERT INTO kafka_records(topic, part, off, ts, key, value, headers, producer) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)")?;
            for (i, r) in recs.iter().enumerate() {
                let h: Vec<Value> = r.headers.iter().map(|(k, v)| json!([k, v.as_ref().map(|x| hex::encode(x))])).collect();
                st.execute(params![t, part, next + i as i64, if r.ts > 0 { r.ts } else { util::now_ms() }, r.key, r.value, json!(h).to_string(), p])?;
            }
        }
        tx.commit()?;
        Ok(next)
    }).await?;
    ctx.live.send("streams.messages", topic, "created");
    Ok(base)
}

pub async fn read(ctx: &Ctx, topic: &str, part: i32, from: i64, max_bytes: usize) -> Result<Vec<Rec>, ApiError> {
    let t = topic.to_string();
    ctx.db.call(move |cn| {
        let mut st = cn.prepare("SELECT off, ts, key, value, headers FROM kafka_records WHERE topic=?1 AND part=?2 AND off>=?3 ORDER BY off LIMIT 500")?;
        let mut out = vec![];
        let mut size = 0usize;
        for r in st.query_map(params![t, part, from], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<Vec<u8>>>(2)?, r.get::<_, Option<Vec<u8>>>(3)?, r.get::<_, String>(4)?)))? {
            let (off, ts, key, value, h) = r?;
            size += key.as_ref().map(|x| x.len()).unwrap_or(0) + value.as_ref().map(|x| x.len()).unwrap_or(0) + 32;
            let headers = serde_json::from_str::<Vec<(String, Option<String>)>>(&h).unwrap_or_default().into_iter().map(|(k, v)| (k, v.and_then(|x| hex::decode(x).ok()))).collect();
            out.push(Rec { offset: off, ts, key, value, headers });
            if size >= max_bytes && !out.is_empty() { break }
        }
        Ok(out)
    }).await
}

/// (log start, high watermark) of a partition.
pub async fn marks(ctx: &Ctx, topic: &str, part: i32) -> Result<(i64, i64), ApiError> {
    let t = topic.to_string();
    ctx.db.call(move |cn| cn.query_row("SELECT COALESCE(MIN(off), 0), COALESCE(MAX(off)+1, 0) FROM kafka_records WHERE topic=?1 AND part=?2", params![t, part], |r| Ok((r.get(0)?, r.get(1)?)))).await
}

pub async fn offset_for_time(ctx: &Ctx, topic: &str, part: i32, ts: i64) -> Result<Option<(i64, i64)>, ApiError> {
    let t = topic.to_string();
    ctx.db.call(move |cn| cn.query_row("SELECT off, ts FROM kafka_records WHERE topic=?1 AND part=?2 AND ts>=?3 ORDER BY off LIMIT 1", params![t, part, ts], |r| Ok((r.get(0)?, r.get(1)?))).optional()).await
}

pub async fn commit(ctx: &Ctx, grp: &str, topic: &str, part: i32, off: i64, meta: &str) -> Result<(), ApiError> {
    let (g, t, m) = (grp.to_string(), topic.to_string(), meta.to_string());
    ctx.db.call(move |cn| cn.execute("INSERT OR REPLACE INTO kafka_offsets VALUES(?1,?2,?3,?4,?5,?6)", params![g, t, part, off, m, util::now_ms()]).map(|_| ())).await?;
    ctx.live.send("streams.groups", grp, "updated");
    Ok(())
}

pub async fn committed(ctx: &Ctx, grp: &str) -> Result<Vec<(String, i32, i64, String)>, ApiError> {
    let g = grp.to_string();
    ctx.db.call(move |cn| {
        let mut st = cn.prepare("SELECT topic, part, off, meta FROM kafka_offsets WHERE grp=?1 ORDER BY topic, part")?;
        let v = st.query_map([g], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

pub async fn groups(ctx: &Ctx) -> Result<Vec<String>, ApiError> {
    ctx.db.call(|cn| {
        let mut st = cn.prepare("SELECT DISTINCT grp FROM kafka_offsets ORDER BY grp")?;
        let v = st.query_map([], |r| r.get(0))?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}

pub async fn messages(ctx: &Ctx, topic: &str, part: Option<i32>, limit: i64) -> Result<Vec<Value>, ApiError> {
    let t = topic.to_string();
    ctx.db.call(move |cn| {
        let mut st = cn.prepare("SELECT part, off, ts, key, value, headers, producer FROM kafka_records WHERE topic=?1 AND (?2 IS NULL OR part=?2) ORDER BY ts DESC, off DESC LIMIT ?3")?;
        let v = st.query_map(params![t, part, limit], |r| {
            let key: Option<Vec<u8>> = r.get(3)?;
            let value: Option<Vec<u8>> = r.get(4)?;
            Ok(json!({"partition": r.get::<_, i32>(0)?, "offset": r.get::<_, i64>(1)?, "at": r.get::<_, i64>(2)?, "key": key.as_ref().map(|k| String::from_utf8_lossy(k).to_string()),
                "valueBytes": value.as_ref().map(|v| v.len()), "_value": value.map(hex::encode), "headers": serde_json::from_str::<Value>(&r.get::<_, String>(5)?).unwrap_or(json!([])), "producer": r.get::<_, String>(6)?}))
        })?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await
}
