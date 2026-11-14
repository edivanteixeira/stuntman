//! The sign-in as the provider runs it: the authorize request, picking a person (or typed claims),
//! the MFA and consent steps, the code back to the app, and the token endpoint. The login page and
//! sign_in_as (agents, tests) both drive it, so they behave the same.

use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use stuntman_core::{util, ApiError, Ctx};

use crate::{config::{self, App, Provider, User}, store, token::{self, Twist}};

/// What the browser or the agent gets next.
pub enum Next {
    /// Show the page at this step (pick, mfa, consent, admin).
    Page(String),
    /// Back to the app: the URL (query/fragment) or an auto-submitting form (form_post).
    Back { url: String, form: Option<Vec<(String, String)>>, sign_in: i64 },
    /// The provider's error page; `back` is where the app would get error=…
    Error { code: String, message: String, back: Option<String> },
}

fn rand_id(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut b);
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, b)
}

fn step(what: &str, detail: &str) -> Value {
    json!({"at": util::now_ms(), "what": what, "detail": detail})
}

/// Normalised authorize parameters.
pub fn params(q: &std::collections::HashMap<String, String>) -> Value {
    let g = |k: &str| q.get(k).cloned().unwrap_or_default();
    json!({"client_id": g("client_id"), "redirect_uri": g("redirect_uri"), "scope": g("scope"), "state": g("state"), "nonce": g("nonce"),
        "code_challenge": g("code_challenge"), "code_challenge_method": g("code_challenge_method"), "response_type": if g("response_type").is_empty() { "code".into() } else { g("response_type") },
        "response_mode": g("response_mode"), "prompt": g("prompt"), "login_hint": g("login_hint")})
}

/// GET /authorize: checks the app and the redirect URI (exact match), then a page waits for a person.
pub async fn start(ctx: &Ctx, p: &Provider, q: Value) -> Result<String, Next> {
    let cid = q["client_id"].as_str().unwrap_or("");
    let Some(app) = p.app(cid) else {
        return Err(Next::Error { code: "AADSTS700016".into(), message: format!("Application with identifier '{cid}' was not found in the directory '{}'.", p.tenant), back: None });
    };
    let ru = q["redirect_uri"].as_str().unwrap_or("").to_string();
    let ru = if ru.is_empty() { app.redirect_uris.first().cloned().unwrap_or_default() } else { ru };
    if !app.redirect_uris.contains(&ru) {
        let _ = store::sign_in(ctx, store::SignIn { provider: p.name.clone(), app: app.client_id.clone(), result: "refused".into(), code: "AADSTS50011".into(),
            sentence: format!("{} asked to come back to {ru}, which is not registered", app.name), steps: vec![step("GET /authorize", &format!("client {}", app.client_id))], ..Default::default() }).await;
        return Err(Next::Error { code: "AADSTS50011".into(), message: format!("The redirect URI '{ru}' specified in the request does not match the redirect URIs configured for the application '{}'.", app.client_id), back: None });
    }
    if let Ok(Some((a, ..))) = store::provoked(ctx, &p.name).await {
        if a == "idp_down" {
            return Err(Next::Error { code: "AADSTS90033".into(), message: "A transient error has occurred. Please try again. (the identity provider is down, provoked in Stuntman)".into(), back: None });
        }
    }
    let id = rand_id(9);
    let mut d = q.clone();
    d["redirect_uri"] = json!(ru);
    d["step"] = json!("pick");
    d["steps"] = json!([step("GET /authorize", &format!("from the browser, client {}", app.client_id)), step("Login page shown", "Stuntman's login page")]);
    store::put(ctx, "pending", &id, &p.name, &d).await.map_err(|e| Next::Error { code: "server_error".into(), message: e.message, back: None })?;
    Ok(id)
}

async fn load(ctx: &Ctx, pid: &str) -> Result<Value, Next> {
    match store::get(ctx, "pending", pid).await {
        Ok(Some((d, at, used))) if !used && util::now_ms() - at < 1_800_000 => Ok(d),
        _ => Err(Next::Error { code: "AADSTS50058".into(), message: "This sign-in page expired or was already completed. Start again from the app.".into(), back: None }),
    }
}

fn back_with(d: &Value, pairs: Vec<(String, String)>) -> (String, Option<Vec<(String, String)>>) {
    let ru = d["redirect_uri"].as_str().unwrap_or("").to_string();
    let mut pairs = pairs;
    if let Some(s) = d["state"].as_str().filter(|s| !s.is_empty()) { pairs.push(("state".into(), s.into())); }
    let mode = d["response_mode"].as_str().unwrap_or("");
    let rt = d["response_type"].as_str().unwrap_or("code");
    if mode == "form_post" { return (ru, Some(pairs)); }
    let qs = pairs.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect::<Vec<_>>().join("&");
    let fragment = mode == "fragment" || (mode.is_empty() && rt.contains("id_token"));
    (format!("{ru}{}{qs}", if fragment { "#" } else if ru.contains('?') { "&" } else { "?" }), None)
}

pub fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// An error returned to the app's redirect URI (consent refused, wrong tenant…), recorded.
async fn refuse(ctx: &Ctx, p: &Provider, pid: &str, d: &Value, user: &str, error: &str, code: &str, sentence: String, by: &str) -> Next {
    let mut steps: Vec<Value> = d["steps"].as_array().cloned().unwrap_or_default();
    let desc = if code.is_empty() { sentence.clone() } else { format!("{code}: {sentence}") };
    steps.push(step("Redirect with an error", &format!("error={error}")));
    let _ = store::use_once(ctx, "pending", pid).await;
    let (url, form) = back_with(d, vec![("error".into(), error.into()), ("error_description".into(), desc)]);
    let id = store::sign_in(ctx, store::SignIn { provider: p.name.clone(), user: user.into(), app: s(d, "client_id"), result: if error == "access_denied" { "cancelled".into() } else { "refused".into() },
        code: if code.is_empty() { error.into() } else { code.into() }, sentence, steps, by: by.into(), pending: pid.into(), ..Default::default() }).await.unwrap_or(0);
    Next::Back { url, form, sign_in: id }
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

/// Who signs in: a user of the provider, or claims typed once (not saved as a user).
pub enum Who {
    User(String),
    Claims(Value),
}

pub fn claims_user(p: &Provider, c: &Value) -> User {
    let email = s(c, "email");
    let name = if s(c, "name").is_empty() { email.split('@').next().unwrap_or("someone").to_string() } else { s(c, "name") };
    let list = |k: &str| c[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).or_else(|| c[k].as_str().map(|x| x.split(',').map(|y| y.trim().to_string()).filter(|y| !y.is_empty()).collect())).unwrap_or_default();
    User { id: format!("claims:{email}"), name, email: email.clone(), oid: config::guid(&format!("{}:{email}", p.tenant)), tenant: if s(c, "tenant").is_empty() { p.tenant.clone() } else { s(c, "tenant") },
        groups: list("groups"), roles: list("roles"), enabled: true, ..Default::default() }
}

/// The person picked on the page (or by sign_in_as).
pub async fn pick(ctx: &Ctx, p: &mut Provider, pid: &str, who: Who, by: &str) -> Next {
    let mut d = match load(ctx, pid).await { Ok(d) => d, Err(e) => return e };
    let user = match &who {
        Who::User(u) => match p.user(u) { Some(u) => u.clone(), None => return Next::Error { code: "AADSTS50034".into(), message: format!("The user account {u} does not exist in the {} directory.", p.tenant), back: None } },
        Who::Claims(c) => claims_user(p, c),
    };
    let app = p.app(&s(&d, "client_id")).cloned().unwrap_or_default();
    let prov = store::provoked(ctx, &p.name).await.ok().flatten().map(|x| x.0).unwrap_or_default();
    let mut steps: Vec<Value> = d["steps"].as_array().cloned().unwrap_or_default();
    steps.push(step(&format!("{} picked {}", if by == "you" { "You" } else { by }, user.name), if by == "you" { "on the page" } else { "via sign_in_as" }));
    d["steps"] = json!(steps);
    d["user"] = json!(user.id);
    if let Who::Claims(c) = &who { d["claims"] = c.clone(); }
    d["by"] = json!(by);
    if !prov.is_empty() { d["provoked"] = json!(prov); let _ = store::provoke_used(ctx, &p.name).await; }
    if prov == "user_cancels" {
        return refuse(ctx, p, pid, &d, &user.id, "access_denied", "", format!("{} cancelled on the sign-in page", user.name), by).await;
    }
    if !user.enabled {
        let _ = store::use_once(ctx, "pending", pid).await;
        let _ = store::sign_in(ctx, store::SignIn { provider: p.name.clone(), user: user.id.clone(), app: app.client_id.clone(), result: "refused".into(), code: "AADSTS50057".into(),
            sentence: format!("{}: account disabled", user.name), steps, by: by.into(), pending: pid.into(), ..Default::default() }).await;
        return Next::Error { code: "AADSTS50057".into(), message: "The user account is disabled.".into(), back: None };
    }
    let other_tenant = prov == "wrong_tenant" || (user.tenant != p.tenant && !user.guest && !app.multi_tenant);
    if other_tenant {
        let t = if user.tenant != p.tenant { user.tenant.clone() } else { p.other_tenants.first().cloned().unwrap_or("another-tenant".into()) };
        let msg = format!("User account '{}' from identity provider '{t}' does not exist in tenant '{}' and cannot access the application '{}' in that tenant.", user.email, p.tenant, app.client_id);
        let next = refuse(ctx, p, pid, &d, &user.id, "access_denied", "AADSTS50020", format!("{} tried from tenant {t}", user.name), by).await;
        if let Next::Back { url, .. } = next { return Next::Error { code: "AADSTS50020".into(), message: msg, back: Some(url) }; }
        return next;
    }
    let locked = store::get(ctx, "lock", &format!("{}/{}", p.name, user.id)).await.ok().flatten().map(|(_, at, _)| util::now_ms() - at < 300_000).unwrap_or(false);
    if locked { return Next::Error { code: "AADSTS50053".into(), message: "Your account is locked for 5 minutes after too many wrong codes.".into(), back: None }; }
    let _ = store::update(ctx, "pending", pid, &d).await;
    if user.mfa || prov == "mfa_required" || prov == "mfa_fails" {
        d["step"] = json!("mfa");
        let _ = store::update(ctx, "pending", pid, &d).await;
        return Next::Page("mfa".into());
    }
    after_mfa(ctx, p, pid, d, &user, false).await
}

pub async fn mfa(ctx: &Ctx, p: &mut Provider, pid: &str, code: &str) -> Next {
    let mut d = match load(ctx, pid).await { Ok(d) => d, Err(e) => return e };
    let Some(user) = user_of(p, &d) else { return Next::Error { code: "AADSTS50034".into(), message: "No user picked".into(), back: None } };
    let ok = code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()) && code != "000000" && !user.phone_lost && s(&d, "provoked") != "mfa_fails";
    let mut steps: Vec<Value> = d["steps"].as_array().cloned().unwrap_or_default();
    if !ok {
        let tries = d["mfa_tries"].as_i64().unwrap_or(0) + 1;
        d["mfa_tries"] = json!(tries);
        steps.push(step("MFA code wrong", &format!("try {tries} of 3")));
        d["steps"] = json!(steps.clone());
        let _ = store::update(ctx, "pending", pid, &d).await;
        if tries >= 3 {
            let _ = store::put(ctx, "lock", &format!("{}/{}", p.name, user.id), &p.name, &json!({})).await;
            let _ = store::use_once(ctx, "pending", pid).await;
            let _ = store::sign_in(ctx, store::SignIn { provider: p.name.clone(), user: user.id.clone(), app: s(&d, "client_id"), result: "refused".into(), code: "AADSTS50076".into(),
                sentence: format!("{}: MFA code wrong three times", user.name), steps, by: s(&d, "by"), pending: pid.into(), ..Default::default() }).await;
            return Next::Error { code: "AADSTS50076".into(), message: "Too many wrong codes: the account is locked for 5 minutes, as the provider does.".into(), back: None };
        }
        return Next::Page("mfa".into());
    }
    steps.push(step("MFA answered", &format!("code {code}")));
    d["steps"] = json!(steps);
    after_mfa(ctx, p, pid, d, &user, true).await
}

fn user_of(p: &Provider, d: &Value) -> Option<User> {
    if d["claims"].is_object() { return Some(claims_user(p, &d["claims"])); }
    p.user(&s(d, "user")).cloned()
}

/// Scopes the person still has to consent to, and those only an administrator can grant.
fn consent_needed(p: &Provider, d: &Value, user: &User, app: &App) -> (bool, Vec<String>) {
    let scopes: Vec<String> = s(d, "scope").split_whitespace().map(String::from).collect();
    let admin_only: Vec<String> = scopes.iter().filter(|x| p.api_for_scope(x).is_some() && !app.admin_consent.contains(x)).cloned().collect();
    let user_needed = !user.consented.contains(&app.client_id) && !scopes.is_empty() && !scopes.iter().all(|x| app.admin_consent.contains(x));
    (user_needed || s(d, "provoked") == "consent_required" || s(d, "prompt") == "consent", admin_only)
}

async fn after_mfa(ctx: &Ctx, p: &mut Provider, pid: &str, mut d: Value, user: &User, mfa_done: bool) -> Next {
    d["mfa_done"] = json!(mfa_done);
    let app = p.app(&s(&d, "client_id")).cloned().unwrap_or_default();
    let (need, admin_only) = consent_needed(p, &d, user, &app);
    if !admin_only.is_empty() {
        d["step"] = json!("admin");
        d["admin_only"] = json!(admin_only);
        let _ = store::update(ctx, "pending", pid, &d).await;
        return Next::Page("admin".into());
    }
    if need && !d["consented_now"].as_bool().unwrap_or(false) {
        d["step"] = json!("consent");
        let _ = store::update(ctx, "pending", pid, &d).await;
        return Next::Page("consent".into());
    }
    finish(ctx, p, pid, d, user).await
}

pub async fn consent(ctx: &Ctx, p: &mut Provider, pid: &str, accept: bool) -> Next {
    let mut d = match load(ctx, pid).await { Ok(d) => d, Err(e) => return e };
    let Some(user) = user_of(p, &d) else { return Next::Error { code: "AADSTS50034".into(), message: "No user picked".into(), back: None } };
    if !accept {
        return refuse(ctx, p, pid, &d, &user.id, "access_denied", "", format!("{} cancelled on the consent page", user.name), &s(&d, "by")).await;
    }
    let mut steps: Vec<Value> = d["steps"].as_array().cloned().unwrap_or_default();
    steps.push(step("Consent accepted", &s(&d, "scope")));
    d["steps"] = json!(steps);
    d["consented_now"] = json!(true);
    // Remembered per user and app until revoked in Users.
    let cid = s(&d, "client_id");
    if let Some(u) = p.users.iter_mut().find(|u| u.id == user.id) {
        if !u.consented.contains(&cid) { u.consented.push(cid); let _ = config::save(ctx, p); ctx.live.send("identity.provider", &p.name, "consent"); }
    }
    finish(ctx, p, pid, d, &user).await
}

pub async fn cancel(ctx: &Ctx, p: &Provider, pid: &str, by: &str) -> Next {
    let d = match load(ctx, pid).await { Ok(d) => d, Err(e) => return e };
    let who = p.user(&s(&d, "user")).map(|u| u.name.clone()).unwrap_or("The person".into());
    refuse(ctx, p, pid, &d, &s(&d, "user"), "access_denied", "", format!("{who} cancelled on the sign-in page"), by).await
}

fn twist_of(d: &Value) -> Twist {
    Twist { expired: s(d, "provoked") == "expired", bad_signature: s(d, "provoked") == "bad_signature" }
}

/// Back to the app with a code (and an id_token when the app asked for one in the response).
async fn finish(ctx: &Ctx, p: &mut Provider, pid: &str, d: Value, user: &User) -> Next {
    if !store::use_once(ctx, "pending", pid).await.unwrap_or(false) {
        return Next::Error { code: "AADSTS50058".into(), message: "This sign-in was already completed.".into(), back: None };
    }
    let app = p.app(&s(&d, "client_id")).cloned().unwrap_or_default();
    let code = format!("1.{}", rand_id(32));
    let mut steps: Vec<Value> = d["steps"].as_array().cloned().unwrap_or_default();
    steps.push(step("Redirect with a code", &format!("to {}", s(&d, "redirect_uri"))));
    let mfa_done = d["mfa_done"].as_bool().unwrap_or(false);
    let claims = token::id_claims(ctx, p, user, &app, d["nonce"].as_str(), mfa_done, twist_of(&d));
    let rt = s(&d, "response_type");
    let mut pairs = vec![];
    let mut tokens = json!({});
    if rt.contains("code") { pairs.push(("code".to_string(), code.clone())); }
    if rt.contains("id_token") {
        match token::issue(ctx, p, &claims, twist_of(&d)).await { Ok(t) => { pairs.push(("id_token".into(), t.clone())); tokens["id_token"] = json!(t); } Err(e) => return Next::Error { code: "server_error".into(), message: e.message, back: None } }
    }
    let sentence = format!("{} signed in to {}{}", user.name, app.name, if mfa_done { " with MFA" } else { "" });
    let result = match s(&d, "provoked").as_str() { "expired" => "signed in  ·  tokens already expired (provoked)", "bad_signature" => "signed in  ·  tokens with a bad signature (provoked)", _ => "signed in" };
    let id = store::sign_in(ctx, store::SignIn { provider: p.name.clone(), user: user.id.clone(), app: app.client_id.clone(), result: result.into(), sentence, flow: flow_name(&d),
        steps, claims: claims.clone(), tokens, by: s(&d, "by"), pending: pid.into(), ..Default::default() }).await.unwrap_or(0);
    let mut cd = d.clone();
    cd["sign_in"] = json!(id);
    let _ = store::put(ctx, "code", &code, &p.name, &cd).await;
    let (url, form) = back_with(&d, pairs);
    Next::Back { url, form, sign_in: id }
}

fn flow_name(d: &Value) -> String {
    let pkce = !s(d, "code_challenge").is_empty();
    format!("OIDC {}{}  ·  scope {}", match s(d, "response_type").as_str() { "code" => "authorization code", "id_token" => "implicit id_token", x => x }, if pkce { " + PKCE" } else { "" }, s(d, "scope"))
}

// ---------------------------------------------------------------- token endpoint

pub struct TokenError {
    pub status: u16,
    pub error: String,
    pub code: String,
    pub message: String,
}

fn terr(status: u16, error: &str, code: &str, message: &str) -> TokenError {
    TokenError { status, error: error.into(), code: code.into(), message: message.into() }
}

/// The client secret the app must send: its value from Secrets (secret://…) or the file.
fn expected_secret(ctx: &Ctx, app: &App) -> String {
    util::resolve_secret(&app.secret, &ctx.data_dir)
}

/// The secret's expiry when it lives in a Key Vault of this environment.
async fn secret_expired(ctx: &Ctx, app: &App) -> bool {
    let Some(name) = app.secret.strip_prefix("secret://") else { return false };
    for v in stuntman_keyvault::config::list(ctx) {
        if let Ok(Some(x)) = stuntman_keyvault::store::current(ctx, &v.name, "secret", name).await {
            return x.exp.map(|e| e <= token::now_s()).unwrap_or(false);
        }
    }
    false
}

async fn client(ctx: &Ctx, p: &Provider, f: &std::collections::HashMap<String, String>, basic: Option<(String, String)>) -> Result<App, TokenError> {
    let (cid, secret) = match basic { Some((c, s)) => (c, Some(s)), None => (f.get("client_id").cloned().unwrap_or_default(), f.get("client_secret").cloned()) };
    let app = p.app(&cid).cloned().ok_or_else(|| terr(400, "unauthorized_client", "AADSTS700016", &format!("Application with identifier '{cid}' was not found in the directory '{}'.", p.tenant)))?;
    let public = app.kind == "spa";
    if !public && !app.any_secret {
        let given = secret.unwrap_or_default();
        if given.is_empty() { return Err(terr(401, "invalid_client", "AADSTS7000218", "The request body must contain the following parameter: 'client_assertion' or 'client_secret'.")); }
        if secret_expired(ctx, &app).await { return Err(terr(401, "invalid_client", "AADSTS7000222", &format!("The provided client secret keys for app '{}' are expired.", app.client_id))); }
        if given != expected_secret(ctx, &app) { return Err(terr(401, "invalid_client", "AADSTS7000215", "Invalid client secret provided. Ensure the secret being sent in the request is the client secret value, not the client secret ID.")); }
    }
    Ok(app)
}

fn pkce_ok(d: &Value, verifier: &str) -> bool {
    let ch = s(d, "code_challenge");
    if ch.is_empty() { return true; }
    match s(d, "code_challenge_method").as_str() {
        "plain" => verifier == ch,
        _ => base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, Sha256::digest(verifier.as_bytes())) == ch,
    }
}

/// POST /token. Returns the JSON body.
pub async fn token(ctx: &Ctx, p: &mut Provider, f: std::collections::HashMap<String, String>, basic: Option<(String, String)>) -> Result<Value, TokenError> {
    match p.provoke.token.as_str() {
        "500" => return Err(terr(500, "server_error", "AADSTS90033", "A transient error has occurred (provoked in Stuntman).")),
        "slow" => tokio::time::sleep(std::time::Duration::from_secs(10)).await,
        "invalid_grant" => return Err(terr(400, "invalid_grant", "AADSTS70000", "The provided grant is invalid (provoked in Stuntman).")),
        _ => {}
    }
    let app = client(ctx, p, &f, basic).await?;
    let gt = f.get("grant_type").cloned().unwrap_or_default();
    let lt = |k: &str, d: i64| config::Lifetimes::secs(k, d);
    match gt.as_str() {
        "authorization_code" => {
            let code = f.get("code").cloned().unwrap_or_default();
            let Some((d, at, used)) = store::get(ctx, "code", &code).await.map_err(|e| terr(500, "server_error", "", &e.message))? else {
                return Err(terr(400, "invalid_grant", "AADSTS9002313", "Invalid request. Request is malformed or invalid: the code was not issued here."));
            };
            if used { return Err(terr(400, "invalid_grant", "AADSTS54005", "OAuth2 Authorization code was already redeemed, please retry with a new valid code or use an existing refresh token.")); }
            if util::now_ms() - at > 600_000 { return Err(terr(400, "invalid_grant", "AADSTS70008", "The provided authorization code or refresh token has expired due to inactivity.")); }
            if s(&d, "client_id") != app.client_id { return Err(terr(400, "invalid_grant", "AADSTS700005", "Provided Authorization Code is intended to use against other tenant or app.")); }
            if f.get("redirect_uri").map(|r| *r != s(&d, "redirect_uri")).unwrap_or(false) { return Err(terr(400, "invalid_grant", "AADSTS50148", "The redirect_uri does not match the one used to get the code.")); }
            if !pkce_ok(&d, f.get("code_verifier").map(String::as_str).unwrap_or("")) { return Err(terr(400, "invalid_grant", "AADSTS501481", "The Code_Verifier does not match the code_challenge supplied in the authorization request.")); }
            let _ = store::use_once(ctx, "code", &code).await;
            let user = user_of(p, &d).ok_or_else(|| terr(400, "invalid_grant", "AADSTS50034", "The user no longer exists."))?;
            let twist = twist_of(&d);
            let scopes: Vec<String> = s(&d, "scope").split_whitespace().map(String::from).collect();
            let idc = token::id_claims(ctx, p, &user, &app, d["nonce"].as_str(), d["mfa_done"].as_bool().unwrap_or(false), twist);
            let atc = token::access_claims(ctx, p, Some(&user), &app, &scopes, twist);
            let id_token = token::issue(ctx, p, &idc, twist).await.map_err(|e| terr(500, "server_error", "", &e.message))?;
            let access = token::issue(ctx, p, &atc, twist).await.map_err(|e| terr(500, "server_error", "", &e.message))?;
            let mut body = json!({"token_type": "Bearer", "scope": scopes.iter().filter(|x| *x != "openid" && *x != "offline_access").cloned().collect::<Vec<_>>().join(" "),
                "expires_in": lt(&p.lifetimes.access, 3600), "ext_expires_in": lt(&p.lifetimes.access, 3600), "access_token": access, "id_token": id_token});
            if scopes.iter().any(|x| x == "offline_access") {
                let rt = format!("1.{}", rand_id(48));
                let _ = store::put(ctx, "refresh", &rt, &p.name, &json!({"client_id": app.client_id, "user": user.id, "scope": s(&d, "scope"), "claims": d["claims"]})).await;
                body["refresh_token"] = json!(rt);
            }
            if let Some(id) = d["sign_in"].as_i64() {
                let _ = store::extend_sign_in(ctx, id, step(&format!("POST /token by {}", app.name), &format!("id, access{} tokens", if body["refresh_token"].is_null() { "" } else { " and refresh" })),
                    Some(idc), json!({"id_token": body["id_token"], "access_token": body["access_token"], "access_claims": atc})).await;
            }
            Ok(body)
        }
        "refresh_token" => {
            let rt = f.get("refresh_token").cloned().unwrap_or_default();
            let Some((d, at, used)) = store::get(ctx, "refresh", &rt).await.map_err(|e| terr(500, "server_error", "", &e.message))? else {
                return Err(terr(400, "invalid_grant", "AADSTS9002313", "Invalid refresh token."));
            };
            let user = user_of(p, &d);
            let uname = user.as_ref().map(|u| u.name.clone()).unwrap_or_default();
            let refused = |code: &'static str, msg: String| TokenError { status: 400, error: "invalid_grant".into(), code: code.into(), message: msg };
            let err = if used { Some(refused("AADSTS70043", "The refresh token was already used: tokens rotate on use.".into())) }
                else if util::now_ms() - at > lt(&p.lifetimes.refresh, 86_400) * 1000 { Some(refused("AADSTS700082", "The refresh token has expired due to inactivity.".into())) }
                else if s(&d, "client_id") != app.client_id { Some(refused("AADSTS700005", "The refresh token was issued to another app.".into())) }
                else if user.as_ref().map(|u| !u.enabled).unwrap_or(true) { Some(refused("AADSTS50057", "The user account is disabled.".into())) }
                else if user.as_ref().map(|u| !u.consented.contains(&app.client_id) && !d["claims"].is_object()).unwrap_or(false) { Some(refused("AADSTS65001", format!("The user or administrator has not consented to use the application '{}'.", app.client_id))) }
                else { None };
            if let Some(e) = err {
                let _ = store::sign_in(ctx, store::SignIn { provider: p.name.clone(), user: s(&d, "user"), app: app.client_id.clone(), kind: "refresh".into(), result: "refused".into(), code: e.code.clone(),
                    sentence: format!("Refresh refused for {uname}"), steps: vec![step(&format!("POST /token by {}", app.name), "grant_type=refresh_token")], ..Default::default() }).await;
                return Err(e);
            }
            let user = user.unwrap();
            let _ = store::use_once(ctx, "refresh", &rt).await;
            let scopes: Vec<String> = s(&d, "scope").split_whitespace().map(String::from).collect();
            let idc = token::id_claims(ctx, p, &user, &app, None, false, Twist::default());
            let atc = token::access_claims(ctx, p, Some(&user), &app, &scopes, Twist::default());
            let new_rt = format!("1.{}", rand_id(48));
            let _ = store::put(ctx, "refresh", &new_rt, &p.name, &d).await;
            let body = json!({"token_type": "Bearer", "scope": s(&d, "scope"), "expires_in": lt(&p.lifetimes.access, 3600), "access_token": token::issue(ctx, p, &atc, Twist::default()).await.map_err(|e| terr(500, "server_error", "", &e.message))?,
                "id_token": token::issue(ctx, p, &idc, Twist::default()).await.map_err(|e| terr(500, "server_error", "", &e.message))?, "refresh_token": new_rt});
            let _ = store::sign_in(ctx, store::SignIn { provider: p.name.clone(), user: user.id.clone(), app: app.client_id.clone(), kind: "refresh".into(), result: "refreshed".into(),
                sentence: format!("Token refreshed for {}", user.name), flow: "refresh_token (rotated on use)".into(), steps: vec![step(&format!("POST /token by {}", app.name), "grant_type=refresh_token")],
                claims: idc, tokens: json!({"access_token": body["access_token"], "id_token": body["id_token"], "access_claims": atc}), ..Default::default() }).await;
            Ok(body)
        }
        "client_credentials" => {
            let scope = f.get("scope").cloned().unwrap_or_default();
            let scopes: Vec<String> = scope.split_whitespace().map(|x| x.trim_end_matches("/.default").to_string() + "/.default").collect();
            let atc = token::access_claims(ctx, p, None, &app, &scopes, Twist::default());
            let at = token::issue(ctx, p, &atc, Twist::default()).await.map_err(|e| terr(500, "server_error", "", &e.message))?;
            let _ = store::sign_in(ctx, store::SignIn { provider: p.name.clone(), app: app.client_id.clone(), kind: "client-credentials".into(), result: "issued".into(),
                sentence: format!("{} got an app token for {}", app.name, atc["aud"].as_str().unwrap_or("")), flow: "client credentials".into(),
                steps: vec![step(&format!("POST /token by {}", app.name), &format!("grant_type=client_credentials  ·  {scope}"))], claims: atc.clone(), tokens: json!({"access_token": at}), ..Default::default() }).await;
            Ok(json!({"token_type": "Bearer", "expires_in": lt(&p.lifetimes.access, 3600), "ext_expires_in": lt(&p.lifetimes.access, 3600), "access_token": at}))
        }
        g => Err(terr(400, "unsupported_grant_type", "AADSTS70003", &format!("The app requested an unsupported grant type '{g}'."))),
    }
}

/// sign_in_as: completes a waiting page (the newest for the app), or starts one for the app's first
/// redirect URI. Returns where the browser would go next (the app's callback) and the sign-in.
pub async fn sign_in_as(ctx: &Ctx, p: &mut Provider, who: Who, app: Option<String>, mfa_code: Option<String>, consent_ok: bool, by: &str) -> Result<Value, ApiError> {
    let pending = store::pending(ctx, Some(p.name.clone())).await?;
    let pid = match pending.iter().find(|x| app.as_ref().map(|a| x["client_id"] == json!(a) || p.app(a).map(|aa| x["client_id"] == json!(aa.client_id)).unwrap_or(false)).unwrap_or(true)) {
        Some(x) => s(x, "id"),
        None => {
            let a = app.as_ref().and_then(|a| p.app(a)).or(p.apps.iter().find(|a| a.kind != "api" && a.kind != "daemon")).cloned()
                .ok_or_else(|| ApiError::bad("No login page is waiting and no app to start one: open the app's sign-in first, or pass app"))?;
            let q = json!({"client_id": a.client_id, "redirect_uri": a.redirect_uris.first().cloned().unwrap_or_default(), "scope": a.permissions.join(" "), "response_type": "code", "state": "", "nonce": ""});
            match start(ctx, p, q).await { Ok(id) => id, Err(Next::Error { code, message, .. }) => return Err(ApiError::bad(format!("{code}: {message}"))), Err(_) => return Err(ApiError::internal("could not start")) }
        }
    };
    let mut next = pick(ctx, p, &pid, who, by).await;
    if let Next::Page(st) = &next {
        if st == "mfa" { next = mfa(ctx, p, &pid, &mfa_code.clone().unwrap_or("482913".into())).await; }
    }
    if let Next::Page(st) = &next {
        if st == "consent" {
            if !consent_ok { return Ok(json!({"pending": pid, "step": "consent", "message": "The consent page is waiting: sign_in_as again with consent=true, or cancel_login"})); }
            next = consent(ctx, p, &pid, true).await;
        }
    }
    Ok(match next {
        Next::Page(st) => json!({"pending": pid, "step": st, "message": match st.as_str() { "mfa" => "MFA code refused: try another 6-digit code", "admin" => "Needs an administrator's consent first (grant_admin_consent)", _ => "waiting" }}),
        Next::Back { url, form, sign_in } => json!({"pending": pid, "callback": url, "form_post": form.map(|f| f.into_iter().map(|(k, v)| (k, json!(v))).collect::<serde_json::Map<String, Value>>()), "sign_in": sign_in}),
        Next::Error { code, message, back } => json!({"pending": pid, "error": code, "message": message, "callback": back}),
    })
}
