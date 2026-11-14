//! The S3 API (also what Cloudflare R2 speaks): http://host:9000/<bucket>/<key> path style, or
//! <bucket>.<host> virtual-host style. SigV4 in the header or presigned in the query; Put, Get
//! (ranges, response-content-* overrides), Head, Delete, DeleteObjects, ListObjects v1/v2, Copy,
//! multipart, Head/Create/Delete Bucket, ListBuckets, conditional requests.

use std::collections::BTreeMap;

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use stuntman_core::{env::Envs, util, Ctx};

use crate::{
    auth,
    config::{self, Account},
    gate::{self, Fail},
    http::{self, Kill, Remote},
    links, ops, rules,
    store::{self, Blob, Request as Rec},
};

fn x(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn resp(status: u16, headers: Vec<(String, String)>, body: Body) -> Response {
    let mut r = Response::new(body);
    *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let h = r.headers_mut();
    for (k, v) in headers {
        if let (Ok(k), Ok(v)) = (HeaderName::try_from(k), HeaderValue::from_str(&v)) {
            h.append(k, v);
        }
    }
    h.insert("x-amz-request-id", HeaderValue::from_str(&format!("{:016X}", rand::random::<u64>())).unwrap());
    h.insert("date", HeaderValue::from_str(&auth::rfc1123(util::now_ms())).unwrap());
    h.insert("server", HeaderValue::from_static("Stuntman-S3"));
    r
}

fn xml(status: u16, body: String) -> Response {
    resp(status, vec![("content-type".into(), "application/xml".into())], Body::from(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>{body}")))
}

fn err(status: u16, code: &str, msg: &str, res: &str) -> Response {
    let mut r = xml(status, format!("<Error><Code>{code}</Code><Message>{}</Message><Resource>{}</Resource><RequestId>{:016X}</RequestId></Error>", x(msg), x(res), rand::random::<u64>()));
    if status == 503 {
        r.headers_mut().insert("retry-after", HeaderValue::from_static("1"));
    }
    r
}

fn code_for(status: u16) -> (&'static str, &'static str) {
    match status {
        401 => ("InvalidAccessKeyId", "The AWS access key Id you provided does not exist in our records (key rejected)."),
        403 => ("AccessDenied", "Access Denied"),
        404 => ("NoSuchKey", "The specified key does not exist."),
        409 => ("OperationAborted", "A conflicting conditional operation is in progress."),
        412 => ("PreconditionFailed", "At least one of the pre-conditions you specified did not hold"),
        413 => ("EntityTooLarge", "Your proposed upload exceeds the maximum allowed object size."),
        507 => ("InsufficientStorage", "The bucket is full (storage limit)."),
        _ => ("SlowDown", "Please reduce your request rate."),
    }
}

struct Call {
    ctx: Ctx,
    method: String,
    raw_path: String,
    q: Vec<(String, String)>,
    qm: BTreeMap<String, String>,
    h: BTreeMap<String, String>,
    body: Vec<u8>,
    caller: String,
    remote: String,
    kill: Option<Kill>,
}

pub async fn handle(State(envs): State<Envs>, mut req: Request) -> Response {
    let kill = req.extensions().get::<Kill>().cloned();
    let remote = req.extensions().get::<Remote>().map(|r| r.0.ip().to_string()).unwrap_or_default();
    let raw_path = req.uri().path().to_string();
    let routed = match http::route(&envs, &mut req) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let ctx = routed.ctx;
    if ctx.state.paused() {
        return err(503, "ServiceUnavailable", &format!("{} is paused in Stuntman", ctx.env), "");
    }
    let h = http::lower_headers(req.headers());
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let q = auth::query(req.uri().query().unwrap_or(""));
    let qm: BTreeMap<String, String> = q.iter().cloned().collect();
    let mut body = axum::body::to_bytes(req.into_body(), 1 << 30).await.map(|b| b.to_vec()).unwrap_or_default();
    let sha = h.get("x-amz-content-sha256").cloned().unwrap_or_default();
    if h.get("content-encoding").map(|v| v.contains("aws-chunked")).unwrap_or(false) || sha.starts_with("STREAMING-") {
        body = auth::unchunk(&body);
    }
    let caller = ops::caller(&h);
    ctx.live.seen(&caller, "Storage");
    let c = Call { ctx, method, raw_path, q, qm, h, body, caller, remote, kill };
    // Virtual-host style: the host's first label is a bucket of this environment.
    let host_first = c.h.get("host").and_then(|h| h.split('.').next()).map(String::from).unwrap_or_default();
    let segs: Vec<String> = path.trim_start_matches('/').splitn(2, '/').map(auth::decode_path).collect();
    let (bucket, key) = match config::by_bucket(&c.ctx, &host_first) {
        Some(_) if c.h.get("host").map(|h| h.contains('.')).unwrap_or(false) => (host_first, auth::decode_path(path.trim_start_matches('/'))),
        _ => (segs.first().cloned().unwrap_or_default(), segs.get(1).cloned().unwrap_or_default()),
    };
    run(c, bucket, key).await
}

/// The account behind an access key id, or by bucket.
fn account_for(c: &Call, bucket: &str, key_id: &str) -> Option<Account> {
    let all = config::list(&c.ctx);
    if !bucket.is_empty() {
        if let Some(a) = all.iter().find(|a| a.kind == config::S3 && a.containers.contains_key(bucket)) {
            return Some(a.clone());
        }
    }
    all.into_iter().find(|a| a.kind == config::S3 && (a.access.keys.iter().any(|k| k == key_id) || bucket.is_empty()))
}

enum Who {
    Ok { via: String, link: Option<String> },
    No(Response, String, Option<String>),
}

fn signed_ok(c: &Call, a: &Account, key_id: &str, signed: &str, sig: &str, amz: &str, scope: &str, payload: &str) -> bool {
    let secret = config::key_value(&c.ctx, &a.account, key_id);
    if secret.is_empty() || !a.access.keys.iter().any(|k| k == key_id) {
        return false;
    }
    auth::sigv4(&auth::V4 { method: &c.method, raw_path: &c.raw_path, query: &c.q, headers: &c.h, signed_headers: signed, payload_hash: payload, amz_date: amz, scope }, &secret) == sig
}

async fn authorize(c: &Call, a: &Account, bucket: &str, key: &str, op: &str) -> Who {
    let any = a.access.any_key;
    if c.qm.get("X-Amz-Algorithm").is_some() {
        let cred = c.qm.get("X-Amz-Credential").cloned().unwrap_or_default();
        let (key_id, scope) = cred.split_once('/').unwrap_or((&cred, ""));
        let amz = c.qm.get("X-Amz-Date").cloned().unwrap_or_default();
        let start = auth::iso_ms(&amz);
        let secs: i64 = c.qm.get("X-Amz-Expires").and_then(|s| s.parse().ok()).unwrap_or(0);
        let sig = c.qm.get("X-Amz-Signature").cloned().unwrap_or_default();
        let ok = signed_ok(c, a, key_id, c.qm.get("X-Amz-SignedHeaders").map(String::as_str).unwrap_or("host"), &sig, &amz, scope, "UNSIGNED-PAYLOAD");
        let allows = match c.method.as_str() { "PUT" | "POST" => "write", "DELETE" => "delete", _ if key.is_empty() => "list", _ => "read" };
        let s = links::Seen {
            kind: "presigned",
            sig,
            account: a.account.clone(),
            container: bucket.into(),
            path: key.into(),
            allows: allows.into(),
            start,
            expiry: start.unwrap_or(0) + secs * 1000,
            ips: String::new(),
            protocol: String::new(),
            signed_with: if ok { key_id.to_string() } else { String::new() },
            url: format!("{}?{}", ops::object_url(&c.ctx, a, bucket, key), c.q.iter().map(|(k, v)| format!("{k}={}", auth::aws_encode(v, false))).collect::<Vec<_>>().join("&")),
        };
        return match links::on_use(&c.ctx, s, op, any || ok, &c.remote, config::S3).await {
            Ok(Ok(id)) => Who::Ok { via: "presigned".into(), link: Some(id) },
            Ok(Err(((st, code, msg, why), id))) => Who::No(err(st, code, &msg, &format!("/{bucket}/{key}")), why, Some(id)),
            Err(e) => Who::No(err(500, "InternalError", &e.message, ""), "error".into(), None),
        };
    }
    match c.h.get("authorization") {
        Some(v) if v.starts_with("AWS4-HMAC-SHA256") => {
            if any {
                return Who::Ok { via: "key".into(), link: None };
            }
            let part = |n: &str| v.split([',', ' ']).find_map(|p| p.trim().strip_prefix(n)).unwrap_or("").to_string();
            let cred = part("Credential=");
            let (key_id, scope) = cred.split_once('/').unwrap_or((&cred, ""));
            let amz = c.h.get("x-amz-date").cloned().unwrap_or_default();
            let payload = c.h.get("x-amz-content-sha256").cloned().unwrap_or("UNSIGNED-PAYLOAD".into());
            if signed_ok(c, a, key_id, &part("SignedHeaders="), &part("Signature="), &amz, scope, &payload) {
                Who::Ok { via: "key".into(), link: None }
            } else if !a.access.keys.iter().any(|k| k == key_id) {
                Who::No(err(403, "InvalidAccessKeyId", "The AWS Access Key Id you provided does not exist in our records.", ""), "unknown key".into(), None)
            } else {
                Who::No(err(403, "SignatureDoesNotMatch", "The request signature we calculated does not match the signature you provided (the secret may have been rotated).", ""), "signature".into(), None)
            }
        }
        _ => {
            let public = a.containers.get(bucket).map(|x| x.access == "public-read").unwrap_or(false) && (op == "get" || op == "head");
            if public || any { Who::Ok { via: if public { "anonymous" } else { "key" }.into(), link: None } } else { Who::No(err(403, "AccessDenied", "Access Denied", ""), "anonymous".into(), None) }
        }
    }
}

async fn record(c: &Call, a: &Account, bucket: &str, key: &str, op: &str, status: u16, via: &str, link: Option<String>, why: Option<String>, size: i64, ctype: &str) -> Option<i64> {
    store::record(
        &c.ctx,
        Rec { account: a.account.clone(), container: bucket.into(), path: key.into(), op: op.into(), method: c.method.clone(), status, caller: c.caller.clone(), via: via.into(), link_id: link, provoked: why, ms: 0, size, ctype: ctype.into(), remote: c.remote.clone(), detail: String::new(), trace_id: ops::trace(&c.h) },
    )
    .await
    .ok()
}

async fn fail(c: &Call, f: Fail, res: &str) -> Response {
    match f {
        Fail::Status(s) => {
            let (code, msg) = code_for(s);
            err(s, code, msg, res)
        }
        Fail::Timeout => {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            err(504, "RequestTimeout", "no answer", res)
        }
        Fail::Reset | Fail::Empty => {
            if let Some(k) = &c.kill {
                k.fire(f == Fail::Reset);
                std::future::pending::<()>().await;
            }
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

fn meta_of(h: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    h.iter().filter_map(|(k, v)| k.strip_prefix("x-amz-meta-").map(|n| (n.to_string(), v.clone()))).collect()
}

fn obj_headers(h: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    ["cache-control", "content-disposition", "content-encoding", "content-language"].iter().filter_map(|k| h.get(*k).filter(|v| !v.is_empty() && !v.contains("aws-chunked")).map(|v| (k.to_string(), v.clone()))).collect()
}

fn etag_eq(a: &str, b: &str) -> bool {
    a.trim_matches('"') == b.trim_matches('"') || a == "*"
}

fn conditions(c: &Call, cur: Option<&Blob>, read: bool) -> Option<u16> {
    if let Some(m) = c.h.get("if-match") {
        match cur {
            Some(b) if m.split(',').any(|x| etag_eq(x.trim(), &b.etag)) => {}
            _ => return Some(412),
        }
    }
    if let (Some(m), Some(b)) = (c.h.get("if-none-match"), cur) {
        if m.trim() == "*" || m.split(',').any(|x| etag_eq(x.trim(), &b.etag)) {
            return Some(if read { 304 } else { 412 });
        }
    }
    None
}

fn obj_out(b: &Blob, qm: &BTreeMap<String, String>, bad_md5: bool) -> Vec<(String, String)> {
    let mut v = vec![
        ("etag".to_string(), if bad_md5 { format!("\"{}\"", store::md5_hex(b"not the content")) } else { b.etag.clone() }),
        ("last-modified".into(), auth::rfc1123(b.at)),
        ("accept-ranges".into(), "bytes".into()),
        ("content-type".into(), b.ctype.clone()),
    ];
    for (k, val) in &b.headers {
        v.push((k.clone(), val.clone()));
    }
    for (k, val) in &b.meta {
        v.push((format!("x-amz-meta-{k}"), val.clone()));
    }
    for (q, h) in [("response-content-disposition", "content-disposition"), ("response-content-type", "content-type"), ("response-cache-control", "cache-control"), ("response-content-encoding", "content-encoding"), ("response-content-language", "content-language"), ("response-expires", "expires")] {
        if let Some(val) = qm.get(q) {
            v.retain(|(k, _)| k != h);
            v.push((h.to_string(), val.clone()));
        }
    }
    v
}

fn range(h: &BTreeMap<String, String>, len: usize) -> Option<(usize, usize)> {
    let r = h.get("range")?.strip_prefix("bytes=")?;
    let (a, b) = r.split_once('-')?;
    if len == 0 {
        return None;
    }
    let (s, e) = if a.is_empty() { (len.saturating_sub(b.parse().ok()?), len - 1) } else { (a.parse().ok()?, if b.is_empty() { len - 1 } else { b.parse::<usize>().ok()?.min(len - 1) }) };
    (s <= e && s < len).then_some((s, e))
}

async fn run(c: Call, bucket: String, key: String) -> Response {
    let key_id = c.qm.get("X-Amz-Credential").or(c.h.get("authorization")).map(|v| v.split("Credential=").last().unwrap_or(v).split('/').next().unwrap_or("").to_string()).unwrap_or_default();
    let Some(a) = account_for(&c, &bucket, &key_id) else {
        if c.method == "PUT" && key.is_empty() {
            return err(403, "AccessDenied", "Make an S3 account in Stuntman first (Services › S3)", &format!("/{bucket}"));
        }
        return err(404, "NoSuchBucket", "The specified bucket does not exist", &format!("/{bucket}"));
    };
    let res = format!("/{bucket}/{key}");
    // ---- service: list buckets
    if bucket.is_empty() {
        if let Who::No(r, _, _) = authorize(&c, &a, "", "", "list").await {
            return r;
        }
        let items: String = config::list(&c.ctx).iter().filter(|x| x.kind == config::S3).flat_map(|x| x.containers.keys().cloned().collect::<Vec<_>>()).map(|b| format!("<Bucket><Name>{}</Name><CreationDate>{}</CreationDate></Bucket>", x(&b), auth::ms_iso(util::now_ms()))).collect();
        return xml(200, format!("<ListAllMyBucketsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Owner><ID>stuntman</ID><DisplayName>stuntman</DisplayName></Owner><Buckets>{items}</Buckets></ListAllMyBucketsResult>"));
    }
    let exists = a.containers.contains_key(&bucket);
    // ---- bucket level
    if key.is_empty() {
        let op = match c.method.as_str() { "PUT" => "put", "DELETE" => "delete", "POST" => "delete", _ => "list" };
        let (via, link) = match authorize(&c, &a, &bucket, "", op).await {
            Who::Ok { via, link } => (via, link),
            Who::No(r, _, _) => return r,
        };
        match c.method.as_str() {
            "HEAD" => return if exists { resp(200, vec![("x-amz-bucket-region".into(), "auto".into())], Body::empty()) } else { resp(404, vec![], Body::empty()) },
            "PUT" => {
                if exists {
                    return err(409, "BucketAlreadyOwnedByYou", "Your previous request to create the named bucket succeeded and you already own it.", &format!("/{bucket}"));
                }
                if let Err(e) = config::valid_container(&bucket) {
                    return err(400, "InvalidBucketName", &e, &format!("/{bucket}"));
                }
                let mut acc = a.clone();
                acc.containers.insert(bucket.clone(), config::Container { access: "private".into() });
                if let Err(e) = config::save(&c.ctx, &acc) {
                    return err(400, "InvalidArgument", &e.message, "");
                }
                record(&c, &a, &bucket, "", "put", 200, &via, link, Some("bucket created by the app".into()), 0, "").await;
                return resp(200, vec![("location".into(), format!("/{bucket}"))], Body::empty());
            }
            "DELETE" => {
                if !exists {
                    return err(404, "NoSuchBucket", "The specified bucket does not exist", &format!("/{bucket}"));
                }
                if !store::list(&c.ctx, &a.account, &bucket, "").await.unwrap_or_default().is_empty() {
                    return err(409, "BucketNotEmpty", "The bucket you tried to delete is not empty", &format!("/{bucket}"));
                }
                let mut acc = a.clone();
                acc.containers.remove(&bucket);
                let _ = config::save(&c.ctx, &acc);
                return resp(204, vec![], Body::empty());
            }
            "POST" if c.qm.contains_key("delete") => return delete_many(&c, &a, &bucket, via, link).await,
            _ => {
                if !exists {
                    return err(404, "NoSuchBucket", "The specified bucket does not exist", &format!("/{bucket}"));
                }
                if c.qm.contains_key("location") {
                    return xml(200, "<LocationConstraint xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">auto</LocationConstraint>".into());
                }
                return list(&c, &a, &bucket, via, link).await;
            }
        }
    }
    if !exists {
        return err(404, "NoSuchBucket", "The specified bucket does not exist", &format!("/{bucket}"));
    }
    // ---- object level
    let op = match c.method.as_str() {
        "GET" => "get",
        "HEAD" => "head",
        "DELETE" => "delete",
        "PUT" if c.h.contains_key("x-amz-copy-source") && !c.qm.contains_key("partNumber") => "copy",
        _ => "put",
    };
    let (via, link) = match authorize(&c, &a, &bucket, &key, op).await {
        Who::Ok { via, link } => (via, link),
        Who::No(r, why, link) => {
            let st = r.status().as_u16();
            record(&c, &a, &bucket, &key, op, st, if link.is_some() { "presigned" } else { "key" }, link, Some(why), 0, "").await;
            return r;
        }
    };
    // Multipart steps are recorded once, at the commit.
    if c.qm.contains_key("uploadId") || c.qm.contains_key("uploads") {
        return multipart(&c, &a, &bucket, &key, via, link).await;
    }
    let cur = store::get(&c.ctx, &a.account, &bucket, &key, op == "get").await.ok().flatten();
    let size = if op == "put" { c.body.len() as u64 } else { cur.as_ref().map(|b| b.size as u64).unwrap_or(0) };
    let ctype = c.h.get("content-type").cloned().unwrap_or_default();
    let seen = rules::Seen { op: if op == "copy" { "put".into() } else { op.into() }, account: a.account.clone(), container: bucket.clone(), path: key.clone(), size, ctype: ctype.clone(), caller: c.caller.clone() };
    let mods = match gate::check(&c.ctx, &a, &seen).await {
        Ok(Ok(m)) => m,
        Ok(Err((f, why))) => {
            record(&c, &a, &bucket, &key, op, match &f { Fail::Status(s) => *s, _ => 0 }, &via, link, Some(why), size as i64, &ctype).await;
            return fail(&c, f, &res).await;
        }
        Err(e) => return err(500, "InternalError", &e.message, &res),
    };
    let why = mods.provoked.clone();
    match op {
        "put" | "copy" => {
            if let Some(s) = conditions(&c, cur.as_ref(), false) {
                record(&c, &a, &bucket, &key, op, s, &via, link, why, 0, "").await;
                return err(s, code_for(s).0, code_for(s).1, &res);
            }
            let (data, ctype, meta, headers, copied) = if op == "copy" {
                let src = auth::decode_path(c.h.get("x-amz-copy-source").map(String::as_str).unwrap_or("").split('?').next().unwrap_or("")).trim_start_matches('/').to_string();
                let (sb, sk) = src.split_once('/').unwrap_or((&src, ""));
                let sa = account_for(&c, sb, "").map(|x| x.account).unwrap_or_default();
                let Some(s) = store::get(&c.ctx, &sa, sb, sk, true).await.ok().flatten() else {
                    record(&c, &a, &bucket, &key, op, 404, &via, link, Some(format!("copy source {src} not found")), 0, "").await;
                    return err(404, "NoSuchKey", "The specified key does not exist.", &format!("/{src}"));
                };
                let replace = c.h.get("x-amz-metadata-directive").map(|d| d == "REPLACE").unwrap_or(false);
                (s.data, if replace { ctype.clone() } else { s.ctype }, if replace { meta_of(&c.h) } else { s.meta }, if replace { obj_headers(&c.h) } else { s.headers }, true)
            } else {
                if let Some(m) = c.h.get("content-md5") {
                    if *m != store::md5_b64(&c.body) {
                        record(&c, &a, &bucket, &key, op, 400, &via, link, Some("Content-MD5 did not match".into()), c.body.len() as i64, &ctype).await;
                        return err(400, "BadDigest", "The Content-MD5 you specified did not match what we received.", &res);
                    }
                }
                let sha = c.h.get("x-amz-content-sha256").cloned().unwrap_or_default();
                if sha.len() == 64 && sha != store::sha256_hex(&c.body) {
                    record(&c, &a, &bucket, &key, op, 400, &via, link, Some("x-amz-content-sha256 did not match".into()), c.body.len() as i64, &ctype).await;
                    return err(400, "XAmzContentSHA256Mismatch", "The provided 'x-amz-content-sha256' header does not match what was computed.", &res);
                }
                (c.body.clone(), ctype.clone(), meta_of(&c.h), obj_headers(&c.h), false)
            };
            let size = data.len() as i64;
            let w = ops::Write { container: bucket.clone(), path: key.clone(), data, ctype, meta, headers, by: c.caller.clone(), via: via.clone(), etag: None, truncated: false };
            match ops::write(&c.ctx, &a, w).await {
                Ok(b) => {
                    let seq = record(&c, &a, &bucket, &key, op, 200, &via, link, why, size, &b.ctype).await;
                    ops::emit(&c.ctx, &a, &b, true, if copied { "CopyBlob" } else { "PutBlob" }, &c.caller, seq).await;
                    if copied {
                        return xml(200, format!("<CopyObjectResult><LastModified>{}</LastModified><ETag>{}</ETag></CopyObjectResult>", auth::ms_iso(b.at), x(&b.etag)));
                    }
                    let mut h = vec![("etag".to_string(), b.etag.clone())];
                    if a.behaviour.versions {
                        h.push(("x-amz-version-id".into(), b.seq.to_string()));
                    }
                    resp(200, h, Body::empty())
                }
                Err(e) => err(500, "InternalError", &e.message, &res),
            }
        }
        "delete" => {
            let b = ops::delete(&c.ctx, &a, &bucket, &key).await.ok().flatten();
            let seq = record(&c, &a, &bucket, &key, op, 204, &via, link, why, b.as_ref().map(|b| b.size).unwrap_or(0), "").await;
            if let Some(b) = b {
                ops::emit(&c.ctx, &a, &b, false, "DeleteBlob", &c.caller, seq).await;
            }
            resp(204, vec![], Body::empty())
        }
        _ => {
            let Some(b) = cur else {
                record(&c, &a, &bucket, &key, op, 404, &via, link, why, 0, "").await;
                return if op == "head" { resp(404, vec![], Body::empty()) } else { err(404, "NoSuchKey", "The specified key does not exist.", &res) };
            };
            if let Some(s) = conditions(&c, Some(&b), true) {
                record(&c, &a, &bucket, &key, op, s, &via, link, why, 0, "").await;
                return if s == 304 { resp(304, vec![("etag".into(), b.etag)], Body::empty()) } else { err(s, code_for(s).0, code_for(s).1, &res) };
            }
            let mut h = obj_out(&b, &c.qm, mods.bad_md5);
            if op == "head" {
                h.push(("content-length".into(), b.size.to_string()));
                record(&c, &a, &bucket, &key, op, 200, &via, link, why, b.size, &b.ctype).await;
                return resp(200, h, Body::empty());
            }
            let (status, data) = match range(&c.h, b.data.len()) {
                Some((s, e)) => {
                    h.push(("content-range".into(), format!("bytes {s}-{e}/{}", b.data.len())));
                    (206, b.data[s..=e].to_vec())
                }
                None => (200, b.data.clone()),
            };
            h.push(("content-length".into(), data.len().to_string()));
            let why = if mods.truncate { why.or(Some("cut off halfway".into())) } else { why };
            record(&c, &a, &bucket, &key, op, status, &via, link, why, data.len() as i64, &b.ctype).await;
            resp(status, h, if mods.truncate { http::cut_body(data) } else { Body::from(data) })
        }
    }
}

async fn list(c: &Call, a: &Account, bucket: &str, via: String, link: Option<String>) -> Response {
    let seen = rules::Seen { op: "list".into(), account: a.account.clone(), container: bucket.into(), caller: c.caller.clone(), ..Default::default() };
    let why = match gate::check(&c.ctx, a, &seen).await {
        Ok(Ok(m)) => m.provoked,
        Ok(Err((f, why))) => {
            record(c, a, bucket, "", "list", match &f { Fail::Status(s) => *s, _ => 0 }, &via, link, Some(why), 0, "").await;
            return fail(c, f, &format!("/{bucket}")).await;
        }
        Err(e) => return err(500, "InternalError", &e.message, ""),
    };
    let v2 = c.qm.get("list-type").map(|v| v == "2").unwrap_or(false);
    let prefix = c.qm.get("prefix").cloned().unwrap_or_default();
    let delim = c.qm.get("delimiter").cloned().unwrap_or_default();
    let max: usize = c.qm.get("max-keys").and_then(|m| m.parse().ok()).unwrap_or(1000);
    let after = if v2 { c.qm.get("continuation-token").or(c.qm.get("start-after")).cloned().unwrap_or_default() } else { c.qm.get("marker").cloned().unwrap_or_default() };
    let all = store::list(&c.ctx, &a.account, bucket, &prefix).await.unwrap_or_default();
    let mut items: Vec<(String, Option<Blob>)> = vec![];
    let mut prefs = std::collections::BTreeSet::new();
    for b in all {
        if !delim.is_empty() {
            if let Some(i) = b.path[prefix.len()..].find(&delim) {
                let p = b.path[..prefix.len() + i + delim.len()].to_string();
                if prefs.insert(p.clone()) {
                    items.push((p, None));
                }
                continue;
            }
        }
        items.push((b.path.clone(), Some(b)));
    }
    items.sort_by(|x, y| x.0.cmp(&y.0));
    let start = items.iter().position(|(n, _)| *n > after).unwrap_or(items.len());
    let page: Vec<_> = items[start..].iter().take(max).cloned().collect();
    let truncated = start + max < items.len();
    let mut body = String::new();
    for (n, b) in &page {
        match b {
            None => body.push_str(&format!("<CommonPrefixes><Prefix>{}</Prefix></CommonPrefixes>", x(n))),
            Some(b) => body.push_str(&format!("<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>", x(n), auth::ms_iso(b.at), x(&b.etag), b.size)),
        }
    }
    let last = page.last().map(|p| p.0.clone()).unwrap_or_default();
    record(c, a, bucket, &prefix, "list", 200, &via, link, why, page.len() as i64, "").await;
    let tail = if v2 {
        format!("<KeyCount>{}</KeyCount>{}{}", page.len(), if truncated { format!("<NextContinuationToken>{}</NextContinuationToken>", x(&last)) } else { String::new() }, c.qm.get("continuation-token").map(|t| format!("<ContinuationToken>{}</ContinuationToken>", x(t))).unwrap_or_default())
    } else {
        format!("<Marker>{}</Marker>{}", x(&after), if truncated { format!("<NextMarker>{}</NextMarker>", x(&last)) } else { String::new() })
    };
    xml(200, format!(
        "<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>{}</Name><Prefix>{}</Prefix><Delimiter>{}</Delimiter><MaxKeys>{max}</MaxKeys><IsTruncated>{truncated}</IsTruncated>{tail}{body}</ListBucketResult>",
        x(bucket), x(&prefix), x(&delim)
    ))
}

async fn delete_many(c: &Call, a: &Account, bucket: &str, via: String, link: Option<String>) -> Response {
    let text = String::from_utf8_lossy(&c.body).to_string();
    let keys: Vec<String> = regex::Regex::new(r"<Key>([^<]*)</Key>").unwrap().captures_iter(&text).map(|m| m[1].replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")).collect();
    let quiet = text.contains("<Quiet>true</Quiet>");
    let mut out = String::new();
    for k in keys {
        let b = ops::delete(&c.ctx, a, bucket, &k).await.ok().flatten();
        let seq = record(c, a, bucket, &k, "delete", 204, &via, link.clone(), None, b.as_ref().map(|b| b.size).unwrap_or(0), "").await;
        if let Some(b) = b {
            ops::emit(&c.ctx, a, &b, false, "DeleteBlob", &c.caller, seq).await;
        }
        if !quiet {
            out.push_str(&format!("<Deleted><Key>{}</Key></Deleted>", x(&k)));
        }
    }
    xml(200, format!("<DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{out}</DeleteResult>"))
}

async fn multipart(c: &Call, a: &Account, bucket: &str, key: &str, via: String, link: Option<String>) -> Response {
    let res = format!("/{bucket}/{key}");
    if c.method == "POST" && c.qm.contains_key("uploads") {
        return match store::new_upload(&c.ctx, &a.account, bucket, key, c.h.get("content-type").map(String::as_str).unwrap_or(""), &meta_of(&c.h)).await {
            Ok(id) => xml(200, format!("<InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Bucket>{}</Bucket><Key>{}</Key><UploadId>{id}</UploadId></InitiateMultipartUploadResult>", x(bucket), x(key))),
            Err(e) => err(500, "InternalError", &e.message, &res),
        };
    }
    let id = c.qm.get("uploadId").cloned().unwrap_or_default();
    let Some((_, _, _, ctype, meta)) = store::upload(&c.ctx, &id).await.ok().flatten() else {
        return err(404, "NoSuchUpload", "The specified upload does not exist.", &res);
    };
    match c.method.as_str() {
        "PUT" => {
            let n: i64 = c.qm.get("partNumber").and_then(|p| p.parse().ok()).unwrap_or(1);
            match store::put_part(&c.ctx, &id, n, c.body.clone()).await {
                Ok(etag) => resp(200, vec![("etag".into(), etag)], Body::empty()),
                Err(e) => err(500, "InternalError", &e.message, &res),
            }
        }
        "DELETE" => {
            let _ = store::abort_upload(&c.ctx, &id).await;
            resp(204, vec![], Body::empty())
        }
        _ => {
            let text = String::from_utf8_lossy(&c.body).to_string();
            let parts: Vec<i64> = regex::Regex::new(r"<PartNumber>(\d+)</PartNumber>").unwrap().captures_iter(&text).filter_map(|m| m[1].parse().ok()).collect();
            let seen = rules::Seen { op: "put".into(), account: a.account.clone(), container: bucket.into(), path: key.into(), ctype: ctype.clone(), caller: c.caller.clone(), ..Default::default() };
            let why = match gate::check(&c.ctx, a, &seen).await {
                Ok(Ok(m)) => m.provoked,
                Ok(Err((f, why))) => {
                    record(c, a, bucket, key, "put", match &f { Fail::Status(s) => *s, _ => 0 }, &via, link, Some(why), 0, &ctype).await;
                    return fail(c, f, &res).await;
                }
                Err(e) => return err(500, "InternalError", &e.message, &res),
            };
            let (data, etag) = match store::finish_upload(&c.ctx, &id, parts).await {
                Ok(Ok(x)) => x,
                Ok(Err(p)) => return err(400, "InvalidPart", &format!("One or more of the specified parts could not be found: {p}"), &res),
                Err(e) => return err(500, "InternalError", &e.message, &res),
            };
            let size = data.len() as i64;
            let w = ops::Write { container: bucket.into(), path: key.into(), data, ctype, meta, headers: BTreeMap::new(), by: c.caller.clone(), via: via.clone(), etag: Some(etag), truncated: false };
            match ops::write(&c.ctx, a, w).await {
                Ok(b) => {
                    let seq = record(c, a, bucket, key, "put", 200, &via, link, why, size, &b.ctype).await;
                    ops::emit(&c.ctx, a, &b, true, "PutBlockList", &c.caller, seq).await;
                    xml(200, format!("<CompleteMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Location>{}</Location><Bucket>{}</Bucket><Key>{}</Key><ETag>{}</ETag></CompleteMultipartUploadResult>", x(&ops::object_url(&c.ctx, a, bucket, key)), x(bucket), x(key), x(&b.etag)))
                }
                Err(e) => err(500, "InternalError", &e.message, &res),
            }
        }
    }
}
