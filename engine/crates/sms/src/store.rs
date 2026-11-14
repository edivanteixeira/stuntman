//! SMS messages both ways, the opt-out list and the callbacks sent to the app. SQLite of the environment.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS sms_messages(
  id TEXT PRIMARY KEY, at INTEGER NOT NULL, dir TEXT NOT NULL, number TEXT NOT NULL, sender TEXT NOT NULL DEFAULT '',
  text TEXT NOT NULL DEFAULT '', status TEXT NOT NULL, reason TEXT NOT NULL DEFAULT '', history TEXT NOT NULL DEFAULT '[]',
  body TEXT NOT NULL DEFAULT '{}', trace_id TEXT, caller TEXT NOT NULL DEFAULT '', encoding TEXT NOT NULL DEFAULT '',
  segments INTEGER NOT NULL DEFAULT 1, flavour TEXT NOT NULL DEFAULT '', external_id TEXT NOT NULL DEFAULT '', by_who TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS sms_messages_number ON sms_messages(number, at);
CREATE TABLE IF NOT EXISTS sms_optouts(number TEXT PRIMARY KEY, at INTEGER NOT NULL, by_who TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS sms_hooks(
  id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, kind TEXT NOT NULL, message_id TEXT NOT NULL DEFAULT '',
  url TEXT NOT NULL, status INTEGER, ms INTEGER, error TEXT NOT NULL DEFAULT '', attempt INTEGER NOT NULL DEFAULT 1, body TEXT NOT NULL DEFAULT '');
";

pub struct Msg {
    pub id: String,
    pub dir: &'static str,
    pub number: String,
    pub sender: String,
    pub text: String,
    pub status: String,
    pub reason: String,
    pub body: Value,
    pub trace_id: Option<String>,
    pub caller: String,
    pub flavour: String,
    pub external_id: String,
    pub by: String,
}

const COLS: &str = "id, at, dir, number, sender, text, status, reason, history, body, trace_id, caller, encoding, segments, flavour, external_id, by_who";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let p = |i: usize| -> rusqlite::Result<Value> { Ok(serde_json::from_str(&r.get::<_, String>(i)?).unwrap_or(Value::Null)) };
    let text: String = r.get(5)?;
    let (_, forced, units, _) = crate::config::encoding(&text);
    Ok(json!({"id": r.get::<_, String>(0)?, "at": r.get::<_, i64>(1)?, "dir": r.get::<_, String>(2)?, "number": r.get::<_, String>(3)?, "sender": r.get::<_, String>(4)?,
        "text": text, "status": r.get::<_, String>(6)?, "reason": r.get::<_, String>(7)?, "history": p(8)?, "body": p(9)?, "traceId": r.get::<_, Option<String>>(10)?,
        "caller": r.get::<_, String>(11)?, "encoding": r.get::<_, String>(12)?, "segments": r.get::<_, i64>(13)?, "flavour": r.get::<_, String>(14)?,
        "externalId": r.get::<_, String>(15)?, "by": r.get::<_, String>(16)?, "units": units, "forcedBy": forced.map(|c| c.to_string())}))
}

pub async fn insert(ctx: &Ctx, m: Msg) -> Result<(), ApiError> {
    let (enc, _, _, seg) = crate::config::encoding(&m.text);
    let hist = json!([{"status": m.status, "at": util::now_ms(), "by": m.by}]).to_string();
    ctx.db.call(move |c| c.execute(&format!("INSERT INTO sms_messages({COLS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)"),
        params![m.id, util::now_ms(), m.dir, m.number, m.sender, m.text, m.status, m.reason, hist, m.body.to_string(), m.trace_id, m.caller, enc, seg as i64, m.flavour, m.external_id, m.by]).map(|_| ())).await?;
    ctx.live.send("sms.messages", "", "new");
    Ok(())
}

pub async fn get(ctx: &Ctx, id: &str) -> Result<Value, ApiError> {
    let i = id.to_string();
    ctx.db.call(move |c| c.query_row(&format!("SELECT {COLS} FROM sms_messages WHERE id=?1"), [i], row).optional()).await?
        .ok_or_else(|| ApiError::not_found(format!("No SMS {id}")))
}

#[derive(Default, Clone)]
pub struct Filter {
    pub number: Option<String>,
    pub status: Option<String>,
    pub dir: Option<String>,
    pub text: Option<String>,
    pub since_ms: Option<i64>,
    pub q: Option<String>,
    pub limit: u32,
}

pub async fn list(ctx: &Ctx, f: Filter) -> Result<Vec<Value>, ApiError> {
    ctx.db.call(move |c| {
        let mut w = vec!["1=1".to_string()];
        let mut v: Vec<rusqlite::types::Value> = vec![];
        if let Some(n) = &f.number { w.push(format!("number=?{}", v.len() + 1)); v.push(crate::config::digits(n).into()); }
        if let Some(s) = &f.status { w.push(format!("status=?{}", v.len() + 1)); v.push(s.clone().into()); }
        if let Some(d) = &f.dir { w.push(format!("dir=?{}", v.len() + 1)); v.push(d.clone().into()); }
        if let Some(t) = &f.text { w.push(format!("text LIKE ?{}", v.len() + 1)); v.push(format!("%{t}%").into()); }
        if let Some(s) = f.since_ms { w.push(format!("at>=?{}", v.len() + 1)); v.push(s.into()); }
        if let Some(q) = &f.q { let k = v.len() + 1; w.push(format!("(number LIKE ?{k} OR text LIKE ?{k} OR id LIKE ?{k})")); v.push(format!("%{}%", q.trim_start_matches('+').replace(' ', "")).into()); }
        let sql = format!("SELECT {COLS} FROM sms_messages WHERE {} ORDER BY at DESC, rowid DESC LIMIT {}", w.join(" AND "), if f.limit == 0 { 200 } else { f.limit });
        let mut st = c.prepare(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(v), row)?.collect::<Result<Vec<_>, _>>();
        rows
    }).await
}

pub async fn set_status(ctx: &Ctx, id: &str, status: &str, reason: &str, by: &str) -> Result<Value, ApiError> {
    let (i, s, r, b) = (id.to_string(), status.to_string(), reason.to_string(), by.to_string());
    ctx.db.call(move |c| {
        let h: Option<String> = c.query_row("SELECT history FROM sms_messages WHERE id=?1", [&i], |r| r.get(0)).optional()?;
        let mut h: Vec<Value> = h.and_then(|h| serde_json::from_str(&h).ok()).unwrap_or_default();
        h.push(json!({"status": s, "at": util::now_ms(), "reason": r, "by": b}));
        c.execute("UPDATE sms_messages SET status=?2, reason=CASE WHEN ?3='' THEN reason ELSE ?3 END, history=?4 WHERE id=?1", params![i, s, r, json!(h).to_string()])
    }).await?;
    ctx.live.send("sms.messages", id, status);
    get(ctx, id).await
}

/// Notes the app's answer to a callback on the last history entry of that status.
pub async fn note_hook(ctx: &Ctx, id: &str, status: &str, answer: Value) -> Result<(), ApiError> {
    let (i, s) = (id.to_string(), status.to_string());
    ctx.db.call(move |c| {
        let h: Option<String> = c.query_row("SELECT history FROM sms_messages WHERE id=?1", [&i], |r| r.get(0)).optional()?;
        let mut h: Vec<Value> = h.and_then(|h| serde_json::from_str(&h).ok()).unwrap_or_default();
        if let Some(e) = h.iter_mut().rev().find(|e| e["status"] == s.as_str()) { e["webhook"] = answer; }
        c.execute("UPDATE sms_messages SET history=?2 WHERE id=?1", params![i, json!(h).to_string()])
    }).await?;
    ctx.live.send("sms.messages", id, "webhook");
    Ok(())
}

pub async fn sent_last_second(ctx: &Ctx) -> Result<i64, ApiError> {
    let since = util::now_ms() - 1000;
    ctx.db.call(move |c| c.query_row("SELECT COUNT(*) FROM sms_messages WHERE dir='out' AND at>=?1", [since], |r| r.get(0))).await
}

pub async fn count_today(ctx: &Ctx, number: &str) -> Result<i64, ApiError> {
    let (n, since) = (number.to_string(), util::now_ms() - 86_400_000);
    ctx.db.call(move |c| c.query_row("SELECT COUNT(*) FROM sms_messages WHERE dir='out' AND number=?1 AND at>=?2", params![n, since], |r| r.get(0))).await
}

pub async fn opted_out(ctx: &Ctx, number: &str) -> Result<Option<i64>, ApiError> {
    let n = number.to_string();
    ctx.db.call(move |c| c.query_row("SELECT at FROM sms_optouts WHERE number=?1", [n], |r| r.get(0)).optional()).await
}
pub async fn opt_out(ctx: &Ctx, number: &str, by: &str) -> Result<(), ApiError> {
    let (n, b) = (number.to_string(), by.to_string());
    ctx.db.call(move |c| c.execute("INSERT OR REPLACE INTO sms_optouts(number, at, by_who) VALUES(?1,?2,?3)", params![n, util::now_ms(), b]).map(|_| ())).await?;
    ctx.live.send("sms.optouts", number, "added");
    Ok(())
}
pub async fn opt_in(ctx: &Ctx, number: &str) -> Result<bool, ApiError> {
    let n = number.to_string();
    let x = ctx.db.call(move |c| c.execute("DELETE FROM sms_optouts WHERE number=?1", [n])).await?;
    ctx.live.send("sms.optouts", number, "removed");
    Ok(x > 0)
}
pub async fn optouts(ctx: &Ctx) -> Result<Vec<Value>, ApiError> {
    ctx.db.call(|c| {
        let mut s = c.prepare("SELECT number, at, by_who FROM sms_optouts ORDER BY at DESC")?;
        let r = s.query_map([], |r| Ok(json!({"number": r.get::<_, String>(0)?, "at": r.get::<_, i64>(1)?, "by": r.get::<_, String>(2)?})))?.collect::<Result<Vec<_>, _>>();
        r
    }).await
}

#[allow(clippy::too_many_arguments)]
pub async fn hook(ctx: &Ctx, kind: &str, message_id: &str, url: &str, status: Option<u16>, ms: i64, error: &str, attempt: u32, body: &str) -> Result<(), ApiError> {
    let (k, m, u, e, b) = (kind.to_string(), message_id.to_string(), url.to_string(), error.to_string(), body.to_string());
    ctx.db.call(move |c| c.execute("INSERT INTO sms_hooks(at, kind, message_id, url, status, ms, error, attempt, body) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![util::now_ms(), k, m, u, status, ms, e, attempt, b]).map(|_| ())).await?;
    ctx.live.send("sms.hooks", message_id, "sent");
    Ok(())
}
pub async fn hooks(ctx: &Ctx, limit: u32) -> Result<Vec<Value>, ApiError> {
    ctx.db.call(move |c| {
        let mut s = c.prepare("SELECT id, at, kind, message_id, url, status, ms, error, attempt, body FROM sms_hooks ORDER BY id DESC LIMIT ?1")?;
        let r = s.query_map([limit], |r| Ok(json!({"id": r.get::<_, i64>(0)?, "at": r.get::<_, i64>(1)?, "kind": r.get::<_, String>(2)?, "messageId": r.get::<_, String>(3)?,
            "url": r.get::<_, String>(4)?, "status": r.get::<_, Option<u16>>(5)?, "ms": r.get::<_, Option<i64>>(6)?, "error": r.get::<_, String>(7)?, "attempt": r.get::<_, u32>(8)?, "body": r.get::<_, String>(9)?})))?
            .collect::<Result<Vec<_>, _>>();
        r
    }).await
}
