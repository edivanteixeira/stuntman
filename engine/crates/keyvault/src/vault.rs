//! The provider-neutral core of Secrets: who is calling (token → identity), whether they may
//! (roles and scopes, or any token with what would fail recorded), throttling, latency and
//! provocations, the access log, events, and the item operations. The Azure adapter (azure.rs)
//! speaks Key Vault's REST on top of this; another provider would be another adapter.

use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64U, Engine};
use stuntman_core::{util, ApiError, Ctx};
use serde_json::{json, Value};

use crate::{config, crypto, store};

pub const SECRET: &str = "secret";
pub const KEY: &str = "key";
pub const CERT: &str = "certificate";

/// What an operation needs: read a secret, write one, use a key, manage keys, read or manage certificates.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Need {
    SecretRead,
    SecretWrite,
    KeyUse,
    KeyManage,
    CertRead,
    CertManage,
}

fn grants(role: &str, n: Need) -> bool {
    match role {
        "administrator" => true,
        "secrets-officer" => matches!(n, Need::SecretRead | Need::SecretWrite),
        "secrets-user" => n == Need::SecretRead,
        "crypto-officer" => matches!(n, Need::KeyUse | Need::KeyManage),
        "crypto-user" => n == Need::KeyUse,
        "certificates-officer" => matches!(n, Need::CertRead | Need::CertManage),
        "certificate-user" => matches!(n, Need::CertRead | Need::SecretRead),
        _ => false,
    }
}

#[derive(Clone, Debug, Default)]
pub struct Caller {
    pub identity: String,
    pub oid: String,
    pub claims: Value,
    pub client: String,
    pub trace_id: Option<String>,
    pub known: bool,
}

/// Claims of a bearer token (any JWT; the signature is not checked: this is a test double).
pub fn claims(token: &str) -> Value {
    token.split('.').nth(1).and_then(|p| B64U.decode(p.trim_end_matches('=')).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null)
}

pub fn caller(v: &config::Vault, token: &str, client: &str, trace: Option<String>) -> Caller {
    let c = claims(token);
    let pick = |k: &str| c[k].as_str().map(String::from).filter(|s| !s.is_empty());
    let ids: Vec<String> = ["oid", "name", "appid", "azp", "sub", "upn"].iter().filter_map(|k| pick(k)).collect();
    let found = v.access.identities.iter().find(|i| ids.iter().any(|x| x.eq_ignore_ascii_case(&i.id)));
    Caller {
        // Not configured: the token's own name, so the log still says who it was.
        identity: found.map(|i| i.id.clone()).or_else(|| ids.first().cloned()).unwrap_or_else(|| "unknown".into()),
        oid: pick("oid").unwrap_or_default(),
        claims: c,
        client: client.to_string(),
        trace_id: trace,
        known: found.is_some(),
    }
}

/// Ok((role shown in the log, what a real vault would say if any-token let it through)) or Err((status, code, message, inner)).
pub fn authorize(v: &config::Vault, c: &Caller, need: Need, item: &str) -> Result<(String, String), (u16, String, String, String)> {
    let refusal = if !c.known && c.claims.is_null() {
        Some((401, "Unauthorized".to_string(), "AKV10032: Invalid issuer or the token has no known object id for this vault.".to_string(), "TokenWithoutKnownOid".to_string()))
    } else {
        // A valid token with no identity in the vault block: no roles, so 403 like Azure RBAC.
        let id = v.access.identities.iter().find(|i| i.id == c.identity).cloned().unwrap_or_default();
        let ok_role = id.roles.iter().find(|r| grants(r, need));
        match ok_role {
            None => Some((403, "Forbidden".into(), format!("Caller is not authorized to perform action on resource. Caller: {} · roles: {}", c.identity, id.roles.iter().map(|r| config::role_label(r)).collect::<Vec<_>>().join(", ")), "ForbiddenByRbac".into())),
            Some(_) if !config::in_scope(&id.scope, item) => Some((403, "Forbidden".into(), format!("{} has no role on {item}: its scope is {}", c.identity, id.scope.join(", ")), "ForbiddenByRbac".into())),
            Some(r) => return Ok((config::role_label(r), String::new())),
        }
    };
    let r = refusal.unwrap();
    if v.access.any_token {
        Ok(("any token".into(), format!("{} {}", r.0, r.3)))
    } else {
        Err(r)
    }
}

/// 429 when the vault is over its throttle window.
pub async fn throttled(ctx: &Ctx, v: &config::Vault) -> Option<u64> {
    let (max, win) = v.behaviour.throttle()?;
    let n = store::ops_since(ctx, &v.name, util::now_ms() - win).await.ok()?;
    (n >= max).then_some(4)
}

pub async fn latency(v: &config::Vault) {
    let ms = v.behaviour.latency_ms();
    if ms > 0 { tokio::time::sleep(Duration::from_millis(ms)).await; }
}

/// Items referenced as secret://<name> in this environment's other files (who uses a secret).
pub fn used_by(ctx: &Ctx, name: &str) -> Vec<Value> {
    let needle = format!("secret://{name}");
    let mut out = vec![];
    let root = ctx.data_dir.as_ref().clone();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() { if !p.ends_with("secrets") { stack.push(p); } continue; }
            if p.extension().map(|x| x == "yaml").unwrap_or(false) {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    for l in t.lines().filter(|l| l.contains(&needle)) {
                        let rel = p.strip_prefix(&root).map(|r| r.to_string_lossy().to_string()).unwrap_or_default();
                        out.push(json!({"file": rel, "field": l.split(':').next().unwrap_or("").trim()}));
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- items

pub fn now_s() -> i64 {
    util::now_ms() / 1000
}

/// Writes a new secret version; the environment's secret://name references follow the current value.
pub async fn set_secret(ctx: &Ctx, v: &config::Vault, name: &str, value: &str, content_type: &str, tags: Value, nbf: Option<i64>, exp: Option<i64>, enabled: bool) -> Result<store::Version, ApiError> {
    config::item_name_ok(name)?;
    if store::deleted_at(ctx, &v.name, SECRET, name).await?.is_some() {
        return Err(ApiError::new(409, format!("Secret {name} is currently in a deleted but recoverable state, and its name cannot be reused; in this state, the secret can only be recovered or purged.")));
    }
    let ver = store::add(ctx, &v.name, SECRET, name, store::Version { id: store::new_version_id(), created: now_s(), enabled, nbf, exp, value: value.into(), content_type: content_type.into(), tags, extra: json!({}) }).await?;
    let _ = util::set_secret(&ctx.data_dir, name, value);
    event(ctx, v, SECRET, name, "SecretNewVersionCreated", &ver.id, ver.nbf, ver.exp).await;
    Ok(ver)
}

pub async fn create_key(ctx: &Ctx, v: &config::Vault, name: &str, size: usize, ops: Vec<String>, tags: Value, exp: Option<i64>) -> Result<store::Version, ApiError> {
    config::item_name_ok(name)?;
    if store::deleted_at(ctx, &v.name, KEY, name).await?.is_some() {
        return Err(ApiError::new(409, format!("Key {name} is currently in a deleted but recoverable state.")));
    }
    let size = if size == 0 { 2048 } else { size };
    let pem = tokio::task::spawn_blocking(move || crypto::generate(size)).await.map_err(|e| ApiError::internal(e.to_string()))?.map_err(ApiError::internal)?;
    let ops = if ops.is_empty() { vec!["sign".into(), "verify".into(), "wrapKey".into(), "unwrapKey".into(), "encrypt".into(), "decrypt".into()] } else { ops };
    let ver = store::add(ctx, &v.name, KEY, name, store::Version { id: store::new_version_id(), created: now_s(), enabled: true, exp, value: pem, tags, extra: json!({"kty": "RSA", "size": size, "ops": ops}), ..Default::default() }).await?;
    event(ctx, v, KEY, name, "KeyNewVersionCreated", &ver.id, None, None).await;
    Ok(ver)
}

pub async fn issue_cert(ctx: &Ctx, v: &config::Vault, name: &str, subject: &str, sans: Vec<String>, days: i64, key_pem: Option<String>, imported_der: Option<Vec<u8>>) -> Result<store::Version, ApiError> {
    config::item_name_ok(name)?;
    let key = match key_pem { Some(k) => k, None => tokio::task::spawn_blocking(|| crypto::generate(2048)).await.map_err(|e| ApiError::internal(e.to_string()))?.map_err(ApiError::internal)? };
    let cn = subject.trim_start_matches("CN=").split(',').next().unwrap_or(name).trim().to_string();
    let der = match imported_der {
        Some(d) => d,
        None => stuntman_apis::ca::get().issue(&cn, &sans, &crypto::pkcs8_der(&key).map_err(ApiError::internal)?, 0, days.max(1), false).map_err(ApiError::internal)?,
    };
    let (subj, issuer, nb, na) = crypto::cert_info(&der);
    let ver = store::add(ctx, &v.name, CERT, name, store::Version { id: store::new_version_id(), created: now_s(), enabled: true, nbf: Some(nb), exp: Some(na), value: key,
        content_type: "application/x-pkcs12".into(), tags: json!({}), extra: json!({"cer": crypto::b64(&der), "subject": subj, "issuer": issuer, "sans": sans, "days": days, "renew_before_days": 30, "exportable": true}) }).await?;
    event(ctx, v, CERT, name, "CertificateNewVersionCreated", &ver.id, Some(nb), Some(na)).await;
    Ok(ver)
}

/// A certificate version's DER as a provocation wants it (expired, not yet valid, untrusted issuer).
pub fn cert_der_for(ver: &store::Version, answer: Option<&str>) -> Result<Vec<u8>, String> {
    let der = crypto::unb64(ver.extra["cer"].as_str().unwrap_or(""))?;
    let (from, untrusted) = match answer { Some("expired") => (-400, false), Some("not_yet_valid") => (30, false), Some("untrusted") => (0, true), _ => return Ok(der) };
    let (subj, ..) = crypto::cert_info(&der);
    let cn = subj.split(',').find_map(|p| p.trim().strip_prefix("CN=")).unwrap_or("certificate").to_string();
    let sans: Vec<String> = ver.extra["sans"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    stuntman_apis::ca::get().issue(&cn, &sans, &crypto::pkcs8_der(&ver.value)?, from, 365, untrusted)
}

// ---------------------------------------------------------------- events

/// Event Grid-shaped Key Vault event to the vault's subscribers (recorded in the access log too).
pub async fn event(ctx: &Ctx, v: &config::Vault, kind: &str, name: &str, ev: &str, version: &str, nbf: Option<i64>, exp: Option<i64>) {
    if store::event_sent(ctx, &v.name, kind, name, ev, version).await.unwrap_or(true) { return; }
    let obj = match kind { SECRET => "Secret", KEY => "Key", _ => "Certificate" };
    let body = json!([{"id": util::uuid(), "topic": format!("/subscriptions/stuntman/resourceGroups/{}/providers/Microsoft.KeyVault/vaults/{}", ctx.env, v.name),
        "subject": name, "eventType": format!("Microsoft.KeyVault.{ev}"), "eventTime": util::now_ms(), "dataVersion": "1", "metadataVersion": "1",
        "data": {"Id": format!("https://{}/{}s/{name}/{version}", config::host(ctx, &v.name), obj.to_lowercase()), "VaultName": v.name, "ObjectType": obj, "ObjectName": name, "Version": version, "NBF": nbf, "EXP": exp}}]);
    let mut status = 0u16;
    for url in &v.subscribers {
        status = ctx.http.post(url).header("aeg-event-type", "Notification").json(&body).timeout(Duration::from_secs(10)).send().await.map(|r| r.status().as_u16()).unwrap_or(0);
    }
    let _ = store::log(ctx, store::Entry { vault: v.name.clone(), identity: "stuntman".into(), op: "Event".into(), kind: kind.into(), item: name.into(), version: version.into(),
        status: if v.subscribers.is_empty() { 204 } else { status }, request: body.to_string(), why: format!("{ev} to {} subscriber(s)", v.subscribers.len()), ..Default::default() }).await;
}

/// Near-expiry events (30 days before), once per version.
pub async fn sweep(ctx: &Ctx) {
    for v in config::list(ctx) {
        for (kind, ev) in [(SECRET, "SecretNearExpiry"), (CERT, "CertificateNearExpiry"), (SECRET, "SecretExpired"), (CERT, "CertificateExpired")] {
            for name in store::names(ctx, &v.name, kind).await.unwrap_or_default() {
                let Ok(Some(cur)) = store::current(ctx, &v.name, kind, &name).await else { continue };
                let Some(exp) = cur.exp else { continue };
                let left = exp - now_s();
                let due = if ev.ends_with("Expired") { left <= 0 } else { left > 0 && left <= 30 * 86_400 };
                if due { event(ctx, &v, kind, &name, ev, &cur.id, cur.nbf, cur.exp).await; }
            }
        }
    }
}

/// The access log's findings: reads on every request, refusals, unknown callers, would-fail.
pub async fn patterns(ctx: &Ctx, vault: Option<String>, since: i64) -> Result<Vec<Value>, ApiError> {
    let rows = store::entries(ctx, store::Filter { vault, since_ms: Some(since), limit: 20_000, ..Default::default() }).await?;
    let hours = ((util::now_ms() - since) as f64 / 3_600_000.0).max(1.0 / 60.0);
    let mut out = vec![];
    let mut reads = std::collections::BTreeMap::<(String, String, String), Vec<i64>>::new();
    for r in rows.iter().filter(|r| r["status"].as_i64().unwrap_or(0) < 400 && r["op"].as_str().map(|o| o.starts_with("Get")).unwrap_or(false)) {
        reads.entry((r["identity"].as_str().unwrap_or("").into(), r["client"].as_str().unwrap_or("").into(), r["item"].as_str().unwrap_or("").into())).or_default().push(r["at"].as_i64().unwrap_or(0));
    }
    for ((id, client, item), at) in reads {
        let n = at.len() as i64;
        if n >= 20 {
            let gap = (at.first().unwrap() - at.last().unwrap()) / (n - 1).max(1);
            if gap < 60_000 {
                out.push(json!({"kind": "not_cached", "item": item, "identity": id, "client": client, "perHour": (n as f64 / hours).round() as i64,
                    "text": format!("{item} read on every request by {id} ({}/h, {} ms apart): cache it", (n as f64 / hours).round() as i64, gap)}));
            }
        }
    }
    let mut refused = std::collections::BTreeMap::<(String, String, i64), i64>::new();
    for r in rows.iter().filter(|r| r["status"].as_i64().unwrap_or(0) >= 400 && r["op"] != "Event") {
        *refused.entry((r["identity"].as_str().unwrap_or("").into(), r["item"].as_str().unwrap_or("").into(), r["status"].as_i64().unwrap_or(0))).or_default() += 1;
    }
    for ((id, item, st), n) in refused {
        let text = if id == "unknown" { format!("{n} call(s) with an unknown token ({st}) on {item}") } else { format!("{id} refused on {item} ({st}) {n} time(s)") };
        out.push(json!({"kind": if id == "unknown" { "unknown_caller" } else { "refused" }, "item": item, "identity": id, "status": st, "count": n, "text": text}));
    }
    let wf: Vec<&Value> = rows.iter().filter(|r| !r["wouldFail"].as_str().unwrap_or("").is_empty()).collect();
    if !wf.is_empty() {
        out.push(json!({"kind": "would_fail", "count": wf.len(), "text": format!("{} operation(s) allowed only because any token is on", wf.len())}));
    }
    Ok(out)
}

/// The managed identity token Stuntman's IMDS gives an identity (an unsigned JWT with its oid).
pub fn mint(identity: &str, resource: &str) -> (String, i64) {
    let exp = now_s() + 86_399;
    let h = B64U.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let p = B64U.encode(json!({"oid": identity, "name": identity, "appid": identity, "sub": identity, "aud": resource, "iss": "https://sts.stuntman.local/stuntman/", "tid": "stuntman", "iat": now_s(), "exp": exp, "idtyp": "app"}).to_string());
    (format!("{h}.{p}.{}", B64U.encode(b"stuntman")), exp)
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn roles_scope_and_any_token() {
        let mut v = config::Vault { name: "acme-kv-stg".into(), ..Default::default() };
        v.access.any_token = false;
        v.access.identities = vec![config::Identity { id: "acme-worker-mi".into(), roles: vec!["secrets-user".into()], scope: vec!["smtp-*".into(), "zenvia-*".into()] }];
        let (tok, _) = mint("acme-worker-mi", "https://vault.stg-a.stuntman.local");
        let c = caller(&v, &tok, "app", None);
        assert!(c.known && authorize(&v, &c, Need::SecretRead, "smtp-password").is_ok());
        assert_eq!(authorize(&v, &c, Need::SecretRead, "db-connection").unwrap_err().0, 403);
        assert_eq!(authorize(&v, &c, Need::KeyUse, "smtp-key").unwrap_err().3, "ForbiddenByRbac");
        let u = caller(&v, "not-a-jwt", "app", None);
        assert_eq!(authorize(&v, &u, Need::SecretRead, "x").unwrap_err().0, 401);
        v.access.any_token = true;
        let (role, wf) = authorize(&v, &u, Need::SecretRead, "x").unwrap();
        assert!(role == "any token" && wf.starts_with("401"));
    }
}
