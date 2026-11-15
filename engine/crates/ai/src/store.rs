//! Every model call (prompt, answer, tokens, cost) and the failures forced for the next calls.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS ai_calls(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, at INTEGER NOT NULL, provider TEXT NOT NULL, api TEXT NOT NULL, model TEXT NOT NULL,
  status INTEGER NOT NULL, why TEXT NOT NULL DEFAULT '', tokens_in INTEGER NOT NULL DEFAULT 0, tokens_out INTEGER NOT NULL DEFAULT 0,
  cached INTEGER NOT NULL DEFAULT 0, cost REAL NOT NULL DEFAULT 0, stream INTEGER NOT NULL DEFAULT 0, ms INTEGER NOT NULL DEFAULT 0,
  caller TEXT NOT NULL DEFAULT '', prompt TEXT NOT NULL DEFAULT '', answer TEXT NOT NULL DEFAULT '', request TEXT NOT NULL DEFAULT '{}', response TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS ai_calls_at ON ai_calls(provider, at);
CREATE TABLE IF NOT EXISTS ai_force(seq INTEGER PRIMARY KEY AUTOINCREMENT, provider TEXT NOT NULL, kind TEXT NOT NULL, left INTEGER NOT NULL, by_who TEXT NOT NULL, at INTEGER NOT NULL);
";

pub struct Call {
    pub id: String,
    pub provider: String,
    pub api: String,
    pub model: String,
    pub status: u16,
    pub why: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cached: u64,
    pub cost: f64,
    pub stream: bool,
    pub ms: i64,
    pub caller: String,
    pub prompt: Value,
    pub answer: String,
    pub request: Value,
    pub response: String,
}

pub async fn record(ctx: &Ctx, c: Call) -> Result<(), ApiError> {
    let id = c.id.clone();
    ctx.db
        .call(move |cn| {
            cn.execute(
                "INSERT INTO ai_calls(id, at, provider, api, model, status, why, tokens_in, tokens_out, cached, cost, stream, ms, caller, prompt, answer, request, response) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
                params![c.id, util::now_ms(), c.provider, c.api, c.model, c.status, c.why, c.tokens_in as i64, c.tokens_out as i64, c.cached as i64, c.cost, c.stream, c.ms, c.caller,
                    c.prompt.to_string(), c.answer, c.request.to_string().chars().take(256 * 1024).collect::<String>(), c.response.chars().take(256 * 1024).collect::<String>()],
            )
            .map(|_| ())
        })
        .await?;
    ctx.live.send("ai.calls", &id, "row");
    Ok(())
}

fn row(r: &rusqlite::Row, full: bool) -> rusqlite::Result<Value> {
    let mut v = json!({"id": r.get::<_, String>(0)?, "at": r.get::<_, i64>(1)?, "provider": r.get::<_, String>(2)?, "api": r.get::<_, String>(3)?, "model": r.get::<_, String>(4)?,
        "status": r.get::<_, i64>(5)?, "why": r.get::<_, String>(6)?, "tokensIn": r.get::<_, i64>(7)?, "tokensOut": r.get::<_, i64>(8)?, "cached": r.get::<_, i64>(9)?,
        "cost": r.get::<_, f64>(10)?, "stream": r.get::<_, bool>(11)?, "ms": r.get::<_, i64>(12)?, "caller": r.get::<_, String>(13)?,
        "prompt": serde_json::from_str::<Value>(&r.get::<_, String>(14)?).unwrap_or(Value::Null), "answer": r.get::<_, String>(15)?});
    if full {
        v["request"] = serde_json::from_str(&r.get::<_, String>(16)?).unwrap_or(Value::Null);
        v["response"] = json!(r.get::<_, String>(17)?);
    }
    Ok(v)
}

const COLS: &str = "id, at, provider, api, model, status, why, tokens_in, tokens_out, cached, cost, stream, ms, caller, prompt, answer, request, response";

pub async fn list(ctx: &Ctx, provider: Option<String>, since: i64, limit: i64) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |cn| {
            let q = format!("SELECT {COLS} FROM ai_calls WHERE at>=?1 AND (?2 IS NULL OR provider=?2) ORDER BY seq DESC LIMIT ?3");
            let mut st = cn.prepare(&q)?;
            let v = st.query_map(params![since, provider, limit], |r| row(r, false))?.filter_map(|x| x.ok()).collect();
            Ok(v)
        })
        .await
}

pub async fn get(ctx: &Ctx, id: &str) -> Result<Option<Value>, ApiError> {
    let id = id.to_string();
    ctx.db.call(move |cn| cn.query_row(&format!("SELECT {COLS} FROM ai_calls WHERE id=?1"), [id], |r| row(r, true)).optional()).await
}

pub async fn spend(ctx: &Ctx, since: i64) -> Result<Value, ApiError> {
    ctx.db
        .call(move |cn| {
            let mut st = cn.prepare("SELECT provider, COUNT(*), SUM(tokens_in), SUM(tokens_out), SUM(cached), SUM(cost), SUM(CASE WHEN status>=400 OR status=0 THEN 1 ELSE 0 END), SUM(CASE WHEN why LIKE 'forced%' THEN 1 ELSE 0 END) FROM ai_calls WHERE at>=?1 GROUP BY provider")?;
            let rows: Vec<Value> = st
                .query_map([since], |r| Ok(json!({"provider": r.get::<_, String>(0)?, "calls": r.get::<_, i64>(1)?, "tokensIn": r.get::<_, Option<i64>>(2)?.unwrap_or(0), "tokensOut": r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    "cached": r.get::<_, Option<i64>>(4)?.unwrap_or(0), "cost": r.get::<_, Option<f64>>(5)?.unwrap_or(0.0), "failures": r.get::<_, i64>(6)?, "forced": r.get::<_, i64>(7)?})))?
                .filter_map(|x| x.ok())
                .collect();
            Ok(Value::Array(rows))
        })
        .await
}

pub async fn force(ctx: &Ctx, provider: &str, kind: &str, count: i64, by: &str) -> Result<(), ApiError> {
    let (p, k, b) = (provider.to_string(), kind.to_string(), by.to_string());
    ctx.db
        .call(move |cn| {
            if k == "none" {
                cn.execute("DELETE FROM ai_force WHERE provider=?1", [p]).map(|_| ())
            } else {
                cn.execute("INSERT INTO ai_force(provider, kind, left, by_who, at) VALUES(?1,?2,?3,?4,?5)", params![p, k, count, b, util::now_ms()]).map(|_| ())
            }
        })
        .await?;
    ctx.live.send("ai.force", provider, "updated");
    Ok(())
}

pub async fn forced(ctx: &Ctx) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(|cn| {
            let mut st = cn.prepare("SELECT provider, kind, left, by_who FROM ai_force WHERE left>0 ORDER BY seq")?;
            let v = st.query_map([], |r| Ok(json!({"provider": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?, "left": r.get::<_, i64>(2)?, "by": r.get::<_, String>(3)?})))?.filter_map(|x| x.ok()).collect();
            Ok(v)
        })
        .await
}

/// Takes one forced failure for this provider (or for every provider).
pub async fn take_force(ctx: &Ctx, provider: &str) -> Result<Option<(String, String)>, ApiError> {
    let p = provider.to_string();
    ctx.db
        .call(move |cn| {
            let f: Option<(i64, String, String)> = cn
                .query_row("SELECT seq, kind, by_who FROM ai_force WHERE left>0 AND (provider=?1 OR provider='') ORDER BY seq LIMIT 1", [p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?;
            if let Some((seq, _, _)) = &f {
                cn.execute("UPDATE ai_force SET left=left-1 WHERE seq=?1", [seq])?;
                cn.execute("DELETE FROM ai_force WHERE left<=0", [])?;
            }
            Ok(f.map(|x| (x.1, x.2)))
        })
        .await
}
