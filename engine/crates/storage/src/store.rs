//! Objects, versions, every request (the History and the rules' "Try it"), next reads, account
//! reachability, links, events and multipart uploads. One SQLite file per environment, shared
//! with the other services.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use stuntman_core::{util, ApiError, Ctx};
use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::Digest;

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS storage_blobs(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, account TEXT NOT NULL, container TEXT NOT NULL, path TEXT NOT NULL,
  size INTEGER NOT NULL, ctype TEXT NOT NULL DEFAULT '', md5 TEXT NOT NULL DEFAULT '', etag TEXT NOT NULL,
  meta TEXT NOT NULL DEFAULT '{}', headers TEXT NOT NULL DEFAULT '{}', data BLOB NOT NULL, at INTEGER NOT NULL,
  by_who TEXT NOT NULL, via TEXT NOT NULL DEFAULT '', current INTEGER NOT NULL DEFAULT 1, deleted_at INTEGER,
  truncated INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS storage_blobs_cur ON storage_blobs(account, container, current, path);
CREATE TABLE IF NOT EXISTS storage_blocks(account TEXT NOT NULL, container TEXT NOT NULL, path TEXT NOT NULL, block_id TEXT NOT NULL,
  data BLOB NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(account, container, path, block_id));
CREATE TABLE IF NOT EXISTS storage_uploads(id TEXT PRIMARY KEY, account TEXT NOT NULL, container TEXT NOT NULL, path TEXT NOT NULL,
  ctype TEXT NOT NULL DEFAULT '', meta TEXT NOT NULL DEFAULT '{}', at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS storage_parts(upload_id TEXT NOT NULL, n INTEGER NOT NULL, data BLOB NOT NULL, etag TEXT NOT NULL, PRIMARY KEY(upload_id, n));
CREATE TABLE IF NOT EXISTS storage_requests(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, account TEXT NOT NULL, container TEXT NOT NULL DEFAULT '',
  path TEXT NOT NULL DEFAULT '', op TEXT NOT NULL, method TEXT NOT NULL, status INTEGER NOT NULL, caller TEXT NOT NULL DEFAULT '',
  via TEXT NOT NULL DEFAULT '', link_id TEXT, provoked TEXT, ms INTEGER, size INTEGER, ctype TEXT NOT NULL DEFAULT '',
  remote TEXT NOT NULL DEFAULT '', detail TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS storage_requests_obj ON storage_requests(account, container, path);
CREATE TABLE IF NOT EXISTS storage_next(account TEXT NOT NULL, container TEXT NOT NULL, path TEXT NOT NULL, result TEXT NOT NULL,
  times INTEGER NOT NULL, by_who TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(account, container, path));
CREATE TABLE IF NOT EXISTS storage_reach(account TEXT PRIMARY KEY, mode TEXT NOT NULL, since INTEGER NOT NULL, by_who TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS storage_links(
  id TEXT PRIMARY KEY, account TEXT NOT NULL, container TEXT NOT NULL, path TEXT NOT NULL, kind TEXT NOT NULL, allows TEXT NOT NULL,
  start INTEGER, expiry INTEGER NOT NULL, ips TEXT NOT NULL DEFAULT '', protocol TEXT NOT NULL DEFAULT '', signed_with TEXT NOT NULL DEFAULT '',
  issued_by TEXT NOT NULL, issued_at INTEGER NOT NULL, url TEXT NOT NULL, sig TEXT UNIQUE, revoked_at INTEGER, revoked_by TEXT,
  next_use TEXT, next_times INTEGER NOT NULL DEFAULT 0, removed INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS storage_events(
  id TEXT PRIMARY KEY, sub TEXT NOT NULL, type TEXT NOT NULL, account TEXT NOT NULL, container TEXT NOT NULL, path TEXT NOT NULL,
  payload TEXT NOT NULL, at INTEGER NOT NULL, by_who TEXT NOT NULL, cause INTEGER, status TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
  next_at INTEGER, last_status INTEGER, last_ms INTEGER, history TEXT NOT NULL DEFAULT '[]', behaviour TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS storage_events_due ON storage_events(status, next_at);
CREATE TABLE IF NOT EXISTS storage_subs(name TEXT PRIMARY KEY, validated INTEGER NOT NULL DEFAULT 0, error TEXT NOT NULL DEFAULT '',
  next_delivery TEXT NOT NULL DEFAULT '', at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS storage_rule_hits(service TEXT NOT NULL, name TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS storage_rule_counts(service TEXT NOT NULL, name TEXT NOT NULL, n INTEGER NOT NULL, PRIMARY KEY(service, name));
CREATE TABLE IF NOT EXISTS storage_seed(account TEXT PRIMARY KEY, loaded_at INTEGER NOT NULL, files INTEGER NOT NULL);
";

#[derive(Clone, Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Blob {
    pub seq: i64,
    pub account: String,
    pub container: String,
    pub path: String,
    pub size: i64,
    pub ctype: String,
    /// base64 MD5 (Content-MD5)
    pub md5: String,
    pub etag: String,
    pub meta: BTreeMap<String, String>,
    /// Content-Encoding, Cache-Control… as the app sent them.
    pub headers: BTreeMap<String, String>,
    pub at: i64,
    pub by: String,
    pub via: String,
    pub truncated: bool,
    #[serde(skip)]
    pub data: Vec<u8>,
}

const COLS: &str = "seq, account, container, path, size, ctype, md5, etag, meta, headers, at, by_who, via, truncated";

fn blob(r: &Row, with_data: bool) -> rusqlite::Result<Blob> {
    Ok(Blob {
        seq: r.get(0)?,
        account: r.get(1)?,
        container: r.get(2)?,
        path: r.get(3)?,
        size: r.get(4)?,
        ctype: r.get(5)?,
        md5: r.get(6)?,
        etag: r.get(7)?,
        meta: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
        headers: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
        at: r.get(10)?,
        by: r.get(11)?,
        via: r.get(12)?,
        truncated: r.get::<_, i64>(13)? != 0,
        data: if with_data { r.get(14)? } else { vec![] },
    })
}

pub fn md5_b64(d: &[u8]) -> String {
    B64.encode(md5::compute(d).0)
}

pub fn md5_hex(d: &[u8]) -> String {
    format!("{:x}", md5::compute(d))
}

pub fn sha256_hex(d: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(d))
}

pub struct Put {
    pub account: String,
    pub container: String,
    pub path: String,
    pub data: Vec<u8>,
    pub ctype: String,
    pub meta: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    pub by: String,
    pub via: String,
    /// azure-blob | s3: shapes the ETag.
    pub kind: String,
    pub versions: bool,
    pub truncated: bool,
    /// S3 multipart ETag ("md5-of-md5s-N") when set.
    pub etag: Option<String>,
}

pub async fn put(ctx: &Ctx, p: Put) -> Result<Blob, ApiError> {
    let md5 = md5_b64(&p.data);
    let now = util::now_ms();
    let etag = p.etag.clone().unwrap_or_else(|| if p.kind == crate::config::S3 { format!("\"{}\"", md5_hex(&p.data)) } else { format!("\"0x8DD{:013X}\"", (now as u64) * 10_000 + rand::random::<u16>() as u64 % 10_000) });
    let size = p.data.len() as i64;
    let seq = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if p.versions {
                tx.execute("UPDATE storage_blobs SET current=0 WHERE account=?1 AND container=?2 AND path=?3 AND current=1", params![p.account, p.container, p.path])?;
            } else {
                tx.execute("DELETE FROM storage_blobs WHERE account=?1 AND container=?2 AND path=?3 AND deleted_at IS NULL", params![p.account, p.container, p.path])?;
            }
            tx.execute(
                "INSERT INTO storage_blobs(account, container, path, size, ctype, md5, etag, meta, headers, data, at, by_who, via, truncated) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![p.account, p.container, p.path, size, p.ctype, md5, etag, json!(p.meta).to_string(), json!(p.headers).to_string(), p.data, now, p.by, p.via, p.truncated as i64],
            )?;
            let seq = tx.last_insert_rowid();
            tx.commit()?;
            Ok(seq)
        })
        .await?;
    get_seq(ctx, seq).await?.ok_or_else(|| ApiError::internal("put lost"))
}

async fn get_seq(ctx: &Ctx, seq: i64) -> Result<Option<Blob>, ApiError> {
    ctx.db.call(move |c| c.query_row(&format!("SELECT {COLS} FROM storage_blobs WHERE seq=?1"), [seq], |r| blob(r, false)).optional()).await
}

/// The current version, with or without its bytes.
pub async fn get(ctx: &Ctx, account: &str, container: &str, path: &str, data: bool) -> Result<Option<Blob>, ApiError> {
    let (a, c, p) = (account.to_string(), container.to_string(), path.to_string());
    let cols = if data { format!("{COLS}, data") } else { COLS.to_string() };
    ctx.db
        .call(move |cn| cn.query_row(&format!("SELECT {cols} FROM storage_blobs WHERE account=?1 AND container=?2 AND path=?3 AND current=1 AND deleted_at IS NULL ORDER BY seq DESC LIMIT 1"), params![a, c, p], |r| blob(r, data)).optional())
        .await
}

pub async fn versions(ctx: &Ctx, account: &str, container: &str, path: &str) -> Result<Vec<Blob>, ApiError> {
    let (a, c, p) = (account.to_string(), container.to_string(), path.to_string());
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare(&format!("SELECT {COLS} FROM storage_blobs WHERE account=?1 AND container=?2 AND path=?3 AND deleted_at IS NULL ORDER BY seq DESC"))?;
            let v = s.query_map(params![a, c, p], |r| blob(r, false))?.collect::<Result<Vec<_>, _>>();
            v
        })
        .await
}

/// Deletes the current version. Soft delete keeps it (hidden) until the retention ends.
pub async fn delete(ctx: &Ctx, account: &str, container: &str, path: &str, soft: bool) -> Result<bool, ApiError> {
    let (a, c, p) = (account.to_string(), container.to_string(), path.to_string());
    let now = util::now_ms();
    ctx.db
        .call(move |cn| {
            let n = if soft {
                cn.execute("UPDATE storage_blobs SET deleted_at=?4, current=0 WHERE account=?1 AND container=?2 AND path=?3 AND deleted_at IS NULL", params![a, c, p, now])?
            } else {
                cn.execute("DELETE FROM storage_blobs WHERE account=?1 AND container=?2 AND path=?3", params![a, c, p])?
            };
            Ok(n > 0)
        })
        .await
}

/// Current objects under a prefix (whole container when empty), newest first.
pub async fn list(ctx: &Ctx, account: &str, container: &str, prefix: &str) -> Result<Vec<Blob>, ApiError> {
    let (a, c) = (account.to_string(), container.to_string());
    let like = format!("{}%", prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare(&format!(
                "SELECT {COLS} FROM storage_blobs WHERE account=?1 AND container=?2 AND current=1 AND deleted_at IS NULL AND path LIKE ?3 ESCAPE '\\' ORDER BY path"
            ))?;
            let v = s.query_map(params![a, c, like], |r| blob(r, false))?.collect::<Result<Vec<_>, _>>();
            v
        })
        .await
}

/// (objects, bytes) per container of an account.
pub async fn counts(ctx: &Ctx, account: &str) -> Result<BTreeMap<String, (i64, i64)>, ApiError> {
    let a = account.to_string();
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare("SELECT container, count(*), coalesce(sum(size),0) FROM storage_blobs WHERE account=?1 AND current=1 AND deleted_at IS NULL GROUP BY container")?;
            let v = s.query_map([a], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))?.collect::<Result<BTreeMap<_, _>, _>>();
            v
        })
        .await
}

pub async fn used_bytes(ctx: &Ctx, account: &str) -> Result<i64, ApiError> {
    let a = account.to_string();
    ctx.db.call(move |cn| cn.query_row("SELECT coalesce(sum(size),0) FROM storage_blobs WHERE account=?1 AND deleted_at IS NULL", [a], |r| r.get(0))).await
}

pub async fn drop_container(ctx: &Ctx, account: &str, container: &str) -> Result<i64, ApiError> {
    let (a, c) = (account.to_string(), container.to_string());
    ctx.db
        .call(move |cn| {
            let n: i64 = cn.query_row("SELECT count(*) FROM storage_blobs WHERE account=?1 AND container=?2 AND current=1 AND deleted_at IS NULL", params![a, c], |r| r.get(0))?;
            cn.execute("DELETE FROM storage_blobs WHERE account=?1 AND container=?2", params![a, c])?;
            Ok(n)
        })
        .await
}

pub async fn drop_account(ctx: &Ctx, account: &str) -> Result<(), ApiError> {
    let a = account.to_string();
    ctx.db
        .call(move |cn| {
            for t in ["storage_blobs", "storage_blocks", "storage_next", "storage_reach", "storage_links"] {
                cn.execute(&format!("DELETE FROM {t} WHERE account=?1"), [&a])?;
            }
            Ok(())
        })
        .await
}

/// Ends soft-delete retention and old versions beyond 50.
pub async fn sweep(ctx: &Ctx, account: &str, soft_ms: Option<u64>) -> Result<(), ApiError> {
    let a = account.to_string();
    let cut = soft_ms.map(|m| util::now_ms() - m as i64).unwrap_or(i64::MAX);
    ctx.db
        .call(move |cn| {
            cn.execute("DELETE FROM storage_blobs WHERE account=?1 AND deleted_at IS NOT NULL AND deleted_at < ?2", params![a, cut])?;
            cn.execute("DELETE FROM storage_blocks WHERE at < ?1", [util::now_ms() - 7 * 86_400_000])?;
            Ok(())
        })
        .await
}

// ---------------------------------------------------------------- blocks and multipart

pub async fn put_block(ctx: &Ctx, account: &str, container: &str, path: &str, id: &str, data: Vec<u8>) -> Result<(), ApiError> {
    let (a, c, p, i) = (account.to_string(), container.to_string(), path.to_string(), id.to_string());
    ctx.db
        .call(move |cn| cn.execute("INSERT OR REPLACE INTO storage_blocks VALUES(?1,?2,?3,?4,?5,?6)", params![a, c, p, i, data, util::now_ms()]).map(|_| ()))
        .await
}

/// The bytes of the listed blocks in order; Err(id) names a block that is missing.
pub async fn take_blocks(ctx: &Ctx, account: &str, container: &str, path: &str, ids: Vec<String>) -> Result<Result<Vec<u8>, String>, ApiError> {
    let (a, c, p) = (account.to_string(), container.to_string(), path.to_string());
    ctx.db
        .call(move |cn| {
            let mut out = vec![];
            for id in &ids {
                let d: Option<Vec<u8>> = cn
                    .query_row("SELECT data FROM storage_blocks WHERE account=?1 AND container=?2 AND path=?3 AND block_id=?4", params![a, c, p, id], |r| r.get(0))
                    .optional()?;
                match d {
                    Some(d) => out.extend(d),
                    None => return Ok(Err(id.clone())),
                }
            }
            cn.execute("DELETE FROM storage_blocks WHERE account=?1 AND container=?2 AND path=?3", params![a, c, p])?;
            Ok(Ok(out))
        })
        .await
}

pub async fn new_upload(ctx: &Ctx, account: &str, container: &str, path: &str, ctype: &str, meta: &BTreeMap<String, String>) -> Result<String, ApiError> {
    let id = format!("{}{}", util::id("up"), util::id("x").trim_start_matches("x_"));
    let (i, a, c, p, t, m) = (id.clone(), account.to_string(), container.to_string(), path.to_string(), ctype.to_string(), json!(meta).to_string());
    ctx.db.call(move |cn| cn.execute("INSERT INTO storage_uploads VALUES(?1,?2,?3,?4,?5,?6,?7)", params![i, a, c, p, t, m, util::now_ms()]).map(|_| ())).await?;
    Ok(id)
}

/// (account, container, path, ctype, meta) of an open multipart upload.
pub async fn upload(ctx: &Ctx, id: &str) -> Result<Option<(String, String, String, String, BTreeMap<String, String>)>, ApiError> {
    let i = id.to_string();
    ctx.db
        .call(move |cn| {
            cn.query_row("SELECT account, container, path, ctype, meta FROM storage_uploads WHERE id=?1", [i], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default()))
            })
            .optional()
        })
        .await
}

pub async fn put_part(ctx: &Ctx, id: &str, n: i64, data: Vec<u8>) -> Result<String, ApiError> {
    let etag = format!("\"{}\"", md5_hex(&data));
    let (i, e) = (id.to_string(), etag.clone());
    ctx.db.call(move |cn| cn.execute("INSERT OR REPLACE INTO storage_parts VALUES(?1,?2,?3,?4)", params![i, n, data, e]).map(|_| ())).await?;
    Ok(etag)
}

/// Joins the parts (all of them, in number order) and closes the upload. Returns bytes and the
/// multipart ETag.
pub async fn finish_upload(ctx: &Ctx, id: &str, wanted: Vec<i64>) -> Result<Result<(Vec<u8>, String), String>, ApiError> {
    let i = id.to_string();
    ctx.db
        .call(move |cn| {
            let mut out = vec![];
            let mut md5s = vec![];
            for n in &wanted {
                let d: Option<(Vec<u8>, String)> = cn.query_row("SELECT data, etag FROM storage_parts WHERE upload_id=?1 AND n=?2", params![i, n], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
                match d {
                    Some((d, _)) => {
                        md5s.extend(md5::compute(&d).0);
                        out.extend(d);
                    }
                    None => return Ok(Err(format!("part {n}"))),
                }
            }
            cn.execute("DELETE FROM storage_parts WHERE upload_id=?1", [&i])?;
            cn.execute("DELETE FROM storage_uploads WHERE id=?1", [&i])?;
            Ok(Ok((out, format!("\"{}-{}\"", md5_hex(&md5s), wanted.len()))))
        })
        .await
}

pub async fn abort_upload(ctx: &Ctx, id: &str) -> Result<(), ApiError> {
    let i = id.to_string();
    ctx.db
        .call(move |cn| {
            cn.execute("DELETE FROM storage_parts WHERE upload_id=?1", [&i])?;
            cn.execute("DELETE FROM storage_uploads WHERE id=?1", [&i]).map(|_| ())
        })
        .await
}

// ---------------------------------------------------------------- requests (history, "Try it")

#[derive(Clone, Debug, Default)]
pub struct Request {
    pub account: String,
    pub container: String,
    pub path: String,
    /// put | get | head | delete | list | link | copy
    pub op: String,
    pub method: String,
    pub status: u16,
    pub caller: String,
    /// key | sas | presigned | anonymous | ui | mcp | api | seed
    pub via: String,
    pub link_id: Option<String>,
    /// Who made it fail: "rule:<name>", "next read", "reachability", "link:<result>".
    pub provoked: Option<String>,
    pub ms: i64,
    pub size: i64,
    pub ctype: String,
    pub remote: String,
    pub detail: String,
    pub trace_id: Option<String>,
}

pub async fn record(ctx: &Ctx, r: Request) -> Result<i64, ApiError> {
    let t = (r.account.clone(), r.container.clone(), r.path.clone(), r.op.clone(), r.status, r.caller.clone(), r.provoked.clone(), r.via.clone(), r.ms, r.trace_id.clone());
    let seq = ctx
        .db
        .call(move |cn| {
            cn.execute(
                "INSERT INTO storage_requests(at, account, container, path, op, method, status, caller, via, link_id, provoked, ms, size, ctype, remote, detail) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![util::now_ms(), r.account, r.container, r.path, r.op, r.method, r.status, r.caller, r.via, r.link_id, r.provoked, r.ms, r.size, r.ctype, r.remote, r.detail],
            )?;
            Ok(cn.last_insert_rowid())
        })
        .await?;
    ctx.live.send("storage.request", &seq.to_string(), "created");
    timeline(ctx, seq, t).await;
    Ok(seq)
}

/// The Timeline row: writes, deletes and failures one by one; reads fold per caller, container and minute.
async fn timeline(ctx: &Ctx, seq: i64, t: (String, String, String, String, u16, String, Option<String>, String, i64, Option<String>)) {
    let (account, container, path, op, status, caller, provoked, via, ms, trace) = t;
    // An exists check (HEAD → 404) before an upload is the app asking, not a failure.
    let asking = op == "head" && status == 404 && provoked.is_none();
    if via == "ui" || via == "seed" { return; }
    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    let link = format!("/files?account={account}&container={container}");
    let outcome = if provoked.is_some() { "provoked" } else if status >= 400 && !asking { "failed" } else { "ok" };
    let reads = ["get", "head", "list"].contains(&op.as_str()) && outcome == "ok";
    if reads {
        let minute = util::now_ms() / 60_000;
        let (c2, k2, cl2) = (container.clone(), account.clone(), caller.clone());
        let n: i64 = ctx.db.call(move |cn| cn.query_row("SELECT count(*) FROM storage_requests WHERE account=?1 AND container=?2 AND caller=?3 AND op IN ('get','head','list') AND status<400 AND at/60000=?4",
            params![k2, c2, cl2, minute], |r| r.get(0))).await.unwrap_or(1);
        let id = format!("st:{account}:{container}:{caller}:{minute}");
        let label = if n <= 1 { format!("{caller} read {name}") } else { format!("{caller} read {n} objects in {container}") };
        let _ = stuntman_core::trips::record(ctx, stuntman_core::trips::Trip { id: &id, module: "storage", label: &label, status: "200", trace_id: trace.as_deref(), link: &link, host: &account,
            actor: &caller, outcome: "ok", detail: &format!("{account}  ·  {}", if n <= 1 { "1 read".to_string() } else { format!("folded  ·  {n} reads in 1 min") }) }).await;
        return;
    }
    let verb = match op.as_str() { "put" => "Upload of", "delete" => "Delete of", "copy" => "Copy of", "link" => "Link to", "get" | "head" => "Read of", "list" => "List of", _ => "Request for" };
    let label = if asking { format!("{caller} checked {name}: not there yet") } else if status >= 400 { format!("{verb} {name} failed: {status}") } else { format!("{verb} {name}") };
    let _ = stuntman_core::trips::record(ctx, stuntman_core::trips::Trip { id: &format!("st:{seq}"), module: "storage", label: &label, status: &status.to_string(), trace_id: trace.as_deref(), link: &link, host: &account,
        actor: &caller, outcome, detail: &format!("{}{account}/{container}  ·  {ms} ms", provoked.map(|p| format!("{p}  ·  ")).unwrap_or_default()) }).await;
}

pub fn request_json(r: &Row) -> rusqlite::Result<Value> {
    Ok(json!({"seq": r.get::<_, i64>(0)?, "at": r.get::<_, i64>(1)?, "account": r.get::<_, String>(2)?, "container": r.get::<_, String>(3)?,
        "path": r.get::<_, String>(4)?, "op": r.get::<_, String>(5)?, "method": r.get::<_, String>(6)?, "status": r.get::<_, i64>(7)?,
        "caller": r.get::<_, String>(8)?, "via": r.get::<_, String>(9)?, "linkId": r.get::<_, Option<String>>(10)?, "provoked": r.get::<_, Option<String>>(11)?,
        "ms": r.get::<_, Option<i64>>(12)?, "size": r.get::<_, Option<i64>>(13)?, "ctype": r.get::<_, String>(14)?, "remote": r.get::<_, String>(15)?, "detail": r.get::<_, String>(16)?}))
}

const RCOLS: &str = "seq, at, account, container, path, op, method, status, caller, via, link_id, provoked, ms, size, ctype, remote, detail";

pub async fn requests_of(ctx: &Ctx, account: &str, container: &str, path: &str) -> Result<Vec<Value>, ApiError> {
    let (a, c, p) = (account.to_string(), container.to_string(), path.to_string());
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare(&format!("SELECT {RCOLS} FROM storage_requests WHERE account=?1 AND container=?2 AND path=?3 ORDER BY seq DESC LIMIT 100"))?;
            let v = s.query_map(params![a, c, p], request_json)?.collect::<Result<Vec<_>, _>>();
            v
        })
        .await
}

/// The last N requests of a kind of service (for "Try it"), newest first.
pub async fn last_requests(ctx: &Ctx, accounts: Vec<String>, n: i64) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |cn| {
            let q = format!("SELECT {RCOLS} FROM storage_requests WHERE account IN ({}) AND via NOT IN ('ui','mcp','api','seed') ORDER BY seq DESC LIMIT ?1", accounts.iter().map(|a| format!("'{}'", a.replace('\'', "''"))).collect::<Vec<_>>().join(","));
            let mut s = cn.prepare(&if accounts.is_empty() { format!("SELECT {RCOLS} FROM storage_requests WHERE 0 LIMIT ?1") } else { q })?;
            let v = s.query_map([n], request_json)?.collect::<Result<Vec<_>, _>>();
            v
        })
        .await
}

pub async fn reads(ctx: &Ctx, account: &str, container: &str, paths: Vec<String>) -> Result<BTreeMap<String, i64>, ApiError> {
    let (a, c) = (account.to_string(), container.to_string());
    ctx.db
        .call(move |cn| {
            let mut out = BTreeMap::new();
            let mut s = cn.prepare("SELECT count(*) FROM storage_requests WHERE account=?1 AND container=?2 AND path=?3 AND op='get' AND status < 300")?;
            for p in paths {
                let n: i64 = s.query_row(params![a, c, p], |r| r.get(0))?;
                out.insert(p, n);
            }
            Ok(out)
        })
        .await
}

// ---------------------------------------------------------------- next read, reachability

pub async fn set_next(ctx: &Ctx, account: &str, container: &str, path: &str, result: &str, times: i64, by: &str) -> Result<(), ApiError> {
    let (a, c, p, r, b) = (account.to_string(), container.to_string(), path.to_string(), result.to_string(), by.to_string());
    ctx.db
        .call(move |cn| {
            if r == "normal" || times <= 0 {
                cn.execute("DELETE FROM storage_next WHERE account=?1 AND container=?2 AND path=?3", params![a, c, p]).map(|_| ())
            } else {
                cn.execute("INSERT OR REPLACE INTO storage_next VALUES(?1,?2,?3,?4,?5,?6,?7)", params![a, c, p, r, times, b, util::now_ms()]).map(|_| ())
            }
        })
        .await
}

/// (result, times left, by) armed on an object.
pub async fn next_of(ctx: &Ctx, account: &str, container: &str, path: &str) -> Result<Option<(String, i64, String)>, ApiError> {
    let (a, c, p) = (account.to_string(), container.to_string(), path.to_string());
    ctx.db
        .call(move |cn| cn.query_row("SELECT result, times, by_who FROM storage_next WHERE account=?1 AND container=?2 AND path=?3", params![a, c, p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional())
        .await
}

/// Uses one armed read: returns the result to answer with.
pub async fn take_next(ctx: &Ctx, account: &str, container: &str, path: &str) -> Result<Option<(String, String)>, ApiError> {
    let (a, c, p) = (account.to_string(), container.to_string(), path.to_string());
    ctx.db
        .call(move |cn| {
            let tx = cn.transaction()?;
            let n: Option<(String, i64, String)> = tx.query_row("SELECT result, times, by_who FROM storage_next WHERE account=?1 AND container=?2 AND path=?3", params![a, c, p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
            if let Some((_, times, _)) = &n {
                if *times <= 1 {
                    tx.execute("DELETE FROM storage_next WHERE account=?1 AND container=?2 AND path=?3", params![a, c, p])?;
                } else {
                    tx.execute("UPDATE storage_next SET times=times-1 WHERE account=?1 AND container=?2 AND path=?3", params![a, c, p])?;
                }
            }
            tx.commit()?;
            Ok(n.map(|(r, _, b)| (r, b)))
        })
        .await
}

pub async fn nexts(ctx: &Ctx, account: &str, container: &str) -> Result<BTreeMap<String, String>, ApiError> {
    let (a, c) = (account.to_string(), container.to_string());
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare("SELECT path, result FROM storage_next WHERE account=?1 AND container=?2")?;
            let v = s.query_map(params![a, c], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<BTreeMap<_, _>, _>>();
            v
        })
        .await
}

pub async fn set_reach(ctx: &Ctx, account: &str, mode: &str, by: &str) -> Result<(), ApiError> {
    let (a, m, b) = (account.to_string(), mode.to_string(), by.to_string());
    ctx.db
        .call(move |cn| {
            if m == "ok" {
                cn.execute("DELETE FROM storage_reach WHERE account=?1", [a]).map(|_| ())
            } else {
                cn.execute("INSERT OR REPLACE INTO storage_reach VALUES(?1,?2,?3,?4)", params![a, m, util::now_ms(), b]).map(|_| ())
            }
        })
        .await
}

pub async fn reach(ctx: &Ctx, account: &str) -> Result<Option<Value>, ApiError> {
    let a = account.to_string();
    ctx.db
        .call(move |cn| cn.query_row("SELECT mode, since, by_who FROM storage_reach WHERE account=?1", [a], |r| Ok(json!({"mode": r.get::<_, String>(0)?, "since": r.get::<_, i64>(1)?, "by": r.get::<_, String>(2)?}))).optional())
        .await
}

// ---------------------------------------------------------------- seeds

pub async fn seed_info(ctx: &Ctx, account: &str) -> Result<Option<(i64, i64)>, ApiError> {
    let a = account.to_string();
    ctx.db.call(move |cn| cn.query_row("SELECT loaded_at, files FROM storage_seed WHERE account=?1", [a], |r| Ok((r.get(0)?, r.get(1)?))).optional()).await
}

pub async fn set_seed(ctx: &Ctx, account: &str, files: i64) -> Result<(), ApiError> {
    let a = account.to_string();
    ctx.db.call(move |cn| cn.execute("INSERT OR REPLACE INTO storage_seed VALUES(?1,?2,?3)", params![a, util::now_ms(), files]).map(|_| ())).await
}

/// Changes since the seed was loaded: (uploaded by the app, added by people, deletes).
pub async fn changes_since(ctx: &Ctx, account: &str, since: i64) -> Result<(i64, i64, i64), ApiError> {
    let a = account.to_string();
    ctx.db
        .call(move |cn| {
            cn.query_row(
                "SELECT
                   (SELECT count(*) FROM storage_requests WHERE account=?1 AND at>?2 AND op IN ('put','copy') AND status<300 AND via NOT IN ('ui','mcp','api','seed')),
                   (SELECT count(*) FROM storage_requests WHERE account=?1 AND at>?2 AND op IN ('put','copy') AND status<300 AND via IN ('ui','mcp','api')),
                   (SELECT count(*) FROM storage_requests WHERE account=?1 AND at>?2 AND op='delete' AND status<300)",
                params![a, since],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
        })
        .await
}

pub fn wipe_objects(cn: &rusqlite::Connection, account: &str) -> rusqlite::Result<()> {
    cn.execute("DELETE FROM storage_blobs WHERE account=?1", [account])?;
    cn.execute("DELETE FROM storage_blocks WHERE account=?1", [account])?;
    cn.execute("DELETE FROM storage_next WHERE account=?1", [account]).map(|_| ())
}
