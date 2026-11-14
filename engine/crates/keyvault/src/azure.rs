//! Azure Key Vault's data plane (any api-version 7.x), as the Azure SDKs call it over HTTPS at
//! https://<vault>.vault.<environment domain>:<port>: the Bearer challenge, secrets, keys (real
//! sign/verify/wrap/unwrap/encrypt/decrypt), certificates (their .pfx secret and their key),
//! soft delete and recovery, Azure's error shapes, and the provocations set on screen.

use std::time::Duration;

use axum::{
    body::Bytes,
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use stuntman_core::{util, Ctx};
use serde_json::{json, Value};
use sha2::Digest;

use crate::{
    config, crypto, store,
    vault::{self, Need, CERT, KEY, SECRET},
};

fn azerr(status: u16, code: &str, message: &str, inner: Option<&str>) -> Response {
    let mut e = json!({"code": code, "message": message});
    if let Some(i) = inner { e["innererror"] = json!({"code": i}); }
    (StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST), Json(json!({"error": e}))).into_response()
}

fn not_found(kind: &str, name: &str) -> Response {
    let (code, what) = match kind { SECRET => ("SecretNotFound", "secret"), KEY => ("KeyNotFound", "key"), _ => ("CertificateNotFound", "certificate") };
    azerr(404, code, &format!("A {what} with (name/id) {name} was not found in this key vault. If you recently deleted this {what} you may be able to recover it using the correct recovery command."), None)
}

fn attrs(v: &store::Version, recover_days: u32) -> Value {
    // Azure leaves nbf/exp out when unset; the SDKs read them as numbers.
    let mut a = json!({"enabled": v.enabled, "created": v.created, "updated": v.created, "recoveryLevel": "Recoverable+Purgeable", "recoverableDays": recover_days});
    if let Some(x) = v.nbf { a["nbf"] = json!(x); }
    if let Some(x) = v.exp { a["exp"] = json!(x); }
    a
}

struct Req {
    ctx: Ctx,
    vault: config::Vault,
    base: String,
    method: Method,
    segs: Vec<String>,
    body: Value,
    caller: vault::Caller,
    started: i64,
    request: String,
}

impl Req {
    fn id(&self, kind: &str, name: &str, ver: &str) -> String {
        let coll = match kind { SECRET => "secrets", KEY => "keys", _ => "certificates" };
        if ver.is_empty() { format!("{}/{coll}/{name}", self.base) } else { format!("{}/{coll}/{name}/{ver}", self.base) }
    }
    async fn log(&self, op: &str, kind: &str, item: &str, ver: &str, status: u16, role: &str, why: &str, would_fail: &str) {
        let _ = store::log(&self.ctx, store::Entry { vault: self.vault.name.clone(), identity: self.caller.identity.clone(), oid: self.caller.oid.clone(), role: role.into(), op: op.into(),
            kind: kind.into(), item: item.into(), version: ver.into(), status, ms: util::now_ms() - self.started, client: self.caller.client.clone(), trace_id: self.caller.trace_id.clone(),
            request: self.request.clone(), why: why.into(), would_fail: would_fail.into() }).await;
    }
}

/// One data-plane request for the vault named by the host's first label.
pub async fn handle(ctx: Ctx, host: String, method: Method, path: String, query: String, h: HeaderMap, body: Bytes) -> Response {
    let vname = host.split('.').next().unwrap_or("").to_string();
    let Ok(v) = config::get(&ctx, &vname) else { return azerr(404, "VaultNotFound", &format!("No vault {vname} in {}", ctx.env), None) };
    let base = format!("https://{host}");
    let auth = h.get("authorization").and_then(|x| x.to_str().ok()).unwrap_or("").to_string();
    if !auth.to_lowercase().starts_with("bearer ") {
        // The SDK's first call: answer the challenge so it fetches a token for this vault's resource.
        let resource = format!("https://{}", host.split_once('.').map(|(_, r)| r).unwrap_or(&host).split(':').next().unwrap_or(""));
        return (StatusCode::UNAUTHORIZED, [("WWW-Authenticate", format!("Bearer authorization=\"https://login.microsoftonline.com/stuntman\", resource=\"{resource}\""))],
            Json(json!({"error": {"code": "Unauthorized", "message": "AKV10000: Request is missing a Bearer or PoP token."}}))).into_response();
    }
    let client = h.get("x-stuntman-app").or(h.get("x-duble-app")).or(h.get("user-agent")).and_then(|x| x.to_str().ok()).map(|s| s.split(['/', ' ']).next().unwrap_or("app").to_string()).unwrap_or("app".into());
    let trace = h.get("traceparent").and_then(|t| t.to_str().ok()).and_then(|t| t.split('-').nth(1)).filter(|t| t.len() == 32).map(String::from);
    let caller = vault::caller(&v, &auth[7..], &client, trace);
    if caller.identity != "unknown" { ctx.live.seen(&caller.identity, "Key Vault"); }
    let segs: Vec<String> = path.trim_matches('/').split('/').filter(|s| !s.is_empty()).map(String::from).collect();
    let request = format!("{} {}{}\nAuthorization: Bearer {}…  (oid {})\n{}", method, path, if query.is_empty() { String::new() } else { format!("?{query}") },
        &auth[7..auth.len().min(17)], caller.oid, h.get("traceparent").and_then(|t| t.to_str().ok()).map(|t| format!("traceparent: {t}")).unwrap_or_default());
    let r = Req { ctx: ctx.clone(), vault: v, base, method, segs, body: serde_json::from_slice(&body).unwrap_or(Value::Null), caller, started: util::now_ms(), request };
    if let Some(after) = vault::throttled(&ctx, &r.vault).await {
        r.log("Throttled", "", "", "", 429, "", "over the vault's throttle", "").await;
        return (StatusCode::TOO_MANY_REQUESTS, [("Retry-After", after.to_string())], Json(json!({"error": {"code": "Throttled", "message": "Request was not processed because too many requests were received. Reason: VaultRequestTypeLimitReached"}}))).into_response();
    }
    vault::latency(&r.vault).await;
    route(&r).await
}

async fn route(r: &Req) -> Response {
    let s: Vec<&str> = r.segs.iter().map(String::as_str).collect();
    let m = r.method.as_str();
    match (m, s.as_slice()) {
        ("GET", ["secrets"]) => list(r, SECRET).await,
        ("GET", ["keys"]) => list(r, KEY).await,
        ("GET", ["certificates"]) => list(r, CERT).await,
        ("GET", ["deletedsecrets"]) => list_deleted(r, SECRET).await,
        ("GET", ["deletedkeys"]) => list_deleted(r, KEY).await,
        ("GET", ["deletedcertificates"]) => list_deleted(r, CERT).await,
        ("GET", ["secrets", n, "versions"]) => versions(r, SECRET, n).await,
        ("GET", ["keys", n, "versions"]) => versions(r, KEY, n).await,
        ("GET", ["certificates", n, "versions"]) => versions(r, CERT, n).await,
        ("GET", ["secrets", n]) => get_secret(r, n, "").await,
        ("GET", ["secrets", n, ver]) => get_secret(r, n, ver).await,
        ("PUT", ["secrets", n]) => put_secret(r, n).await,
        ("PATCH", ["secrets", n, ver]) => patch(r, SECRET, n, ver).await,
        ("PATCH", ["keys", n, ver]) => patch(r, KEY, n, ver).await,
        ("DELETE", ["secrets", n]) => delete(r, SECRET, n).await,
        ("DELETE", ["keys", n]) => delete(r, KEY, n).await,
        ("DELETE", ["certificates", n]) => delete(r, CERT, n).await,
        ("GET", ["deletedsecrets", n]) => get_deleted(r, SECRET, n).await,
        ("GET", ["deletedkeys", n]) => get_deleted(r, KEY, n).await,
        ("GET", ["deletedcertificates", n]) => get_deleted(r, CERT, n).await,
        ("POST", ["deletedsecrets", n, "recover"]) => recover(r, SECRET, n).await,
        ("POST", ["deletedkeys", n, "recover"]) => recover(r, KEY, n).await,
        ("POST", ["deletedcertificates", n, "recover"]) => recover(r, CERT, n).await,
        ("DELETE", ["deletedsecrets", n]) => purge(r, SECRET, n).await,
        ("DELETE", ["deletedkeys", n]) => purge(r, KEY, n).await,
        ("DELETE", ["deletedcertificates", n]) => purge(r, CERT, n).await,
        ("POST", ["keys", n, "create"]) | ("POST", ["keys", n, "rotate"]) => create_key(r, n).await,
        ("GET", ["keys", n]) => get_key(r, n, "").await,
        ("GET", ["keys", n, ver]) => get_key(r, n, ver).await,
        ("POST", ["keys", n, op]) if is_op(op) => key_op(r, n, "", op).await,
        ("POST", ["keys", n, ver, op]) if is_op(op) => key_op(r, n, ver, op).await,
        ("POST", ["certificates", n, "create"]) => create_cert(r, n).await,
        ("POST", ["certificates", n, "import"]) => import_cert(r, n).await,
        ("GET", ["certificates", n, "pending"]) => pending(r, n).await,
        ("GET", ["certificates", n, "policy"]) => policy(r, n).await,
        ("GET", ["certificates", n]) => get_cert(r, n, "").await,
        ("GET", ["certificates", n, ver]) => get_cert(r, n, ver).await,
        _ => azerr(400, "BadParameter", &format!("{} /{} is not a Key Vault operation Stuntman knows", m, r.segs.join("/")), None),
    }
}

fn is_op(op: &str) -> bool {
    ["sign", "verify", "wrapkey", "unwrapkey", "encrypt", "decrypt"].contains(&op.to_lowercase().as_str())
}

/// Authorization + per-item provocation. Ok((role, would_fail, provoked answer)).
async fn gate(r: &Req, op: &str, kind: &str, name: &str, need: Need) -> Result<(String, String, Option<String>), Response> {
    let (role, wf) = match vault::authorize(&r.vault, &r.caller, need, name) {
        Ok(x) => x,
        Err((st, code, msg, inner)) => {
            r.log(op, kind, name, "", st, "", &msg, "").await;
            return Err(azerr(st, &code, &msg, Some(&inner)));
        }
    };
    let prov = store::provoked(&r.ctx, &r.vault.name, kind, name).await.ok().flatten();
    let answer = prov.map(|p| p.0);
    let what = match kind { SECRET => "secret", KEY => "key", _ => "certificate" };
    let refuse = |st: u16, code: &str, msg: String, inner: Option<&str>| (st, code.to_string(), msg, inner.map(String::from));
    let out = match answer.as_deref() {
        Some("403") => Some(refuse(403, "Forbidden", format!("Caller is not authorized to perform action on resource (provoked on {name})."), Some("ForbiddenByRbac"))),
        Some("404") | Some("missing") => Some(refuse(404, match kind { SECRET => "SecretNotFound", KEY => "KeyNotFound", _ => "CertificateNotFound" }, format!("A {what} with (name/id) {name} was not found in this key vault."), None)),
        Some("disabled") => Some(refuse(403, "Forbidden", format!("Operation {} is not allowed on a disabled {what}.", op.trim_start_matches("Get").to_lowercase()), Some(match kind { SECRET => "SecretDisabled", _ => "KeyDisabled" }))),
        Some("429") => Some(refuse(429, "Throttled", "Request was not processed because too many requests were received (provoked).".into(), None)),
        _ => None,
    };
    if let Some((st, code, msg, inner)) = out {
        r.log(op, kind, name, "", st, &role, &format!("provoked: {}", answer.clone().unwrap_or_default()), &wf).await;
        let mut resp = azerr(st, &code, &msg, inner.as_deref());
        if st == 429 { resp.headers_mut().insert("Retry-After", "4".parse().unwrap()); }
        return Err(resp);
    }
    if answer.as_deref() == Some("slow") { tokio::time::sleep(Duration::from_secs(if kind == KEY { 2 } else { 3 })).await; }
    Ok((role, wf, answer))
}

async fn list(r: &Req, kind: &str) -> Response {
    let need = match kind { SECRET => Need::SecretRead, KEY => Need::KeyUse, _ => Need::CertRead };
    let op = match kind { SECRET => "ListSecrets", KEY => "ListKeys", _ => "ListCertificates" };
    let (role, wf) = match vault::authorize(&r.vault, &r.caller, need, "*") { Ok(x) => x, Err((st, code, msg, inner)) => { r.log(op, kind, "", "", st, "", &msg, "").await; return azerr(st, &code, &msg, Some(&inner)); } };
    let mut out = vec![];
    for n in store::names(&r.ctx, &r.vault.name, kind).await.unwrap_or_default() {
        if let Ok(Some(v)) = store::current(&r.ctx, &r.vault.name, kind, &n).await {
            let mut o = json!({"id": r.id(kind, &n, ""), "attributes": attrs(&v, r.vault.behaviour.soft_delete_days), "tags": v.tags});
            if kind == SECRET { o["contentType"] = json!(v.content_type); }
            if kind == KEY { o["kid"] = o["id"].clone(); }
            if kind == CERT { o["x5t"] = json!(crypto::b64u(&crypto::thumbprint_sha1(&crypto::unb64(v.extra["cer"].as_str().unwrap_or("")).unwrap_or_default()))); }
            out.push(o);
        }
    }
    r.log(op, kind, "", "", 200, &role, "", &wf).await;
    Json(json!({"value": out, "nextLink": null})).into_response()
}

async fn list_deleted(r: &Req, kind: &str) -> Response {
    let out: Vec<Value> = store::deleted(&r.ctx, &r.vault.name, kind).await.unwrap_or_default().into_iter().map(|(n, at)| {
        let coll = match kind { SECRET => "deletedsecrets", KEY => "deletedkeys", _ => "deletedcertificates" };
        json!({"recoveryId": format!("{}/{coll}/{n}", r.base), "deletedDate": at / 1000, "scheduledPurgeDate": at / 1000 + r.vault.behaviour.soft_delete_days as i64 * 86_400, "id": r.id(kind, &n, "")})
    }).collect();
    Json(json!({"value": out, "nextLink": null})).into_response()
}

async fn versions(r: &Req, kind: &str, n: &str) -> Response {
    let vs = store::versions(&r.ctx, &r.vault.name, kind, n).await.unwrap_or_default();
    if vs.is_empty() { return not_found(kind, n); }
    let out: Vec<Value> = vs.iter().map(|v| json!({"id": r.id(kind, n, &v.id), "attributes": attrs(v, r.vault.behaviour.soft_delete_days), "tags": v.tags})).collect();
    Json(json!({"value": out, "nextLink": null})).into_response()
}

fn secret_json(r: &Req, n: &str, v: &store::Version) -> Value {
    json!({"value": v.value, "id": r.id(SECRET, n, &v.id), "contentType": v.content_type, "attributes": attrs(v, r.vault.behaviour.soft_delete_days), "tags": v.tags})
}

async fn get_secret(r: &Req, n: &str, ver: &str) -> Response {
    let (role, wf, answer) = match gate(r, "GetSecret", SECRET, n, Need::SecretRead).await { Ok(x) => x, Err(e) => return e };
    match store::one(&r.ctx, &r.vault.name, SECRET, n, Some(ver)).await.ok().flatten() {
        Some(v) if !v.enabled => {
            r.log("GetSecret", SECRET, n, &v.id, 403, &role, "disabled version", &wf).await;
            azerr(403, "Forbidden", "Operation get is not allowed on a disabled secret.", Some("SecretDisabled"))
        }
        Some(v) => {
            r.log("GetSecret", SECRET, n, &v.id, 200, &role, "", &wf).await;
            Json(secret_json(r, n, &v)).into_response()
        }
        None => {
            // A certificate's secret: the .pfx (or PEM) with the private key, as Key Vault gives it.
            if let Ok(Some(c)) = store::one(&r.ctx, &r.vault.name, CERT, n, Some(ver)).await {
                let cprov = store::provoked(&r.ctx, &r.vault.name, CERT, n).await.ok().flatten().map(|p| p.0);
                if cprov.as_deref() == Some("missing") { r.log("GetSecret", SECRET, n, "", 404, &role, "provoked: certificate missing", &wf).await; return not_found(SECRET, n); }
                let der = match vault::cert_der_for(&c, cprov.as_deref()) { Ok(d) => d, Err(e) => return azerr(500, "InternalError", &e, None) };
                let pwd = if cprov.as_deref() == Some("wrong_password") { util::uuid() } else { String::new() };
                let pfx = match crypto::pfx(&c.value, &der, &pwd) { Ok(p) => p, Err(e) => return azerr(500, "InternalError", &e, None) };
                r.log("GetSecret", SECRET, n, &c.id, 200, &role, "the certificate's .pfx", &wf).await;
                let _ = answer;
                return Json(json!({"value": crypto::b64(&pfx), "id": r.id(SECRET, n, &c.id), "contentType": "application/x-pkcs12", "kid": r.id(KEY, n, &c.id), "managed": true,
                    "attributes": attrs(&c, r.vault.behaviour.soft_delete_days), "tags": c.tags})).into_response();
            }
            r.log("GetSecret", SECRET, n, ver, 404, &role, "not found", &wf).await;
            not_found(SECRET, n)
        }
    }
}

async fn put_secret(r: &Req, n: &str) -> Response {
    let (role, wf, _) = match gate(r, "SetSecret", SECRET, n, Need::SecretWrite).await { Ok(x) => x, Err(e) => return e };
    let b = &r.body;
    let Some(value) = b["value"].as_str() else { return azerr(400, "BadParameter", "Property value is required.", None) };
    match vault::set_secret(&r.ctx, &r.vault, n, value, b["contentType"].as_str().unwrap_or(""), b["tags"].clone().as_object().map(|_| b["tags"].clone()).unwrap_or(json!({})),
        b["attributes"]["nbf"].as_i64(), b["attributes"]["exp"].as_i64(), b["attributes"]["enabled"].as_bool().unwrap_or(true)).await {
        Ok(v) => { r.log("SetSecret", SECRET, n, &v.id, 200, &role, "", &wf).await; Json(secret_json(r, n, &v)).into_response() }
        Err(e) if e.status == 409 => { r.log("SetSecret", SECRET, n, "", 409, &role, &e.message, &wf).await; azerr(409, "Conflict", &e.message, Some("ObjectIsDeletedButRecoverable")) }
        Err(e) => azerr(e.status, "BadParameter", &e.message, None),
    }
}

async fn patch(r: &Req, kind: &str, n: &str, ver: &str) -> Response {
    let need = if kind == SECRET { Need::SecretWrite } else { Need::KeyManage };
    let op = if kind == SECRET { "UpdateSecret" } else { "UpdateKey" };
    let (role, wf, _) = match gate(r, op, kind, n, need).await { Ok(x) => x, Err(e) => return e };
    let a = &r.body["attributes"];
    let ok = store::update(&r.ctx, &r.vault.name, kind, n, ver, a["enabled"].as_bool(), a.get("nbf").map(|x| x.as_i64()), a.get("exp").map(|x| x.as_i64()),
        r.body["contentType"].as_str().map(String::from), r.body.get("tags").cloned()).await;
    if ok.is_err() { return azerr(500, "InternalError", "update failed", None); }
    r.log(op, kind, n, ver, 200, &role, "", &wf).await;
    match store::one(&r.ctx, &r.vault.name, kind, n, Some(ver)).await.ok().flatten() {
        Some(v) if kind == SECRET => { let mut j = secret_json(r, n, &v); j.as_object_mut().map(|o| o.remove("value")); Json(j).into_response() }
        Some(v) => Json(key_json(r, n, &v)).into_response(),
        None => not_found(kind, n),
    }
}

async fn delete(r: &Req, kind: &str, n: &str) -> Response {
    let need = match kind { SECRET => Need::SecretWrite, KEY => Need::KeyManage, _ => Need::CertManage };
    let op = match kind { SECRET => "DeleteSecret", KEY => "DeleteKey", _ => "DeleteCertificate" };
    let (role, wf, _) = match gate(r, op, kind, n, need).await { Ok(x) => x, Err(e) => return e };
    if store::current(&r.ctx, &r.vault.name, kind, n).await.ok().flatten().is_none() { return not_found(kind, n); }
    let _ = store::soft_delete(&r.ctx, &r.vault.name, kind, n).await;
    r.log(op, kind, n, "", 200, &role, "soft-deleted", &wf).await;
    get_deleted(r, kind, n).await
}

async fn get_deleted(r: &Req, kind: &str, n: &str) -> Response {
    let Some(at) = store::deleted_at(&r.ctx, &r.vault.name, kind, n).await.ok().flatten() else {
        return azerr(404, match kind { SECRET => "SecretNotFound", KEY => "KeyNotFound", _ => "CertificateNotFound" }, &format!("Deleted {kind} {n} was not found"), None);
    };
    let coll = match kind { SECRET => "deletedsecrets", KEY => "deletedkeys", _ => "deletedcertificates" };
    Json(json!({"recoveryId": format!("{}/{coll}/{n}", r.base), "deletedDate": at / 1000, "scheduledPurgeDate": at / 1000 + r.vault.behaviour.soft_delete_days as i64 * 86_400, "id": r.id(kind, n, "")})).into_response()
}

async fn recover(r: &Req, kind: &str, n: &str) -> Response {
    let ok = store::recover(&r.ctx, &r.vault.name, kind, n).await.unwrap_or(false);
    r.log("Recover", kind, n, "", if ok { 200 } else { 404 }, "", "", "").await;
    if !ok { return not_found(kind, n); }
    match store::current(&r.ctx, &r.vault.name, kind, n).await.ok().flatten() {
        Some(v) if kind == SECRET => { let mut j = secret_json(r, n, &v); j.as_object_mut().map(|o| o.remove("value")); Json(j).into_response() }
        Some(v) if kind == KEY => Json(key_json(r, n, &v)).into_response(),
        Some(v) => Json(cert_json(r, n, &v, None)).into_response(),
        None => not_found(kind, n),
    }
}

async fn purge(r: &Req, kind: &str, n: &str) -> Response {
    let _ = store::purge(&r.ctx, &r.vault.name, kind, n).await;
    r.log("Purge", kind, n, "", 204, "", "", "").await;
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------- keys

fn key_json(r: &Req, n: &str, v: &store::Version) -> Value {
    let ops: Vec<String> = v.extra["ops"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    json!({"key": crypto::jwk(&v.value, &r.id(KEY, n, &v.id), &ops).unwrap_or(Value::Null), "attributes": attrs(v, r.vault.behaviour.soft_delete_days), "tags": v.tags})
}

/// A key version, or the key behind a certificate.
async fn key_version(r: &Req, n: &str, ver: &str) -> Option<store::Version> {
    if let Ok(Some(v)) = store::one(&r.ctx, &r.vault.name, KEY, n, Some(ver)).await { return Some(v); }
    store::one(&r.ctx, &r.vault.name, CERT, n, Some(ver)).await.ok().flatten().map(|mut c| { c.extra = json!({"kty": "RSA", "ops": ["sign", "verify"]}); c })
}

async fn create_key(r: &Req, n: &str) -> Response {
    let (role, wf, _) = match gate(r, "CreateKey", KEY, n, Need::KeyManage).await { Ok(x) => x, Err(e) => return e };
    let kty = r.body["kty"].as_str().unwrap_or("RSA");
    if !kty.starts_with("RSA") { return azerr(400, "BadParameter", &format!("Key type {kty} is not supported in Stuntman yet (RSA, RSA-HSM)."), None); }
    let prev = store::current(&r.ctx, &r.vault.name, KEY, n).await.ok().flatten();
    let size = r.body["key_size"].as_u64().map(|x| x as usize).or(prev.as_ref().and_then(|p| p.extra["size"].as_u64().map(|x| x as usize))).unwrap_or(2048);
    let ops: Vec<String> = r.body["key_ops"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .or(prev.as_ref().and_then(|p| p.extra["ops"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()))).unwrap_or_default();
    match vault::create_key(&r.ctx, &r.vault, n, size, ops, r.body.get("tags").cloned().unwrap_or(json!({})), r.body["attributes"]["exp"].as_i64()).await {
        Ok(v) => { r.log("CreateKey", KEY, n, &v.id, 200, &role, "", &wf).await; Json(key_json(r, n, &v)).into_response() }
        Err(e) if e.status == 409 => azerr(409, "Conflict", &e.message, Some("ObjectIsDeletedButRecoverable")),
        Err(e) => azerr(e.status, "BadParameter", &e.message, None),
    }
}

async fn get_key(r: &Req, n: &str, ver: &str) -> Response {
    let (role, wf, _) = match gate(r, "GetKey", KEY, n, Need::KeyUse).await { Ok(x) => x, Err(e) => return e };
    match key_version(r, n, ver).await {
        Some(v) => { r.log("GetKey", KEY, n, &v.id, 200, &role, "", &wf).await; Json(key_json(r, n, &v)).into_response() }
        None => { r.log("GetKey", KEY, n, ver, 404, &role, "not found", &wf).await; not_found(KEY, n) }
    }
}

async fn key_op(r: &Req, n: &str, ver: &str, op: &str) -> Response {
    let op = op.to_lowercase();
    let label = match op.as_str() { "sign" => "Sign", "verify" => "Verify", "wrapkey" => "WrapKey", "unwrapkey" => "UnwrapKey", "encrypt" => "Encrypt", _ => "Decrypt" };
    let (role, wf, answer) = match gate(r, label, KEY, n, Need::KeyUse).await { Ok(x) => x, Err(e) => return e };
    let Some(v) = key_version(r, n, ver).await else { r.log(label, KEY, n, ver, 404, &role, "not found", &wf).await; return not_found(KEY, n) };
    if !v.enabled {
        r.log(label, KEY, n, &v.id, 403, &role, "disabled key", &wf).await;
        return azerr(403, "Forbidden", &format!("Operation {op} is not allowed on a disabled key."), Some("KeyDisabled"));
    }
    // Expired (exp) or not yet valid (nbf): Azure refuses the operation the same way.
    let now = vault::now_s();
    if v.exp.map(|e| e <= now).unwrap_or(false) || v.nbf.map(|b| b > now).unwrap_or(false) {
        let why = if v.exp.map(|e| e <= now).unwrap_or(false) { "expired" } else { "not yet valid" };
        r.log(label, KEY, n, &v.id, 403, &role, &format!("{why} key"), &wf).await;
        return azerr(403, "Forbidden", &format!("Operation {op} is not allowed on an {why} key."), Some(if why == "expired" { "KeyExpired" } else { "KeyNotYetValid" }));
    }
    let allowed: Vec<String> = v.extra["ops"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect()).unwrap_or_default();
    if !allowed.is_empty() && !allowed.contains(&op) {
        r.log(label, KEY, n, &v.id, 403, &role, &format!("operation not allowed: the key allows {}", allowed.join(", ")), &wf).await;
        return azerr(403, "Forbidden", &format!("Operation {op} is not permitted on this key."), Some("KeyOperationForbidden"));
    }
    let alg = r.body["alg"].as_str().unwrap_or("").to_string();
    let val = match crypto::unb64u(r.body["value"].as_str().unwrap_or("")) { Ok(x) => x, Err(e) => return azerr(400, "BadParameter", &e, None) };
    let kid = r.id(KEY, n, &v.id);
    let out = match op.as_str() {
        "sign" => crypto::sign(&v.value, &alg, &val).map(|mut s| { if answer.as_deref() == Some("wrong_signature") { s[0] ^= 0xff; } json!({"kid": kid, "value": crypto::b64u(&s)}) }),
        "verify" => crypto::unb64u(r.body["digest"].as_str().unwrap_or("")).and_then(|d| crypto::verify(&v.value, &alg, &d, &val)).map(|ok| json!({"value": ok})),
        "wrapkey" | "encrypt" => crypto::encrypt(&v.value, &alg, &val).map(|c| json!({"kid": kid, "value": crypto::b64u(&c)})),
        _ => crypto::decrypt(&v.value, &alg, &val).map(|p| json!({"kid": kid, "value": crypto::b64u(&p)})),
    };
    match out {
        Ok(j) => { r.log(label, KEY, n, &v.id, 200, &role, if answer.as_deref() == Some("wrong_signature") && op == "sign" { "provoked: wrong signature" } else { "" }, &wf).await; Json(j).into_response() }
        Err(e) => { r.log(label, KEY, n, &v.id, 400, &role, &e, &wf).await; azerr(400, "BadParameter", &e, None) }
    }
}

// ---------------------------------------------------------------- certificates

fn cert_json(r: &Req, n: &str, v: &store::Version, answer: Option<&str>) -> Value {
    let der = vault::cert_der_for(v, answer).unwrap_or_default();
    let (subj, _, nb, na) = crypto::cert_info(&der);
    let mut a = attrs(v, r.vault.behaviour.soft_delete_days);
    a["nbf"] = json!(nb);
    a["exp"] = json!(na);
    json!({"id": r.id(CERT, n, &v.id), "kid": r.id(KEY, n, &v.id), "sid": r.id(SECRET, n, &v.id), "x5t": crypto::b64u(&crypto::thumbprint_sha1(&der)), "cer": crypto::b64(&der),
        "attributes": a, "tags": v.tags, "policy": {"id": format!("{}/certificates/{n}/policy", r.base), "x509_props": {"subject": subj, "sans": {"dns_names": v.extra["sans"]}, "validity_months": (v.extra["days"].as_i64().unwrap_or(365) / 30)},
        "key_props": {"kty": "RSA", "key_size": crypto::bits(&v.value), "exportable": true, "reuse_key": false}, "secret_props": {"contentType": "application/x-pkcs12"},
        "issuer": {"name": "Self"}, "lifetime_actions": [{"trigger": {"days_before_expiry": 30}, "action": {"action_type": "AutoRenew"}}]}})
}

async fn get_cert(r: &Req, n: &str, ver: &str) -> Response {
    let (role, wf, answer) = match gate(r, "GetCertificate", CERT, n, Need::CertRead).await { Ok(x) => x, Err(e) => return e };
    match store::one(&r.ctx, &r.vault.name, CERT, n, Some(ver)).await.ok().flatten() {
        Some(v) => { r.log("GetCertificate", CERT, n, &v.id, 200, &role, answer.as_deref().map(|a| format!("provoked: {a}")).unwrap_or_default().as_str(), &wf).await; Json(cert_json(r, n, &v, answer.as_deref())).into_response() }
        None => { r.log("GetCertificate", CERT, n, ver, 404, &role, "not found", &wf).await; not_found(CERT, n) }
    }
}

async fn create_cert(r: &Req, n: &str) -> Response {
    let (role, wf, _) = match gate(r, "CreateCertificate", CERT, n, Need::CertManage).await { Ok(x) => x, Err(e) => return e };
    let p = &r.body["policy"];
    let subject = p["x509_props"]["subject"].as_str().unwrap_or("").to_string();
    let subject = if subject.is_empty() { format!("CN={n}") } else { subject };
    let sans: Vec<String> = p["x509_props"]["sans"]["dns_names"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let days = p["x509_props"]["validity_months"].as_i64().map(|m| m * 30).unwrap_or(365);
    match vault::issue_cert(&r.ctx, &r.vault, n, &subject, sans, days, None, None).await {
        Ok(v) => {
            r.log("CreateCertificate", CERT, n, &v.id, 202, &role, "issued by the Stuntman Test CA", &wf).await;
            (StatusCode::ACCEPTED, Json(json!({"id": format!("{}/certificates/{n}/pending", r.base), "issuer": {"name": "Self"}, "csr": "", "cancellation_requested": false,
                "status": "completed", "status_details": "", "target": r.id(CERT, n, ""), "request_id": util::uuid()}))).into_response()
        }
        Err(e) => azerr(e.status, "BadParameter", &e.message, None),
    }
}

async fn pending(r: &Req, n: &str) -> Response {
    if store::current(&r.ctx, &r.vault.name, CERT, n).await.ok().flatten().is_none() { return azerr(404, "PendingCertificateNotFound", &format!("Pending certificate not found: {n}"), None); }
    Json(json!({"id": format!("{}/certificates/{n}/pending", r.base), "status": "completed", "target": r.id(CERT, n, ""), "issuer": {"name": "Self"}})).into_response()
}

async fn policy(r: &Req, n: &str) -> Response {
    match store::current(&r.ctx, &r.vault.name, CERT, n).await.ok().flatten() { Some(v) => Json(cert_json(r, n, &v, None)["policy"].clone()).into_response(), None => not_found(CERT, n) }
}

async fn import_cert(r: &Req, n: &str) -> Response {
    let (role, wf, _) = match gate(r, "ImportCertificate", CERT, n, Need::CertManage).await { Ok(x) => x, Err(e) => return e };
    let (key, der) = match crypto::import(r.body["value"].as_str().unwrap_or(""), r.body["pwd"].as_str().unwrap_or("")) { Ok(x) => x, Err(e) => { r.log("ImportCertificate", CERT, n, "", 400, &role, &e, &wf).await; return azerr(400, "BadParameter", &e, None) } };
    match vault::issue_cert(&r.ctx, &r.vault, n, "", vec![], 0, Some(key), Some(der)).await {
        Ok(v) => { r.log("ImportCertificate", CERT, n, &v.id, 200, &role, "imported", &wf).await; Json(cert_json(r, n, &v, None)).into_response() }
        Err(e) => azerr(e.status, "BadParameter", &e.message, None),
    }
}

/// Used by the plain-HTTP mirror and tests: the digest the sign operation expects.
pub fn sha256(d: &[u8]) -> Vec<u8> {
    sha2::Sha256::digest(d).to_vec()
}
