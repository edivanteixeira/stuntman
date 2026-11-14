//! Trips: every call the app made to Stuntman that carried a trace id (an e-mail sent, a webhook
//! delivered…). Service modules record them; telemetry joins them to the app's spans, so a trace
//! shows where the app waited on a provider and a trip opens the trace that caused it.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::{util, ApiError, Ctx};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS trips(
  id TEXT PRIMARY KEY, module TEXT NOT NULL, label TEXT NOT NULL, status TEXT NOT NULL DEFAULT '',
  trace_id TEXT, at INTEGER NOT NULL, link TEXT NOT NULL DEFAULT '', host TEXT NOT NULL DEFAULT '', rule TEXT);
CREATE INDEX IF NOT EXISTS trips_trace ON trips(trace_id);
CREATE INDEX IF NOT EXISTS trips_at ON trips(at);
CREATE TABLE IF NOT EXISTS beats(start INTEGER PRIMARY KEY, last INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS forced_sends(module TEXT PRIMARY KEY, answer TEXT NOT NULL, left INTEGER NOT NULL, by_who TEXT NOT NULL DEFAULT '', at INTEGER NOT NULL);
";

pub struct Trip<'a> {
    pub id: &'a str,
    /// "email", "webhook"… — also the colour of the provider line on traces.
    pub module: &'a str,
    pub label: &'a str,
    pub status: &'a str,
    pub trace_id: Option<&'a str>,
    /// Where the trip opens on screen, e.g. "/notifications/msg_0412".
    pub link: &'a str,
    /// The host the app called, as a span would show it ("sendgrid", "smtp", "hooks").
    pub host: &'a str,
    /// Who did it: the app (its client name), "you", "mcp:<agent>", "api:<test>".
    pub actor: &'a str,
    /// ok | failed | provoked (empty: ok)
    pub outcome: &'a str,
    /// The second line under the sentence.
    pub detail: &'a str,
}

pub async fn record(ctx: &Ctx, t: Trip<'_>) -> Result<(), ApiError> {
    let (id, m, l, s, tr, link, h) = (t.id.to_string(), t.module.to_string(), t.label.to_string(), t.status.to_string(), t.trace_id.map(|x| x.to_lowercase()), t.link.to_string(), t.host.to_string());
    let (a, o, d) = (t.actor.to_string(), if t.outcome.is_empty() { "ok".to_string() } else { t.outcome.to_string() }, t.detail.to_string());
    ctx.db
        .call(move |c| {
            c.execute(
                "INSERT INTO trips(id, module, label, status, trace_id, at, link, host, actor, outcome, detail) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                 ON CONFLICT(id) DO UPDATE SET label=?3, status=?4, trace_id=COALESCE(?5, trace_id), link=?7, host=?8,
                   actor=CASE WHEN ?9='' THEN actor ELSE ?9 END, outcome=?10, detail=CASE WHEN ?11='' THEN detail ELSE ?11 END",
                params![id, m, l, s, tr, util::now_ms(), link, h, a, o, d],
            )
        })
        .await?;
    ctx.live.send("timeline", &t.id.to_string(), "row");
    Ok(())
}

/// Names the rule that shaped the trip ("Contract uploads fail twice", or "bounces@… · bounce").
pub async fn set_rule(ctx: &Ctx, id: &str, rule: &str) -> Result<(), ApiError> {
    let (i, r) = (id.to_string(), rule.to_string());
    ctx.db.call(move |c| c.execute("UPDATE trips SET rule = ?2 WHERE id = ?1", params![i, r])).await?;
    Ok(())
}

pub async fn set_status(ctx: &Ctx, id: &str, status: &str) -> Result<(), ApiError> {
    let (i, s) = (id.to_string(), status.to_string());
    ctx.db.call(move |c| c.execute("UPDATE trips SET status = ?2 WHERE id = ?1", params![i, s])).await?;
    Ok(())
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({"id": r.get::<_, String>(0)?, "module": r.get::<_, String>(1)?, "label": r.get::<_, String>(2)?, "status": r.get::<_, String>(3)?,
              "traceId": r.get::<_, Option<String>>(4)?, "at": r.get::<_, i64>(5)?, "link": r.get::<_, String>(6)?, "host": r.get::<_, String>(7)?,
              "rule": r.get::<_, Option<String>>(8)?}))
}

pub async fn of_trace(ctx: &Ctx, trace_id: &str) -> Result<Vec<Value>, ApiError> {
    let t = trace_id.to_lowercase();
    ctx.db
        .call(move |c| {
            let mut st = c.prepare("SELECT id, module, label, status, trace_id, at, link, host, rule FROM trips WHERE trace_id = ?1 ORDER BY at")?;
            let v = st.query_map([t], row)?.collect::<rusqlite::Result<Vec<_>>>();
            v
        })
        .await
}

pub async fn get(ctx: &Ctx, id: &str) -> Result<Option<Value>, ApiError> {
    let i = id.to_string();
    ctx.db.call(move |c| c.query_row("SELECT id, module, label, status, trace_id, at, link, host, rule FROM trips WHERE id = ?1", [i], row).optional()).await
}

/// Trips in a time window, for "what Stuntman answered while a metric jumped".
pub async fn between(ctx: &Ctx, from_ms: i64, to_ms: i64) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |c| {
            let mut st = c.prepare("SELECT id, module, label, status, trace_id, at, link, host, rule FROM trips WHERE at BETWEEN ?1 AND ?2 ORDER BY at")?;
            let v = st.query_map(params![from_ms, to_ms], row)?.collect::<rusqlite::Result<Vec<_>>>();
            v
        })
        .await
}

/// Older data folders: add the columns that came later.
pub fn upgrade(db: &crate::store::Db) {
    let _ = db.migrate("ALTER TABLE trips ADD COLUMN rule TEXT");
    let _ = db.migrate("ALTER TABLE trips ADD COLUMN actor TEXT NOT NULL DEFAULT ''");
    let _ = db.migrate("ALTER TABLE trips ADD COLUMN outcome TEXT NOT NULL DEFAULT 'ok'");
    let _ = db.migrate("ALTER TABLE trips ADD COLUMN detail TEXT NOT NULL DEFAULT ''");
}

/// Sets the outcome later (a failure found after the row was written).
pub async fn set_outcome(ctx: &Ctx, id: &str, outcome: &str) -> Result<(), ApiError> {
    let (i, o) = (id.to_string(), outcome.to_string());
    ctx.db.call(move |c| c.execute("UPDATE trips SET outcome = ?2 WHERE id = ?1", params![i, o])).await?;
    ctx.live.send("timeline", id, "row");
    Ok(())
}

// ---------------------------------------------------------------- the Timeline (B1)

#[derive(Default)]
pub struct Filter {
    pub from_ms: i64,
    pub to_ms: i64,
    pub module: Option<String>,
    /// app | you | mcp | api (prefix match) or an exact actor
    pub actor: Option<String>,
    /// failed | provoked
    pub outcome: Option<String>,
    pub text: Option<String>,
    pub limit: u32,
}

fn trow(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({"id": r.get::<_, String>(0)?, "module": r.get::<_, String>(1)?, "label": r.get::<_, String>(2)?, "status": r.get::<_, String>(3)?,
              "traceId": r.get::<_, Option<String>>(4)?, "at": r.get::<_, i64>(5)?, "link": r.get::<_, String>(6)?, "host": r.get::<_, String>(7)?,
              "rule": r.get::<_, Option<String>>(8)?, "actor": r.get::<_, String>(9)?, "outcome": r.get::<_, String>(10)?, "detail": r.get::<_, String>(11)?}))
}
const TCOLS: &str = "id, module, label, status, trace_id, at, link, host, rule, actor, outcome, detail";

pub async fn list(ctx: &Ctx, f: Filter) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |c| {
            let mut w = vec!["at BETWEEN ?1 AND ?2".to_string()];
            let mut v: Vec<rusqlite::types::Value> = vec![f.from_ms.into(), f.to_ms.into()];
            if let Some(m) = f.module.filter(|x| !x.is_empty()) { w.push(format!("module = ?{}", v.len() + 1)); v.push(m.into()); }
            match f.actor.as_deref().filter(|x| !x.is_empty()) {
                Some("you") => w.push("actor = 'you'".into()),
                Some("mcp") => w.push("actor LIKE 'mcp:%'".into()),
                Some("api") => w.push("actor LIKE 'api:%'".into()),
                Some("app") => w.push("actor <> 'you' AND actor NOT LIKE 'mcp:%' AND actor NOT LIKE 'api:%'".into()),
                Some(a) => { w.push(format!("actor = ?{}", v.len() + 1)); v.push(a.to_string().into()); }
                None => {}
            }
            match f.outcome.as_deref() {
                Some("failed") => w.push("outcome IN ('failed', 'provoked')".into()),
                Some("provoked") => w.push("(outcome = 'provoked' OR rule IS NOT NULL)".into()),
                _ => {}
            }
            if let Some(q) = f.text.filter(|x| !x.is_empty()) { w.push(format!("(label LIKE ?{0} OR detail LIKE ?{0} OR actor LIKE ?{0} OR trace_id LIKE ?{0} OR id LIKE ?{0})", v.len() + 1)); v.push(format!("%{q}%").into()); }
            let sql = format!("SELECT {TCOLS} FROM trips WHERE {} ORDER BY at DESC LIMIT {}", w.join(" AND "), if f.limit == 0 { 500 } else { f.limit });
            let rows = c.prepare(&sql)?.query_map(rusqlite::params_from_iter(v), trow)?.collect::<rusqlite::Result<Vec<_>>>();
            rows
        })
        .await
}

pub async fn one(ctx: &Ctx, id: &str) -> Result<Option<Value>, ApiError> {
    let i = id.to_string();
    ctx.db.call(move |c| c.query_row(&format!("SELECT {TCOLS} FROM trips WHERE id = ?1"), [i], trow).optional()).await
}

pub async fn same_trace(ctx: &Ctx, trace_id: &str) -> Result<Vec<Value>, ApiError> {
    let t = trace_id.to_lowercase();
    ctx.db.call(move |c| { let mut st = c.prepare(&format!("SELECT {TCOLS} FROM trips WHERE trace_id = ?1 ORDER BY at"))?; let v = st.query_map([t], trow)?.collect::<rusqlite::Result<Vec<_>>>(); v }).await
}

/// Counts per module and per actor kind in a window (the Timeline's left column).
pub async fn counts(ctx: &Ctx, from_ms: i64, to_ms: i64) -> Result<Value, ApiError> {
    ctx.db.call(move |c| {
        let m: Vec<(String, i64)> = c.prepare("SELECT module, count(*) FROM trips WHERE at BETWEEN ?1 AND ?2 GROUP BY module")?.query_map(params![from_ms, to_ms], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        let a: Vec<(String, i64)> = c.prepare("SELECT actor, count(*) FROM trips WHERE at BETWEEN ?1 AND ?2 GROUP BY actor")?.query_map(params![from_ms, to_ms], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(json!({"modules": m.into_iter().collect::<std::collections::BTreeMap<_, _>>(), "actors": a.into_iter().collect::<std::collections::BTreeMap<_, _>>()}))
    }).await
}

/// Activity per module in buckets (the lanes at the top of the Timeline).
pub async fn lanes(ctx: &Ctx, from_ms: i64, to_ms: i64, buckets: i64) -> Result<Value, ApiError> {
    let size = ((to_ms - from_ms) / buckets.max(1)).max(1);
    ctx.db.call(move |c| {
        let rows: Vec<(String, i64, i64, i64, i64)> = c.prepare("SELECT module, (at - ?1) / ?3 b, count(*), sum(outcome = 'provoked' OR rule IS NOT NULL), sum(outcome = 'failed') FROM trips WHERE at BETWEEN ?1 AND ?2 GROUP BY module, b")?
            .query_map(params![from_ms, to_ms, size], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, Option<i64>>(3)?.unwrap_or(0), r.get::<_, Option<i64>>(4)?.unwrap_or(0))))?.collect::<rusqlite::Result<_>>()?;
        let mut out = serde_json::Map::new();
        let mut provoked = vec![];
        let mut failed = vec![];
        for (m, b, n, p, f) in rows {
            if p > 0 { provoked.push(json!({"bucket": b, "count": p, "module": m})); }
            if f > 0 { failed.push(json!({"bucket": b, "count": f, "module": m})); }
            let e = out.entry(m).or_insert(json!(vec![0i64; buckets as usize]));
            if let Some(slot) = e.get_mut(b as usize) { *slot = json!(n); }
        }
        Ok(json!({"bucketMs": size, "modules": out, "provoked": provoked, "failed": failed}))
    }).await
}

// ---------------------------------------------------------------- heartbeat (gaps)

/// While the process runs, one row per start with the last time it was seen; the time between a
/// run's last beat and the next start is when Stuntman was not receiving.
pub async fn beat(ctx: &Ctx, start: i64) -> Result<(), ApiError> {
    ctx.db.call(move |c| c.execute("INSERT INTO beats(start, last) VALUES(?1, ?2) ON CONFLICT(start) DO UPDATE SET last = ?2", params![start, util::now_ms()])).await?;
    Ok(())
}

/// Offline stretches overlapping a window: [{from, to}].
pub async fn gaps(ctx: &Ctx, from_ms: i64, to_ms: i64) -> Result<Vec<Value>, ApiError> {
    let rows: Vec<(i64, i64)> = ctx.db.call(|c| c.prepare("SELECT start, last FROM beats ORDER BY start")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()).await?;
    Ok(rows.windows(2).filter_map(|w| { let (a, b) = (w[0].1, w[1].0); (b - a > 6_000 && b >= from_ms && a <= to_ms).then(|| json!({"from": a, "to": b})) }).collect())
}
