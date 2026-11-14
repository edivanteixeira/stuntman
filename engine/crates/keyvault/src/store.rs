//! Items (secrets, keys, certificates) with their versions, soft-deleted names, provocations on
//! the next operations, and the access log (every operation: who, what, the answer).

use stuntman_core::{util, ApiError, Ctx};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS kv_versions(
  vault TEXT NOT NULL, kind TEXT NOT NULL, name TEXT NOT NULL, id TEXT NOT NULL, created INTEGER NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1, nbf INTEGER, exp INTEGER, value TEXT NOT NULL DEFAULT '', content_type TEXT NOT NULL DEFAULT '',
  tags TEXT NOT NULL DEFAULT '{}', extra TEXT NOT NULL DEFAULT '{}', PRIMARY KEY(vault, kind, name, id));
CREATE TABLE IF NOT EXISTS kv_deleted(vault TEXT NOT NULL, kind TEXT NOT NULL, name TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(vault, kind, name));
CREATE TABLE IF NOT EXISTS kv_provoke(vault TEXT NOT NULL, kind TEXT NOT NULL, name TEXT NOT NULL, answer TEXT NOT NULL, until INTEGER, by TEXT NOT NULL DEFAULT '', PRIMARY KEY(vault, kind, name));
CREATE TABLE IF NOT EXISTS kv_log(
  id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, vault TEXT NOT NULL, identity TEXT NOT NULL, oid TEXT NOT NULL DEFAULT '', role TEXT NOT NULL DEFAULT '',
  op TEXT NOT NULL, kind TEXT NOT NULL DEFAULT '', item TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '', status INTEGER NOT NULL, ms INTEGER NOT NULL DEFAULT 0,
  client TEXT NOT NULL DEFAULT '', trace_id TEXT, request TEXT NOT NULL DEFAULT '', why TEXT NOT NULL DEFAULT '', would_fail TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS kv_log_at ON kv_log(vault, at);
CREATE TABLE IF NOT EXISTS kv_events(vault TEXT NOT NULL, kind TEXT NOT NULL, name TEXT NOT NULL, event TEXT NOT NULL, version TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(vault, kind, name, event, version));
";

pub fn new_version_id() -> String {
    util::uuid().replace('-', "")
}

#[derive(Clone, Default)]
pub struct Version {
    pub id: String,
    pub created: i64,
    pub enabled: bool,
    pub nbf: Option<i64>,
    pub exp: Option<i64>,
    pub value: String,
    pub content_type: String,
    pub tags: Value,
    pub extra: Value,
}

fn ver(r: &rusqlite::Row) -> rusqlite::Result<Version> {
    Ok(Version { id: r.get(0)?, created: r.get(1)?, enabled: r.get::<_, i64>(2)? == 1, nbf: r.get(3)?, exp: r.get(4)?, value: r.get(5)?, content_type: r.get(6)?,
        tags: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or(json!({})), extra: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or(json!({})) })
}
const VCOLS: &str = "id, created, enabled, nbf, exp, value, content_type, tags, extra";

pub async fn add(ctx: &Ctx, vault: &str, kind: &str, name: &str, v: Version) -> Result<Version, ApiError> {
    let (a, k, n, v2) = (vault.to_string(), kind.to_string(), name.to_string(), v.clone());
    ctx.db.call(move |c| {
        c.execute("DELETE FROM kv_deleted WHERE vault=?1 AND kind=?2 AND name=?3", params![a, k, n])?;
        c.execute(&format!("INSERT INTO kv_versions(vault, kind, name, {VCOLS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)"),
            params![a, k, n, v2.id, v2.created, v2.enabled as i64, v2.nbf, v2.exp, v2.value, v2.content_type, v2.tags.to_string(), v2.extra.to_string()])
    }).await?;
    ctx.live.send("keyvault.items", name, "version");
    Ok(v)
}

pub async fn versions(ctx: &Ctx, vault: &str, kind: &str, name: &str) -> Result<Vec<Version>, ApiError> {
    let (a, k, n) = (vault.to_string(), kind.to_string(), name.to_string());
    ctx.db.call(move |c| c.prepare(&format!("SELECT {VCOLS} FROM kv_versions WHERE vault=?1 AND kind=?2 AND name=?3 ORDER BY created DESC, rowid DESC"))?
        .query_map(params![a, k, n], ver)?.collect()).await
}

pub async fn deleted_at(ctx: &Ctx, vault: &str, kind: &str, name: &str) -> Result<Option<i64>, ApiError> {
    let (a, k, n) = (vault.to_string(), kind.to_string(), name.to_string());
    ctx.db.call(move |c| c.query_row("SELECT at FROM kv_deleted WHERE vault=?1 AND kind=?2 AND name=?3", params![a, k, n], |r| r.get(0)).optional()).await
}

/// Current version (the newest), unless deleted.
pub async fn current(ctx: &Ctx, vault: &str, kind: &str, name: &str) -> Result<Option<Version>, ApiError> {
    if deleted_at(ctx, vault, kind, name).await?.is_some() { return Ok(None); }
    Ok(versions(ctx, vault, kind, name).await?.into_iter().next())
}

pub async fn one(ctx: &Ctx, vault: &str, kind: &str, name: &str, version: Option<&str>) -> Result<Option<Version>, ApiError> {
    match version.filter(|v| !v.is_empty()) {
        None => current(ctx, vault, kind, name).await,
        Some(id) => {
            if deleted_at(ctx, vault, kind, name).await?.is_some() { return Ok(None); }
            Ok(versions(ctx, vault, kind, name).await?.into_iter().find(|v| v.id == id))
        }
    }
}

pub async fn update(ctx: &Ctx, vault: &str, kind: &str, name: &str, id: &str, enabled: Option<bool>, nbf: Option<Option<i64>>, exp: Option<Option<i64>>, content_type: Option<String>, tags: Option<Value>) -> Result<(), ApiError> {
    let (a, k, n, i) = (vault.to_string(), kind.to_string(), name.to_string(), id.to_string());
    ctx.db.call(move |c| {
        if let Some(e) = enabled { c.execute("UPDATE kv_versions SET enabled=?5 WHERE vault=?1 AND kind=?2 AND name=?3 AND id=?4", params![a, k, n, i, e as i64])?; }
        if let Some(x) = nbf { c.execute("UPDATE kv_versions SET nbf=?5 WHERE vault=?1 AND kind=?2 AND name=?3 AND id=?4", params![a, k, n, i, x])?; }
        if let Some(x) = exp { c.execute("UPDATE kv_versions SET exp=?5 WHERE vault=?1 AND kind=?2 AND name=?3 AND id=?4", params![a, k, n, i, x])?; }
        if let Some(x) = content_type { c.execute("UPDATE kv_versions SET content_type=?5 WHERE vault=?1 AND kind=?2 AND name=?3 AND id=?4", params![a, k, n, i, x])?; }
        if let Some(x) = tags { c.execute("UPDATE kv_versions SET tags=?5 WHERE vault=?1 AND kind=?2 AND name=?3 AND id=?4", params![a, k, n, i, x.to_string()])?; }
        Ok(())
    }).await?;
    ctx.live.send("keyvault.items", name, "updated");
    Ok(())
}

/// Names of a kind in a vault (not deleted), with their current version.
pub async fn names(ctx: &Ctx, vault: &str, kind: &str) -> Result<Vec<String>, ApiError> {
    let (a, k) = (vault.to_string(), kind.to_string());
    ctx.db.call(move |c| c.prepare("SELECT DISTINCT name FROM kv_versions v WHERE vault=?1 AND kind=?2 AND NOT EXISTS (SELECT 1 FROM kv_deleted d WHERE d.vault=v.vault AND d.kind=v.kind AND d.name=v.name) ORDER BY name")?
        .query_map(params![a, k], |r| r.get(0))?.collect()).await
}

pub async fn deleted(ctx: &Ctx, vault: &str, kind: &str) -> Result<Vec<(String, i64)>, ApiError> {
    let (a, k) = (vault.to_string(), kind.to_string());
    ctx.db.call(move |c| c.prepare("SELECT name, at FROM kv_deleted WHERE vault=?1 AND kind=?2 ORDER BY at DESC")?.query_map(params![a, k], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()).await
}

pub async fn soft_delete(ctx: &Ctx, vault: &str, kind: &str, name: &str) -> Result<(), ApiError> {
    let (a, k, n) = (vault.to_string(), kind.to_string(), name.to_string());
    ctx.db.call(move |c| c.execute("INSERT OR REPLACE INTO kv_deleted(vault, kind, name, at) VALUES(?1,?2,?3,?4)", params![a, k, n, util::now_ms()])).await?;
    ctx.live.send("keyvault.items", name, "deleted");
    Ok(())
}

pub async fn recover(ctx: &Ctx, vault: &str, kind: &str, name: &str) -> Result<bool, ApiError> {
    let (a, k, n) = (vault.to_string(), kind.to_string(), name.to_string());
    let changed = ctx.db.call(move |c| c.execute("DELETE FROM kv_deleted WHERE vault=?1 AND kind=?2 AND name=?3", params![a, k, n])).await?;
    ctx.live.send("keyvault.items", name, "recovered");
    Ok(changed > 0)
}

pub async fn purge(ctx: &Ctx, vault: &str, kind: &str, name: &str) -> Result<(), ApiError> {
    let (a, k, n) = (vault.to_string(), kind.to_string(), name.to_string());
    ctx.db.call(move |c| {
        c.execute("DELETE FROM kv_deleted WHERE vault=?1 AND kind=?2 AND name=?3", params![a, k, n])?;
        c.execute("DELETE FROM kv_versions WHERE vault=?1 AND kind=?2 AND name=?3", params![a, k, n])
    }).await?;
    ctx.live.send("keyvault.items", name, "purged");
    Ok(())
}

// ---------------------------------------------------------------- provocations

pub async fn provoke(ctx: &Ctx, vault: &str, kind: &str, name: &str, answer: &str, until: Option<i64>, by: &str) -> Result<(), ApiError> {
    let (a, k, n, ans, b) = (vault.to_string(), kind.to_string(), name.to_string(), answer.to_string(), by.to_string());
    ctx.db.call(move |c| if ans == "normal" { c.execute("DELETE FROM kv_provoke WHERE vault=?1 AND kind=?2 AND name=?3", params![a, k, n]) } else {
        c.execute("INSERT OR REPLACE INTO kv_provoke(vault, kind, name, answer, until, by) VALUES(?1,?2,?3,?4,?5,?6)", params![a, k, n, ans, until, b]) }).await?;
    ctx.live.send("keyvault.items", name, "provoked");
    Ok(())
}

pub async fn provoked(ctx: &Ctx, vault: &str, kind: &str, name: &str) -> Result<Option<(String, Option<i64>, String)>, ApiError> {
    let (a, k, n) = (vault.to_string(), kind.to_string(), name.to_string());
    let r: Option<(String, Option<i64>, String)> = ctx.db.call(move |c| c.query_row("SELECT answer, until, by FROM kv_provoke WHERE vault=?1 AND kind=?2 AND name=?3", params![a, k, n], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()).await?;
    Ok(r.filter(|(_, u, _)| u.map(|u| u > util::now_ms()).unwrap_or(true)))
}

// ---------------------------------------------------------------- access log

#[derive(Default, Clone)]
pub struct Entry {
    pub vault: String,
    pub identity: String,
    pub oid: String,
    pub role: String,
    pub op: String,
    pub kind: String,
    pub item: String,
    pub version: String,
    pub status: u16,
    pub ms: i64,
    pub client: String,
    pub trace_id: Option<String>,
    pub request: String,
    pub why: String,
    /// Allowed only because "any token" is on: what a real vault would have answered.
    pub would_fail: String,
}

pub async fn log(ctx: &Ctx, e: Entry) -> Result<i64, ApiError> {
    let t = (e.vault.clone(), e.identity.clone(), e.op.clone(), e.item.clone(), e.status, e.why.clone(), e.would_fail.clone(), e.trace_id.clone());
    let id = ctx.db.call(move |c| {
        c.execute("INSERT INTO kv_log(at, vault, identity, oid, role, op, kind, item, version, status, ms, client, trace_id, request, why, would_fail) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![util::now_ms(), e.vault, e.identity, e.oid, e.role, e.op, e.kind, e.item, e.version, e.status, e.ms, e.client, e.trace_id, e.request, e.why, e.would_fail])?;
        Ok(c.last_insert_rowid())
    }).await?;
    ctx.live.send("keyvault.log", &id.to_string(), "op");
    timeline(ctx, id, t).await;
    Ok(id)
}

fn entry(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({"id": r.get::<_, i64>(0)?, "at": r.get::<_, i64>(1)?, "vault": r.get::<_, String>(2)?, "identity": r.get::<_, String>(3)?, "oid": r.get::<_, String>(4)?,
        "role": r.get::<_, String>(5)?, "op": r.get::<_, String>(6)?, "kind": r.get::<_, String>(7)?, "item": r.get::<_, String>(8)?, "version": r.get::<_, String>(9)?,
        "status": r.get::<_, i64>(10)?, "ms": r.get::<_, i64>(11)?, "client": r.get::<_, String>(12)?, "traceId": r.get::<_, Option<String>>(13)?,
        "request": r.get::<_, String>(14)?, "why": r.get::<_, String>(15)?, "wouldFail": r.get::<_, String>(16)?}))
}
const LCOLS: &str = "id, at, vault, identity, oid, role, op, kind, item, version, status, ms, client, trace_id, request, why, would_fail";

#[derive(Default, Clone)]
pub struct Filter {
    pub vault: Option<String>,
    pub identity: Option<String>,
    pub op: Option<String>,
    pub item: Option<String>,
    pub result: Option<String>,
    pub since_ms: Option<i64>,
    pub limit: u32,
}

pub async fn entries(ctx: &Ctx, f: Filter) -> Result<Vec<Value>, ApiError> {
    ctx.db.call(move |c| {
        let mut w = vec!["1=1".to_string()];
        let mut v: Vec<rusqlite::types::Value> = vec![];
        let mut add = |col: &str, x: Option<String>| if let Some(x) = x.filter(|x| !x.is_empty() && x != "all") { w.push(format!("{col}=?{}", v.len() + 1)); v.push(x.into()); };
        add("vault", f.vault);
        add("identity", f.identity);
        add("op", f.op);
        add("item", f.item);
        match f.result.as_deref() {
            Some("ok") => w.push("status<400".into()),
            Some("refused") => w.push("status>=400".into()),
            Some("would_fail") => w.push("would_fail<>''".into()),
            Some(s) if s.parse::<i64>().is_ok() => w.push(format!("status={s}")),
            _ => {}
        }
        if let Some(s) = f.since_ms { w.push(format!("at>=?{}", v.len() + 1)); v.push(s.into()); }
        let sql = format!("SELECT {LCOLS} FROM kv_log WHERE {} ORDER BY id DESC LIMIT {}", w.join(" AND "), if f.limit == 0 { 500 } else { f.limit });
        let rows = c.prepare(&sql)?.query_map(rusqlite::params_from_iter(v), entry)?.collect::<Result<Vec<_>, _>>();
        rows
    }).await
}

/// Names the app asked for that the vault does not have (404s in the window), most asked first.
pub async fn asked_missing(ctx: &Ctx, vault: &str, kind: &str, since: i64) -> Result<Vec<Value>, ApiError> {
    let (a, k) = (vault.to_string(), kind.to_string());
    let rows: Vec<(String, i64, i64, String)> = ctx.db.call(move |c| c.prepare(
        "SELECT item, count(*), max(at), (SELECT identity FROM kv_log l2 WHERE l2.vault=l.vault AND l2.item=l.item AND l2.status=404 ORDER BY id DESC LIMIT 1)
         FROM kv_log l WHERE vault=?1 AND kind=?2 AND status=404 AND at>=?3 AND item<>'' GROUP BY item ORDER BY count(*) DESC LIMIT 20")?
        .query_map(params![a, k, since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect()).await?;
    let mut out = vec![];
    for (item, n, last, who) in rows {
        if current(ctx, vault, kind, &item).await?.is_none() { out.push(json!({"name": item, "count": n, "last": last, "identity": who})); }
    }
    Ok(out)
}

pub async fn entry_by_id(ctx: &Ctx, id: i64) -> Result<Value, ApiError> {
    ctx.db.call(move |c| c.query_row(&format!("SELECT {LCOLS} FROM kv_log WHERE id=?1"), [id], entry).optional()).await?
        .ok_or_else(|| ApiError::not_found(format!("No access log entry {id}")))
}

pub async fn ops_since(ctx: &Ctx, vault: &str, since: i64) -> Result<i64, ApiError> {
    let a = vault.to_string();
    ctx.db.call(move |c| c.query_row("SELECT count(*) FROM kv_log WHERE vault=?1 AND at>=?2 AND op NOT IN ('Show', 'Event', 'SetSecret', 'DeleteSecret', 'DeleteKey', 'DeleteCertificate', 'Throttled')", params![a, since], |r| r.get(0))).await
}

/// Who read an item in a window: per identity and client, the count and the gap between reads.
/// "2 ms", "40 s", "6 min", "2 h": a read gap as people say it.
fn human(ms: i64) -> String {
    match ms { m if m < 1000 => format!("{m} ms"), m if m < 60_000 => format!("{} s", m / 1000), m if m < 3_600_000 => format!("{} min", (m + 30_000) / 60_000), m => format!("{} h", (m + 1_800_000) / 3_600_000) }
}

pub async fn readers(ctx: &Ctx, vault: &str, item: &str, since: i64) -> Result<Vec<Value>, ApiError> {
    let (a, i) = (vault.to_string(), item.to_string());
    let rows: Vec<(String, String, i64, i64, i64)> = ctx.db.call(move |c| c.prepare(
        "SELECT identity, client, count(*), min(at), max(at) FROM kv_log WHERE vault=?1 AND item=?2 AND at>=?3 AND status<400 AND op NOT IN ('Show', 'Event', 'SetSecret', 'DeleteSecret', 'DeleteKey', 'DeleteCertificate', 'Throttled') GROUP BY identity, client ORDER BY count(*) DESC")?
        .query_map(params![a, i, since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect()).await?;
    Ok(rows.into_iter().map(|(id, client, n, first, last)| {
        let gap = if n > 1 { (last - first) / (n - 1) } else { 0 };
        let pattern = if n <= 2 { "at start-up only".to_string() } else if gap >= 60_000 { format!("about 1 every {} per process: cached", human(gap)) } else { format!("every {}: not cached", human(gap.max(1))) };
        json!({"identity": id, "client": client, "reads": n, "first": first, "last": last, "gapMs": gap, "pattern": pattern, "cached": n <= 2 || gap >= 60_000})
    }).collect())
}

pub async fn reads_by_item(ctx: &Ctx, vault: &str, since: i64) -> Result<std::collections::HashMap<String, i64>, ApiError> {
    let a = vault.to_string();
    let rows: Vec<(String, i64)> = ctx.db.call(move |c| c.prepare("SELECT item, count(*) FROM kv_log WHERE vault=?1 AND at>=?2 AND status<400 AND op NOT IN ('Show', 'Event', 'SetSecret', 'DeleteSecret', 'DeleteKey', 'DeleteCertificate', 'Throttled') GROUP BY item")?
        .query_map(params![a, since], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()).await?;
    Ok(rows.into_iter().collect())
}

pub async fn event_sent(ctx: &Ctx, vault: &str, kind: &str, name: &str, event: &str, version: &str) -> Result<bool, ApiError> {
    let (a, k, n, e, v) = (vault.to_string(), kind.to_string(), name.to_string(), event.to_string(), version.to_string());
    let changed = ctx.db.call(move |c| c.execute("INSERT OR IGNORE INTO kv_events(vault, kind, name, event, version, at) VALUES(?1,?2,?3,?4,?5,?6)", params![a, k, n, e, v, util::now_ms()])).await?;
    Ok(changed == 0)
}

pub async fn events_of(ctx: &Ctx, vault: &str, name: &str) -> Result<Vec<Value>, ApiError> {
    let (a, n) = (vault.to_string(), name.to_string());
    ctx.db.call(move |c| c.prepare("SELECT kind, event, version, at FROM kv_events WHERE vault=?1 AND name=?2 ORDER BY at DESC LIMIT 10")?
        .query_map(params![a, n], |r| Ok(json!({"kind": r.get::<_, String>(0)?, "event": r.get::<_, String>(1)?, "version": r.get::<_, String>(2)?, "at": r.get::<_, i64>(3)?})))?.collect()).await
}

/// The Timeline row: changes, refusals and "would fail" one by one; reads and crypto operations fold
/// per identity and minute ("acme-api read 4 secrets · 12 reads").
async fn timeline(ctx: &Ctx, id: i64, t: (String, String, String, String, u16, String, String, Option<String>)) {
    let (vault, identity, op, item, status, why, would_fail, trace) = t;
    if op == "Event" { return; }
    let link = format!("/secrets/log?vault={vault}");
    let provoked = why.starts_with("provoked");
    let reading = ["GetSecret", "GetKey", "GetCertificate", "Sign", "Verify", "WrapKey", "UnwrapKey", "Encrypt", "Decrypt", "GetSecrets", "GetKeys", "GetCertificates"].contains(&op.as_str());
    if reading && status < 400 && would_fail.is_empty() && !provoked {
        let minute = util::now_ms() / 60_000;
        let (v2, i2) = (vault.clone(), identity.clone());
        let (n, items, ops): (i64, i64, String) = ctx.db.call(move |c| c.query_row("SELECT count(*), count(DISTINCT item), group_concat(DISTINCT op) FROM kv_log WHERE vault=?1 AND identity=?2 AND status<400 AND at/60000=?3 AND op NOT IN ('Show','Event','SetSecret')",
            params![v2, i2, minute], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<String>>(2)?.unwrap_or_default())))).await.unwrap_or((1, 1, String::new()));
        // "read" only when it only read; a Sign or an UnwrapKey is named, never hidden in a fold.
        let only_reads = ops.split(',').all(|o| o.starts_with("Get"));
        let label = if n <= 1 { if op.starts_with("Get") { format!("{identity} read {item}") } else { format!("{identity}  ·  {op} {item}") } }
            else if only_reads { format!("{identity} read {items} item(s) in {vault}") } else { format!("{identity}  ·  {} on {items} item(s) in {vault}", ops.replace(',', ", ")) };
        let _ = stuntman_core::trips::record(ctx, stuntman_core::trips::Trip { id: &format!("kv:{vault}:{identity}:{minute}"), module: "keyvault", label: &label, status: "200", trace_id: trace.as_deref(),
            link: &link, host: &vault, actor: &identity, outcome: "ok", detail: &if n <= 1 { format!("{op}  ·  {vault}") } else { format!("folded  ·  {n} operations in 1 min") } }).await;
        return;
    }
    let label = if status >= 400 { format!("{identity}  ·  {op} {item}: {status}") } else if op == "Show" { format!("{identity} revealed {item}") } else { format!("{identity}  ·  {op} {item}") };
    let outcome = if provoked { "provoked" } else if status >= 400 || !would_fail.is_empty() { "failed" } else { "ok" };
    let detail = if !would_fail.is_empty() { format!("allowed only because any token is on: a real vault answers {would_fail}") } else if why.is_empty() { vault.clone() } else { why.clone() };
    let _ = stuntman_core::trips::record(ctx, stuntman_core::trips::Trip { id: &format!("kv:{id}"), module: "keyvault", label: &label, status: &status.to_string(), trace_id: trace.as_deref(),
        link: &link, host: &vault, actor: &identity, outcome, detail: &detail }).await;
}
