//! Signing keys (current and next, as in a JWKS rotation), the issuer and endpoints of a provider,
//! JWTs signed RS256, and the claims each flavour puts in id and access tokens.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64U, Engine};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use stuntman_core::{util, ApiError, Ctx};
use stuntman_keyvault::crypto;

use crate::{config::{self, App, Lifetimes, Provider, User}, store};

pub fn now_s() -> i64 {
    util::now_ms() / 1000
}

/// <public base>/login/<tenant>: what the app sets as its Authority (without /v2.0). The public base
/// is the environment's public_url, else the address the call came by (as Entra's discovery always
/// names its public host), else http://localhost:8025[/e/env].
pub fn base(ctx: &Ctx, p: &Provider) -> String {
    format!("{}/login/{}", ctx.public_base(), p.tenant)
}

pub fn issuer(ctx: &Ctx, p: &Provider) -> String {
    format!("{}/v2.0", base(ctx, p))
}

fn new_kid() -> String {
    util::uuid().replace('-', "")[..8].to_string()
}

/// Makes the current and next keys exist (first use, or a file written by hand). Saves the provider when it changed.
pub async fn ensure_keys(ctx: &Ctx, p: &mut Provider) -> Result<(), ApiError> {
    let mut changed = false;
    for slot in [0, 1] {
        let kid = if slot == 0 { p.keys.current.clone() } else { p.keys.next.clone() };
        if kid.is_empty() || store::key(ctx, &p.name, &kid).await?.is_none() {
            let kid = if kid.is_empty() { new_kid() } else { kid };
            let pem = tokio::task::spawn_blocking(|| crypto::generate(2048)).await.map_err(|e| ApiError::internal(e.to_string()))?.map_err(ApiError::internal)?;
            store::add_key(ctx, &p.name, &kid, &pem).await?;
            if slot == 0 { p.keys.current = kid } else { p.keys.next = kid }
            changed = true;
        }
    }
    if changed { config::save(ctx, p)?; }
    Ok(())
}

/// The next key becomes current; a new next one is published.
pub async fn rotate(ctx: &Ctx, p: &mut Provider) -> Result<(), ApiError> {
    ensure_keys(ctx, p).await?;
    p.keys.current = p.keys.next.clone();
    p.keys.next = String::new();
    ensure_keys(ctx, p).await?;
    config::save(ctx, p)
}

pub async fn jwks(ctx: &Ctx, p: &Provider, drop_current: bool) -> Result<Value, ApiError> {
    let mut keys = vec![];
    for kid in [&p.keys.current, &p.keys.next] {
        if drop_current && *kid == p.keys.current { continue; }
        if let Some(pem) = store::key(ctx, &p.name, kid).await? {
            let mut k = crypto::jwk(&pem, kid, &[]).map_err(ApiError::internal)?;
            if let Some(o) = k.as_object_mut() { o.remove("key_ops"); o.insert("use".into(), json!("sig")); o.insert("alg".into(), json!("RS256")); o.insert("issuer".into(), json!(issuer(ctx, p))); }
            keys.push(k);
        }
    }
    Ok(json!({"keys": keys}))
}

/// RS256 JWT with the given key.
pub fn sign(pem: &str, kid: &str, claims: &Value) -> Result<String, ApiError> {
    let header = json!({"typ": "JWT", "alg": "RS256", "kid": kid});
    let input = format!("{}.{}", B64U.encode(header.to_string()), B64U.encode(claims.to_string()));
    let sig = crypto::sign(pem, "RS256", &Sha256::digest(input.as_bytes())).map_err(ApiError::internal)?;
    Ok(format!("{input}.{}", B64U.encode(sig)))
}

/// Header and payload of a JWT (no check).
pub fn decode(token: &str) -> Option<(Value, Value)> {
    let mut it = token.split('.');
    let h = B64U.decode(it.next()?.trim_end_matches('=')).ok()?;
    let p = B64U.decode(it.next()?.trim_end_matches('=')).ok()?;
    Some((serde_json::from_slice(&h).ok()?, serde_json::from_slice(&p).ok()?))
}

/// What can go wrong with the tokens of one sign-in (from the "next sign-ins" provocation).
#[derive(Default, Clone, Copy)]
pub struct Twist {
    pub expired: bool,
    pub bad_signature: bool,
}

/// Signs claims with the current key, or with a key the JWKS does not have (bad signature).
pub async fn issue(ctx: &Ctx, p: &Provider, claims: &Value, twist: Twist) -> Result<String, ApiError> {
    if twist.bad_signature {
        let pem = tokio::task::spawn_blocking(|| crypto::generate(2048)).await.map_err(|e| ApiError::internal(e.to_string()))?.map_err(ApiError::internal)?;
        return sign(&pem, &p.keys.current, claims);
    }
    let pem = store::key(ctx, &p.name, &p.keys.current).await?.ok_or_else(|| ApiError::internal("signing key missing"))?;
    sign(&pem, &p.keys.current, claims)
}

/// iat, nbf, exp for a lifetime, with the tenant's clock skew and the "already expired" twist.
fn times(p: &Provider, lifetime: &str, twist: Twist) -> (i64, i64, i64) {
    let skew = Lifetimes::secs(&p.lifetimes.clock_skew, 0);
    let now = now_s() + skew;
    let life = Lifetimes::secs(lifetime, 3600);
    if twist.expired { (now - life - 300, now - life - 300, now - 300) } else { (now, now, now + life) }
}

pub const GRAPH: &str = "00000003-0000-0000-c000-000000000000";

/// upn of a guest: olivia_gmail.example.com#EXT#@contoso-stg.example.com
pub fn upn(p: &Provider, u: &User) -> String {
    if u.guest { format!("{}#EXT#@{}.example.com", u.email.replace('@', "_"), p.tenant) } else { u.email.clone() }
}

fn groups_claims(ctx: &Ctx, p: &Provider, u: &User, c: &mut Map<String, Value>) {
    if u.groups.is_empty() { return; }
    if u.groups.len() > 200 && p.flavour == "entra-v2" {
        // Groups overage, as Entra does: no groups in the token, a pointer to ask Graph instead.
        c.insert("_claim_names".into(), json!({"groups": "src1"}));
        c.insert("_claim_sources".into(), json!({"src1": {"endpoint": format!("{}/v1.0/users/{}/getMemberObjects", base(ctx, p), u.oid)}}));
    } else {
        c.insert("groups".into(), json!(u.groups));
    }
}

/// The id token's claims for a user signing in to an app.
pub fn id_claims(ctx: &Ctx, p: &Provider, u: &User, app: &App, nonce: Option<&str>, mfa_done: bool, twist: Twist) -> Value {
    let (iat, nbf, exp) = times(p, &p.lifetimes.id, twist);
    let mut c = Map::new();
    c.insert("iss".into(), json!(issuer(ctx, p)));
    c.insert("aud".into(), json!(app.client_id));
    c.insert("iat".into(), json!(iat));
    c.insert("nbf".into(), json!(nbf));
    c.insert("exp".into(), json!(exp));
    c.insert("sub".into(), json!(u.oid));
    c.insert("name".into(), json!(u.name));
    c.insert("email".into(), json!(u.email));
    if let Some(n) = nonce.filter(|n| !n.is_empty()) { c.insert("nonce".into(), json!(n)); }
    match p.flavour.as_str() {
        "google" => {
            c.insert("email_verified".into(), json!(true));
            if let Some(d) = u.email.split('@').nth(1) { c.insert("hd".into(), json!(d)); }
            c.insert("azp".into(), json!(app.client_id));
        }
        "okta" | "oidc" => {
            c.insert("preferred_username".into(), json!(u.email));
            c.insert("auth_time".into(), json!(iat));
            groups_claims(ctx, p, u, &mut c);
        }
        _ => {
            c.insert("ver".into(), json!("2.0"));
            c.insert("oid".into(), json!(u.oid));
            c.insert("tid".into(), json!(p.tenant_id));
            c.insert("preferred_username".into(), json!(upn(p, u)));
            if u.guest { c.insert("idp".into(), json!("live.com")); }
            if !app.optional_claims.is_empty() || u.groups.len() > 200 { groups_claims(ctx, p, u, &mut c); }
            if !u.roles.is_empty() { c.insert("roles".into(), json!(u.roles)); }
            c.insert("amr".into(), json!(if mfa_done { vec!["pwd", "mfa"] } else { vec!["pwd"] }));
        }
    }
    for (k, v) in &u.extra { c.insert(k.clone(), v.clone()); }
    Value::Object(c)
}

/// The access token for the resource the scopes name (an API registered here, or Microsoft Graph).
pub fn access_claims(ctx: &Ctx, p: &Provider, u: Option<&User>, app: &App, scopes: &[String], twist: Twist) -> Value {
    let (iat, nbf, exp) = times(p, &p.lifetimes.access, twist);
    let api = scopes.iter().find_map(|s| p.api_for_scope(s));
    let aud = api.map(|a| a.app_id_uri.clone()).unwrap_or(GRAPH.to_string());
    let mut c = json!({"iss": issuer(ctx, p), "aud": aud, "iat": iat, "nbf": nbf, "exp": exp, "azp": app.client_id, "tid": p.tenant_id, "ver": "2.0"});
    match u {
        Some(u) => {
            let scp: Vec<String> = scopes.iter().filter_map(|s| match api { Some(a) => s.strip_prefix(&format!("{}/", a.app_id_uri)).map(String::from), None => (!["openid", "profile", "email", "offline_access"].contains(&s.as_str())).then(|| s.clone()) })
                .chain(api.is_none().then(|| vec!["openid".to_string(), "profile".into(), "email".into()]).into_iter().flatten()).collect();
            c["sub"] = json!(u.oid); c["oid"] = json!(u.oid); c["name"] = json!(u.name); c["preferred_username"] = json!(upn(p, u));
            c["scp"] = json!(scp.join(" "));
            if !u.roles.is_empty() { c["roles"] = json!(u.roles); }
        }
        // Client credentials: the app itself, with the permissions consented for it.
        None => { c["sub"] = json!(config::guid(&app.client_id)); c["oid"] = json!(config::guid(&app.client_id)); c["roles"] = json!(app.admin_consent.iter().filter_map(|s| s.rsplit('/').next().map(String::from)).collect::<Vec<_>>()); c["idtyp"] = json!("app"); }
    }
    c
}

#[cfg(test)]
mod t {
    #[test]
    fn jwt_round_trip() {
        let pem = stuntman_keyvault::crypto::generate(2048).unwrap();
        let t = super::sign(&pem, "k1", &serde_json::json!({"sub": "x"})).unwrap();
        let (h, p) = super::decode(&t).unwrap();
        assert_eq!(h["kid"], "k1");
        assert_eq!(p["sub"], "x");
    }
}
