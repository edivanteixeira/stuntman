//! Every call a plugin answered (A1), every request no route took (A2), tokens, the routes' kept
//! state, wait counters and forced answers. SQLite of the environment.

use stuntman_core::{util, ApiError, Ctx};
use rusqlite::{params, OptionalExtension, Row};
use serde_json::{json, Value};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS api_calls(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, at INTEGER NOT NULL, service TEXT NOT NULL, route TEXT NOT NULL,
  method TEXT NOT NULL, path TEXT NOT NULL, status INTEGER NOT NULL, answer TEXT NOT NULL, sentence TEXT NOT NULL DEFAULT '',
  billable INTEGER NOT NULL DEFAULT 0, caller TEXT NOT NULL DEFAULT '', trace_id TEXT, ms INTEGER NOT NULL DEFAULT 0,
  provoked TEXT, sent TEXT NOT NULL DEFAULT '{}', answered TEXT NOT NULL DEFAULT '{}', request TEXT NOT NULL DEFAULT '{}',
  response TEXT NOT NULL DEFAULT '{}', steps TEXT NOT NULL DEFAULT '[]', sends TEXT NOT NULL DEFAULT '[]');
CREATE INDEX IF NOT EXISTS api_calls_service ON api_calls(service, at);
CREATE TABLE IF NOT EXISTS api_unmatched(
  id TEXT PRIMARY KEY, at INTEGER NOT NULL, method TEXT NOT NULL, path TEXT NOT NULL, host TEXT NOT NULL DEFAULT '',
  service TEXT NOT NULL DEFAULT '', caller TEXT NOT NULL DEFAULT '', request TEXT NOT NULL DEFAULT '{}', closest TEXT NOT NULL DEFAULT '{}',
  ignored INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS api_tokens(token TEXT PRIMARY KEY, service TEXT NOT NULL, client TEXT NOT NULL, issued INTEGER NOT NULL, expires INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS api_state(service TEXT NOT NULL, collection TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, at INTEGER NOT NULL,
  PRIMARY KEY(service, collection, key));
CREATE TABLE IF NOT EXISTS api_wait(service TEXT NOT NULL, route TEXT NOT NULL, key TEXT NOT NULL, n INTEGER NOT NULL, PRIMARY KEY(service, route, key));
CREATE TABLE IF NOT EXISTS api_force_log(seq INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, service TEXT NOT NULL, answer TEXT NOT NULL,
  route TEXT NOT NULL DEFAULT '', count INTEGER, until INTEGER, by_who TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS api_scheduled(seq INTEGER PRIMARY KEY AUTOINCREMENT, due INTEGER NOT NULL, job TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS api_idem(service TEXT NOT NULL, key TEXT NOT NULL, hash TEXT NOT NULL, status INTEGER NOT NULL, headers TEXT NOT NULL,
  body BLOB NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(service, key));
CREATE TABLE IF NOT EXISTS api_force(service TEXT PRIMARY KEY, answer TEXT NOT NULL, route TEXT NOT NULL DEFAULT '', left INTEGER, until INTEGER,
  by_who TEXT NOT NULL, at INTEGER NOT NULL);
";

pub struct Call {
    pub id: String,
    pub service: String,
    pub route: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub answer: String,
    pub sentence: String,
    pub billable: bool,
    pub caller: String,
    pub trace_id: Option<String>,
    pub ms: i64,
    pub provoked: Option<String>,
    pub sent: Value,
    pub answered: Value,
    pub request: Value,
    pub response: Value,
    pub steps: Value,
}

pub async fn record(ctx: &Ctx, c: Call) -> Result<(), ApiError> {
    let id = c.id.clone();
    ctx.db
        .call(move |cn| {
            cn.execute(
                "INSERT INTO api_calls(id, at, service, route, method, path, status, answer, sentence, billable, caller, trace_id, ms, provoked, sent, answered, request, response, steps)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
                params![c.id, util::now_ms(), c.service, c.route, c.method, c.path, c.status, c.answer, c.sentence, c.billable as i64, c.caller, c.trace_id, c.ms, c.provoked,
                        c.sent.to_string(), c.answered.to_string(), c.request.to_string(), c.response.to_string(), c.steps.to_string()],
            )
            .map(|_| ())
        })
        .await?;
    ctx.live.send("apis.call", &id, "created");
    Ok(())
}

pub async fn add_send(ctx: &Ctx, id: &str, s: Value) -> Result<(), ApiError> {
    let i = id.to_string();
    ctx.db
        .call(move |cn| {
            let cur: String = cn.query_row("SELECT sends FROM api_calls WHERE id=?1", [&i], |r| r.get(0)).optional()?.unwrap_or("[]".into());
            let mut v: Vec<Value> = serde_json::from_str(&cur).unwrap_or_default();
            v.push(s);
            cn.execute("UPDATE api_calls SET sends=?2 WHERE id=?1", params![i, json!(v).to_string()]).map(|_| ())
        })
        .await?;
    ctx.live.send("apis.call", id, "updated");
    Ok(())
}

const COLS: &str = "id, at, service, route, method, path, status, answer, sentence, billable, caller, trace_id, ms, provoked, sent, answered, request, response, steps, sends";

fn call_json(r: &Row, raw: bool) -> rusqlite::Result<Value> {
    let j = |i: usize| -> rusqlite::Result<Value> { Ok(serde_json::from_str(&r.get::<_, String>(i)?).unwrap_or(Value::Null)) };
    let mut v = json!({"id": r.get::<_, String>(0)?, "at": r.get::<_, i64>(1)?, "service": r.get::<_, String>(2)?, "route": r.get::<_, String>(3)?,
        "method": r.get::<_, String>(4)?, "path": r.get::<_, String>(5)?, "status": r.get::<_, i64>(6)?, "answer": r.get::<_, String>(7)?,
        "sentence": r.get::<_, String>(8)?, "billable": r.get::<_, i64>(9)? != 0, "caller": r.get::<_, String>(10)?, "traceId": r.get::<_, Option<String>>(11)?,
        "ms": r.get::<_, i64>(12)?, "provoked": r.get::<_, Option<String>>(13)?, "sent": j(14)?, "answered": j(15)?, "steps": j(18)?, "sends": j(19)?});
    if raw {
        v["request"] = j(16)?;
        v["response"] = j(17)?;
    }
    Ok(v)
}

pub async fn list(ctx: &Ctx, service: Option<String>, answer: Option<String>, since: i64, limit: i64) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare(&format!("SELECT {COLS} FROM api_calls WHERE (?1 IS NULL OR service=?1) AND (?2 IS NULL OR answer=?2) AND at>=?3 ORDER BY seq DESC LIMIT ?4"))?;
            let v = s.query_map(params![service, answer, since, limit], |r| call_json(r, false))?.collect::<Result<Vec<_>, _>>();
            v
        })
        .await
}

pub async fn get(ctx: &Ctx, id: &str, raw: bool) -> Result<Value, ApiError> {
    let i = id.to_string();
    ctx.db
        .call(move |cn| cn.query_row(&format!("SELECT {COLS} FROM api_calls WHERE id=?1"), [i], |r| call_json(r, raw)).optional())
        .await?
        .ok_or_else(|| ApiError::not_found(format!("No API call {id}")))
}

/// The calls a recording kept (A9), oldest first, with request and response.
pub async fn recorded(ctx: &Ctx, service: &str, since: i64, until: i64) -> Result<Vec<Value>, ApiError> {
    let s2 = service.to_string();
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare(&format!("SELECT {COLS} FROM api_calls WHERE service=?1 AND answer='recorded' AND at>=?2 AND at<=?3 ORDER BY seq LIMIT 5000"))?;
            let v = s.query_map(params![s2, since, until], |r| call_json(r, true))?.collect::<Result<Vec<_>, _>>();
            v
        })
        .await
}

pub async fn count_recorded(ctx: &Ctx, service: &str, since: i64) -> Result<i64, ApiError> {
    let s2 = service.to_string();
    ctx.db.call(move |cn| cn.query_row("SELECT count(*) FROM api_calls WHERE service=?1 AND answer='recorded' AND at>=?2", params![s2, since], |r| r.get(0))).await
}

/// (calls, billable) per service since a time, and calls per route today.
pub async fn counts(ctx: &Ctx, since: i64) -> Result<std::collections::BTreeMap<String, (i64, i64)>, ApiError> {
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare("SELECT service, count(*), sum(billable) FROM api_calls WHERE at>=?1 GROUP BY service")?;
            let v = s.query_map([since], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get::<_, Option<i64>>(2)?.unwrap_or(0)))))?.collect::<Result<_, _>>();
            v
        })
        .await
}

pub async fn route_counts(ctx: &Ctx, service: &str, since: i64) -> Result<std::collections::BTreeMap<String, i64>, ApiError> {
    let s2 = service.to_string();
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare("SELECT route, count(*) FROM api_calls WHERE service=?1 AND at>=?2 GROUP BY route")?;
            let v = s.query_map(params![s2, since], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?.collect::<Result<_, _>>();
            v
        })
        .await
}

/// Billable calls by CPF-like "key" (repeated ones point at a retry loop).
pub async fn repeated(ctx: &Ctx, service: &str, since: i64) -> Result<Vec<Value>, ApiError> {
    let s2 = service.to_string();
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare("SELECT sent, count(*) FROM api_calls WHERE service=?1 AND at>=?2 AND billable=1 GROUP BY sent HAVING count(*) > 1 ORDER BY count(*) DESC LIMIT 20")?;
            let v = s.query_map(params![s2, since], |r| Ok(json!({"sent": serde_json::from_str::<Value>(&r.get::<_, String>(0)?).unwrap_or(Value::Null), "calls": r.get::<_, i64>(1)?})))?.collect::<Result<_, _>>();
            v
        })
        .await
}

// ---------------------------------------------------------------- unmatched

pub async fn unmatched_add(ctx: &Ctx, method: &str, path: &str, host: &str, service: &str, caller: &str, request: Value, closest: Value) -> Result<String, ApiError> {
    let id = util::id("unm");
    let (i, m, p, h, s, c) = (id.clone(), method.to_string(), path.to_string(), host.to_string(), service.to_string(), caller.to_string());
    ctx.db
        .call(move |cn| cn.execute("INSERT INTO api_unmatched VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0)", params![i, util::now_ms(), m, p, h, s, c, request.to_string(), closest.to_string()]).map(|_| ()))
        .await?;
    ctx.live.send("apis.unmatched", &id, "created");
    Ok(id)
}

pub async fn unmatched(ctx: &Ctx, since: i64, ignored: bool) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare("SELECT id, at, method, path, host, service, caller, request, closest, ignored FROM api_unmatched WHERE at>=?1 AND (ignored=0 OR ?2) ORDER BY at DESC LIMIT 500")?;
            let v = s
                .query_map(params![since, ignored], |r| {
                    Ok(json!({"id": r.get::<_, String>(0)?, "at": r.get::<_, i64>(1)?, "method": r.get::<_, String>(2)?, "path": r.get::<_, String>(3)?,
                        "host": r.get::<_, String>(4)?, "service": r.get::<_, String>(5)?, "caller": r.get::<_, String>(6)?,
                        "request": serde_json::from_str::<Value>(&r.get::<_, String>(7)?).unwrap_or(Value::Null),
                        "closest": serde_json::from_str::<Value>(&r.get::<_, String>(8)?).unwrap_or(Value::Null), "ignored": r.get::<_, i64>(9)? != 0}))
                })?
                .collect::<Result<Vec<_>, _>>();
            v
        })
        .await
}

pub async fn unmatched_one(ctx: &Ctx, id: &str) -> Result<Value, ApiError> {
    unmatched(ctx, 0, true).await?.into_iter().find(|u| u["id"] == json!(id)).ok_or_else(|| ApiError::not_found(format!("No unmatched request {id}")))
}

/// Ignoring hides every request with the same method and path.
pub async fn ignore(ctx: &Ctx, id: &str) -> Result<i64, ApiError> {
    let u = unmatched_one(ctx, id).await?;
    let (m, p) = (u["method"].as_str().unwrap_or("").to_string(), u["path"].as_str().unwrap_or("").to_string());
    let n = ctx.db.call(move |cn| cn.execute("UPDATE api_unmatched SET ignored=1 WHERE method=?1 AND path=?2", params![m, p])).await?;
    ctx.live.send("apis.unmatched", id, "updated");
    Ok(n as i64)
}

// ---------------------------------------------------------------- tokens

pub async fn issue_token(ctx: &Ctx, service: &str, client: &str, ttl_ms: i64) -> Result<String, ApiError> {
    let t = format!("dbl_{}{}", util::uuid().replace('-', ""), &util::uuid()[..8]);
    let (tk, s, c) = (t.clone(), service.to_string(), client.to_string());
    let now = util::now_ms();
    ctx.db.call(move |cn| cn.execute("INSERT INTO api_tokens VALUES(?1,?2,?3,?4,?5)", params![tk, s, c, now, now + ttl_ms]).map(|_| ())).await?;
    Ok(t)
}

/// (client, issued, expires) of a token of this service.
pub async fn token(ctx: &Ctx, service: &str, t: &str) -> Result<Option<(String, i64, i64)>, ApiError> {
    let (s, tk) = (service.to_string(), t.to_string());
    ctx.db.call(move |cn| cn.query_row("SELECT client, issued, expires FROM api_tokens WHERE service=?1 AND token=?2", params![s, tk], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()).await
}

pub async fn expire_tokens(ctx: &Ctx, service: &str) -> Result<i64, ApiError> {
    let s = service.to_string();
    let now = util::now_ms();
    ctx.db.call(move |cn| cn.execute("UPDATE api_tokens SET expires=?2 WHERE service=?1 AND expires>?2", params![s, now]).map(|n| n as i64)).await
}

// ---------------------------------------------------------------- state, wait

pub async fn keep(ctx: &Ctx, service: &str, collection: &str, key: &str, value: Value) -> Result<(), ApiError> {
    let (s, c, k) = (service.to_string(), collection.to_string(), key.to_string());
    ctx.db.call(move |cn| cn.execute("INSERT OR REPLACE INTO api_state VALUES(?1,?2,?3,?4,?5)", params![s, c, k, value.to_string(), util::now_ms()]).map(|_| ())).await
}

/// One kept value.
pub async fn kept(ctx: &Ctx, service: &str, collection: &str, key: &str) -> Result<Option<Value>, ApiError> {
    let (s, c, k) = (service.to_string(), collection.to_string(), key.to_string());
    let v: Option<String> = ctx.db.call(move |cn| cn.query_row("SELECT value FROM api_state WHERE service=?1 AND collection=?2 AND key=?3", params![s, c, k], |r| r.get(0)).optional()).await?;
    Ok(v.and_then(|v| serde_json::from_str(&v).ok()))
}

/// collection → {key: value}
pub async fn state(ctx: &Ctx, service: &str) -> Result<Value, ApiError> {
    let s = service.to_string();
    ctx.db
        .call(move |cn| {
            let mut q = cn.prepare("SELECT collection, key, value FROM api_state WHERE service=?1")?;
            let mut out = serde_json::Map::new();
            for row in q.query_map([s], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
                let (c, k, v) = row?;
                out.entry(c).or_insert(json!({}))[k] = serde_json::from_str(&v).unwrap_or(Value::Null);
            }
            Ok(Value::Object(out))
        })
        .await
}

pub async fn wait_count(ctx: &Ctx, service: &str, route: &str, key: &str) -> Result<i64, ApiError> {
    let (s, r, k) = (service.to_string(), route.to_string(), key.to_string());
    ctx.db
        .call(move |cn| {
            cn.execute("INSERT INTO api_wait VALUES(?1,?2,?3,1) ON CONFLICT(service, route, key) DO UPDATE SET n=n+1", params![s, r, k])?;
            cn.query_row("SELECT n FROM api_wait WHERE service=?1 AND route=?2 AND key=?3", params![s, r, k], |x| x.get(0))
        })
        .await
}

// ---------------------------------------------------------------- forced answers

pub async fn force(ctx: &Ctx, service: &str, answer: &str, route: &str, count: Option<i64>, until: Option<i64>, by: &str) -> Result<(), ApiError> {
    let (s, a, r, b) = (service.to_string(), answer.to_string(), route.to_string(), by.to_string());
    ctx.db
        .call(move |cn| {
            cn.execute("INSERT INTO api_force_log(at, service, answer, route, count, until, by_who) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![util::now_ms(), s, if a.is_empty() { "default" } else { a.as_str() }, r, count, until, b])?;
            if a.is_empty() || a == "default" {
                cn.execute("DELETE FROM api_force WHERE service=?1", [s]).map(|_| ())
            } else {
                cn.execute("INSERT OR REPLACE INTO api_force VALUES(?1,?2,?3,?4,?5,?6,?7)", params![s, a, r, count, until, b, util::now_ms()]).map(|_| ())
            }
        })
        .await?;
    ctx.live.send("apis.force", service, "updated");
    Ok(())
}

pub async fn forced(ctx: &Ctx, service: &str) -> Result<Option<Value>, ApiError> {
    let s = service.to_string();
    let now = util::now_ms();
    ctx.db
        .call(move |cn| {
            cn.query_row("SELECT answer, route, left, until, by_who, at FROM api_force WHERE service=?1 AND (until IS NULL OR until>?2) AND (left IS NULL OR left>0)", params![s, now], |r| {
                Ok(json!({"answer": r.get::<_, String>(0)?, "route": r.get::<_, String>(1)?, "left": r.get::<_, Option<i64>>(2)?, "until": r.get::<_, Option<i64>>(3)?, "by": r.get::<_, String>(4)?, "at": r.get::<_, i64>(5)?}))
            })
            .optional()
        })
        .await
}

/// A force for `forced_route` ("" = every route) hits `route`; `named_only` routes need their name.
fn force_applies(forced_route: &str, route: &str, named_only: bool) -> bool {
    forced_route == route || (forced_route.is_empty() && !named_only)
}

/// Takes one forced answer for this route, if one applies. `named_only`: only a force that names
/// this route (the token endpoint is never hit by a service-wide force).
pub async fn take_force(ctx: &Ctx, service: &str, route: &str, named_only: bool) -> Result<Option<(String, String)>, ApiError> {
    let Some(f) = forced(ctx, service).await? else { return Ok(None) };
    if !force_applies(f["route"].as_str().unwrap_or(""), route, named_only) {
        return Ok(None);
    }
    if f["left"].is_number() {
        let s = service.to_string();
        ctx.db.call(move |cn| cn.execute("UPDATE api_force SET left=left-1 WHERE service=?1", [s]).map(|_| ())).await?;
        ctx.live.send("apis.force", service, "updated");
    }
    Ok(Some((f["answer"].as_str().unwrap_or("").to_string(), f["by"].as_str().unwrap_or("").to_string())))
}

pub async fn wipe_service(ctx: &Ctx, service: &str) -> Result<(), ApiError> {
    let s = service.to_string();
    ctx.db
        .call(move |cn| {
            for t in ["api_tokens", "api_state", "api_wait", "api_force"] {
                cn.execute(&format!("DELETE FROM {t} WHERE service=?1"), [&s])?;
            }
            Ok(())
        })
        .await
}

/// Who forced what, newest first (A1/A4).
pub async fn force_log(ctx: &Ctx, service: Option<String>, limit: i64) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |cn| {
            let mut q = cn.prepare("SELECT at, service, answer, route, count, until, by_who FROM api_force_log WHERE (?1 IS NULL OR service=?1) ORDER BY seq DESC LIMIT ?2")?;
            let v = q
                .query_map(params![service, limit], |r| {
                    Ok(json!({"at": r.get::<_, i64>(0)?, "service": r.get::<_, String>(1)?, "answer": r.get::<_, String>(2)?, "route": r.get::<_, String>(3)?,
                        "count": r.get::<_, Option<i64>>(4)?, "until": r.get::<_, Option<i64>>(5)?, "by": r.get::<_, String>(6)?}))
                })?
                .collect::<Result<Vec<_>, _>>();
            v
        })
        .await
}

#[cfg(test)]
mod t {
    #[test]
    fn force_on_the_token_route_only_by_name() {
        use super::force_applies;
        assert!(force_applies("", "POST /v", false));
        assert!(!force_applies("", "POST /token", true));
        assert!(force_applies("POST /token", "POST /token", true));
        assert!(!force_applies("POST /v", "POST /token", true));
        assert!(!force_applies("POST /token", "POST /v", false));
    }
}

/// A webhook kept until the environment's clock reaches `due` (A10c).
pub async fn schedule(ctx: &Ctx, due: i64, job: Value) -> Result<(), ApiError> {
    ctx.db.call(move |cn| cn.execute("INSERT INTO api_scheduled(due, job) VALUES(?1,?2)", params![due, job.to_string()]).map(|_| ())).await?;
    ctx.live.send("apis.scheduled", "", "updated");
    Ok(())
}

/// Takes the jobs whose time has come.
pub async fn take_due(ctx: &Ctx, now: i64) -> Result<Vec<Value>, ApiError> {
    let v: Vec<Value> = ctx
        .db
        .call(move |cn| {
            let mut st = cn.prepare("SELECT job FROM api_scheduled WHERE due<=?1 ORDER BY due, seq")?;
            let v = st.query_map([now], |r| r.get::<_, String>(0))?.filter_map(|x| x.ok()).filter_map(|t| serde_json::from_str(&t).ok()).collect::<Vec<Value>>();
            cn.execute("DELETE FROM api_scheduled WHERE due<=?1", [now])?;
            Ok(v)
        })
        .await?;
    if !v.is_empty() {
        ctx.live.send("apis.scheduled", "", "updated");
    }
    Ok(v)
}

/// What waits to be sent: [{due, url, event, service}].
pub async fn scheduled(ctx: &Ctx) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(|cn| {
            let mut st = cn.prepare("SELECT due, job FROM api_scheduled ORDER BY due, seq LIMIT 200")?;
            let v = st
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .filter_map(|x| x.ok())
                .map(|(d, t)| {
                    let j: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    json!({"due": d, "url": j["send"]["url"], "event": j["send"]["event"], "call": j["call"]})
                })
                .collect();
            Ok(v)
        })
        .await
}

pub async fn idem_get(ctx: &Ctx, service: &str, key: &str, since: i64) -> Result<Option<(String, u16, Vec<(String, String)>, Vec<u8>)>, ApiError> {
    let (s, k) = (service.to_string(), key.to_string());
    ctx.db
        .call(move |cn| {
            cn.query_row("SELECT hash, status, headers, body FROM api_idem WHERE service=?1 AND key=?2 AND at>?3", params![s, k, since], |r| {
                let h: String = r.get(2)?;
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u16, serde_json::from_str(&h).unwrap_or_default(), r.get::<_, Vec<u8>>(3)?))
            })
            .optional()
        })
        .await
}

pub async fn idem_put(ctx: &Ctx, service: &str, key: &str, hash: &str, status: u16, headers: &[(String, String)], body: &[u8]) -> Result<(), ApiError> {
    let (s, k, h, hs, b) = (service.to_string(), key.to_string(), hash.to_string(), serde_json::to_string(headers).unwrap_or_default(), body.to_vec());
    ctx.db.call(move |cn| cn.execute("INSERT OR REPLACE INTO api_idem VALUES(?1,?2,?3,?4,?5,?6,?7)", params![s, k, h, status as i64, hs, b, util::now_ms()]).map(|_| ())).await
}
