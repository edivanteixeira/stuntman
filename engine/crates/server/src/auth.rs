//! Sign-in for a shared Stuntman (A1–A7). Off unless STUNTMAN_AUTH_ISSUER is set.
//!
//! The rule is by route: the screens, /api and /mcp need someone; the app's provider routes, the
//! simulated identity (/login/…) and the Test CA stay open. Someone is a person with a session
//! (OIDC sign-in, a signed cookie), the admin key (STUNTMAN_ADMIN_KEY), an environment's token
//! (stm_env_…, only its /e/<name>/api and /mcp) or a person's agent with an OAuth token from the
//! central MCP (stm_at_…). Every person is checked against the list on every use.

use std::{collections::HashMap, sync::{Mutex, OnceLock}};

use axum::{
    extract::{Form, Query},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use stuntman_core::{tools::Args, util, ApiError, Tool};
use subtle::ConstantTimeEq;

use crate::srv;

pub struct Conf {
    pub issuer: String,
    client_id: String,
    client_secret: String,
    emails: Vec<String>,
    domains: Vec<String>,
    admin_key: Option<String>,
}

fn list_var(k: &str) -> Vec<String> {
    std::env::var(k).unwrap_or_default().split([',', ' ', ';']).map(|s| s.trim().trim_start_matches('@').to_lowercase()).filter(|s| !s.is_empty()).collect()
}

pub fn conf() -> Option<&'static Conf> {
    static C: OnceLock<Option<Conf>> = OnceLock::new();
    C.get_or_init(|| {
        let issuer = std::env::var("STUNTMAN_AUTH_ISSUER").ok().map(|s| s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty())?;
        Some(Conf {
            issuer,
            client_id: std::env::var("STUNTMAN_AUTH_CLIENT_ID").unwrap_or_default(),
            client_secret: std::env::var("STUNTMAN_AUTH_CLIENT_SECRET").unwrap_or_default(),
            emails: list_var("STUNTMAN_AUTH_ALLOWED_EMAILS"),
            domains: list_var("STUNTMAN_AUTH_ALLOWED_DOMAINS"),
            admin_key: std::env::var("STUNTMAN_ADMIN_KEY").ok().filter(|s| !s.trim().is_empty()),
        })
    })
    .as_ref()
}

pub fn on() -> bool {
    conf().is_some()
}

pub fn allowed(email: &str) -> bool {
    let Some(c) = conf() else { return true };
    let e = email.trim().to_lowercase();
    let d = e.rsplit_once('@').map(|x| x.1.to_string()).unwrap_or_default();
    !e.is_empty() && (c.emails.contains(&e) || c.domains.iter().any(|x| d == *x || d.ends_with(&format!(".{x}"))))
}

const SESSION_HOURS: i64 = 12;
const ACCESS_HOURS: i64 = 8;
const REFRESH_DAYS: i64 = 30;

// ---------------------------------------------------------------- signing (cookies)

fn key() -> &'static [u8] {
    static K: OnceLock<Vec<u8>> = OnceLock::new();
    K.get_or_init(|| {
        let p = srv().root.join("keys/session.key");
        if let Ok(k) = std::fs::read(&p) {
            if k.len() >= 32 {
                return k;
            }
        }
        let k: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(&p, &k);
        k
    })
}

fn sign(v: &Value) -> String {
    let body = B64.encode(v.to_string());
    let mut m = Hmac::<Sha256>::new_from_slice(key()).unwrap();
    m.update(body.as_bytes());
    format!("{body}.{}", B64.encode(m.finalize().into_bytes()))
}

fn unsign(s: &str) -> Option<Value> {
    let (body, mac) = s.split_once('.')?;
    let mut m = Hmac::<Sha256>::new_from_slice(key()).unwrap();
    m.update(body.as_bytes());
    m.verify_slice(&B64.decode(mac).ok()?).ok()?;
    let v: Value = serde_json::from_slice(&B64.decode(body).ok()?).ok()?;
    (v["x"].as_i64().unwrap_or(0) > util::now_ms()).then_some(v)
}

fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|c| {
        let (k, v) = c.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

fn set_cookie(name: &str, value: &str, path: &str, max_age: i64, secure: bool) -> String {
    format!("{name}={value}; Path={path}; Max-Age={max_age}; HttpOnly; SameSite=Lax{}", if secure { "; Secure" } else { "" })
}

fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

fn token(prefix: &str) -> String {
    format!("{prefix}{}", B64.encode((0..32).map(|_| rand::random::<u8>()).collect::<Vec<u8>>()))
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

// ---------------------------------------------------------------- where people reach this server

/// scheme://host of the request (a proxy's X-Forwarded-* first), or STUNTMAN_PUBLIC_URL.
pub fn origin(h: &HeaderMap) -> String {
    if let Some(p) = stuntman_core::shared_public_url() {
        return p;
    }
    let hv = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).map(|v| v.split(',').next().unwrap_or("").trim().to_string()).filter(|v| !v.is_empty());
    let host = hv("x-forwarded-host").or(hv("host")).unwrap_or(format!("localhost:{}", srv().port));
    format!("{}://{host}", hv("x-forwarded-proto").unwrap_or("http".into()))
}

fn secure(h: &HeaderMap) -> bool {
    origin(h).starts_with("https://")
}

// ---------------------------------------------------------------- stores (data/auth/*.json)

#[derive(Default, serde::Serialize, serde::Deserialize, Clone)]
struct Store {
    #[serde(default)]
    clients: HashMap<String, Value>,
    #[serde(default)]
    codes: HashMap<String, Value>,
    #[serde(default)]
    grants: Vec<Value>,
    /// email → the environment their new MCP sessions start in
    #[serde(default)]
    current: HashMap<String, String>,
}

static LOCK: Mutex<()> = Mutex::new(());

fn store_path() -> std::path::PathBuf {
    srv().root.join("auth/oauth.json")
}

fn load() -> Store {
    std::fs::read(store_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(s: &Store) {
    let p = store_path();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let _ = std::fs::write(&p, serde_json::to_vec_pretty(s).unwrap_or_default());
}

fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let _g = LOCK.lock().unwrap();
    let mut s = load();
    let r = f(&mut s);
    save(&s);
    r
}

pub fn current_env(email: &str) -> Option<String> {
    load().current.get(&email.to_lowercase()).cloned().filter(|e| srv().envs.get(e).is_some())
}

/// An environment's token for automation: environments/<name>/token, gone with the environment.
pub fn env_token(name: &str) -> Option<String> {
    std::fs::read_to_string(crate::environments::dir(name).join("token")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn new_env_token(name: &str) -> Result<String, ApiError> {
    let t = token("stm_env_");
    std::fs::write(crate::environments::dir(name).join("token"), &t).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(t)
}

// ---------------------------------------------------------------- who is calling

#[derive(Clone, Debug, PartialEq)]
pub enum Who {
    Person { email: String, name: String },
    Agent { email: String, grant: String },
    Admin,
    EnvToken(String),
}

impl Who {
    pub fn header(&self) -> String {
        match self {
            Who::Person { email, .. } => format!("person:{email}"),
            Who::Agent { email, .. } => format!("agent:{email}"),
            Who::Admin => "admin".into(),
            Who::EnvToken(e) => format!("env:{e}"),
        }
    }
    pub fn email(&self) -> Option<&str> {
        match self {
            Who::Person { email, .. } | Who::Agent { email, .. } => Some(email),
            _ => None,
        }
    }
}

/// The person of a session cookie, still on the list.
pub fn session(h: &HeaderMap) -> Option<Who> {
    let v = unsign(&cookie(h, "stm_session")?)?;
    let email = v["e"].as_str()?.to_string();
    allowed(&email).then(|| Who::Person { name: v["n"].as_str().unwrap_or(&email).to_string(), email })
}

/// Who a request is: a bearer token first (admin key, environment token, OAuth), then the session.
pub fn identify(h: &HeaderMap, env: Option<&str>) -> Result<Option<Who>, Response> {
    let bearer = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ").or(v.strip_prefix("bearer "))).map(|s| s.trim().to_string());
    if let Some(b) = bearer {
        let c = conf().unwrap();
        if c.admin_key.as_deref().map(|k| same(k, &b)).unwrap_or(false) {
            return Ok(Some(Who::Admin));
        }
        if b.starts_with("stm_env_") {
            let Some(e) = env else { return Err(deny(403, "An environment's token works only under /e/<name>/: this address is not one environment")) };
            return match env_token(e) {
                Some(t) if same(&t, &b) => Ok(Some(Who::EnvToken(e.to_string()))),
                _ => {
                    let other = srv().envs.all().into_iter().find(|c| env_token(&c.env).map(|t| same(&t, &b)).unwrap_or(false)).map(|c| c.env.to_string());
                    Err(match other {
                        Some(o) => deny(403, &format!("This token is for {o}, not {e}")),
                        None => deny(401, "This token is not valid (rotated, or its environment was deleted)"),
                    })
                }
            };
        }
        if b.starts_with("stm_at_") {
            let h2 = sha(&b);
            let now = util::now_ms();
            let found = with(|s| {
                let g = s.grants.iter_mut().find(|g| g["access"].as_str() == Some(&h2))?;
                g["lastUsed"] = json!(now);
                Some(g.clone())
            });
            if let Some(g) = found {
                let email = g["email"].as_str().unwrap_or("").to_string();
                if g["accessExpires"].as_i64().unwrap_or(0) > now && allowed(&email) {
                    return Ok(Some(Who::Agent { email, grant: g["id"].as_str().unwrap_or("").to_string() }));
                }
            }
            return Err(deny(401, "This access token expired or was revoked: sign in again"));
        }
        return Err(deny(401, "Unknown token"));
    }
    Ok(session(h))
}

fn deny(status: u16, m: &str) -> Response {
    (StatusCode::from_u16(status).unwrap(), Json(json!({"error": m}))).into_response()
}

/// What kind of route a path (after /e/<name>) is.
#[derive(PartialEq, Debug)]
pub enum Kind {
    Open,
    Api,
    Mcp,
    Page,
}

pub fn kind(path: &str, h: &HeaderMap, method: &str) -> Kind {
    let open = ["/login/", "/api/test-ca", "/api/keyvault/test-ca", "/auth/", "/oauth/", "/.well-known/oauth"];
    if open.iter().any(|p| path.starts_with(p)) {
        return Kind::Open;
    }
    if path == "/api" || path.starts_with("/api/") {
        return Kind::Api;
    }
    if path == "/mcp" {
        return Kind::Mcp;
    }
    let last = path.rsplit('/').next().unwrap_or("");
    let html = h.get(header::ACCEPT).and_then(|v| v.to_str().ok()).map(|a| a.contains("text/html")).unwrap_or(false);
    if method == "GET" && html && !last.contains('.') {
        return Kind::Page;
    }
    Kind::Open
}

/// The gate: Ok(identity header value) to go on, or the answer that stops the request.
pub fn gate(h: &HeaderMap, method: &str, path: &str, env: &str, prefixed: bool, full: &str) -> Result<Option<String>, Response> {
    if !on() {
        return Ok(None);
    }
    let k = kind(path, h, method);
    if k == Kind::Open {
        return Ok(None);
    }
    let who = identify(h, prefixed.then_some(env))?;
    match (k, who) {
        (_, Some(w)) => Ok(Some(w.header())),
        (Kind::Page, None) => Err(Redirect::to(&format!("/auth/login?next={}", enc(full))).into_response()),
        (Kind::Mcp, None) => {
            let o = origin(h);
            let suffix = if prefixed { format!("/e/{env}/mcp") } else { "/mcp".into() };
            let mut r = deny(401, "Sign in: this MCP needs a token (OAuth, or Authorization: Bearer)");
            r.headers_mut().insert(header::WWW_AUTHENTICATE, format!("Bearer resource_metadata=\"{o}/.well-known/oauth-protected-resource{suffix}\"").parse().unwrap());
            Err(r)
        }
        _ => Err(deny(401, "Sign in to Stuntman, or send Authorization: Bearer <key or token>")),
    }
}

fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

// ---------------------------------------------------------------- the screen's sign-in (OIDC code flow)

async fn discovery() -> Result<Value, String> {
    static D: tokio::sync::OnceCell<Value> = tokio::sync::OnceCell::const_new();
    D.get_or_try_init(|| async {
        let u = format!("{}/.well-known/openid-configuration", conf().unwrap().issuer);
        let r = srv().http.get(&u).send().await.map_err(|e| format!("{u}: {e}"))?;
        r.json::<Value>().await.map_err(|e| format!("{u}: {e}"))
    })
    .await
    .cloned()
}

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title} · Stuntman</title>
<link href="https://fonts.googleapis.com/css2?family=Archivo:wght@400;500;600;700&family=JetBrains+Mono:wght@400&display=swap" rel="stylesheet">
<style>body{{margin:0;background:#fafaf8;color:#16181b;font:14px Archivo,system-ui,sans-serif}}.logo{{text-align:center;font-weight:700;font-size:26px;margin:120px 0 36px}}
.card{{max-width:376px;margin:0 auto;background:#fff;border:1px solid #e3e3df;border-radius:12px;padding:28px 32px}}h1{{font-size:20px;margin:0 0 18px}}.t2{{color:#55575c;line-height:1.5}}
.mono{{font-family:'JetBrains Mono',monospace;font-size:13px;margin:6px 0 16px}}.cap.mono{{margin:16px 0 0}}.cap{{color:#6e6e68;font-size:11px}}.row{{display:flex;gap:16px}}
button,.btn{{display:block;flex:1;text-align:center;border-radius:6px;padding:11px 0;font:500 14px Archivo,sans-serif;cursor:pointer;text-decoration:none;border:1px solid #d6d6d1;background:#fff;color:#16181b}}
.primary{{background:#16181b;color:#fff;border-color:#16181b}}table{{margin:14px 0 22px;font-size:12px}}td{{padding:3px 14px 3px 0}}td:first-child{{color:#6e6e68}}
.foot{{text-align:center;color:#6e6e68;font-size:12px;margin-top:28px}}</style></head><body><div class="logo">Stuntman</div>{body}</body></html>"#))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A local path to come back to (never another site).
fn safe_next(n: Option<&String>) -> String {
    n.filter(|n| n.starts_with('/') && !n.starts_with("//")).cloned().unwrap_or("/".into())
}

async fn login(h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(c) = conf() else { return Redirect::to("/").into_response() };
    let d = match discovery().await {
        Ok(d) => d,
        Err(e) => return (StatusCode::BAD_GATEWAY, page("Sign-in unavailable", &format!("<div class=\"card\"><h1>Sign-in is not answering</h1><p class=\"t2\">{}</p></div>", esc(&e)))).into_response(),
    };
    let (state, nonce, verifier) = (token(""), token(""), token(""));
    let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
    let st = sign(&json!({"s": state, "n": nonce, "v": verifier, "next": safe_next(q.get("next")), "x": util::now_ms() + 600_000}));
    let redirect = format!("{}/auth/callback", origin(&h));
    let url = format!("{}?client_id={}&response_type=code&response_mode=query&scope={}&redirect_uri={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
        d["authorization_endpoint"].as_str().unwrap_or(""), enc(&c.client_id), enc("openid email profile"), enc(&redirect), state, nonce, challenge);
    ([(header::SET_COOKIE, set_cookie("stm_oidc", &st, "/auth", 600, secure(&h)))], Redirect::to(&url)).into_response()
}

fn no_access(email: &str, origin: &str) -> Response {
    let host = origin.split("://").nth(1).unwrap_or(origin);
    (StatusCode::FORBIDDEN, page("No access", &format!(r#"<div class="card"><h1>No access</h1><div class="t2">You signed in as</div><div class="mono">{}</div>
<p class="t2">That address is not on this Stuntman's list. Ask the people who run it to add it, or sign in with another account.</p>
<a class="btn primary" href="/auth/logout?again=1">Sign in with another account</a><p class="cap mono">{}</p></div>
<div class="foot">This is the sign-in to Stuntman itself. The identity providers Stuntman simulates (/e/&lt;name&gt;/login/…) stay open.</div>"#, esc(email), esc(host)))).into_response()
}

async fn callback(h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(c) = conf() else { return Redirect::to("/").into_response() };
    let fail = |m: String| (StatusCode::BAD_REQUEST, page("Sign-in failed", &format!("<div class=\"card\"><h1>Sign-in failed</h1><p class=\"t2\">{}</p><a class=\"btn primary\" href=\"/auth/login\">Try again</a></div>", esc(&m)))).into_response();
    if let Some(e) = q.get("error") {
        return fail(format!("{e}: {}", q.get("error_description").cloned().unwrap_or_default()));
    }
    let Some(st) = cookie(&h, "stm_oidc").and_then(|s| unsign(&s)) else { return fail("The sign-in took too long or started elsewhere.".into()) };
    if q.get("state").map(String::as_str) != st["s"].as_str() {
        return fail("The answer does not belong to this sign-in (state).".into());
    }
    let d = match discovery().await { Ok(d) => d, Err(e) => return fail(e) };
    let form = [("grant_type", "authorization_code"), ("code", q.get("code").map(String::as_str).unwrap_or("")), ("redirect_uri", &format!("{}/auth/callback", origin(&h))),
        ("client_id", &c.client_id), ("client_secret", &c.client_secret), ("code_verifier", st["v"].as_str().unwrap_or(""))];
    let r = match srv().http.post(d["token_endpoint"].as_str().unwrap_or("")).form(&form).send().await {
        Ok(r) => r,
        Err(e) => return fail(format!("token endpoint: {e}")),
    };
    let t: Value = r.json().await.unwrap_or(Value::Null);
    let Some(idt) = t["id_token"].as_str() else { return fail(format!("No id_token: {}", t["error_description"].as_str().or(t["error"].as_str()).unwrap_or("?"))) };
    let claims = match verify(idt, &d, &c.client_id, st["n"].as_str().unwrap_or("")).await { Ok(c) => c, Err(e) => return fail(e) };
    let email = claims["email"].as_str().or(claims["preferred_username"].as_str()).or(claims["upn"].as_str()).unwrap_or("").to_lowercase();
    if !allowed(&email) {
        return no_access(&email, &origin(&h));
    }
    let name = claims["name"].as_str().unwrap_or(&email).to_string();
    let sess = sign(&json!({"e": email, "n": name, "x": util::now_ms() + SESSION_HOURS * 3_600_000}));
    let next = st["next"].as_str().unwrap_or("/").to_string();
    let mut r = Redirect::to(&next).into_response();
    r.headers_mut().append(header::SET_COOKIE, set_cookie("stm_session", &sess, "/", SESSION_HOURS * 3600, secure(&h)).parse().unwrap());
    r.headers_mut().append(header::SET_COOKIE, set_cookie("stm_oidc", "", "/auth", 0, secure(&h)).parse().unwrap());
    r
}

/// RS256 against the issuer's keys; iss, aud, exp and nonce.
async fn verify(jwt: &str, d: &Value, aud: &str, nonce: &str) -> Result<Value, String> {
    use rsa::{pkcs1v15::{Signature, VerifyingKey}, signature::Verifier, BigUint, RsaPublicKey};
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return Err("The id_token is not a JWT".into());
    }
    let head: Value = serde_json::from_slice(&B64.decode(parts[0]).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let claims: Value = serde_json::from_slice(&B64.decode(parts[1]).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if head["alg"] != "RS256" {
        return Err(format!("id_token alg {} (RS256 expected)", head["alg"]));
    }
    let jwks: Value = srv().http.get(d["jwks_uri"].as_str().unwrap_or("")).send().await.map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    let kid = head["kid"].as_str().unwrap_or("");
    let k = jwks["keys"].as_array().and_then(|ks| ks.iter().find(|k| k["kid"].as_str() == Some(kid) || kid.is_empty())).ok_or("The issuer has no key for this token (kid)")?;
    let n = BigUint::from_bytes_be(&B64.decode(k["n"].as_str().unwrap_or("")).map_err(|e| e.to_string())?);
    let e = BigUint::from_bytes_be(&B64.decode(k["e"].as_str().unwrap_or("")).map_err(|e| e.to_string())?);
    let key = VerifyingKey::<Sha256>::new(RsaPublicKey::new(n, e).map_err(|e| e.to_string())?);
    let sig = Signature::try_from(B64.decode(parts[2]).map_err(|e| e.to_string())?.as_slice()).map_err(|e| e.to_string())?;
    key.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).map_err(|_| "The id_token signature is not the issuer's".to_string())?;
    if claims["iss"].as_str() != d["issuer"].as_str() {
        return Err(format!("iss {} is not the issuer {}", claims["iss"], d["issuer"]));
    }
    let aud_ok = claims["aud"].as_str() == Some(aud) || claims["aud"].as_array().map(|a| a.iter().any(|x| x.as_str() == Some(aud))).unwrap_or(false);
    if !aud_ok {
        return Err("The id_token is for another app (aud)".into());
    }
    if claims["exp"].as_i64().unwrap_or(0) * 1000 < util::now_ms() - 60_000 {
        return Err("The id_token expired".into());
    }
    if claims["nonce"].as_str() != Some(nonce) {
        return Err("The id_token belongs to another sign-in (nonce)".into());
    }
    Ok(claims)
}

async fn logout(h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let to = if q.contains_key("again") { "/auth/login" } else { "/" };
    let mut r = if q.contains_key("again") || !on() { Redirect::to(to).into_response() } else {
        page("Signed out", "<div class=\"card\"><h1>You signed out</h1><a class=\"btn primary\" href=\"/auth/login\">Sign in again</a></div>").into_response()
    };
    r.headers_mut().append(header::SET_COOKIE, set_cookie("stm_session", "", "/", 0, secure(&h)).parse().unwrap());
    r
}

// ---------------------------------------------------------------- the central MCP's authorization server

async fn protected_resource(h: HeaderMap, uri: axum::http::Uri) -> Response {
    let o = origin(&h);
    let suffix = uri.path().strip_prefix("/.well-known/oauth-protected-resource").unwrap_or("");
    let resource = if suffix.is_empty() { format!("{o}/mcp") } else { format!("{o}{suffix}") };
    Json(json!({"resource": resource, "authorization_servers": [o], "bearer_methods_supported": ["header"], "resource_name": "Stuntman"})).into_response()
}

async fn as_metadata(h: HeaderMap) -> Response {
    let o = origin(&h);
    Json(json!({"issuer": o, "authorization_endpoint": format!("{o}/oauth/authorize"), "token_endpoint": format!("{o}/oauth/token"),
        "registration_endpoint": format!("{o}/oauth/register"), "response_types_supported": ["code"], "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"], "token_endpoint_auth_methods_supported": ["none"], "scopes_supported": ["mcp"]})).into_response()
}

async fn register(Json(b): Json<Value>) -> Response {
    let uris: Vec<String> = b["redirect_uris"].as_array().cloned().unwrap_or_default().iter().filter_map(|u| u.as_str().map(String::from)).collect();
    if uris.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_redirect_uri", "error_description": "redirect_uris is required"}))).into_response();
    }
    let id = util::id("client");
    let name = b["client_name"].as_str().unwrap_or("MCP client").to_string();
    let c = json!({"client_id": id, "client_name": name, "redirect_uris": uris, "token_endpoint_auth_method": "none", "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"], "client_id_issued_at": util::now_ms() / 1000});
    with(|s| s.clients.insert(id.clone(), c.clone()));
    (StatusCode::CREATED, Json(c)).into_response()
}

fn oauth_error(redirect: &str, state: &str, e: &str) -> Response {
    Redirect::to(&format!("{redirect}{}error={e}&state={}", if redirect.contains('?') { "&" } else { "?" }, enc(state))).into_response()
}

async fn authorize(h: HeaderMap, uri: axum::http::Uri, Query(q): Query<HashMap<String, String>>) -> Response {
    let g = |k: &str| q.get(k).cloned().unwrap_or_default();
    let Some(client) = load().clients.get(&g("client_id")).cloned() else {
        return (StatusCode::BAD_REQUEST, page("Unknown app", "<div class=\"card\"><h1>Unknown app</h1><p class=\"t2\">This app did not register with Stuntman (client_id).</p></div>")).into_response();
    };
    if !client["redirect_uris"].as_array().map(|a| a.iter().any(|u| u.as_str() == Some(&g("redirect_uri")))).unwrap_or(false) {
        return (StatusCode::BAD_REQUEST, page("Wrong address", "<div class=\"card\"><h1>Wrong return address</h1><p class=\"t2\">redirect_uri is not one the app registered.</p></div>")).into_response();
    }
    if g("code_challenge").is_empty() || q.get("code_challenge_method").map(|m| m != "S256").unwrap_or(true) {
        return oauth_error(&g("redirect_uri"), &g("state"), "invalid_request");
    }
    let Some(Who::Person { email, .. }) = session(&h) else {
        let next = format!("{}?{}", uri.path(), uri.query().unwrap_or(""));
        return Redirect::to(&format!("/auth/login?next={}", enc(&next))).into_response();
    };
    let name = client["client_name"].as_str().unwrap_or("An app").to_string();
    let starts = current_env(&email).unwrap_or("the environment you pick (Use in MCP, or use_environment)".into());
    let hidden: String = ["client_id", "redirect_uri", "code_challenge", "state", "resource"].iter().map(|k| format!("<input type=\"hidden\" name=\"{k}\" value=\"{}\">", esc(&g(k)))).collect();
    let host = origin(&h);
    page(&format!("Allow {name}?"), &format!(r#"<div class="card"><h1>Allow {n}?</h1><div class="t2">{n} wants to use this Stuntman as</div><div class="mono">{e}</div>
<p class="t2">It can read and change every environment, as you can on the screen. Its sessions start in {s} until you pick another (Use in MCP, or use_environment in the session).</p>
<table><tr><td>Client</td><td>{n} · registered</td></tr><tr><td>Access</td><td>{a} hours, renewed while you are on the list</td></tr></table>
<form method="post" action="/oauth/authorize">{hidden}<div class="row"><button class="primary" name="decision" value="allow">Allow</button><button name="decision" value="cancel">Cancel</button></div></form>
<p class="cap mono">{host}/authorize</p></div>"#, n = esc(&name), e = esc(&email), s = esc(&starts), a = ACCESS_HOURS, host = esc(host.split("://").nth(1).unwrap_or(&host)))).into_response()
}

async fn authorize_post(h: HeaderMap, Form(f): Form<HashMap<String, String>>) -> Response {
    let g = |k: &str| f.get(k).cloned().unwrap_or_default();
    let Some(client) = load().clients.get(&g("client_id")).cloned() else { return deny(400, "unknown client") };
    if !client["redirect_uris"].as_array().map(|a| a.iter().any(|u| u.as_str() == Some(&g("redirect_uri")))).unwrap_or(false) {
        return deny(400, "redirect_uri is not one the app registered");
    }
    let Some(Who::Person { email, name }) = session(&h) else { return deny(401, "Sign in first") };
    if g("decision") != "allow" {
        return oauth_error(&g("redirect_uri"), &g("state"), "access_denied");
    }
    let code = token("");
    let c = json!({"client": g("client_id"), "redirect": g("redirect_uri"), "challenge": g("code_challenge"), "email": email, "name": name, "x": util::now_ms() + 300_000});
    with(|s| { s.codes.retain(|_, v| v["x"].as_i64().unwrap_or(0) > util::now_ms()); s.codes.insert(code.clone(), c) });
    let r = g("redirect_uri");
    Redirect::to(&format!("{r}{}code={code}&state={}", if r.contains('?') { "&" } else { "?" }, enc(&g("state")))).into_response()
}

fn token_error(e: &str, d: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": e, "error_description": d}))).into_response()
}

async fn token_ep(Form(f): Form<HashMap<String, String>>) -> Response {
    let g = |k: &str| f.get(k).cloned().unwrap_or_default();
    let now = util::now_ms();
    match g("grant_type").as_str() {
        "authorization_code" => {
            let Some(c) = with(|s| s.codes.remove(&g("code"))) else { return token_error("invalid_grant", "unknown or used code") };
            if c["x"].as_i64().unwrap_or(0) < now || c["client"].as_str() != Some(&g("client_id")) || c["redirect"].as_str() != Some(&g("redirect_uri")) {
                return token_error("invalid_grant", "the code expired or belongs to another app");
            }
            if B64.encode(Sha256::digest(g("code_verifier").as_bytes())) != c["challenge"].as_str().unwrap_or("") {
                return token_error("invalid_grant", "code_verifier does not match the code_challenge");
            }
            let email = c["email"].as_str().unwrap_or("").to_string();
            if !allowed(&email) {
                return token_error("invalid_grant", "this person is no longer on the list");
            }
            let (at, rt) = (token("stm_at_"), token("stm_rt_"));
            let client_name = load().clients.get(&g("client_id")).and_then(|c| c["client_name"].as_str().map(String::from)).unwrap_or("MCP client".into());
            with(|s| s.grants.push(json!({"id": util::id("grant"), "email": email, "name": c["name"], "client": client_name, "clientId": g("client_id"),
                "access": sha(&at), "refresh": sha(&rt), "created": now, "lastUsed": null, "accessExpires": now + ACCESS_HOURS * 3_600_000,
                "refreshExpires": now + REFRESH_DAYS * 86_400_000})));
            Json(json!({"access_token": at, "token_type": "Bearer", "expires_in": ACCESS_HOURS * 3600, "refresh_token": rt, "scope": "mcp"})).into_response()
        }
        "refresh_token" => {
            let rh = sha(&g("refresh_token"));
            let at = token("stm_at_");
            let r = with(|s| {
                let gr = s.grants.iter_mut().find(|x| x["refresh"].as_str() == Some(&rh))?;
                if gr["refreshExpires"].as_i64().unwrap_or(0) < now || !allowed(gr["email"].as_str().unwrap_or("")) {
                    return Some(Err(()));
                }
                gr["access"] = json!(sha(&at));
                gr["accessExpires"] = json!(now + ACCESS_HOURS * 3_600_000);
                Some(Ok(()))
            });
            match r {
                Some(Ok(())) => Json(json!({"access_token": at, "token_type": "Bearer", "expires_in": ACCESS_HOURS * 3600, "refresh_token": g("refresh_token"), "scope": "mcp"})).into_response(),
                _ => token_error("invalid_grant", "the refresh token expired, was revoked, or its person left the list"),
            }
        }
        _ => token_error("unsupported_grant_type", "authorization_code or refresh_token"),
    }
}

/// Routes on the root of the main port, before any environment.
pub fn routes() -> Router {
    Router::new()
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", get(logout))
        .route("/.well-known/oauth-protected-resource", get(protected_resource))
        .route("/.well-known/oauth-protected-resource/{*rest}", get(protected_resource))
        .route("/.well-known/oauth-authorization-server", get(as_metadata))
        .route("/.well-known/oauth-authorization-server/{*rest}", get(as_metadata))
        .route("/oauth/register", post(register))
        .route("/oauth/authorize", get(authorize).post(authorize_post))
        .route("/oauth/token", post(token_ep))
}

// ---------------------------------------------------------------- tools

fn who_of(a: &Args) -> Option<String> {
    a.str("_who")
}

fn is_admin(a: &Args) -> bool {
    !on() || who_of(a).as_deref() == Some("admin")
}

fn email_of(a: &Args) -> Option<String> {
    who_of(a).and_then(|w| w.split_once(':').filter(|(k, _)| *k == "person" || *k == "agent").map(|x| x.1.to_string()))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("whoami", "Who this call is: a person (screen or agent), the admin key or an environment's token; where their MCP sessions start.", |_ctx, a| async move {
            let w = who_of(&a).unwrap_or_default();
            let email = email_of(&a);
            Ok(json!({"auth": on(), "who": w, "email": email, "admin": w == "admin", "currentEnvironment": email.as_deref().and_then(current_env),
                "mcp": stuntman_core::shared_public_url().map(|p| format!("{p}/mcp"))}))
        })
        .route("GET", "/whoami"),
        Tool::new("set_current_environment", "Where a person's new MCP sessions start (Use in MCP). People set their own; the admin key sets anyone's.", |_ctx, a| async move {
            let email = a.need("email")?.to_lowercase();
            let env = a.need("environment")?;
            if !is_admin(&a) && email_of(&a).as_deref() != Some(email.as_str()) {
                return Err(ApiError::new(403, "Only the admin key sets another person's environment"));
            }
            if srv().envs.get(&env).is_none() {
                return Err(ApiError::not_found(format!("No environment “{env}”")));
            }
            with(|s| s.current.insert(email.clone(), env.clone()));
            Ok(json!({"email": email, "currentEnvironment": env}))
        })
        .req("email", "the person's address").req("environment", "environment name")
        .route("PUT", "/users/{email}/current-environment"),
        Tool::new("get_environment_token", "The environment's token for automation (only its /e/<name>/api and /e/<name>/mcp). People and the admin key only.", |_ctx, a| async move {
            let n = a.need("name")?;
            if who_of(&a).map(|w| w.starts_with("env:")).unwrap_or(false) {
                return Err(ApiError::new(403, "A token does not read tokens"));
            }
            srv().envs.get(&n).ok_or_else(|| ApiError::not_found(format!("No environment “{n}”")))?;
            Ok(json!({"name": n, "token": env_token(&n)}))
        })
        .req("name", "environment name")
        .route("GET", "/environments/{name}/token"),
        Tool::new("rotate_environment_token", "A new token for the environment's /e/<name>/api and /mcp; the old one stops at once. Shown once.", |_ctx, a| async move {
            let n = a.need("name")?;
            srv().envs.get(&n).ok_or_else(|| ApiError::not_found(format!("No environment “{n}”")))?;
            if let Some(w) = who_of(&a) {
                if w.starts_with("env:") && w != format!("env:{n}") {
                    return Err(ApiError::new(403, "This token is for another environment"));
                }
            }
            Ok(json!({"name": n, "token": new_env_token(&n)?}))
        })
        .req("name", "environment name")
        .route("POST", "/environments/{name}/token"),
        Tool::new("list_mcp_access", "People's agents signed in to the central MCP: person, client, last used, expiry, where they start.", |_ctx, _a| async move {
            let s = load();
            let now = util::now_ms();
            let rows: Vec<Value> = s.grants.iter().map(|g| {
                let email = g["email"].as_str().unwrap_or("");
                json!({"id": g["id"], "email": email, "name": g["name"], "client": g["client"], "created": g["created"], "lastUsed": g["lastUsed"],
                    "expires": g["refreshExpires"], "expired": g["refreshExpires"].as_i64().unwrap_or(0) < now || !allowed(email),
                    "startsIn": s.current.get(email).cloned().filter(|e| srv().envs.get(e).is_some())})
            }).collect();
            Ok(json!({"grants": rows, "signIn": conf().map(|c| json!({"provider": c.issuer, "clientId": c.client_id,
                "allowedEmails": c.emails.len(), "allowedDomains": c.domains, "sessionHours": SESSION_HOURS, "adminKey": c.admin_key.is_some()}))}))
        })
        .route("GET", "/mcp-access"),
        Tool::new("revoke_mcp_access", "Ends an agent's access and refresh tokens at once: by grant id, or every grant of a person (email).", |_ctx, a| async move {
            if on() && !is_admin(&a) && email_of(&a).is_none() {
                return Err(ApiError::new(403, "People and the admin key revoke access"));
            }
            let (id, email) = (a.str("grant"), a.str("email").map(|e| e.to_lowercase()));
            if id.is_none() && email.is_none() {
                return Err(ApiError::bad("grant or email"));
            }
            let n = with(|s| {
                let before = s.grants.len();
                s.grants.retain(|g| !(id.as_deref().map(|i| g["id"].as_str() == Some(i)).unwrap_or(false) || email.as_deref().map(|e| g["email"].as_str() == Some(e)).unwrap_or(false)));
                before - s.grants.len()
            });
            Ok(json!({"revoked": n}))
        })
        .param("grant", "grant id").param("email", "every grant of this person")
        .route("DELETE", "/mcp-access/{grant}"),
    ]
}

/// Environments deleted: they leave people's "current" (sessions get the list on their next call).
pub fn forget_env(name: &str) {
    if load().current.values().any(|e| e == name) {
        with(|s| s.current.retain(|_, e| e != name));
    }
}

#[cfg(test)]
mod t {
    #[test]
    fn same() {
        assert!(super::same("abc", "abc") && !super::same("abc", "abd") && !super::same("ab", "abc"));
    }
}
