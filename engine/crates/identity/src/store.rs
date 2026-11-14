//! Signing keys, the login pages waiting for a person, authorization codes and refresh tokens, the
//! "next sign-ins" provocation and the sign-ins themselves (sentence, steps, claims, tokens).

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS id_keys(provider TEXT NOT NULL, kid TEXT NOT NULL, pem TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY(provider, kid));
CREATE TABLE IF NOT EXISTS id_state(kind TEXT NOT NULL, id TEXT NOT NULL, provider TEXT NOT NULL, at INTEGER NOT NULL, data TEXT NOT NULL, used INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(kind, id));
CREATE TABLE IF NOT EXISTS id_provoke(provider TEXT PRIMARY KEY, answer TEXT NOT NULL, until INTEGER, count INTEGER, by TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS id_signins(
  id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, provider TEXT NOT NULL, user TEXT NOT NULL DEFAULT '', app TEXT NOT NULL DEFAULT '',
  kind TEXT NOT NULL DEFAULT 'sign-in', result TEXT NOT NULL, code TEXT NOT NULL DEFAULT '', sentence TEXT NOT NULL, flow TEXT NOT NULL DEFAULT '',
  steps TEXT NOT NULL DEFAULT '[]', claims TEXT NOT NULL DEFAULT '{}', tokens TEXT NOT NULL DEFAULT '{}', by TEXT NOT NULL DEFAULT '', pending TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS id_signins_at ON id_signins(provider, at);
";

// ---------------------------------------------------------------- keys

pub async fn key(ctx: &Ctx, provider: &str, kid: &str) -> Result<Option<String>, ApiError> {
    let (p, k) = (provider.to_string(), kid.to_string());
    ctx.db.call(move |c| c.query_row("SELECT pem FROM id_keys WHERE provider=?1 AND kid=?2", params![p, k], |r| r.get(0)).optional()).await
}

pub async fn add_key(ctx: &Ctx, provider: &str, kid: &str, pem: &str) -> Result<(), ApiError> {
    let (p, k, m) = (provider.to_string(), kid.to_string(), pem.to_string());
    ctx.db.call(move |c| c.execute("INSERT OR REPLACE INTO id_keys(provider, kid, pem, created) VALUES(?1,?2,?3,?4)", params![p, k, m, util::now_ms()])).await?;
    Ok(())
}

pub async fn key_created(ctx: &Ctx, provider: &str, kid: &str) -> Result<Option<i64>, ApiError> {
    let (p, k) = (provider.to_string(), kid.to_string());
    ctx.db.call(move |c| c.query_row("SELECT created FROM id_keys WHERE provider=?1 AND kid=?2", params![p, k], |r| r.get(0)).optional()).await
}

// ---------------------------------------------------------------- pending pages, codes, refresh tokens

pub async fn put(ctx: &Ctx, kind: &str, id: &str, provider: &str, data: &Value) -> Result<(), ApiError> {
    let (k, i, p, d) = (kind.to_string(), id.to_string(), provider.to_string(), data.to_string());
    ctx.db.call(move |c| c.execute("INSERT OR REPLACE INTO id_state(kind, id, provider, at, data, used) VALUES(?1,?2,?3,?4,?5,0)", params![k, i, p, util::now_ms(), d])).await?;
    if kind == "pending" { ctx.live.send("identity.pending", id, "changed"); }
    Ok(())
}

/// (data, at, used)
pub async fn get(ctx: &Ctx, kind: &str, id: &str) -> Result<Option<(Value, i64, bool)>, ApiError> {
    let (k, i) = (kind.to_string(), id.to_string());
    let r: Option<(String, i64, i64)> = ctx.db.call(move |c| c.query_row("SELECT data, at, used FROM id_state WHERE kind=?1 AND id=?2", params![k, i], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()).await?;
    Ok(r.map(|(d, at, u)| (serde_json::from_str(&d).unwrap_or(Value::Null), at, u != 0)))
}

pub async fn update(ctx: &Ctx, kind: &str, id: &str, data: &Value) -> Result<(), ApiError> {
    let (k, i, d) = (kind.to_string(), id.to_string(), data.to_string());
    ctx.db.call(move |c| c.execute("UPDATE id_state SET data=?3 WHERE kind=?1 AND id=?2", params![k, i, d])).await?;
    if kind == "pending" { ctx.live.send("identity.pending", id, "changed"); }
    Ok(())
}

/// Marks as used; false when it already was (a code or refresh token redeemed twice).
pub async fn use_once(ctx: &Ctx, kind: &str, id: &str) -> Result<bool, ApiError> {
    let (k, i) = (kind.to_string(), id.to_string());
    let n = ctx.db.call(move |c| c.execute("UPDATE id_state SET used=1 WHERE kind=?1 AND id=?2 AND used=0", params![k, i])).await?;
    if kind == "pending" { ctx.live.send("identity.pending", id, "done"); }
    Ok(n == 1)
}

/// Login pages still waiting for a person (last 30 min), newest first.
pub async fn pending(ctx: &Ctx, provider: Option<String>) -> Result<Vec<Value>, ApiError> {
    let since = util::now_ms() - 1_800_000;
    let rows: Vec<(String, String, i64, String)> = ctx.db.call(move |c| c.prepare("SELECT id, provider, at, data FROM id_state WHERE kind='pending' AND used=0 AND at>=?1 ORDER BY at DESC")?
        .query_map(params![since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect()).await?;
    Ok(rows.into_iter().filter(|r| provider.as_deref().map(|p| p == r.1).unwrap_or(true)).map(|(id, p, at, d)| {
        let mut v: Value = serde_json::from_str(&d).unwrap_or(json!({}));
        v["id"] = json!(id); v["provider"] = json!(p); v["at"] = json!(at);
        v
    }).collect())
}

/// Refresh tokens of a user for an app (to stop them when consent is revoked or the user disabled).
pub async fn drop_refresh(ctx: &Ctx, provider: &str, user: &str, app: Option<&str>) -> Result<usize, ApiError> {
    let (p, u, a) = (provider.to_string(), user.to_string(), app.map(String::from));
    ctx.db.call(move |c| {
        let rows: Vec<(String, String)> = c.prepare("SELECT id, data FROM id_state WHERE kind='refresh' AND provider=?1 AND used=0")?.query_map(params![p], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        let mut n = 0;
        for (id, d) in rows {
            let v: Value = serde_json::from_str(&d).unwrap_or(Value::Null);
            if v["user"] == json!(u) && a.as_ref().map(|a| v["client_id"] == json!(a)).unwrap_or(true) {
                n += c.execute("UPDATE id_state SET used=1 WHERE kind='refresh' AND id=?1", params![id])?;
            }
        }
        Ok(n)
    }).await
}

// ---------------------------------------------------------------- next sign-ins (I1)

pub async fn provoke(ctx: &Ctx, provider: &str, answer: &str, until: Option<i64>, count: Option<i64>, by: &str) -> Result<(), ApiError> {
    let (p, a, b) = (provider.to_string(), answer.to_string(), by.to_string());
    ctx.db.call(move |c| if a == "normal" { c.execute("DELETE FROM id_provoke WHERE provider=?1", params![p]) } else {
        c.execute("INSERT OR REPLACE INTO id_provoke(provider, answer, until, count, by) VALUES(?1,?2,?3,?4,?5)", params![p, a, until, count, b]) }).await?;
    ctx.live.send("identity.provoke", provider, answer);
    Ok(())
}

/// (answer, until, count left, by), if one is in force.
pub async fn provoked(ctx: &Ctx, provider: &str) -> Result<Option<(String, Option<i64>, Option<i64>, String)>, ApiError> {
    let p = provider.to_string();
    let r: Option<(String, Option<i64>, Option<i64>, String)> = ctx.db.call(move |c| c.query_row("SELECT answer, until, count, by FROM id_provoke WHERE provider=?1", params![p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()).await?;
    Ok(r.filter(|(_, u, n, _)| u.map(|u| u > util::now_ms()).unwrap_or(true) && n.map(|n| n > 0).unwrap_or(true)))
}

/// One sign-in used the provocation: count it down.
pub async fn provoke_used(ctx: &Ctx, provider: &str) -> Result<(), ApiError> {
    let p = provider.to_string();
    ctx.db.call(move |c| c.execute("UPDATE id_provoke SET count=count-1 WHERE provider=?1 AND count IS NOT NULL", params![p])).await?;
    Ok(())
}

// ---------------------------------------------------------------- sign-ins

#[derive(Default, Clone)]
pub struct SignIn {
    pub provider: String,
    pub user: String,
    pub app: String,
    /// sign-in | refresh | client-credentials
    pub kind: String,
    /// signed in | refused | cancelled | refreshed | issued
    pub result: String,
    /// AADSTS50020, access_denied…
    pub code: String,
    pub sentence: String,
    pub flow: String,
    pub steps: Vec<Value>,
    pub claims: Value,
    pub tokens: Value,
    pub by: String,
    pub pending: String,
}

pub async fn sign_in(ctx: &Ctx, s: SignIn) -> Result<i64, ApiError> {
    let t = (s.provider.clone(), s.app.clone(), s.result.clone(), s.code.clone(), s.sentence.clone(), s.by.clone());
    let id = ctx.db.call(move |c| {
        c.execute("INSERT INTO id_signins(at, provider, user, app, kind, result, code, sentence, flow, steps, claims, tokens, by, pending) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![util::now_ms(), s.provider, s.user, s.app, if s.kind.is_empty() { "sign-in".to_string() } else { s.kind }, s.result, s.code, s.sentence, s.flow,
                json!(s.steps).to_string(), s.claims.to_string(), s.tokens.to_string(), s.by, s.pending])?;
        Ok(c.last_insert_rowid())
    }).await?;
    ctx.live.send("identity.signins", &id.to_string(), "new");
    let (provider, app, result, code, sentence, by) = t;
    let outcome = if result.contains("provoked") { "provoked" } else if code.is_empty() { "ok" } else { "failed" };
    let actor = if by.is_empty() || by == "you" { if by == "you" { "you".to_string() } else { app.clone() } } else { by.clone() };
    let _ = stuntman_core::trips::record(ctx, stuntman_core::trips::Trip { id: &format!("id:{id}"), module: "identity", label: &sentence, status: &result, trace_id: None,
        link: &format!("/identity?provider={provider}&id={id}"), host: &provider, actor: &actor, outcome, detail: &if code.is_empty() { result.clone() } else { format!("{code}  ·  {result}") } }).await;
    Ok(id)
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let parse = |s: String| serde_json::from_str::<Value>(&s).unwrap_or(Value::Null);
    Ok(json!({"id": r.get::<_, i64>(0)?, "at": r.get::<_, i64>(1)?, "provider": r.get::<_, String>(2)?, "user": r.get::<_, String>(3)?, "app": r.get::<_, String>(4)?,
        "kind": r.get::<_, String>(5)?, "result": r.get::<_, String>(6)?, "code": r.get::<_, String>(7)?, "sentence": r.get::<_, String>(8)?, "flow": r.get::<_, String>(9)?,
        "steps": parse(r.get(10)?), "claims": parse(r.get(11)?), "tokens": parse(r.get(12)?), "by": r.get::<_, String>(13)?, "pending": r.get::<_, String>(14)?}))
}
const COLS: &str = "id, at, provider, user, app, kind, result, code, sentence, flow, steps, claims, tokens, by, pending";

pub async fn sign_ins(ctx: &Ctx, provider: Option<String>, user: Option<String>, result: Option<String>, since: i64, limit: u32) -> Result<Vec<Value>, ApiError> {
    ctx.db.call(move |c| {
        let mut w = vec!["at>=?1".to_string()];
        let mut v: Vec<rusqlite::types::Value> = vec![since.into()];
        for (col, x) in [("provider", provider), ("user", user)] {
            if let Some(x) = x.filter(|x| !x.is_empty()) { w.push(format!("{col}=?{}", v.len() + 1)); v.push(x.into()); }
        }
        match result.as_deref() {
            Some("ok") => w.push("code=''".into()),
            Some("failed") => w.push("code<>''".into()),
            Some(r) if !r.is_empty() => { w.push(format!("(result=?{0} OR code=?{0})", v.len() + 1)); v.push(r.to_string().into()); }
            _ => {}
        }
        let sql = format!("SELECT {COLS} FROM id_signins WHERE {} ORDER BY id DESC LIMIT {limit}", w.join(" AND "));
        let rows = c.prepare(&sql)?.query_map(rusqlite::params_from_iter(v), row)?.collect::<Result<Vec<_>, _>>();
        rows
    }).await
}

pub async fn sign_in_by_id(ctx: &Ctx, id: i64) -> Result<Value, ApiError> {
    ctx.db.call(move |c| c.query_row(&format!("SELECT {COLS} FROM id_signins WHERE id=?1"), [id], row).optional()).await?
        .ok_or_else(|| ApiError::not_found(format!("No sign-in {id}")))
}

pub async fn counts(ctx: &Ctx, since: i64) -> Result<std::collections::HashMap<String, i64>, ApiError> {
    let rows: Vec<(String, i64)> = ctx.db.call(move |c| c.prepare("SELECT provider, count(*) FROM id_signins WHERE at>=?1 GROUP BY provider")?.query_map(params![since], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()).await?;
    Ok(rows.into_iter().collect())
}

/// Adds to a sign-in after the fact (the app redeeming its code: a step and the tokens).
pub async fn extend_sign_in(ctx: &Ctx, id: i64, step: Value, claims: Option<Value>, tokens: Value) -> Result<(), ApiError> {
    ctx.db.call(move |c| {
        let (steps, cl): (String, String) = c.query_row("SELECT steps, claims FROM id_signins WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut s: Vec<Value> = serde_json::from_str(&steps).unwrap_or_default();
        s.push(step);
        let cl = claims.map(|x| x.to_string()).unwrap_or(cl);
        c.execute("UPDATE id_signins SET steps=?2, claims=?3, tokens=?4 WHERE id=?1", params![id, json!(s).to_string(), cl, tokens.to_string()])
    }).await?;
    ctx.live.send("identity.signins", &id.to_string(), "tokens");
    Ok(())
}
