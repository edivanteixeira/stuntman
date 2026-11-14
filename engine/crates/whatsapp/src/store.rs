//! Messages both ways (the app's sends and the person's replies), each with its status history,
//! templates the app asked for that are not declared, and every webhook Stuntman sent to the app.

use stuntman_core::{util, ApiError, Ctx};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS wa_messages(
  id TEXT PRIMARY KEY, at INTEGER NOT NULL, dir TEXT NOT NULL, number TEXT NOT NULL, kind TEXT NOT NULL,
  template TEXT NOT NULL DEFAULT '', lang TEXT NOT NULL DEFAULT '', text TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT '', error INTEGER, history TEXT NOT NULL DEFAULT '[]', body TEXT NOT NULL DEFAULT '{}',
  trace_id TEXT, caller TEXT NOT NULL DEFAULT '', context_id TEXT NOT NULL DEFAULT '', by TEXT NOT NULL DEFAULT '',
  params TEXT NOT NULL DEFAULT '[]', category TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS wa_messages_number ON wa_messages(number, at);
CREATE TABLE IF NOT EXISTS wa_undeclared(
  name TEXT NOT NULL, lang TEXT NOT NULL, count INTEGER NOT NULL, first INTEGER NOT NULL, last INTEGER NOT NULL,
  params INTEGER NOT NULL DEFAULT 0, body TEXT NOT NULL DEFAULT '{}', caller TEXT NOT NULL DEFAULT '', PRIMARY KEY(name, lang));
CREATE TABLE IF NOT EXISTS wa_hooks(
  id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, kind TEXT NOT NULL, wamid TEXT NOT NULL DEFAULT '',
  url TEXT NOT NULL, status INTEGER, ms INTEGER, error TEXT NOT NULL DEFAULT '', attempt INTEGER NOT NULL DEFAULT 1,
  body TEXT NOT NULL DEFAULT '', signature TEXT NOT NULL DEFAULT '');
";

pub struct Msg {
    pub id: String,
    pub dir: &'static str,
    pub number: String,
    pub kind: String,
    pub template: String,
    pub lang: String,
    pub text: String,
    pub status: String,
    pub body: Value,
    pub trace_id: Option<String>,
    pub caller: String,
    pub context_id: String,
    pub by: String,
    pub params: Vec<String>,
    pub category: String,
}

pub async fn insert(ctx: &Ctx, m: Msg) -> Result<(), ApiError> {
    let hist = json!([{"status": m.status, "at": util::now_ms()}]).to_string();
    ctx.db.call(move |c| c.execute(
        "INSERT INTO wa_messages(id, at, dir, number, kind, template, lang, text, status, history, body, trace_id, caller, context_id, by, params, category)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![m.id, util::now_ms(), m.dir, m.number, m.kind, m.template, m.lang, m.text, m.status, hist, m.body.to_string(), m.trace_id, m.caller, m.context_id, m.by,
                json!(m.params).to_string(), m.category],
    )).await?;
    ctx.live.send("whatsapp.messages", "", "new");
    Ok(())
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let p = |i: usize| -> rusqlite::Result<Value> { Ok(serde_json::from_str(&r.get::<_, String>(i)?).unwrap_or(Value::Null)) };
    Ok(json!({"id": r.get::<_, String>(0)?, "at": r.get::<_, i64>(1)?, "dir": r.get::<_, String>(2)?, "number": r.get::<_, String>(3)?,
        "kind": r.get::<_, String>(4)?, "template": r.get::<_, String>(5)?, "lang": r.get::<_, String>(6)?, "text": r.get::<_, String>(7)?,
        "status": r.get::<_, String>(8)?, "error": r.get::<_, Option<i64>>(9)?, "history": p(10)?, "body": p(11)?, "traceId": r.get::<_, Option<String>>(12)?,
        "caller": r.get::<_, String>(13)?, "contextId": r.get::<_, String>(14)?, "by": r.get::<_, String>(15)?, "params": p(16)?, "category": r.get::<_, String>(17)?}))
}
const COLS: &str = "id, at, dir, number, kind, template, lang, text, status, error, history, body, trace_id, caller, context_id, by, params, category";

pub async fn get(ctx: &Ctx, id: &str) -> Result<Value, ApiError> {
    let i = id.to_string();
    ctx.db.call(move |c| c.query_row(&format!("SELECT {COLS} FROM wa_messages WHERE id=?1"), [i], row).optional()).await?
        .ok_or_else(|| ApiError::not_found(format!("No WhatsApp message {id}")))
}

#[derive(Default, Clone)]
pub struct Filter {
    pub number: Option<String>,
    pub status: Option<String>,
    pub template: Option<String>,
    pub text: Option<String>,
    pub dir: Option<String>,
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
        if let Some(t) = &f.template { w.push(format!("template=?{}", v.len() + 1)); v.push(t.clone().into()); }
        if let Some(d) = &f.dir { w.push(format!("dir=?{}", v.len() + 1)); v.push(d.clone().into()); }
        if let Some(t) = &f.text { w.push(format!("text LIKE ?{}", v.len() + 1)); v.push(format!("%{t}%").into()); }
        if let Some(s) = f.since_ms { w.push(format!("at>=?{}", v.len() + 1)); v.push(s.into()); }
        if let Some(q) = &f.q { let k = v.len() + 1; w.push(format!("(number LIKE ?{k} OR text LIKE ?{k} OR template LIKE ?{k} OR id LIKE ?{k})")); v.push(format!("%{}%", q.trim_start_matches('+')).into()); }
        let sql = format!("SELECT {COLS} FROM wa_messages WHERE {} ORDER BY at DESC, rowid DESC LIMIT {}", w.join(" AND "), if f.limit == 0 { 200 } else { f.limit });
        let mut st = c.prepare(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(v), row)?.collect::<Result<Vec<_>, _>>();
        rows
    }).await
}

/// Adds a status to the message (and its history); returns the message after.
pub async fn set_status(ctx: &Ctx, id: &str, status: &str, error: Option<u32>, by: &str) -> Result<Value, ApiError> {
    let (i, s, b) = (id.to_string(), status.to_string(), by.to_string());
    ctx.db.call(move |c| {
        let h: Option<String> = c.query_row("SELECT history FROM wa_messages WHERE id=?1", [&i], |r| r.get(0)).optional()?;
        let mut h: Vec<Value> = h.and_then(|h| serde_json::from_str(&h).ok()).unwrap_or_default();
        h.push(json!({"status": s, "at": util::now_ms(), "error": error, "by": b}));
        c.execute("UPDATE wa_messages SET status=?2, error=COALESCE(?3, error), history=?4 WHERE id=?1", params![i, s, error, json!(h).to_string()])
    }).await?;
    ctx.live.send("whatsapp.messages", id, status);
    get(ctx, id).await
}

/// Notes the webhook answer on the last history entry of that status.
pub async fn note_hook(ctx: &Ctx, id: &str, status: &str, answer: Value) -> Result<(), ApiError> {
    let (i, s) = (id.to_string(), status.to_string());
    ctx.db.call(move |c| {
        let h: Option<String> = c.query_row("SELECT history FROM wa_messages WHERE id=?1", [&i], |r| r.get(0)).optional()?;
        let mut h: Vec<Value> = h.and_then(|h| serde_json::from_str(&h).ok()).unwrap_or_default();
        if let Some(e) = h.iter_mut().rev().find(|e| e["status"] == s.as_str()) { e["webhook"] = answer; }
        c.execute("UPDATE wa_messages SET history=?2 WHERE id=?1", params![i, json!(h).to_string()])
    }).await?;
    ctx.live.send("whatsapp.messages", id, "webhook");
    Ok(())
}

/// When the person last wrote (the 24 h window opens from there).
pub async fn last_inbound(ctx: &Ctx, number: &str) -> Result<Option<i64>, ApiError> {
    let n = crate::config::digits(number);
    ctx.db.call(move |c| c.query_row("SELECT max(at) FROM wa_messages WHERE number=?1 AND dir='in'", [n], |r| r.get::<_, Option<i64>>(0))).await
}

pub async fn count_out(ctx: &Ctx, number: &str) -> Result<i64, ApiError> {
    let n = crate::config::digits(number);
    ctx.db.call(move |c| c.query_row("SELECT count(*) FROM wa_messages WHERE number=?1 AND dir='out'", [n], |r| r.get(0))).await
}

/// Different numbers the app wrote to in the last 24 h (the messaging tier).
pub async fn numbers_24h(ctx: &Ctx) -> Result<Vec<String>, ApiError> {
    let since = util::now_ms() - 86_400_000;
    ctx.db.call(move |c| c.prepare("SELECT DISTINCT number FROM wa_messages WHERE dir='out' AND status!='refused' AND at>=?1")?.query_map([since], |r| r.get(0))?.collect()).await
}

pub async fn sent_last_second(ctx: &Ctx) -> Result<i64, ApiError> {
    let since = util::now_ms() - 1000;
    ctx.db.call(move |c| c.query_row("SELECT count(*) FROM wa_messages WHERE dir='out' AND status!='refused' AND at>=?1", [since], |r| r.get(0))).await
}

pub async fn undeclared(ctx: &Ctx, name: &str, lang: &str, params_n: usize, body: &Value, caller: &str) -> Result<(), ApiError> {
    let (n, l, b, cl, now) = (name.to_string(), lang.to_string(), body.to_string(), caller.to_string(), util::now_ms());
    ctx.db.call(move |c| c.execute(
        "INSERT INTO wa_undeclared(name, lang, count, first, last, params, body, caller) VALUES(?1,?2,1,?3,?3,?4,?5,?6)
         ON CONFLICT(name, lang) DO UPDATE SET count=count+1, last=?3, params=?4, body=?5, caller=?6", params![n, l, now, params_n as i64, b, cl])).await?;
    ctx.live.send("whatsapp.templates", name, "undeclared");
    Ok(())
}

pub async fn list_undeclared(ctx: &Ctx) -> Result<Vec<Value>, ApiError> {
    ctx.db.call(|c| c.prepare("SELECT name, lang, count, first, last, params, body, caller FROM wa_undeclared ORDER BY last DESC")?.query_map([], |r| Ok(json!({
        "name": r.get::<_, String>(0)?, "language": r.get::<_, String>(1)?, "count": r.get::<_, i64>(2)?, "first": r.get::<_, i64>(3)?, "last": r.get::<_, i64>(4)?,
        "params": r.get::<_, i64>(5)?, "body": serde_json::from_str::<Value>(&r.get::<_, String>(6)?).unwrap_or(Value::Null), "caller": r.get::<_, String>(7)?})))?.collect()).await
}

pub async fn forget_undeclared(ctx: &Ctx, name: &str) -> Result<(), ApiError> {
    let n = name.to_string();
    ctx.db.call(move |c| c.execute("DELETE FROM wa_undeclared WHERE name=?1", [n])).await?;
    Ok(())
}

pub async fn hook(ctx: &Ctx, kind: &str, wamid: &str, url: &str, status: Option<u16>, ms: i64, error: &str, attempt: u32, body: &str, sig: &str) -> Result<(), ApiError> {
    let (k, w, u, e, b, s) = (kind.to_string(), wamid.to_string(), url.to_string(), error.to_string(), body.to_string(), sig.to_string());
    ctx.db.call(move |c| c.execute("INSERT INTO wa_hooks(at, kind, wamid, url, status, ms, error, attempt, body, signature) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![util::now_ms(), k, w, u, status, ms, e, attempt, b, s])).await?;
    ctx.live.send("whatsapp.hooks", wamid, "sent");
    Ok(())
}

pub async fn hooks(ctx: &Ctx, limit: u32) -> Result<Vec<Value>, ApiError> {
    ctx.db.call(move |c| c.prepare(&format!("SELECT at, kind, wamid, url, status, ms, error, attempt FROM wa_hooks ORDER BY id DESC LIMIT {limit}"))?.query_map([], |r| Ok(json!({
        "at": r.get::<_, i64>(0)?, "kind": r.get::<_, String>(1)?, "wamid": r.get::<_, String>(2)?, "url": r.get::<_, String>(3)?, "status": r.get::<_, Option<i64>>(4)?,
        "ms": r.get::<_, Option<i64>>(5)?, "error": r.get::<_, String>(6)?, "attempt": r.get::<_, i64>(7)?})))?.collect()).await
}

/// One row per number: the last message, the window and the person's name (W1 left list).
pub async fn conversations(ctx: &Ctx, q: Option<String>) -> Result<Vec<Value>, ApiError> {
    let all = list(ctx, Filter { q, limit: 2000, ..Default::default() }).await?;
    let mut seen = std::collections::HashMap::<String, usize>::new();
    let mut out: Vec<Value> = vec![];
    for m in all {
        let n = m["number"].as_str().unwrap_or("").to_string();
        match seen.get(&n) {
            Some(&i) => { out[i]["count"] = json!(out[i]["count"].as_i64().unwrap_or(1) + 1); }
            None => { seen.insert(n.clone(), out.len()); let mut c = json!({"number": n, "last": m, "count": 1}); c["lastIn"] = Value::Null; out.push(c); }
        }
    }
    for c in out.iter_mut() {
        let n = c["number"].as_str().unwrap_or("").to_string();
        let li = last_inbound(ctx, &n).await?;
        c["window"] = crate::flow::window_json(li);
        c["profileName"] = json!(crate::config::recipient_for(ctx, &n).profile_name);
    }
    Ok(out)
}

pub async fn usage(ctx: &Ctx, name: &str, since_ms: i64) -> Result<Value, ApiError> {
    let (n, s) = (name.to_string(), since_ms);
    ctx.db.call(move |c| {
        let q = |sql: &str| -> rusqlite::Result<i64> { c.query_row(sql, params![n, s], |r| r.get(0)) };
        let sent = q("SELECT count(*) FROM wa_messages WHERE dir='out' AND template=?1 AND at>=?2")?;
        let failed = q("SELECT count(*) FROM wa_messages WHERE dir='out' AND template=?1 AND at>=?2 AND status='failed'")?;
        let read = q("SELECT count(*) FROM wa_messages WHERE dir='out' AND template=?1 AND at>=?2 AND status='read'")?;
        let taps = q("SELECT count(*) FROM wa_messages WHERE dir='in' AND kind='button' AND context_id IN (SELECT id FROM wa_messages WHERE template=?1) AND at>=?2")?;
        let mut st = c.prepare("SELECT error, count(*) FROM wa_messages WHERE dir='out' AND template=?1 AND at>=?2 AND status='failed' GROUP BY error")?;
        let by: Vec<Value> = st.query_map(params![n, s], |r| Ok(json!({"code": r.get::<_, Option<i64>>(0)?, "count": r.get::<_, i64>(1)?})))?.collect::<Result<_, _>>()?;
        let last: Option<String> = c.query_row("SELECT params FROM wa_messages WHERE dir='out' AND template=?1 ORDER BY at DESC LIMIT 1", [&n], |r| r.get(0)).optional()?;
        let last_at: Option<i64> = c.query_row("SELECT max(at) FROM wa_messages WHERE dir='out' AND template=?1", [&n], |r| r.get(0))?;
        let last_by: Option<String> = c.query_row("SELECT caller FROM wa_messages WHERE dir='out' AND template=?1 ORDER BY at DESC LIMIT 1", [&n], |r| r.get(0)).optional()?;
        Ok(json!({"sent": sent, "failed": failed, "read": read, "buttonsTapped": taps, "failuresByCode": by,
                  "lastParams": last.and_then(|l| serde_json::from_str::<Value>(&l).ok()), "lastAt": last_at, "lastBy": last_by}))
    }).await
}
