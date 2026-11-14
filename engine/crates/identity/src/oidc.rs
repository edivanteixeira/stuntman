//! The provider's HTTP side, under /login/<tenant>/ in each environment: OIDC discovery, JWKS, the
//! authorize page and its form, the token endpoint, logout, userinfo and a small Graph subset (me,
//! users, memberOf, getMemberObjects for the groups overage). The provider's own failures (I5) apply here.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Json, Router,
};
use base64::Engine;
use serde_json::{json, Value};
use stuntman_core::Ctx;

use crate::{config::{self, Provider}, flow::{self, Next, Who}, page, store, token};

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route("/login/{tenant}/v2.0/.well-known/openid-configuration", get(discovery))
        .route("/login/{tenant}/.well-known/openid-configuration", get(discovery))
        .route("/login/{tenant}/discovery/v2.0/keys", get(keys))
        .route("/login/{tenant}/discovery/keys", get(keys))
        .route("/login/{tenant}/oauth2/v2.0/authorize", get(authorize).post(authorize_form))
        .route("/login/{tenant}/oauth2/v2.0/token", post(token_ep))
        .route("/login/{tenant}/oauth2/v2.0/logout", get(logout))
        .route("/login/{tenant}/oidc/userinfo", get(userinfo))
        .route("/login/{tenant}/v1.0/me", get(graph_me))
        .route("/login/{tenant}/v1.0/users/{id}", get(graph_user))
        .route("/login/{tenant}/v1.0/users/{id}/memberOf", get(graph_member_of))
        .route("/login/{tenant}/v1.0/users/{id}/getMemberObjects", post(graph_member_of))
        .route("/oauth-clients/callback", get(client_callback))
        .route("/oauth-clients/{file}", get(client_metadata))
}

fn not_found(tenant: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_tenant", "error_description": format!("AADSTS90002: Tenant '{tenant}' not found. Check the Authority: identity providers in this environment are in Stuntman › Identity.")}))).into_response()
}

async fn provider(ctx: &Ctx, tenant: &str) -> Option<Provider> {
    let mut p = config::by_tenant(ctx, tenant)?;
    token::ensure_keys(ctx, &mut p).await.ok()?;
    Some(p)
}

async fn discovery(State(ctx): State<Ctx>, Path(tenant): Path<String>) -> Response {
    let Some(p) = provider(&ctx, &tenant).await else { return not_found(&tenant) };
    match p.provoke.discovery.as_str() {
        "500" => return (StatusCode::INTERNAL_SERVER_ERROR, "discovery unavailable (provoked in Stuntman)").into_response(),
        "slow" => tokio::time::sleep(std::time::Duration::from_secs(10)).await,
        _ => {}
    }
    let b = token::base(&ctx, &p);
    let iss = if p.provoke.discovery == "wrong_issuer" { format!("{}/{}/v2.0", b.rsplit_once('/').map(|x| x.0).unwrap_or(&b), p.other_tenants.first().cloned().unwrap_or("fabrikam-stg".into())) } else { token::issuer(&ctx, &p) };
    Json(json!({
        "issuer": iss, "authorization_endpoint": format!("{b}/oauth2/v2.0/authorize"), "token_endpoint": format!("{b}/oauth2/v2.0/token"),
        "jwks_uri": format!("{b}/discovery/v2.0/keys"), "end_session_endpoint": format!("{b}/oauth2/v2.0/logout"), "userinfo_endpoint": format!("{b}/oidc/userinfo"),
        "response_types_supported": ["code", "id_token", "code id_token", "id_token token"], "response_modes_supported": ["query", "fragment", "form_post"],
        "subject_types_supported": ["pairwise"], "id_token_signing_alg_values_supported": ["RS256"], "scopes_supported": ["openid", "profile", "email", "offline_access"],
        "token_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"], "code_challenge_methods_supported": ["S256", "plain"],
        "grant_types_supported": ["authorization_code", "refresh_token", "client_credentials"], "tenant_region_scope": "NA", "cloud_instance_name": "stuntman.local",
        "claims_supported": ["sub", "iss", "aud", "exp", "iat", "nbf", "name", "email", "preferred_username", "oid", "tid", "nonce", "groups", "roles", "amr"],
    })).into_response()
}

async fn keys(State(ctx): State<Ctx>, Path(tenant): Path<String>) -> Response {
    let Some(p) = provider(&ctx, &tenant).await else { return not_found(&tenant) };
    match p.provoke.jwks.as_str() {
        "unavailable" => return (StatusCode::SERVICE_UNAVAILABLE, "JWKS unavailable (provoked in Stuntman)").into_response(),
        "wrong_key" => {
            // The kids are right, the keys are not: every signature fails.
            let mut v = token::jwks(&ctx, &p, false).await.unwrap_or(json!({"keys": []}));
            if let Some(keys) = v["keys"].as_array_mut() {
                for k in keys { if let Ok(pem) = stuntman_keyvault::crypto::generate(2048) { if let Ok(j) = stuntman_keyvault::crypto::jwk(&pem, "", &[]) { k["n"] = j["n"].clone(); } } }
            }
            return Json(v).into_response();
        }
        _ => {}
    }
    match token::jwks(&ctx, &p, p.provoke.jwks == "kid_missing").await { Ok(v) => Json(v).into_response(), Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.message).into_response() }
}

fn render(p: &Provider, pid: &str, d: &Value, step: &str, env: &str) -> Response {
    let html = match step { "mfa" => page::mfa(p, pid, d, "authorize"), "consent" => page::consent(p, pid, d, "authorize"), "admin" => page::admin(p, pid, d, "authorize"), _ => page::pick(p, pid, d, env, "authorize") };
    Html(html).into_response()
}

async fn answer(ctx: &Ctx, p: &Provider, pid: &str, next: Next) -> Response {
    match next {
        Next::Page(step) => match store::get(ctx, "pending", pid).await { Ok(Some((d, ..))) => render(p, pid, &d, &step, &ctx.env), _ => Html(page::error(Some(p), "AADSTS50058", "This sign-in expired.", None)).into_response() },
        Next::Back { url, form: Some(fields), .. } => Html(page::form_post(&url, &fields)).into_response(),
        Next::Back { url, form: None, .. } => Redirect::to(&url).into_response(),
        Next::Error { code, message, back } => {
            let st = if code == "AADSTS90033" { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::BAD_REQUEST };
            (st, Html(page::error(Some(p), &code, &message, back.as_deref()))).into_response()
        }
    }
}

async fn authorize(State(ctx): State<Ctx>, Path(tenant): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(p) = provider(&ctx, &tenant).await else { return not_found(&tenant) };
    match flow::start(&ctx, &p, flow::params(&q)).await {
        Ok(pid) => { let d = store::get(&ctx, "pending", &pid).await.ok().flatten().map(|x| x.0).unwrap_or_default(); render(&p, &pid, &d, "pick", &ctx.env) }
        Err(next) => answer(&ctx, &p, "", next).await,
    }
}

async fn authorize_form(State(ctx): State<Ctx>, Path(tenant): Path<String>, Form(f): Form<HashMap<String, String>>) -> Response {
    let _w = crate::WRITE.lock().await;
    let Some(mut p) = provider(&ctx, &tenant).await else { return not_found(&tenant) };
    let pid = f.get("pending").cloned().unwrap_or_default();
    let g = |k: &str| f.get(k).cloned().unwrap_or_default();
    let next = match g("do").as_str() {
        "cancel" => flow::cancel(&ctx, &p, &pid, "you").await,
        "mfa" => flow::mfa(&ctx, &mut p, &pid, g("code").trim()).await,
        "consent" => flow::consent(&ctx, &mut p, &pid, true).await,
        _ => {
            let who = if !g("c_email").trim().is_empty() {
                Who::Claims(json!({"email": g("c_email").trim(), "name": g("c_name").trim(), "groups": g("c_groups"), "roles": g("c_roles"), "tenant": g("c_tenant").trim()}))
            } else { Who::User(g("user")) };
            flow::pick(&ctx, &mut p, &pid, who, "you").await
        }
    };
    answer(&ctx, &p, &pid, next).await
}

fn basic(h: &HeaderMap) -> Option<(String, String)> {
    let v = h.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Basic ")?;
    let d = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(v.trim()).ok()?).ok()?;
    let (u, p) = d.split_once(':')?;
    let dec = |s: &str| url_decode(s);
    Some((dec(u), dec(p)))
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() { if let Ok(x) = u8::from_str_radix(&s[i + 1..i + 3], 16) { out.push(x); i += 3; continue; } }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

async fn token_ep(State(ctx): State<Ctx>, Path(tenant): Path<String>, h: HeaderMap, Form(f): Form<HashMap<String, String>>) -> Response {
    let Some(mut p) = provider(&ctx, &tenant).await else { return not_found(&tenant) };
    ctx.live.seen(&f.get("client_id").cloned().or(basic(&h).map(|x| x.0)).unwrap_or("app".into()), "Identity");
    match flow::token(&ctx, &mut p, f, basic(&h)).await {
        Ok(v) => ([(header::CACHE_CONTROL, "no-store")], Json(v)).into_response(),
        Err(e) => (StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST), Json(json!({"error": e.error, "error_description": format!("{}: {}", e.code, e.message),
            "error_codes": [e.code.trim_start_matches("AADSTS").parse::<i64>().unwrap_or(0)], "timestamp": chrono_like(), "trace_id": stuntman_core::util::uuid(), "correlation_id": stuntman_core::util::uuid()}))).into_response(),
    }
}

fn chrono_like() -> String {
    let s = token::now_s();
    format!("{s}")
}

async fn logout(Query(q): Query<HashMap<String, String>>) -> Response {
    match q.get("post_logout_redirect_uri") {
        Some(u) => Redirect::to(&format!("{u}{}", q.get("state").map(|s| format!("{}state={}", if u.contains('?') { "&" } else { "?" }, flow::enc(s))).unwrap_or_default())).into_response(),
        None => Html("<p style=\"font-family:sans-serif\">You signed out (Stuntman).</p>").into_response(),
    }
}

/// The user a bearer token names (decoded, not verified: this is a stand-in for tests).
async fn bearer_user(ctx: &Ctx, tenant: &str, h: &HeaderMap) -> Result<(Provider, config::User), Response> {
    let p = provider(ctx, tenant).await.ok_or_else(|| not_found(tenant))?;
    let t = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    let oid = token::decode(t).map(|(_, c)| c["oid"].as_str().or(c["sub"].as_str()).unwrap_or("").to_string()).unwrap_or_default();
    let u = p.users.iter().find(|u| u.oid == oid).cloned().ok_or_else(|| (StatusCode::UNAUTHORIZED, Json(json!({"error": {"code": "InvalidAuthenticationToken", "message": "Access token is empty or names no user of this tenant."}}))).into_response())?;
    Ok((p, u))
}

fn user_json(u: &config::User) -> Value {
    json!({"id": u.oid, "displayName": u.name, "mail": u.email, "userPrincipalName": u.email, "accountEnabled": u.enabled})
}

async fn userinfo(State(ctx): State<Ctx>, Path(tenant): Path<String>, h: HeaderMap) -> Response {
    match bearer_user(&ctx, &tenant, &h).await { Ok((_, u)) => Json(json!({"sub": u.oid, "name": u.name, "email": u.email, "given_name": u.name.split_whitespace().next(), "family_name": u.name.split_whitespace().last()})).into_response(), Err(r) => r }
}

async fn graph_me(State(ctx): State<Ctx>, Path(tenant): Path<String>, h: HeaderMap) -> Response {
    match bearer_user(&ctx, &tenant, &h).await { Ok((_, u)) => Json(user_json(&u)).into_response(), Err(r) => r }
}

async fn graph_user(State(ctx): State<Ctx>, Path((tenant, id)): Path<(String, String)>, h: HeaderMap) -> Response {
    let (p, _) = match bearer_user(&ctx, &tenant, &h).await { Ok(x) => x, Err(r) => return r };
    match p.users.iter().find(|u| u.oid == id || u.email == id) { Some(u) => Json(user_json(u)).into_response(), None => (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "Request_ResourceNotFound"}}))).into_response() }
}

async fn graph_member_of(State(ctx): State<Ctx>, Path((tenant, id)): Path<(String, String)>, h: HeaderMap) -> Response {
    let (p, _) = match bearer_user(&ctx, &tenant, &h).await { Ok(x) => x, Err(r) => return r };
    let Some(u) = p.users.iter().find(|u| u.oid == id || u.email == id) else { return (StatusCode::NOT_FOUND, Json(json!({"error": {"code": "Request_ResourceNotFound"}}))).into_response() };
    ctx.live.send("identity.graph", &u.id, "memberOf");
    Json(json!({"value": u.groups.iter().map(|g| json!({"@odata.type": "#microsoft.graph.group", "id": config::guid(&format!("{}:{g}", p.tenant)), "displayName": g})).collect::<Vec<_>>()})).into_response()
}

/// I6: the metadata document of a client whose id is a URL.
async fn client_metadata(State(ctx): State<Ctx>, Path(file): Path<String>) -> Response {
    let id = file.trim_end_matches(".json");
    match crate::clients::load(&ctx).clients.into_iter().find(|c| c.id == id) {
        Some(c) => { ctx.live.send("identity.runs", id, "metadata fetched"); Json(crate::clients::metadata(&ctx, &c)).into_response() }
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "no such client"}))).into_response(),
    }
}

/// I6: the app's authorization server sends the browser back here with the run's code.
async fn client_callback(State(ctx): State<Ctx>, Query(q): Query<HashMap<String, String>>) -> Response {
    let run = q.get("state").cloned().unwrap_or_default();
    match crate::clients::resume(&ctx, &run, q.get("code").cloned(), q.get("error").cloned().map(|e| format!("{e} {}", q.get("error_description").cloned().unwrap_or_default()))).await {
        Ok(_) => Html(format!(r#"<!doctype html><meta charset="utf-8"><title>Back in Stuntman</title><body style="font:14px Archivo,system-ui,sans-serif;max-width:520px;margin:80px auto;color:#16181b">
<h2 style="font-weight:600">Signed in. The run continues in Stuntman.</h2><p style="color:#6e6e68">Run {} · you can close this tab.</p></body>"#, page::esc(&run))).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Html(format!("<p style=\"font-family:sans-serif\">{}</p>", page::esc(&e.message)))).into_response(),
    }
}
