//! K1–K5 as tools: each is an MCP tool and a control route under /api/keyvault (names from the
//! frames' "what each control calls"; create_vault_key because create_key is the e-mail service's).

use stuntman_core::{tools::Args, util, ApiError, Ctx, Tool};
use serde_json::{json, Value};

use crate::{config, crypto, store, vault::{self, CERT, KEY, SECRET}};

fn since(a: &Args, d: &str) -> Result<i64, ApiError> {
    let s = a.str("since").unwrap_or(d.into());
    Ok(util::now_ms() - util::parse_ms(&s).ok_or_else(|| ApiError::bad("since: 15m, 1h, 1d"))? as i64)
}
fn until(a: &Args) -> Result<Option<i64>, ApiError> {
    Ok(match a.str("for") { Some(f) if f != "until changed" => Some(util::now_ms() + util::parse_ms(&f).ok_or_else(|| ApiError::bad("for: 10m, 1h"))? as i64), _ => None })
}
fn who(a: &Args) -> String {
    a.str("by").unwrap_or("you".into())
}

/// Reading a value on screen (or by an agent) is an operation too: it goes to the access log.
async fn show(ctx: &Ctx, v: &config::Vault, kind: &str, name: &str, version: &str, by: &str) {
    let _ = store::log(ctx, store::Entry { vault: v.name.clone(), identity: by.into(), op: "Show".into(), kind: kind.into(), item: name.into(), version: version.into(), status: 200, why: "revealed in Stuntman".into(), ..Default::default() }).await;
}

async fn version_rows(ctx: &Ctx, v: &config::Vault, kind: &str, name: &str) -> Result<Vec<Value>, ApiError> {
    let reads = store::entries(ctx, store::Filter { vault: Some(v.name.clone()), item: Some(name.into()), since_ms: Some(util::now_ms() - 86_400_000), limit: 20_000, ..Default::default() }).await?;
    let vs = store::versions(ctx, &v.name, kind, name).await?;
    let now = vault::now_s();
    Ok(vs.iter().enumerate().map(|(i, x)| {
        let state = if x.exp.map(|e| e <= now).unwrap_or(false) { "expired" } else if !x.enabled { "disabled" } else if i == 0 { "current" } else { "enabled, older" };
        let n = reads.iter().filter(|r| r["version"] == x.id.as_str() && r["status"].as_i64().unwrap_or(0) < 400 && !["Show", "Event", "SetSecret"].contains(&r["op"].as_str().unwrap_or(""))).count();
        json!({"id": x.id, "created": x.created, "enabled": x.enabled, "nbf": x.nbf, "exp": x.exp, "state": state, "reads": n, "contentType": x.content_type, "tags": x.tags, "extra": strip(&x.extra)})
    }).collect())
}
fn strip(e: &Value) -> Value {
    let mut e = e.clone();
    if let Some(o) = e.as_object_mut() { o.remove("cer"); }
    e
}

async fn overview(ctx: &Ctx, v: &config::Vault, kind: &str) -> Result<Vec<Value>, ApiError> {
    let reads = store::reads_by_item(ctx, &v.name, util::now_ms() - 3_600_000).await?;
    let mut out = vec![];
    for n in store::names(ctx, &v.name, kind).await? {
        let cur = store::current(ctx, &v.name, kind, &n).await?.unwrap_or_default();
        let p = store::provoked(ctx, &v.name, kind, &n).await?;
        let days_left = cur.exp.map(|e| (e - vault::now_s()) / 86_400);
        out.push(json!({"name": n, "readsLastHour": reads.get(&n).copied().unwrap_or(0), "enabled": cur.enabled, "exp": cur.exp, "daysLeft": days_left, "version": cur.id,
            "provoked": p.map(|(a, u, b)| json!({"answer": a, "until": u, "by": b})), "extra": strip(&cur.extra), "contentType": cur.content_type, "tags": cur.tags}));
    }
    Ok(out)
}

async fn refusals(ctx: &Ctx, vault: &str, item: &str) -> Result<Value, ApiError> {
    Ok(json!(store::entries(ctx, store::Filter { vault: Some(vault.into()), item: Some(item.into()), result: Some("refused".into()), since_ms: Some(util::now_ms() - 3_600_000), limit: 3, ..Default::default() }).await?))
}

pub fn all() -> Vec<Tool> {
    vec![
        // ---------------------------------------------------------------- vaults
        Tool::new("list_vaults", "Vaults in this environment: name, URL, counts of secrets, keys and certificates.", |ctx, _a| async move {
            let mut out = vec![];
            for v in config::list(&ctx) {
                let (s, k, c) = (store::names(&ctx, &v.name, SECRET).await?.len(), store::names(&ctx, &v.name, KEY).await?.len(), store::names(&ctx, &v.name, CERT).await?.len());
                out.push(json!({"name": v.name, "kind": v.kind, "url": format!("https://{}:{}", config::host(&ctx, &v.name), crate::port()), "secrets": s, "keys": k, "certificates": c, "anyToken": v.access.any_token}));
            }
            let env_secrets: Vec<String> = std::fs::read_to_string(ctx.path("secrets.yaml")).ok().and_then(|t| serde_yaml::from_str::<std::collections::BTreeMap<String, serde_yaml::Value>>(&t).ok()).map(|m| m.into_keys().collect()).unwrap_or_default();
            Ok(json!({"vaults": out, "envSecrets": env_secrets, "urlTemplate": format!("https://{}:{}", config::host(&ctx, "{name}"), crate::port()), "imds": format!("http://localhost:{}{}/metadata/identity/oauth2/token", std::env::var("STUNTMAN_PORT").unwrap_or("8025".into()), ctx.prefix()), "port": crate::port(), "bindError": crate::tls::BIND_ERROR.lock().unwrap().clone()}))
        }).route("GET", "/keyvault/vaults"),
        // ---------------------------------------------------------------- K1 secrets
        Tool::new("list_secrets", "Secrets of a vault: names, current version, state, expiry and reads in the last hour.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let deleted: Vec<Value> = store::deleted(&ctx, &v.name, SECRET).await?.into_iter().map(|(n, at)| json!({"name": n, "deletedAt": at})).collect();
            Ok(json!({"vault": v.name, "secrets": overview(&ctx, &v, SECRET).await?, "deleted": deleted, "askedFor": store::asked_missing(&ctx, &v.name, SECRET, util::now_ms() - 86_400_000).await?}))
        }).param("vault", "default: the first vault").route("GET", "/keyvault/secrets"),
        Tool::new("secret_detail", "One secret without its value: versions with reads, who reads it, provocation, events, and the files that reference secret://<name>.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            let p = store::provoked(&ctx, &v.name, SECRET, &n).await?;
            Ok(json!({"vault": v.name, "name": n, "versions": version_rows(&ctx, &v, SECRET, &n).await?, "readers": store::readers(&ctx, &v.name, &n, util::now_ms() - 3_600_000).await?,
                "provoked": p.map(|(a, u, b)| json!({"answer": a, "until": u, "by": b})), "events": store::events_of(&ctx, &v.name, &n).await?, "usedBy": vault::used_by(&ctx, &n), "subscribers": v.subscribers, "refusals": refusals(&ctx, &v.name, &n).await?}))
        }).req("name", "secret name").param("vault", "vault").route("GET", "/keyvault/secrets/{name}/detail"),
        Tool::new("get_secret", "The value (synthetic) and attributes of a secret version; recorded in the access log.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            let x = store::one(&ctx, &v.name, SECRET, &n, a.str("version").as_deref()).await?.ok_or_else(|| ApiError::not_found(format!("No secret {n} in {}", v.name)))?;
            show(&ctx, &v, SECRET, &n, &x.id, &who(&a)).await;
            Ok(json!({"name": n, "version": x.id, "value": x.value, "contentType": x.content_type, "enabled": x.enabled, "exp": x.exp, "tags": x.tags}))
        }).req("name", "secret name").param("version", "default current").param("vault", "vault").param("by", "who").route("GET", "/keyvault/secrets/{name}"),
        Tool::new("set_secret", "A new version of a secret (creates it); SecretNewVersionCreated goes to subscribers. Values are synthetic: never paste a real one.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            let val = a.need("value")?;
            if val.starts_with("eyJ") && val.len() > 200 || val.starts_with("sk_live") || val.starts_with("AKIA") { return Err(ApiError::bad("That looks like a real token: Stuntman keeps synthetic values only")); }
            let exp = match a.str("expires") { Some(e) => Some(vault::now_s() + util::parse_ms(&e).ok_or_else(|| ApiError::bad("expires: 30d, 12h"))? as i64 / 1000), None => None };
            let x = vault::set_secret(&ctx, &v, &n, &val, &a.str("content_type").unwrap_or_default(), a.get("tags").cloned().unwrap_or(json!({})), None, exp, true).await?;
            let _ = store::log(&ctx, store::Entry { vault: v.name.clone(), identity: who(&a), op: "SetSecret".into(), kind: SECRET.into(), item: n.clone(), version: x.id.clone(), status: 200, why: "from Stuntman".into(), ..Default::default() }).await;
            Ok(json!({"name": n, "version": x.id}))
        }).req("name", "secret name").req("value", "synthetic value").param("expires", "e.g. 30d").param("content_type", "text/plain").param("tags", "{owner: messaging}").param("vault", "vault").param("by", "who")
          .route("PUT", "/keyvault/secrets/{name}"),
        Tool::new("set_secret_state", "Disables, enables or expires a secret version (default the current one).", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            let x = store::one(&ctx, &v.name, SECRET, &n, a.str("version").as_deref()).await?.ok_or_else(|| ApiError::not_found(format!("No secret {n}")))?;
            let exp = match a.str("expires") { Some(e) if e == "now" => Some(Some(vault::now_s() - 1)), Some(e) if e == "never" => Some(None), Some(e) => Some(Some(vault::now_s() + util::parse_ms(&e).ok_or_else(|| ApiError::bad("expires: now, never, 30d"))? as i64 / 1000)), None => None };
            store::update(&ctx, &v.name, SECRET, &n, &x.id, a.bool("enabled"), None, exp, None, None).await?;
            Ok(json!({"name": n, "version": x.id}))
        }).req("name", "secret name").param("enabled", "true or false").param("expires", "now, never or 30d").param("version", "default current").param("vault", "vault").route("PATCH", "/keyvault/secrets/{name}"),
        Tool::new("provoke_secret", "Makes the next reads of a secret fail: 403, 404, disabled, 429 or slow (3 s), for a while or until changed; normal stops it.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let ans = a.need("answer")?;
            if !["normal", "403", "404", "disabled", "429", "slow"].contains(&ans.as_str()) { return Err(ApiError::bad("answer: normal, 403, 404, disabled, 429 or slow")); }
            store::provoke(&ctx, &v.name, SECRET, &a.need("name")?, &ans, until(&a)?, &who(&a)).await?;
            Ok(json!({"answer": ans}))
        }).req("name", "secret name").req("answer", "normal | 403 | 404 | disabled | 429 | slow").param("for", "10m (default: until changed)").param("vault", "vault").param("by", "who").route("POST", "/keyvault/secrets/{name}/provoke"),
        Tool::new("secret_reads", "Who read a secret, how often, and whether it looks cached (per process).", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            Ok(json!({"name": n, "readers": store::readers(&ctx, &v.name, &n, since(&a, "1h")?).await?}))
        }).req("name", "secret name").param("since", "1h").param("vault", "vault").route("GET", "/keyvault/secrets/{name}/reads"),
        Tool::new("secret_used_by", "Files in this environment that point at secret://<name> (also before the secret exists).", |ctx, a| async move {
            Ok(json!({"usedBy": vault::used_by(&ctx, &a.need("name")?)}))
        }).req("name", "secret name").route("GET", "/keyvault/used-by/{name}"),
        Tool::new("delete_secret", "Soft-deletes a secret (recreating the name gets 409 until recovered or purged).", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            store::soft_delete(&ctx, &v.name, SECRET, &n).await?;
            Ok(json!({"deleted": n}))
        }).req("name", "secret name").param("vault", "vault").route("DELETE", "/keyvault/secrets/{name}"),
        Tool::new("recover_item", "Recovers a soft-deleted secret, key or certificate.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let kind = a.str("kind").unwrap_or(SECRET.into());
            let ok = store::recover(&ctx, &v.name, &kind, &a.need("name")?).await?;
            if !ok { return Err(ApiError::not_found("Nothing deleted under that name")); }
            Ok(json!({"recovered": true}))
        }).req("name", "name").param("kind", "secret | key | certificate").param("vault", "vault").route("POST", "/keyvault/recover/{name}"),
        // ---------------------------------------------------------------- K2 keys
        Tool::new("list_keys", "Keys of a vault: type, allowed operations, versions, operations in the last hour.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            Ok(json!({"vault": v.name, "keys": overview(&ctx, &v, KEY).await?, "askedFor": store::asked_missing(&ctx, &v.name, KEY, util::now_ms() - 86_400_000).await?}))
        }).param("vault", "vault").route("GET", "/keyvault/keys"),
        Tool::new("key_detail", "One key: operations in the last hour (with p95), versions, who uses it, provocation, public key.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            let cur = store::current(&ctx, &v.name, KEY, &n).await?.ok_or_else(|| ApiError::not_found(format!("No key {n}")))?;
            let rows = store::entries(&ctx, store::Filter { vault: Some(v.name.clone()), item: Some(n.clone()), since_ms: Some(util::now_ms() - 3_600_000), limit: 20_000, ..Default::default() }).await?;
            let mut ops = vec![];
            for op in ["Sign", "Verify", "WrapKey", "UnwrapKey", "Encrypt", "Decrypt"] {
                let mut ms: Vec<i64> = rows.iter().filter(|r| r["op"] == op && r["status"].as_i64().unwrap_or(0) < 400).map(|r| r["ms"].as_i64().unwrap_or(0)).collect();
                ms.sort();
                let p95 = ms.get(((ms.len() as f64) * 0.95) as usize).or(ms.last()).copied();
                ops.push(json!({"op": op, "count": ms.len(), "p95": p95}));
            }
            let p = store::provoked(&ctx, &v.name, KEY, &n).await?;
            let allowed: Vec<String> = cur.extra["ops"].as_array().map(|x| x.iter().filter_map(|y| y.as_str().map(String::from)).collect()).unwrap_or_default();
            Ok(json!({"vault": v.name, "name": n, "kty": cur.extra["kty"], "size": cur.extra["size"], "allowed": allowed, "operations": ops, "versions": version_rows(&ctx, &v, KEY, &n).await?,
                "readers": store::readers(&ctx, &v.name, &n, util::now_ms() - 3_600_000).await?, "provoked": p.map(|(a, u, b)| json!({"answer": a, "until": u, "by": b})),
                "refusals": refusals(&ctx, &v.name, &n).await?, "jwk": crypto::jwk(&cur.value, &format!("https://{}:{}/keys/{n}/{}", config::host(&ctx, &v.name), crate::port(), cur.id), &allowed).map_err(ApiError::internal)?}))
        }).req("name", "key name").param("vault", "vault").route("GET", "/keyvault/keys/{name}/detail"),
        Tool::new("create_vault_key", "Creates a key, or a new version of it (rotation; KeyNewVersionCreated to subscribers). RSA 2048/3072/4096.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            let t = a.str("type").unwrap_or("RSA".into());
            if !t.starts_with("RSA") { return Err(ApiError::bad("type: RSA (EC keys are not in Stuntman yet)")); }
            let exp = match a.str("expires") { Some(e) if e != "never" => Some(vault::now_s() + util::parse_ms(&e).ok_or_else(|| ApiError::bad("expires: 365d, 730d or never"))? as i64 / 1000), _ => None };
            let tags = a.get("tags").cloned().filter(|t| t.is_object()).unwrap_or(json!({}));
            let x = vault::create_key(&ctx, &v, &n, a.u64("size").unwrap_or(2048) as usize, a.list("ops"), tags, exp).await?;
            Ok(json!({"name": n, "version": x.id}))
        }).req("name", "key name").param("type", "RSA").param("size", "2048").param("ops", "sign, verify, wrapKey, unwrapKey, encrypt, decrypt").param("expires", "365d, 730d or never (default)").param("tags", "{owner: signing}").param("vault", "vault").route("POST", "/keyvault/keys/{name}"),
        Tool::new("set_key_state", "Disables or enables a key's current version.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            let x = store::current(&ctx, &v.name, KEY, &n).await?.ok_or_else(|| ApiError::not_found(format!("No key {n}")))?;
            let exp = match a.str("expires") { Some(e) if e == "now" => Some(Some(vault::now_s() - 1)), Some(e) if e == "never" => Some(None), Some(e) => Some(Some(vault::now_s() + util::parse_ms(&e).ok_or_else(|| ApiError::bad("expires: now, never, 365d"))? as i64 / 1000)), None => None };
            store::update(&ctx, &v.name, KEY, &n, &x.id, a.bool("enabled"), None, exp, None, None).await?;
            Ok(json!({"name": n, "enabled": a.bool("enabled")}))
        }).req("name", "key name").param("enabled", "true or false").param("expires", "now, never or 365d").param("vault", "vault").route("PATCH", "/keyvault/keys/{name}"),
        Tool::new("provoke_key", "Makes the next operations on a key fail: disabled, 403, 429, slow (2 s) or wrong_signature (sign returns one that fails verify).", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let ans = a.need("answer")?;
            if !["normal", "disabled", "403", "429", "slow", "wrong_signature"].contains(&ans.as_str()) { return Err(ApiError::bad("answer: normal, disabled, 403, 429, slow or wrong_signature")); }
            store::provoke(&ctx, &v.name, KEY, &a.need("name")?, &ans, until(&a)?, &who(&a)).await?;
            Ok(json!({"answer": ans}))
        }).req("name", "key name").req("answer", "normal | disabled | 403 | 429 | slow | wrong_signature").param("for", "10m").param("vault", "vault").param("by", "who").route("POST", "/keyvault/keys/{name}/provoke"),
        Tool::new("key_public", "The public part of a key, as JWK or PEM, to verify outside.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            let x = match store::current(&ctx, &v.name, KEY, &n).await? { Some(x) => x, None => store::current(&ctx, &v.name, CERT, &n).await?.ok_or_else(|| ApiError::not_found(format!("No key {n}")))? };
            Ok(if a.str("format").as_deref() == Some("pem") { json!({"pem": crypto::public_pem(&x.value).map_err(ApiError::internal)?}) } else {
                json!({"jwk": crypto::jwk(&x.value, &format!("https://{}:{}/keys/{n}/{}", config::host(&ctx, &v.name), crate::port(), x.id), &[]).map_err(ApiError::internal)?}) })
        }).req("name", "key name").param("format", "jwk | pem").param("vault", "vault").route("GET", "/keyvault/keys/{name}/public"),
        // ---------------------------------------------------------------- K3 certificates
        Tool::new("list_certificates", "Certificates: subject, issuer, validity, days left, readers.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            Ok(json!({"vault": v.name, "certificates": overview(&ctx, &v, CERT).await?, "askedFor": store::asked_missing(&ctx, &v.name, CERT, util::now_ms() - 86_400_000).await?}))
        }).param("vault", "vault").route("GET", "/keyvault/certificates"),
        Tool::new("certificate_detail", "One certificate: subject, issuer, thumbprint, validity, how to get it, readers, provocation.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            let cur = store::current(&ctx, &v.name, CERT, &n).await?.ok_or_else(|| ApiError::not_found(format!("No certificate {n}")))?;
            let p = store::provoked(&ctx, &v.name, CERT, &n).await?;
            let der = vault::cert_der_for(&cur, p.as_ref().map(|x| x.0.as_str())).map_err(ApiError::internal)?;
            let (subj, issuer, nb, na) = crypto::cert_info(&der);
            let tp = crypto::thumbprint_sha1(&der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
            Ok(json!({"vault": v.name, "name": n, "subject": subj, "issuer": issuer, "thumbprint": tp, "notBefore": nb, "notAfter": na, "daysLeft": (na - vault::now_s()) / 86_400,
                "key": format!("RSA {}  ·  exportable", crypto::bits(&cur.value)), "renewal": "auto, 30 days before expiry", "version": cur.id, "versions": version_rows(&ctx, &v, CERT, &n).await?,
                "readers": store::readers(&ctx, &v.name, &n, util::now_ms() - 3_600_000).await?, "provoked": p.map(|(a, u, b)| json!({"answer": a, "until": u, "by": b})), "events": store::events_of(&ctx, &v.name, &n).await?, "refusals": refusals(&ctx, &v.name, &n).await?}))
        }).req("name", "certificate").param("vault", "vault").route("GET", "/keyvault/certificates/{name}/detail"),
        Tool::new("issue_certificate", "Issues a certificate from the Stuntman Test CA (RSA 2048 key, exportable).", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            let x = vault::issue_cert(&ctx, &v, &n, &a.str("subject").unwrap_or(format!("CN={n}")), a.list("san"), a.u64("days").unwrap_or(365) as i64, None, None).await?;
            Ok(json!({"name": n, "version": x.id}))
        }).req("name", "certificate name").param("subject", "CN=Acme Signing STG, O=Acme Corp").param("days", "365").param("san", "dns names").param("vault", "vault").route("POST", "/keyvault/certificates/{name}"),
        Tool::new("import_certificate", "Imports your own test certificate (.pfx in base64 with its password, or a PEM with key and certificate).", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            let (key, der) = crypto::import(&a.need("pfx_base64")?, &a.str("password").unwrap_or_default()).map_err(ApiError::bad)?;
            let x = vault::issue_cert(&ctx, &v, &n, "", vec![], 0, Some(key), Some(der)).await?;
            Ok(json!({"name": n, "version": x.id}))
        }).req("name", "certificate name").req("pfx_base64", ".pfx base64 or PEM").param("password", ".pfx password").param("vault", "vault").route("POST", "/keyvault/certificates/{name}/import"),
        Tool::new("renew_certificate", "A new version signed by the Test CA (same subject and validity); CertificateNewVersionCreated.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let n = a.need("name")?;
            let cur = store::current(&ctx, &v.name, CERT, &n).await?.ok_or_else(|| ApiError::not_found(format!("No certificate {n}")))?;
            let sans: Vec<String> = cur.extra["sans"].as_array().map(|x| x.iter().filter_map(|y| y.as_str().map(String::from)).collect()).unwrap_or_default();
            let subj = cur.extra["subject"].as_str().unwrap_or("").to_string();
            let x = vault::issue_cert(&ctx, &v, &n, &subj, sans, cur.extra["days"].as_i64().unwrap_or(365), None, None).await?;
            Ok(json!({"name": n, "version": x.id}))
        }).req("name", "certificate").param("vault", "vault").route("POST", "/keyvault/certificates/{name}/renew"),
        Tool::new("provoke_certificate", "Next reads of a certificate: missing (404), expired, not_yet_valid, wrong_password (.pfx) or untrusted (issuer); normal stops it.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("vault"))?;
            let ans = a.need("answer")?;
            if !["normal", "missing", "expired", "not_yet_valid", "wrong_password", "untrusted"].contains(&ans.as_str()) { return Err(ApiError::bad("answer: normal, missing, expired, not_yet_valid, wrong_password or untrusted")); }
            store::provoke(&ctx, &v.name, CERT, &a.need("name")?, &ans, until(&a)?, &who(&a)).await?;
            Ok(json!({"answer": ans}))
        }).req("name", "certificate").req("answer", "normal | missing | expired | not_yet_valid | wrong_password | untrusted").param("for", "10m").param("vault", "vault").param("by", "who").route("POST", "/keyvault/certificates/{name}/provoke"),
        // ---------------------------------------------------------------- K4 vault settings
        Tool::new("get_vault", "The vault block: identities with roles and scopes, managed identity endpoint, throttling, latency, soft delete, subscribers.", |ctx, a| async move {
            let v = config::pick(&ctx, a.str("name").or(a.str("vault")))?;
            let imds = store::provoked(&ctx, "*", "imds", "").await?;
            Ok(json!({"vault": v, "url": format!("https://{}:{}", config::host(&ctx, &v.name), crate::port()), "yaml": config::text(&ctx, &v.name), "roles": config::ROLES.iter().map(|(k, l)| json!({"id": k, "label": l})).collect::<Vec<_>>(),
                "imds": {"url": format!("http://localhost:{}{}/metadata/identity/oauth2/token", std::env::var("STUNTMAN_PORT").unwrap_or("8025".into()), ctx.prefix()), "provoked": imds.map(|x| x.0)},
                "deleted": {"secrets": store::deleted(&ctx, &v.name, SECRET).await?, "keys": store::deleted(&ctx, &v.name, KEY).await?, "certificates": store::deleted(&ctx, &v.name, CERT).await?}}))
        }).param("name", "vault name (default the first)").route("GET", "/keyvault/vaults/{name}"),
        Tool::new("set_vault", "Creates or changes a vault: any_token, throttle (2000/10s), latency, soft_delete_days, subscribers, seed_on_reset. A new vault imports this environment's secret:// values.", |ctx, a| async move {
            let name = a.need("name")?;
            config::valid_name(&name)?;
            let created = config::get(&ctx, &name).is_err();
            let mut v = config::get(&ctx, &name).unwrap_or(config::Vault { name: name.clone(), environment: ctx.env.to_string(), ..Default::default() });
            if let Some(x) = a.bool("any_token") { v.access.any_token = x; }
            if let Some(x) = a.str("throttle") { v.behaviour.throttle = x; }
            if let Some(x) = a.str("latency") { v.behaviour.latency = x; }
            if let Some(x) = a.u64("soft_delete_days") { v.behaviour.soft_delete_days = x as u32; }
            if let Some(x) = a.bool("seed_on_reset") { v.behaviour.seed_on_reset = x; }
            if a.get("subscribers").is_some() { v.subscribers = a.list("subscribers"); }
            if let Some(ids) = a.get("identities") { v.access.identities = serde_json::from_value(ids.clone()).map_err(|e| ApiError::bad(format!("identities: {e}")))?; }
            if v.behaviour.throttle() .is_none() && v.behaviour.throttle != "off" { return Err(ApiError::bad("throttle: like 2000/10s, or off")); }
            config::save(&ctx, &v)?;
            if created {
                let existing: std::collections::BTreeMap<String, String> = std::fs::read_to_string(ctx.path("secrets.yaml")).ok().and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default();
                for (k, val) in existing.iter().chain(config::seed_values(&ctx, &v).iter()) {
                    if config::item_name_ok(k).is_ok() && store::current(&ctx, &v.name, SECRET, k).await?.is_none() {
                        let _ = vault::set_secret(&ctx, &v, k, val, "text/plain", json!({}), None, None, true).await;
                    }
                }
            }
            ctx.live.send("keyvault.vault", &name, "changed");
            Ok(json!({"vault": v, "created": created}))
        }).req("name", "vault name").param("any_token", "true: every token reads everything").param("throttle", "2000/10s or off").param("latency", "none or 300ms")
          .param("soft_delete_days", "90").param("subscribers", "Event Grid endpoint URLs").param("identities", "[{id, roles, scope}] (replaces)").param("seed_on_reset", "load seed/secrets.yaml on reset")
          .route("PUT", "/keyvault/vaults/{name}"),
        Tool::new("set_vault_identity", "Adds or changes an identity: roles (administrator, secrets-user, secrets-officer, crypto-user, crypto-officer, certificate-user, certificates-officer) and name scopes. No roles removes it.", |ctx, a| async move {
            let mut v = config::pick(&ctx, a.str("vault"))?;
            let id = a.need("id")?;
            let roles = a.list("roles");
            for r in &roles { if !config::ROLES.iter().any(|(k, _)| k == r) { return Err(ApiError::bad(format!("Unknown role {r}"))); } }
            v.access.identities.retain(|i| i.id != id);
            if !roles.is_empty() { v.access.identities.push(config::Identity { id: id.clone(), roles, scope: a.list("scope") }); }
            config::save(&ctx, &v)?;
            ctx.live.send("keyvault.vault", &v.name, "identity");
            Ok(json!({"identities": v.access.identities}))
        }).req("id", "managed identity, e.g. acme-api-mi").param("roles", "list").param("scope", "item patterns (smtp-*); empty = whole vault").param("vault", "vault").route("PUT", "/keyvault/identities/{id}"),
        Tool::new("provoke_imds", "The managed identity endpoint answers 500, slow (5 s) or no_identity; normal stops it.", |ctx, a| async move {
            let ans = a.need("answer")?;
            if !["normal", "500", "slow", "no_identity"].contains(&ans.as_str()) { return Err(ApiError::bad("answer: normal, 500, slow or no_identity")); }
            store::provoke(&ctx, "*", "imds", "", &ans, until(&a)?, &who(&a)).await?;
            Ok(json!({"answer": ans}))
        }).req("answer", "normal | 500 | slow | no_identity").param("for", "10m").param("by", "who").route("POST", "/keyvault/imds/provoke"),
        Tool::new("would_fail", "Operations allowed only because any token is on: what a real vault would have refused.", |ctx, a| async move {
            let rows = store::entries(&ctx, store::Filter { vault: a.str("vault"), result: Some("would_fail".into()), since_ms: Some(since(&a, "1h")?), limit: 500, ..Default::default() }).await?;
            Ok(json!({"operations": rows}))
        }).param("since", "1h").param("vault", "vault").route("GET", "/keyvault/would-fail"),
        // ---------------------------------------------------------------- K5 access log
        Tool::new("vault_log", "Every operation on vault items, newest first: identity, operation, item, answer (kept 24 h).", |ctx, a| async move {
            let rows = store::entries(&ctx, store::Filter { vault: a.str("vault"), identity: a.str("identity"), op: a.str("operation"), item: a.str("item"), result: a.str("result"),
                since_ms: Some(since(&a, "1h")?), limit: a.u64("limit").unwrap_or(500) as u32 }).await?;
            Ok(json!({"entries": rows}))
        }).param("identity", "managed identity").param("operation", "GetSecret, Sign…").param("item", "name").param("result", "ok | refused | would_fail | 403").param("since", "1h").param("limit", "500").param("vault", "vault")
          .route("GET", "/keyvault/log"),
        Tool::new("vault_log_entry", "One operation: caller claims, request, answer, trace id.", |ctx, a| async move {
            let id = a.need("id")?.parse::<i64>().map_err(|_| ApiError::bad("id is a number"))?;
            let mut e = store::entry_by_id(&ctx, id).await?;
            if let Ok(v) = config::get(&ctx, e["vault"].as_str().unwrap_or("")) {
                if let Some(i) = v.access.identities.iter().find(|i| i.id == e["identity"].as_str().unwrap_or("")) {
                    e["roles"] = json!(i.roles.iter().map(|r| config::role_label(r)).collect::<Vec<_>>());
                    e["scope"] = json!(i.scope);
                }
            }
            Ok(e)
        }).req("id", "entry id").route("GET", "/keyvault/log/{id}"),
        Tool::new("vault_patterns", "Findings in the access log: values read on every request, refusals, unknown callers, operations allowed only by any token.", |ctx, a| async move {
            Ok(json!({"findings": vault::patterns(&ctx, a.str("vault"), since(&a, "1h")?).await?}))
        }).param("since", "1h").param("vault", "vault").route("GET", "/keyvault/patterns"),
    ]
}
