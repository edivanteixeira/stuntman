//! S3 · Links: Azure SAS and S3 presigned URLs. A link is listed when Stuntman signs one
//! (create_link) or the first time the app's own link is used. Every use is a request with the
//! link's id, refused ones included.

use stuntman_core::{util, ApiError, Ctx};
use rusqlite::{params, OptionalExtension, Row};
use serde_json::{json, Value};

use crate::{rules, store};

pub struct Seen {
    pub kind: &'static str,
    pub sig: String,
    pub account: String,
    pub container: String,
    pub path: String,
    /// read, write, delete, list (comma separated)
    pub allows: String,
    pub start: Option<i64>,
    pub expiry: i64,
    pub ips: String,
    pub protocol: String,
    pub signed_with: String,
    pub url: String,
}

/// Azure sp letters → words.
pub fn allows_from_sp(sp: &str) -> String {
    let mut v = vec![];
    if sp.contains('r') { v.push("read"); }
    if sp.contains('w') || sp.contains('c') || sp.contains('a') { v.push("write"); }
    if sp.contains('d') { v.push("delete"); }
    if sp.contains('l') { v.push("list"); }
    v.join(",")
}

pub fn need(op: &str) -> &'static str {
    match op {
        "put" | "copy" => "write",
        "delete" => "delete",
        "list" => "list",
        _ => "read",
    }
}

const COLS: &str = "id, account, container, path, kind, allows, start, expiry, ips, protocol, signed_with, issued_by, issued_at, url, revoked_at, revoked_by, next_use, next_times";

fn link(r: &Row) -> rusqlite::Result<Value> {
    Ok(json!({"id": r.get::<_, String>(0)?, "account": r.get::<_, String>(1)?, "container": r.get::<_, String>(2)?, "path": r.get::<_, String>(3)?,
        "kind": r.get::<_, String>(4)?, "allows": r.get::<_, String>(5)?, "start": r.get::<_, Option<i64>>(6)?, "expiry": r.get::<_, i64>(7)?,
        "ips": r.get::<_, String>(8)?, "protocol": r.get::<_, String>(9)?, "signedWith": r.get::<_, String>(10)?, "issuedBy": r.get::<_, String>(11)?,
        "issuedAt": r.get::<_, i64>(12)?, "url": r.get::<_, String>(13)?, "revokedAt": r.get::<_, Option<i64>>(14)?, "revokedBy": r.get::<_, Option<String>>(15)?,
        "nextUse": r.get::<_, Option<String>>(16)?, "nextTimes": r.get::<_, i64>(17)?}))
}

pub fn state(l: &Value) -> &'static str {
    if !l["revokedAt"].is_null() {
        "revoked"
    } else if l["expiry"].as_i64().unwrap_or(0) < util::now_ms() {
        "expired"
    } else {
        "valid"
    }
}

pub async fn insert(ctx: &Ctx, s: &Seen, issued_by: &str) -> Result<String, ApiError> {
    let id = util::id("lnk");
    let (i, b) = (id.clone(), issued_by.to_string());
    let (a, c, p, k, al, st, ex, ip, pr, sw, u, sg) = (s.account.clone(), s.container.clone(), s.path.clone(), s.kind, s.allows.clone(), s.start, s.expiry, s.ips.clone(), s.protocol.clone(), s.signed_with.clone(), s.url.clone(), s.sig.clone());
    ctx.db
        .call(move |cn| {
            cn.execute(
                &format!("INSERT INTO storage_links({COLS}, sig) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,NULL,NULL,NULL,0,?15)"),
                params![i, a, c, p, k, al, st, ex, ip, pr, sw, b, util::now_ms(), u, sg],
            )
            .map(|_| ())
        })
        .await?;
    ctx.live.send("storage.link", &id, "created");
    Ok(id)
}

pub async fn get(ctx: &Ctx, id: &str) -> Result<Value, ApiError> {
    let i = id.to_string();
    ctx.db
        .call(move |cn| cn.query_row(&format!("SELECT {COLS} FROM storage_links WHERE id=?1"), [i], link).optional())
        .await?
        .ok_or_else(|| ApiError::not_found(format!("No link {id}")))
}

pub async fn by_sig(ctx: &Ctx, sig: &str) -> Result<Option<Value>, ApiError> {
    let s = sig.to_string();
    ctx.db.call(move |cn| cn.query_row(&format!("SELECT {COLS} FROM storage_links WHERE sig=?1"), [s], link).optional()).await
}

pub async fn list(ctx: &Ctx) -> Result<Vec<Value>, ApiError> {
    let since = util::now_ms() - 7 * 86_400_000;
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare(&format!(
                "SELECT {COLS}, (SELECT count(*) FROM storage_requests r WHERE r.link_id=l.id AND r.status<300), (SELECT count(*) FROM storage_requests r WHERE r.link_id=l.id AND r.status>=300)
                 FROM storage_links l WHERE removed=0 AND (expiry > ?1 OR revoked_at > ?1 OR issued_at > ?1) ORDER BY issued_at DESC"
            ))?;
            let v = s
                .query_map([since], |r| {
                    let mut v = link(r)?;
                    v["uses"] = json!(r.get::<_, i64>(18)?);
                    v["refused"] = json!(r.get::<_, i64>(19)?);
                    Ok(v)
                })?
                .collect::<Result<Vec<_>, _>>();
            v
        })
        .await
}

pub async fn uses(ctx: &Ctx, id: &str) -> Result<Vec<Value>, ApiError> {
    let i = id.to_string();
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare("SELECT seq, at, account, container, path, op, method, status, caller, via, link_id, provoked, ms, size, ctype, remote, detail FROM storage_requests WHERE link_id=?1 ORDER BY seq")?;
            let v = s.query_map([i], store::request_json)?.collect::<Result<Vec<_>, _>>();
            v
        })
        .await
}

pub async fn set(ctx: &Ctx, id: &str, sql: &'static str, a: Option<String>, b: Option<i64>) -> Result<(), ApiError> {
    let i = id.to_string();
    let n = ctx.db.call(move |cn| cn.execute(sql, params![i, a, b])).await?;
    if n == 0 {
        return Err(ApiError::not_found(format!("No link {id}")));
    }
    ctx.live.send("storage.link", id, "updated");
    Ok(())
}

pub const NEXT_USE: [(&str, &str); 7] = [
    ("normal", "Normal"),
    ("expired", "Expired now · 403 AuthenticationFailed"),
    ("bad_signature", "Wrong signature · 403"),
    ("ip_denied", "IP not allowed · 403"),
    ("permission", "Permission missing · 403"),
    ("not_yet_valid", "Not valid yet (clock skew)"),
    ("gone", "Object gone · 404"),
];

/// A refusal: (status, code, message, why).
pub type Refusal = (u16, &'static str, String, String);

/// A link is being used: find or list it, then decide. Ok(link id) to go on.
pub async fn on_use(ctx: &Ctx, s: Seen, op: &str, sig_ok: bool, remote: &str, kind_name: &str) -> Result<Result<String, (Refusal, String)>, ApiError> {
    let l = match by_sig(ctx, &s.sig).await? {
        Some(l) => l,
        None => {
            let issued_by = if s.signed_with.is_empty() { "the app (seen at first use)".to_string() } else { format!("the app, signed with {} (seen at first use)", s.signed_with) };
            let id = insert(ctx, &s, &issued_by).await?;
            get(ctx, &id).await?
        }
    };
    let id = l["id"].as_str().unwrap_or("").to_string();
    let now = util::now_ms();
    let refuse = |st: u16, code: &'static str, m: String, why: &str| Ok(Err(((st, code, m, why.to_string()), id.clone())));
    let auth_failed = if kind_name == crate::config::AZURE { "AuthenticationFailed" } else { "AccessDenied" };
    if !l["revokedAt"].is_null() {
        return refuse(403, auth_failed, "This link was revoked.".into(), "revoked");
    }
    if let Some(n) = l["nextUse"].as_str().filter(|n| *n != "normal") {
        let n = n.to_string();
        let left = l["nextTimes"].as_i64().unwrap_or(1);
        set(ctx, &id, if left <= 1 { "UPDATE storage_links SET next_use=NULL, next_times=0 WHERE id=?1 AND ?2 IS NULL AND ?3 IS NULL" } else { "UPDATE storage_links SET next_times=next_times-1 WHERE id=?1 AND ?2 IS NULL AND ?3 IS NULL" }, None, None).await?;
        let why = "next use · provoked by you";
        return match n.as_str() {
            "expired" => refuse(403, auth_failed, "Signed expiry time has passed.".into(), why),
            "bad_signature" => refuse(403, if kind_name == crate::config::AZURE { "AuthenticationFailed" } else { "SignatureDoesNotMatch" }, "Signature did not match.".into(), why),
            "ip_denied" => refuse(403, if kind_name == crate::config::AZURE { "AuthorizationSourceIPMismatch" } else { "AccessDenied" }, format!("This request is not authorized from {remote}."), why),
            "permission" => refuse(403, if kind_name == crate::config::AZURE { "AuthorizationPermissionMismatch" } else { "AccessDenied" }, "This request is not authorized to perform this operation using this permission.".into(), why),
            "not_yet_valid" => refuse(403, auth_failed, "Signed start time is in the future (clock skew).".into(), why),
            "gone" => refuse(404, if kind_name == crate::config::AZURE { "BlobNotFound" } else { "NoSuchKey" }, "The specified object does not exist.".into(), why),
            _ => Ok(Ok(id)),
        };
    }
    let rs = rules::Seen { op: "link".into(), account: s.account.clone(), container: s.container.clone(), path: s.path.clone(), ..Default::default() };
    if let Some(r) = rules::decide(ctx, kind_name, &rs).await? {
        if r.then.respond == "expires" {
            let born = l["start"].as_i64().unwrap_or(l["issuedAt"].as_i64().unwrap_or(now));
            if now > born + r.then.seconds.unwrap_or(0) as i64 * 1000 {
                return refuse(403, auth_failed, "Signed expiry time has passed.".into(), &format!("rule · {}", r.name));
            }
        }
    }
    if !sig_ok {
        return refuse(403, if kind_name == crate::config::AZURE { "AuthenticationFailed" } else { "SignatureDoesNotMatch" }, "Signature did not match. The account key may have been rotated.".into(), "signature");
    }
    if l["expiry"].as_i64().unwrap_or(0) < now {
        let ago = (now - l["expiry"].as_i64().unwrap_or(0)) / 60_000;
        return refuse(403, auth_failed, format!("Signed expiry time has passed ({ago} min ago)."), "expired");
    }
    if l["start"].as_i64().map(|st| st > now + 60_000).unwrap_or(false) {
        return refuse(403, auth_failed, "Signed start time is in the future.".into(), "not yet valid");
    }
    let ips = l["ips"].as_str().unwrap_or("");
    if !ips.is_empty() && !ip_ok(ips, remote) {
        return refuse(403, if kind_name == crate::config::AZURE { "AuthorizationSourceIPMismatch" } else { "AccessDenied" }, format!("This request is not authorized from {remote}."), "ip");
    }
    if !l["allows"].as_str().unwrap_or("").split(',').any(|x| x == need(op)) {
        return refuse(403, if kind_name == crate::config::AZURE { "AuthorizationPermissionMismatch" } else { "AccessDenied" }, format!("The link does not allow {}.", need(op)), "permission");
    }
    Ok(Ok(id))
}

/// "203.0.113.24" or a range "203.0.113.0-203.0.113.255".
fn ip_ok(ips: &str, remote: &str) -> bool {
    let n = |s: &str| s.trim().parse::<std::net::Ipv4Addr>().ok().map(u32::from);
    let Some(r) = n(remote) else { return true };
    match ips.split_once('-') {
        Some((a, b)) => matches!((n(a), n(b)), (Some(a), Some(b)) if a <= r && r <= b),
        None => n(ips) == Some(r),
    }
}

pub async fn wait_for(ctx: &Ctx, account: &str, container: &str, prefix: &str, since: i64, timeout_ms: u64) -> Result<Option<Value>, ApiError> {
    let end = util::now_ms() + timeout_ms as i64;
    loop {
        let (a, c, p) = (account.to_string(), container.to_string(), format!("{}%", prefix.replace('%', "\\%")));
        let v = ctx
            .db
            .call(move |cn| cn.query_row(&format!("SELECT {COLS} FROM storage_links WHERE account=?1 AND container=?2 AND path LIKE ?3 ESCAPE '\\' AND issued_at>=?4 ORDER BY issued_at LIMIT 1"), params![a, c, p, since], link).optional())
            .await?;
        if v.is_some() || util::now_ms() > end {
            return Ok(v);
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod t {
    #[test]
    fn ips_and_perms() {
        assert!(super::ip_ok("203.0.113.0-203.0.113.255", "203.0.113.24"));
        assert!(!super::ip_ok("10.0.0.1", "203.0.113.24"));
        assert_eq!(super::allows_from_sp("racwdl"), "read,write,delete,list");
    }
}
