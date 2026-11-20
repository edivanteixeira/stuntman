//! The Azure Blob REST API, Azurite style: http://host:10000/<account>/<container>/<blob>
//! (also <account>.blob.core.windows.net as a real host name). SharedKey, SAS (service and
//! account) and Bearer; Put Blob, Put Block / Block List, Get (ranges), Properties, Metadata,
//! Delete, Copy (finishes at once), List Blobs and Containers, Create/Delete Container,
//! conditional requests (If-Match 412, If-None-Match 304/409).

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

const VERSION: &str = "2024-11-04";

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

struct Out {
    status: u16,
    headers: Vec<(String, String)>,
    body: Body,
}

impl Out {
    fn new(status: u16) -> Self {
        Out { status, headers: vec![], body: Body::empty() }
    }
    fn h(mut self, k: &str, v: impl Into<String>) -> Self {
        self.headers.push((k.to_string(), v.into()));
        self
    }
    fn body(mut self, b: impl Into<Body>) -> Self {
        self.body = b.into();
        self
    }
    fn xml(self, x: String) -> Self {
        self.h("content-type", "application/xml").body(format!("\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?>{x}"))
    }
    fn into(self, version: &str) -> Response {
        let mut r = Response::new(self.body);
        *r.status_mut() = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let h = r.headers_mut();
        for (k, v) in self.headers {
            if let (Ok(k), Ok(v)) = (HeaderName::try_from(k), HeaderValue::from_str(&v)) {
                h.append(k, v);
            }
        }
        h.insert("x-ms-request-id", HeaderValue::from_str(&util::uuid()).unwrap());
        h.insert("x-ms-version", HeaderValue::from_str(version).unwrap_or(HeaderValue::from_static(VERSION)));
        h.insert("date", HeaderValue::from_str(&auth::rfc1123(util::now_ms())).unwrap());
        h.insert("server", HeaderValue::from_static("Stuntman-Azure-Blob"));
        r
    }
}

fn err(status: u16, code: &str, msg: &str) -> Out {
    let x = format!("<Error><Code>{code}</Code><Message>{}\nRequestId:{}\nTime:{}</Message></Error>", xml_escape(msg), util::uuid(), auth::ms_iso(util::now_ms()));
    let mut o = Out::new(status).h("x-ms-error-code", code).xml(x);
    if status == 503 {
        o = o.h("retry-after", "1");
    }
    o
}

fn code_for(status: u16, blob: bool) -> (&'static str, &'static str) {
    match status {
        401 => ("InvalidAuthenticationInfo", "Authentication information is not given in the correct format (key rejected)."),
        403 => ("AuthorizationFailure", "This request is not authorized to perform this operation."),
        404 if blob => ("BlobNotFound", "The specified blob does not exist."),
        404 => ("ContainerNotFound", "The specified container does not exist."),
        409 => ("BlobAlreadyExists", "The specified blob already exists."),
        412 => ("ConditionNotMet", "The condition specified using HTTP conditional header(s) is not met."),
        413 => ("RequestBodyTooLarge", "The request body is too large."),
        507 => ("InsufficientAccountStorage", "The account is full (storage limit)."),
        _ => ("ServerBusy", "The server is busy."),
    }
}

/// The parts of a request every step needs.
struct Call {
    ctx: Ctx,
    method: String,
    /// Path as sent, with the /e/<env> prefix (SharedKey signs it).
    raw_path: String,
    q: Vec<(String, String)>,
    qm: BTreeMap<String, String>,
    h: BTreeMap<String, String>,
    body: Vec<u8>,
    caller: String,
    remote: String,
    version: String,
    kill: Option<Kill>,
    /// The blob name as sent, before "\" became "/": a SAS may be signed over it.
    raw_blob: String,
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
    let h = http::lower_headers(req.headers());
    let version = h.get("x-ms-version").cloned().unwrap_or(VERSION.into());
    if ctx.state.paused() {
        return err(503, "ServerBusy", &format!("{} is paused in Stuntman", ctx.env)).into(&version);
    }
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let q = auth::query(req.uri().query().unwrap_or(""));
    let body = axum::body::to_bytes(req.into_body(), 1 << 30).await.map(|b| b.to_vec()).unwrap_or_default();
    let qm: BTreeMap<String, String> = q.iter().cloned().collect();
    ctx.live.seen(&ops::caller(&h), "Storage");
    let mut c = Call { caller: ops::caller(&h), ctx, method, raw_path, q, qm, h, body, remote, version: version.clone(), kill, raw_blob: String::new() };
    let segs: Vec<String> = path.trim_start_matches('/').splitn(3, '/').map(|s| auth::decode_path(s)).collect();
    // <account>.blob.core.windows.net: the account is the host's first label.
    let host_acct = c.h.get("host").and_then(|h| h.split('.').next()).map(String::from).unwrap_or_default();
    let (account, container, blob) = match config::get(&c.ctx, segs.first().map(String::as_str).unwrap_or("")) {
        Ok(a) => (a, segs.get(1).cloned().unwrap_or_default(), segs.get(2).cloned().unwrap_or_default()),
        Err(_) => match config::get(&c.ctx, &host_acct) {
            Ok(a) if a.kind == config::AZURE => (a, segs.first().cloned().unwrap_or_default(), segs.get(1..).map(|s| s.join("/")).unwrap_or_default()),
            _ => return err(400, "InvalidUri", &format!("No storage account “{}” in {}", segs.first().cloned().unwrap_or_default(), c.ctx.env)).into(&version),
        },
    };
    if account.kind != config::AZURE {
        return err(400, "InvalidUri", &format!("{} is an S3 account: use the S3 port {}", account.account, crate::s3_port())).into(&version);
    }
    // Azure stores "\" in a blob name as "/": a\b\c and a/b/c are the same blob.
    c.raw_blob = blob.clone();
    let blob = blob.replace('\\', "/");
    run(c, account, container, blob).await
}

enum Who {
    Ok { via: String, link: Option<String> },
    No(Out, String, Option<String>),
}

/// SharedKey, SAS or Bearer; anonymous only for public containers.
async fn authorize(c: &Call, a: &Account, container: &str, blob: &str, op: &str) -> Who {
    let any = a.access.any_key;
    let keys: Vec<(String, String)> = a.access.keys.iter().map(|k| (k.clone(), config::key_value(&c.ctx, &a.account, k))).collect();
    if let Some(sig) = c.qm.get("sig") {
        let signed_with = keys.iter().find(|(_, v)| !v.is_empty() && (auth::azure_sas(&c.qm, &a.account, container, blob, v) == *sig || auth::azure_sas(&c.qm, &a.account, container, &c.raw_blob, v) == *sig)).map(|(k, _)| k.clone());
        let s = links::Seen {
            kind: "sas",
            sig: sig.clone(),
            account: a.account.clone(),
            container: container.into(),
            path: blob.into(),
            allows: links::allows_from_sp(c.qm.get("sp").map(String::as_str).unwrap_or("")),
            start: c.qm.get("st").and_then(|s| auth::iso_ms(s)),
            expiry: c.qm.get("se").and_then(|s| auth::iso_ms(s)).unwrap_or(0),
            ips: c.qm.get("sip").cloned().unwrap_or_default(),
            protocol: c.qm.get("spr").cloned().unwrap_or_default(),
            signed_with: signed_with.clone().unwrap_or_default(),
            url: format!("{}?{}", ops::object_url(&c.ctx, a, container, blob), c.q.iter().map(|(k, v)| format!("{k}={}", auth::aws_encode(v, false))).collect::<Vec<_>>().join("&")),
        };
        return match links::on_use(&c.ctx, s, op, any || signed_with.is_some(), &c.remote, config::AZURE).await {
            Ok(Ok(id)) => Who::Ok { via: "sas".into(), link: Some(id) },
            Ok(Err(((st, code, msg, why), id))) => Who::No(err(st, code, &msg), why, Some(id)),
            Err(e) => Who::No(err(500, "InternalError", &e.message), "error".into(), None),
        };
    }
    match c.h.get("authorization") {
        Some(v) if v.starts_with("SharedKey ") => {
            let sig = v.rsplit(':').next().unwrap_or("");
            let ok = any || keys.iter().any(|(_, k)| !k.is_empty() && auth::azure_shared_key(&c.method, &c.raw_path, &c.q, &c.h, &a.account, k) == sig);
            if ok { Who::Ok { via: "key".into(), link: None } } else { Who::No(err(403, "AuthenticationFailed", "Server failed to authenticate the request. The MAC signature is not the same as any computed signature (the key may have been rotated)."), "signature".into(), None) }
        }
        Some(v) if v.starts_with("Bearer ") => {
            if any || a.access.managed_identity { Who::Ok { via: "token".into(), link: None } } else { Who::No(err(403, "AuthorizationFailure", "Managed identity tokens are off for this account."), "token".into(), None) }
        }
        _ => {
            let access = a.containers.get(container).map(|x| x.access.as_str()).unwrap_or("private");
            let public = (access == "blob" && !blob.is_empty() && (op == "get" || op == "head")) || (access == "container" && (op == "get" || op == "head" || op == "list"));
            if public { Who::Ok { via: "anonymous".into(), link: None } } else { Who::No(err(404, "ResourceNotFound", "The specified resource does not exist."), "anonymous".into(), None) }
        }
    }
}

fn meta_of(h: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    h.iter().filter_map(|(k, v)| k.strip_prefix("x-ms-meta-").map(|n| (n.to_string(), v.clone()))).collect()
}

fn blob_headers(h: &BTreeMap<String, String>, prefix: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for k in ["cache-control", "content-disposition", "content-encoding", "content-language"] {
        if let Some(v) = h.get(&format!("{prefix}{k}")).filter(|v| !v.is_empty()) {
            m.insert(k.to_string(), v.clone());
        }
    }
    m
}

fn etag_eq(a: &str, b: &str) -> bool {
    a.trim_matches('"') == b.trim_matches('"') || a == "*"
}

/// If-Match / If-None-Match against the current blob: Some(status) to stop.
fn conditions(c: &Call, cur: Option<&Blob>, read: bool) -> Option<u16> {
    if let Some(m) = c.h.get("if-match") {
        match cur {
            Some(b) if m.split(',').any(|x| etag_eq(x.trim(), &b.etag)) => {}
            _ => return Some(412),
        }
    }
    if let Some(m) = c.h.get("if-none-match") {
        if let Some(b) = cur {
            if m.trim() == "*" {
                return Some(if read { 304 } else { 409 });
            }
            if m.split(',').any(|x| etag_eq(x.trim(), &b.etag)) {
                return Some(if read { 304 } else { 412 });
            }
        }
    }
    None
}

fn props(b: &Blob, o: Out) -> Out {
    let mut o = o
        .h("etag", b.etag.clone())
        .h("last-modified", auth::rfc1123(b.at))
        .h("x-ms-creation-time", auth::rfc1123(b.at))
        .h("x-ms-blob-type", "BlockBlob")
        .h("x-ms-server-encrypted", "true")
        .h("x-ms-access-tier", "Hot")
        .h("x-ms-lease-state", "available")
        .h("x-ms-lease-status", "unlocked")
        .h("accept-ranges", "bytes")
        .h("content-type", b.ctype.clone());
    if !b.md5.is_empty() {
        o = o.h("content-md5", b.md5.clone());
    }
    for (k, v) in &b.headers {
        o = o.h(k, v.clone());
    }
    for (k, v) in &b.meta {
        o = o.h(&format!("x-ms-meta-{k}"), v.clone());
    }
    o
}

fn range(h: &BTreeMap<String, String>, len: usize) -> Option<(usize, usize)> {
    let r = h.get("x-ms-range").or(h.get("range"))?;
    let r = r.strip_prefix("bytes=")?;
    let (a, b) = r.split_once('-')?;
    if len == 0 {
        return None;
    }
    let (s, e) = if a.is_empty() {
        let n: usize = b.parse().ok()?;
        (len.saturating_sub(n), len - 1)
    } else {
        let s: usize = a.parse().ok()?;
        (s, if b.is_empty() { len - 1 } else { b.parse::<usize>().ok()?.min(len - 1) })
    };
    (s <= e && s < len).then_some((s, e))
}

async fn record(c: &Call, a: &Account, container: &str, path: &str, op: &str, status: u16, via: &str, link: Option<String>, why: Option<String>, size: i64, ctype: &str) -> Option<i64> {
    store::record(
        &c.ctx,
        Rec { account: a.account.clone(), container: container.into(), path: path.into(), op: op.into(), method: c.method.clone(), status, caller: c.caller.clone(), via: via.into(), link_id: link, provoked: why, ms: 0, size, ctype: ctype.into(), remote: c.remote.clone(), detail: String::new(), trace_id: ops::trace(&c.h) },
    )
    .await
    .ok()
}

/// Answers a provoked failure in Azure's words.
async fn fail(c: &Call, f: Fail, blob: bool) -> Response {
    match f {
        Fail::Status(s) => {
            let (code, msg) = code_for(s, blob);
            err(s, code, msg).into(&c.version)
        }
        Fail::Timeout => {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            err(504, "OperationTimedOut", "no answer").into(&c.version)
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

async fn run(c: Call, a: Account, container: String, blob: String) -> Response {
    // Azure takes these values in any case (the .NET SDK has sent comp=List, include=Metadata).
    let comp = c.qm.get("comp").map(|v| v.to_lowercase()).unwrap_or_default();
    let restype = c.qm.get("restype").map(|v| v.to_lowercase()).unwrap_or_default();
    // ---- account level
    if container.is_empty() {
        if restype == "account" || (restype == "service" && comp == "properties" && c.method == "GET") {
            return Out::new(200).h("x-ms-sku-name", "Standard_LRS").h("x-ms-account-kind", "StorageV2").h("x-ms-is-hns-enabled", "false")
                .xml("<StorageServiceProperties><DefaultServiceVersion>2024-11-04</DefaultServiceVersion></StorageServiceProperties>".into()).into(&c.version);
        }
        if comp == "list" {
            if let Who::No(o, _, _) = authorize(&c, &a, "", "", "list").await {
                return o.into(&c.version);
            }
            let prefix = c.qm.get("prefix").cloned().unwrap_or_default();
            let items: String = a.containers.keys().filter(|n| n.starts_with(&prefix))
                .map(|n| container_item(n, &a.containers[n].access, &auth::rfc1123(util::now_ms()))).collect();
            return Out::new(200).xml(format!("<EnumerationResults ServiceEndpoint=\"{}/\"><Prefix>{}</Prefix><Containers>{items}</Containers><NextMarker/></EnumerationResults>", ops::endpoint(&c.ctx, &a), xml_escape(&prefix))).into(&c.version);
        }
        return err(400, "InvalidQueryParameterValue", "Unsupported account operation").into(&c.version);
    }
    // ---- container level
    if blob.is_empty() && restype == "container" {
        let exists = a.containers.contains_key(&container);
        match (c.method.as_str(), comp.as_str()) {
            ("PUT", "") => {
                if let Who::No(o, _, _) = authorize(&c, &a, &container, "", "put").await {
                    return o.into(&c.version);
                }
                if exists {
                    return err(409, "ContainerAlreadyExists", "The specified container already exists.").into(&c.version);
                }
                if let Err(e) = config::valid_container(&container) {
                    return err(400, "InvalidResourceName", &e).into(&c.version);
                }
                let mut acc = a.clone();
                let access = c.h.get("x-ms-blob-public-access").cloned().unwrap_or("private".into());
                acc.containers.insert(container.clone(), config::Container { access });
                if let Err(e) = config::save(&c.ctx, &acc) {
                    return err(400, "InvalidHeaderValue", &e.message).into(&c.version);
                }
                record(&c, &a, &container, "", "put", 201, "key", None, Some("container created by the app".into()), 0, "").await;
                return Out::new(201).h("etag", "\"0x8DD000000000000\"").h("last-modified", auth::rfc1123(util::now_ms())).into(&c.version);
            }
            ("DELETE", "") => {
                if !exists {
                    return err(404, "ContainerNotFound", "The specified container does not exist.").into(&c.version);
                }
                if let Who::No(o, _, _) = authorize(&c, &a, &container, "", "delete").await {
                    return o.into(&c.version);
                }
                let mut acc = a.clone();
                acc.containers.remove(&container);
                let _ = config::save(&c.ctx, &acc);
                let _ = store::drop_container(&c.ctx, &a.account, &container).await;
                return Out::new(202).into(&c.version);
            }
            (_, "list") => return list_blobs(&c, &a, &container).await,
            ("GET" | "HEAD", "") | ("GET" | "HEAD", "metadata") => {
                if !exists {
                    return err(404, "ContainerNotFound", "The specified container does not exist.").into(&c.version);
                }
                let access = &a.containers[&container].access;
                let mut o = Out::new(200).h("etag", "\"0x8DD000000000000\"").h("last-modified", auth::rfc1123(util::now_ms())).h("x-ms-lease-state", "available").h("x-ms-lease-status", "unlocked");
                if access != "private" {
                    o = o.h("x-ms-blob-public-access", access.clone());
                }
                return o.into(&c.version);
            }
            _ => return Out::new(200).into(&c.version),
        }
    }
    if !a.containers.contains_key(&container) {
        return err(404, "ContainerNotFound", "The specified container does not exist.").into(&c.version);
    }
    // ---- blob level. No blob name (e.g. a client that took the container for the account) must not
    // become a blob named "": every upload would overwrite it.
    if blob.is_empty() {
        return err(400, "InvalidUri", "The requested URI does not represent any resource on the server. (No blob name in the path: container operations need restype=container.)").into(&c.version);
    }
    let op = match (c.method.as_str(), comp.as_str()) {
        ("GET", "") => "get",
        ("HEAD", "") | ("GET", "metadata") | ("GET", "properties") => "head",
        ("GET", "blocklist") => "head",
        ("DELETE", _) => "delete",
        ("PUT", _) if c.h.contains_key("x-ms-copy-source") => "copy",
        ("PUT", _) => "put",
        _ => "head",
    };
    let (via, link) = match authorize(&c, &a, &container, &blob, op).await {
        Who::Ok { via, link } => (via, link),
        Who::No(o, why, link) => {
            let st = o.status;
            record(&c, &a, &container, &blob, op, st, if link.is_some() { "sas" } else { "key" }, link, Some(why), 0, "").await;
            return o.into(&c.version);
        }
    };
    let cur = store::get(&c.ctx, &a.account, &container, &blob, op == "get").await.ok().flatten();
    let size = if op == "put" { c.body.len() as u64 } else { cur.as_ref().map(|b| b.size as u64).unwrap_or(0) };
    let ctype = c.h.get("x-ms-blob-content-type").or(c.h.get("content-type")).cloned().unwrap_or_default();
    let seen = rules::Seen { op: if op == "copy" { "put".into() } else { op.into() }, account: a.account.clone(), container: container.clone(), path: blob.clone(), size, ctype: ctype.clone(), caller: c.caller.clone() };
    // Block uploads are recorded at the commit; a staged block is not a request worth a rule.
    let staging = comp == "block";
    let mods = if staging { gate::Mods::default() } else {
        match gate::check(&c.ctx, &a, &seen).await {
            Ok(Ok(m)) => m,
            Ok(Err((f, why))) => {
                let st = match &f { Fail::Status(s) => *s, _ => 0 };
                record(&c, &a, &container, &blob, op, st, &via, link, Some(why), size as i64, &ctype).await;
                return fail(&c, f, true).await;
            }
            Err(e) => return err(500, "InternalError", &e.message).into(&c.version),
        }
    };
    let why = mods.provoked.clone();
    match (c.method.as_str(), comp.as_str()) {
        ("PUT", "block") => {
            let id = c.qm.get("blockid").cloned().unwrap_or_default();
            if let Err(e) = store::put_block(&c.ctx, &a.account, &container, &blob, &id, c.body.clone()).await {
                return err(500, "InternalError", &e.message).into(&c.version);
            }
            Out::new(201).h("content-md5", store::md5_b64(&c.body)).h("x-ms-request-server-encrypted", "true").into(&c.version)
        }
        ("PUT", "blocklist") => {
            if let Some(s) = conditions(&c, cur.as_ref(), false) {
                record(&c, &a, &container, &blob, "put", s, &via, link, why, 0, "").await;
                return err(s, code_for(s, true).0, code_for(s, true).1).into(&c.version);
            }
            let text = String::from_utf8_lossy(&c.body).to_string();
            let ids: Vec<String> = regex::Regex::new(r"<(?:Latest|Committed|Uncommitted)>([^<]*)</").unwrap().captures_iter(&text).map(|m| m[1].to_string()).collect();
            let data = match store::take_blocks(&c.ctx, &a.account, &container, &blob, ids).await {
                Ok(Ok(d)) => d,
                Ok(Err(id)) => return err(400, "InvalidBlockList", &format!("The block {id} was not uploaded.")).into(&c.version),
                Err(e) => return err(500, "InternalError", &e.message).into(&c.version),
            };
            let size = data.len() as i64;
            let w = ops::Write { container: container.clone(), path: blob.clone(), data, ctype: c.h.get("x-ms-blob-content-type").cloned().unwrap_or_default(), meta: meta_of(&c.h), headers: blob_headers(&c.h, "x-ms-blob-"), by: c.caller.clone(), via: via.clone(), etag: None, truncated: false };
            write_and_answer(&c, &a, w, size, "PutBlockList", via, link, why, 201).await
        }
        ("PUT", "metadata") | ("PUT", "properties") => {
            let Some(mut b) = store::get(&c.ctx, &a.account, &container, &blob, true).await.ok().flatten() else {
                return err(404, "BlobNotFound", "The specified blob does not exist.").into(&c.version);
            };
            if comp == "metadata" { b.meta = meta_of(&c.h); } else {
                b.headers = blob_headers(&c.h, "x-ms-blob-");
                if let Some(t) = c.h.get("x-ms-blob-content-type") { b.ctype = t.clone(); }
            }
            let w = ops::Write { container: container.clone(), path: blob.clone(), data: b.data, ctype: b.ctype, meta: b.meta, headers: b.headers, by: b.by, via: b.via, etag: None, truncated: b.truncated };
            match ops::write(&c.ctx, &Account { behaviour: config::Behaviour { versions: false, ..a.behaviour.clone() }, ..a.clone() }, w).await {
                Ok(nb) => Out::new(200).h("etag", nb.etag).h("last-modified", auth::rfc1123(nb.at)).into(&c.version),
                Err(e) => err(500, "InternalError", &e.message).into(&c.version),
            }
        }
        ("PUT", "") => {
            if let Some(s) = conditions(&c, cur.as_ref(), false) {
                record(&c, &a, &container, &blob, op, s, &via, link, why, 0, "").await;
                return err(s, code_for(s, true).0, code_for(s, true).1).into(&c.version);
            }
            if let Some(src) = c.h.get("x-ms-copy-source").cloned() {
                return copy(&c, &a, &container, &blob, &src, via, link, why).await;
            }
            if let Some(m) = c.h.get("content-md5") {
                if *m != store::md5_b64(&c.body) {
                    record(&c, &a, &container, &blob, op, 400, &via, link, Some("Content-MD5 did not match".into()), c.body.len() as i64, &ctype).await;
                    return err(400, "Md5Mismatch", "The MD5 value specified in the request did not match with the MD5 value calculated by the server.").into(&c.version);
                }
            }
            let w = ops::Write { container: container.clone(), path: blob.clone(), data: c.body.clone(), ctype: ctype.clone(), meta: meta_of(&c.h), headers: blob_headers(&c.h, "x-ms-blob-"), by: c.caller.clone(), via: via.clone(), etag: None, truncated: false };
            write_and_answer(&c, &a, w, c.body.len() as i64, "PutBlob", via, link, why, 201).await
        }
        ("DELETE", _) => {
            if let Some(s) = conditions(&c, cur.as_ref(), false) {
                record(&c, &a, &container, &blob, op, s, &via, link, why, 0, "").await;
                return err(s, code_for(s, true).0, code_for(s, true).1).into(&c.version);
            }
            match ops::delete(&c.ctx, &a, &container, &blob).await {
                Ok(Some(b)) => {
                    let seq = record(&c, &a, &container, &blob, op, 202, &via, link, why, b.size, &b.ctype).await;
                    ops::emit(&c.ctx, &a, &b, false, "DeleteBlob", &c.caller, seq).await;
                    Out::new(202).h("x-ms-delete-type-permanent", (a.behaviour.soft_delete == "off").to_string()).into(&c.version)
                }
                Ok(None) => {
                    record(&c, &a, &container, &blob, op, 404, &via, link, why, 0, "").await;
                    err(404, "BlobNotFound", "The specified blob does not exist.").into(&c.version)
                }
                Err(e) => err(500, "InternalError", &e.message).into(&c.version),
            }
        }
        ("GET", "blocklist") => Out::new(200).xml("<BlockList><CommittedBlocks/><UncommittedBlocks/></BlockList>".into()).into(&c.version),
        (m, _) => {
            let Some(b) = cur else {
                record(&c, &a, &container, &blob, op, 404, &via, link, why, 0, "").await;
                return err(404, "BlobNotFound", "The specified blob does not exist.").into(&c.version);
            };
            if let Some(s) = conditions(&c, Some(&b), true) {
                record(&c, &a, &container, &blob, op, s, &via, link, why, 0, "").await;
                return if s == 304 { Out::new(304).h("etag", b.etag).into(&c.version) } else { err(s, code_for(s, true).0, code_for(s, true).1).into(&c.version) };
            }
            let mut o = props(&b, Out::new(200));
            if mods.bad_md5 && !b.md5.is_empty() {
                o.headers.retain(|(k, _)| k != "content-md5");
                o = o.h("content-md5", store::md5_b64(b"not the content"));
            }
            for (k, hk) in [("rscd", "content-disposition"), ("rsct", "content-type"), ("rscc", "cache-control"), ("rsce", "content-encoding"), ("rscl", "content-language")] {
                if let Some(v) = c.qm.get(k) {
                    o.headers.retain(|(x, _)| x != hk);
                    o = o.h(hk, v.clone());
                }
            }
            if b.headers.contains_key("x-ms-copy-id") {
                o = o.h("x-ms-copy-status", "success");
            }
            if m == "HEAD" || op == "head" {
                o = o.h("content-length", b.size.to_string());
                record(&c, &a, &container, &blob, "head", 200, &via, link, why, b.size, &b.ctype).await;
                return o.into(&c.version);
            }
            let (status, data) = match range(&c.h, b.data.len()) {
                Some((s, e)) => {
                    o = o.h("content-range", format!("bytes {s}-{e}/{}", b.data.len()));
                    if c.h.get("x-ms-range-get-content-md5").map(|v| v == "true").unwrap_or(false) {
                        o = o.h("x-ms-blob-content-md5", b.md5.clone());
                    }
                    o.headers.retain(|(k, _)| k != "content-md5");
                    (206, b.data[s..=e].to_vec())
                }
                None => (200, b.data.clone()),
            };
            o.status = status;
            o = o.h("content-length", data.len().to_string());
            let why = if mods.truncate { why.or(Some("cut off halfway".into())) } else { why };
            record(&c, &a, &container, &blob, "get", status, &via, link, why, data.len() as i64, &b.ctype).await;
            o.body = if mods.truncate { http::cut_body(data) } else { Body::from(data) };
            o.into(&c.version)
        }
    }
}

async fn write_and_answer(c: &Call, a: &Account, w: ops::Write, size: i64, api: &str, via: String, link: Option<String>, why: Option<String>, status: u16) -> Response {
    let (container, path, ctype) = (w.container.clone(), w.path.clone(), w.ctype.clone());
    match ops::write(&c.ctx, a, w).await {
        Ok(b) => {
            let seq = record(c, a, &container, &path, "put", status, &via, link, why, size, &ctype).await;
            ops::emit(&c.ctx, a, &b, true, api, &c.caller, seq).await;
            let mut o = Out::new(status).h("etag", b.etag.clone()).h("last-modified", auth::rfc1123(b.at)).h("x-ms-request-server-encrypted", "true");
            if !b.md5.is_empty() {
                o = o.h("content-md5", b.md5.clone());
            }
            if a.behaviour.versions {
                o = o.h("x-ms-version-id", auth::ms_iso(b.at));
            }
            o.into(&c.version)
        }
        Err(e) => err(500, "InternalError", &e.message).into(&c.version),
    }
}

/// (account, container, blob) of a copy source URL: host style (<account>.blob.<domain>/<container>/<blob>,
/// as the SDK writes it for a named host) or path style (127.0.0.1:10000/[e/<env>/]<account>/<container>/<blob>).
pub fn copy_source(src: &str, prefix: &str) -> (String, String, String) {
    let rest = src.split("://").nth(1).unwrap_or(src);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = path.split('?').next().unwrap_or("").to_string();
    let path = path.strip_prefix(&format!("{}/", prefix.trim_start_matches('/'))).map(String::from).unwrap_or(path);
    let host = host.split(':').next().unwrap_or("");
    if let Some((acct, _)) = host.split_once(".blob.") {
        // On an emulator port the endpoint repeats the account in the path (see ops::endpoint).
        let path = path.strip_prefix(&format!("{acct}/")).map(String::from).unwrap_or(path);
        let parts: Vec<String> = path.splitn(2, '/').map(auth::decode_path).collect();
        return (acct.to_string(), parts.first().cloned().unwrap_or_default(), parts.get(1).cloned().unwrap_or_default().replace('\\', "/"));
    }
    let parts: Vec<String> = path.splitn(3, '/').map(auth::decode_path).collect();
    (parts.first().cloned().unwrap_or_default(), parts.get(1).cloned().unwrap_or_default(), parts.get(2).cloned().unwrap_or_default().replace('\\', "/"))
}

/// One <Container> of List Containers. A private container has no <PublicAccess> element, as in Azure: the .NET SDK
/// throws "Unknown PublicAccessType value" on an empty one.
fn container_item(name: &str, access: &str, modified: &str) -> String {
    let public = if access == "private" { String::new() } else { format!("<PublicAccess>{}</PublicAccess>", xml_escape(access)) };
    format!("<Container><Name>{}</Name><Properties><Last-Modified>{modified}</Last-Modified><Etag>\"0x8DD000000000000\"</Etag><LeaseStatus>unlocked</LeaseStatus><LeaseState>available</LeaseState>{public}<HasImmutabilityPolicy>false</HasImmutabilityPolicy><HasLegalHold>false</HasLegalHold></Properties></Container>", xml_escape(name))
}

#[cfg(test)]
mod list_t {
    #[test]
    fn private_container_has_no_public_access() {
        let private = super::container_item("acme-docs", "private", "Wed, 18 Nov 2026 13:20:02 GMT");
        assert!(!private.contains("PublicAccess"), "{private}");
        assert!(private.contains("<Name>acme-docs</Name>"));
        let blob = super::container_item("acme-pub", "blob", "Wed, 18 Nov 2026 13:20:02 GMT");
        assert!(blob.contains("<PublicAccess>blob</PublicAccess>"), "{blob}");
        let container = super::container_item("acme-all", "container", "Wed, 18 Nov 2026 13:20:02 GMT");
        assert!(container.contains("<PublicAccess>container</PublicAccess>"), "{container}");
    }
}

#[cfg(test)]
mod copy_t {
    #[test]
    fn both_styles() {
        let s = |a: &str, b: &str, c: &str| (a.to_string(), b.to_string(), c.to_string());
        assert_eq!(super::copy_source("http://acmedev.blob.acme.localhost:10000/uploads/a/b.pdf", "/e/acme"), s("acmedev", "uploads", "a/b.pdf"));
        assert_eq!(super::copy_source("http://127.0.0.1:10000/acme-docs-stg/documents/x%20y.pdf?sv=1", ""), s("acme-docs-stg", "documents", "x y.pdf"));
        assert_eq!(super::copy_source("http://127.0.0.1:10000/e/stg-b/acme/c/d", "/e/stg-b"), s("acme", "c", "d"));
        assert_eq!(super::copy_source("http://acmedev.blob.stg-a.stuntman.local:20000/uploads/a/b.pdf", ""), s("acmedev", "uploads", "a/b.pdf"));
        assert_eq!(super::copy_source("http://acmedev.blob.stg-a.stuntman.local:10000/acmedev/uploads/a/b.pdf", ""), s("acmedev", "uploads", "a/b.pdf"));
        // "\" is "/" in Azure, also in a copy source.
        assert_eq!(super::copy_source("http://127.0.0.1:10000/acme/c/a%5Cb%5Cc.xml", ""), s("acme", "c", "a/b/c.xml"));
    }
}

/// Copy Blob: the source is any blob URL of this Stuntman (same or another account); it finishes at once.
async fn copy(c: &Call, a: &Account, container: &str, blob: &str, src: &str, via: String, link: Option<String>, why: Option<String>) -> Response {
    let (sa, sc, sb) = copy_source(src, &c.ctx.prefix());
    let Some(s) = store::get(&c.ctx, &sa, &sc, &sb, true).await.ok().flatten() else {
        record(c, a, container, blob, "copy", 404, &via, link, Some(format!("copy source {sa}/{sc}/{sb} not found")), 0, "").await;
        return err(404, "CannotVerifyCopySource", "The specified blob does not exist.").into(&c.version);
    };
    let copy_id = util::uuid();
    let mut headers = s.headers.clone();
    headers.insert("x-ms-copy-id".into(), copy_id.clone());
    let meta = if meta_of(&c.h).is_empty() { s.meta.clone() } else { meta_of(&c.h) };
    let size = s.size;
    let w = ops::Write { container: container.into(), path: blob.into(), data: s.data, ctype: s.ctype, meta, headers, by: c.caller.clone(), via: via.clone(), etag: None, truncated: s.truncated };
    match ops::write(&c.ctx, a, w).await {
        Ok(b) => {
            let seq = record(c, a, container, blob, "copy", 202, &via, link, why, size, &b.ctype).await;
            ops::emit(&c.ctx, a, &b, true, "CopyBlob", &c.caller, seq).await;
            Out::new(202).h("etag", b.etag).h("last-modified", auth::rfc1123(b.at)).h("x-ms-copy-id", copy_id).h("x-ms-copy-status", "success").into(&c.version)
        }
        Err(e) => err(500, "InternalError", &e.message).into(&c.version),
    }
}

async fn list_blobs(c: &Call, a: &Account, container: &str) -> Response {
    if !a.containers.contains_key(container) {
        return err(404, "ContainerNotFound", "The specified container does not exist.").into(&c.version);
    }
    let (via, link) = match authorize(c, a, container, "", "list").await {
        Who::Ok { via, link } => (via, link),
        Who::No(o, _, _) => return o.into(&c.version),
    };
    let seen = rules::Seen { op: "list".into(), account: a.account.clone(), container: container.into(), caller: c.caller.clone(), ..Default::default() };
    let why = match gate::check(&c.ctx, a, &seen).await {
        Ok(Ok(m)) => m.provoked,
        Ok(Err((f, why))) => {
            record(c, a, container, "", "list", match &f { Fail::Status(s) => *s, _ => 0 }, &via, link, Some(why), 0, "").await;
            return fail(c, f, false).await;
        }
        Err(e) => return err(500, "InternalError", &e.message).into(&c.version),
    };
    let prefix = c.qm.get("prefix").cloned().unwrap_or_default().replace('\\', "/");
    let delim = c.qm.get("delimiter").cloned().unwrap_or_default();
    let marker = c.qm.get("marker").cloned().unwrap_or_default();
    let max: usize = c.qm.get("maxresults").and_then(|m| m.parse().ok()).unwrap_or(5000);
    let with_meta = c.qm.get("include").map(|i| i.split(',').any(|x| x.trim().eq_ignore_ascii_case("metadata"))).unwrap_or(false);
    let all = store::list(&c.ctx, &a.account, container, &prefix).await.unwrap_or_default();
    let mut items: Vec<(String, Option<Blob>)> = vec![];
    let mut seen_pref = std::collections::BTreeSet::new();
    for b in all {
        if !delim.is_empty() {
            if let Some(i) = b.path[prefix.len()..].find(&delim) {
                let p = b.path[..prefix.len() + i + delim.len()].to_string();
                if seen_pref.insert(p.clone()) {
                    items.push((p, None));
                }
                continue;
            }
        }
        items.push((b.path.clone(), Some(b)));
    }
    items.sort_by(|x, y| x.0.cmp(&y.0));
    let start = items.iter().position(|(n, _)| *n >= marker).unwrap_or(items.len());
    let page: Vec<_> = items[start..].iter().take(max).cloned().collect();
    let next = items.get(start + max).map(|(n, _)| n.clone()).unwrap_or_default();
    let mut x = String::new();
    for (name, b) in &page {
        match b {
            None => x.push_str(&format!("<BlobPrefix><Name>{}</Name></BlobPrefix>", xml_escape(name))),
            Some(b) => {
                let meta = if with_meta { format!("<Metadata>{}</Metadata>", b.meta.iter().map(|(k, v)| format!("<{k}>{}</{k}>", xml_escape(v))).collect::<String>()) } else { String::new() };
                x.push_str(&format!(
                    "<Blob><Name>{}</Name><Properties><Creation-Time>{}</Creation-Time><Last-Modified>{}</Last-Modified><Etag>{}</Etag><Content-Length>{}</Content-Length><Content-Type>{}</Content-Type><Content-Encoding>{}</Content-Encoding><Content-Language /><Content-MD5>{}</Content-MD5><Cache-Control>{}</Cache-Control><Content-Disposition>{}</Content-Disposition><BlobType>BlockBlob</BlobType><AccessTier>Hot</AccessTier><AccessTierInferred>true</AccessTierInferred><LeaseStatus>unlocked</LeaseStatus><LeaseState>available</LeaseState><ServerEncrypted>true</ServerEncrypted></Properties>{meta}</Blob>",
                    xml_escape(name), auth::rfc1123(b.at), auth::rfc1123(b.at), b.etag.trim_matches('"'), b.size, xml_escape(&b.ctype),
                    xml_escape(b.headers.get("content-encoding").map(String::as_str).unwrap_or("")), b.md5,
                    xml_escape(b.headers.get("cache-control").map(String::as_str).unwrap_or("")), xml_escape(b.headers.get("content-disposition").map(String::as_str).unwrap_or(""))
                ));
            }
        }
    }
    record(c, a, container, &prefix, "list", 200, &via, link, why, page.len() as i64, "").await;
    Out::new(200)
        .xml(format!(
            "<EnumerationResults ServiceEndpoint=\"{}/\" ContainerName=\"{}\"><Prefix>{}</Prefix><Marker>{}</Marker><MaxResults>{max}</MaxResults><Delimiter>{}</Delimiter><Blobs>{x}</Blobs><NextMarker>{}</NextMarker></EnumerationResults>",
            ops::endpoint(&c.ctx, a), xml_escape(container), xml_escape(&prefix), xml_escape(&marker), xml_escape(&delim), xml_escape(&next)
        ))
        .into(&c.version)
}
