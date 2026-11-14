//! I6 · OAuth clients: Stuntman as the client against the app's own authorization server, the way an
//! AI assistant or a partner connects to an MCP server (MCP authorization spec): protected resource
//! metadata, authorization server metadata, registration (DCR, a client id that is a URL, or
//! pre-registered), PKCE, loopback redirect, the person signing in on the app's page, the token
//! exchange, the audience, an MCP call and refresh rotation. Each run is ten checks, in order.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64U, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use stuntman_core::{util, yamlfile, ApiError, Ctx};

use crate::{store, token};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Client {
    pub id: String,
    pub name: String,
    /// dcr | metadata_url | preregistered
    pub kind: String,
    /// The MCP server it checks.
    pub resource: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub client_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub client_secret: String,
    /// Fields of the metadata document served for a URL client id.
    pub client_name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct File {
    pub kind: String,
    pub clients: Vec<Client>,
}

fn path(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("identity/oauth-clients.yaml")
}

pub fn load(ctx: &Ctx) -> File {
    let mut f: File = yamlfile::load(&path(ctx)).unwrap_or_default();
    if f.clients.is_empty() {
        f.kind = "oauth-clients".into();
        f.clients = vec![
            Client { id: "mcp-dcr".into(), name: "MCP client, registers itself".into(), kind: "dcr".into(), client_name: "Stuntman MCP client (DCR)".into(), ..Default::default() },
            Client { id: "mcp-client".into(), name: "MCP client with a metadata URL".into(), kind: "metadata_url".into(), client_name: "Stuntman MCP client".into(), ..Default::default() },
        ];
    }
    f
}

pub fn save(ctx: &Ctx, f: &File) -> Result<(), ApiError> {
    yamlfile::save(&path(ctx), f)
}

fn port() -> String {
    std::env::var("STUNTMAN_PORT").unwrap_or("8025".into())
}

/// Loopback callback the runs come back to (127.0.0.1, as the MCP spec lets native clients use).
pub fn callback(ctx: &Ctx) -> String {
    format!("http://127.0.0.1:{}{}/oauth-clients/callback", port(), ctx.prefix())
}

/// The metadata document URL of a client whose id is a URL.
pub fn metadata_url(ctx: &Ctx, client: &str) -> String {
    format!("http://localhost:{}{}/oauth-clients/{client}.json", port(), ctx.prefix())
}

pub fn metadata(ctx: &Ctx, c: &Client) -> Value {
    json!({"client_id": metadata_url(ctx, &c.id), "client_name": if c.client_name.is_empty() { c.name.clone() } else { c.client_name.clone() }, "client_uri": "https://stuntman.example.com",
        "redirect_uris": [callback(ctx)], "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"], "token_endpoint_auth_method": "none"})
}

pub const TITLES: [&str; 10] = ["Protected resource metadata", "Authorization server metadata", "Client registration", "PKCE S256 required", "Loopback redirect allowed",
    "Sign-in in the app's own page", "Token exchange", "Token audience is the MCP resource", "MCP call with the token", "Refresh token rotation"];

fn new_run(c: &Client, resource: &str) -> Value {
    json!({"client": c.id, "clientName": c.name, "kind": c.kind, "resource": resource, "status": "running", "at": util::now_ms(),
        "checks": TITLES.iter().enumerate().map(|(i, t)| json!({"n": i + 1, "title": t, "ok": Value::Null, "detail": "", "sent": "", "got": ""})).collect::<Vec<_>>()})
}

async fn put_run(ctx: &Ctx, id: &str, r: &Value) {
    let _ = store::put(ctx, "run", id, "oauth-clients", r).await;
    ctx.live.send("identity.runs", id, "changed");
}

fn set(r: &mut Value, n: usize, ok: Option<bool>, detail: &str, sent: &str, got: &str) {
    let c = &mut r["checks"][n - 1];
    c["ok"] = json!(ok); c["detail"] = json!(detail); c["sent"] = json!(sent); c["got"] = json!(got.chars().take(1500).collect::<String>());
}

fn fail_rest(r: &mut Value, from: usize, why: &str) {
    for n in from..=10 { if r["checks"][n - 1]["ok"].is_null() { set(r, n, None, why, "", ""); } }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(20)).build().unwrap_or_default()
}

fn origin(u: &str) -> String {
    let after = u.split_once("://").map(|x| x.1).unwrap_or(u);
    let host = after.split('/').next().unwrap_or("");
    format!("{}://{host}", u.split("://").next().unwrap_or("http"))
}

fn path_of(u: &str) -> String {
    let after = u.split_once("://").map(|x| x.1).unwrap_or(u);
    after.find('/').map(|i| after[i..].split('?').next().unwrap_or("").trim_end_matches('/').to_string()).unwrap_or_default()
}

async fn get_json(c: &reqwest::Client, url: &str) -> Result<(u16, Value, String), String> {
    let r = c.get(url).header("Accept", "application/json").send().await.map_err(|e| e.to_string())?;
    let st = r.status().as_u16();
    let t = r.text().await.unwrap_or_default();
    Ok((st, serde_json::from_str(&t).unwrap_or(Value::Null), t))
}

/// An MCP JSON-RPC call; reads plain JSON or the first SSE data line.
async fn mcp(c: &reqwest::Client, url: &str, token: &str, session: Option<&str>, body: Value) -> Result<(u16, Value, Option<String>), String> {
    let mut rq = c.post(url).bearer_auth(token).header("Accept", "application/json, text/event-stream").header("Content-Type", "application/json")
        .header("MCP-Protocol-Version", "2025-06-18").json(&body);
    if let Some(s) = session { rq = rq.header("Mcp-Session-Id", s); }
    let r = rq.send().await.map_err(|e| e.to_string())?;
    let st = r.status().as_u16();
    let sid = r.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(String::from);
    let t = r.text().await.unwrap_or_default();
    let v = serde_json::from_str(&t).ok().or_else(|| t.lines().find_map(|l| l.strip_prefix("data:").and_then(|d| serde_json::from_str(d.trim()).ok()))).unwrap_or(Value::Null);
    Ok((st, v, sid))
}

fn rand_b64(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut b);
    B64U.encode(b)
}

/// Checks 1–6: up to the person signing in. Then the run waits for the callback.
pub async fn start(ctx: &Ctx, c: &Client, resource: &str) -> Result<String, ApiError> {
    let id = format!("run_{}", &util::uuid()[..8]);
    let mut r = new_run(c, resource);
    put_run(ctx, &id, &r).await;
    let ctx2 = ctx.clone();
    let (c2, res, id2) = (c.clone(), resource.to_string(), id.clone());
    tokio::spawn(async move { first_half(&ctx2, &c2, &res, &id2, &mut r).await; });
    Ok(id)
}

async fn first_half(ctx: &Ctx, c: &Client, resource: &str, id: &str, r: &mut Value) {
    let h = http();
    // 1. Protected resource metadata: the 401 names it, or the well-known path.
    let probe = h.post(resource).header("Content-Type", "application/json").header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "stuntman", "version": "1"}}})).send().await;
    let (st, www) = match &probe { Ok(p) => (p.status().as_u16(), p.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).unwrap_or("").to_string()), Err(e) => (0, e.to_string()) };
    if st == 0 { set(r, 1, Some(false), &format!("The MCP server did not answer: {www}"), &format!("POST {resource}"), ""); fail_rest(r, 2, "not run: no MCP server"); r["status"] = json!("failed"); put_run(ctx, id, r).await; return; }
    let prm_url = www.split("resource_metadata=\"").nth(1).and_then(|x| x.split('"').next()).map(String::from)
        .unwrap_or(format!("{}/.well-known/oauth-protected-resource{}", origin(resource), path_of(resource)));
    let prm = get_json(&h, &prm_url).await;
    let (ok1, prm_v) = match &prm { Ok((200, v, _)) if v["authorization_servers"].is_array() => (true, v.clone()), _ => (false, Value::Null) };
    let got = match &prm { Ok((s, _, t)) => format!("{s} {t}"), Err(e) => e.clone() };
    let sent = format!("POST {resource} (no token) → {st}{}\nGET {prm_url}", if www.is_empty() { String::new() } else { format!("\nWWW-Authenticate: {www}") });
    if !ok1 {
        set(r, 1, Some(false), &format!("{} → no metadata. MCP clients can't find the authorization server.", prm_url.replace(&origin(resource), "")), &sent, &got);
        fail_rest(r, 2, "not run: check 1 failed"); r["status"] = json!("failed"); put_run(ctx, id, r).await; return;
    }
    let as_url = prm_v["authorization_servers"][0].as_str().unwrap_or("").trim_end_matches('/').to_string();
    let named = !www.contains("resource_metadata");
    set(r, 1, Some(true), &format!("{} lists the AS{}", prm_url.replace(&origin(resource), ""), if named { "; the 401 did not name it (WWW-Authenticate resource_metadata)" } else { "" }), &sent, &got);
    r["scopes"] = prm_v["scopes_supported"].clone();
    put_run(ctx, id, r).await;
    // 2. Authorization server metadata (RFC 8414; OIDC discovery as a fallback).
    let mut asm = Value::Null;
    let mut tried = vec![];
    for u in [format!("{}/.well-known/oauth-authorization-server{}", origin(&as_url), path_of(&as_url)), format!("{as_url}/.well-known/oauth-authorization-server"), format!("{as_url}/.well-known/openid-configuration")] {
        tried.push(u.clone());
        if let Ok((200, v, _)) = get_json(&h, &u).await { if v["authorization_endpoint"].is_string() { asm = v; break; } }
    }
    if asm.is_null() { set(r, 2, Some(false), "No authorization server metadata at the well-known paths.", &tried.join("\n"), ""); fail_rest(r, 3, "not run: check 2 failed"); r["status"] = json!("failed"); put_run(ctx, id, r).await; return; }
    let iss_note = if asm["issuer"].as_str().map(|i| i.trim_end_matches('/') != as_url).unwrap_or(false) { format!("; issuer {} differs from {as_url}", asm["issuer"].as_str().unwrap_or("")) } else { String::new() };
    set(r, 2, Some(true), &format!("{}{iss_note}", tried.last().unwrap().replace(&origin(&as_url), "")), &tried.join("\n"), &serde_json::to_string_pretty(&asm).unwrap_or_default());
    put_run(ctx, id, r).await;
    let (auth_ep, token_ep) = (asm["authorization_endpoint"].as_str().unwrap_or("").to_string(), asm["token_endpoint"].as_str().unwrap_or("").to_string());
    r["tokenEndpoint"] = json!(token_ep);
    // 3. Registration.
    let cb = callback(ctx);
    let client_id = match c.kind.as_str() {
        "dcr" => {
            let Some(reg) = asm["registration_endpoint"].as_str() else { set(r, 3, Some(false), "The AS has no registration_endpoint: clients can't register themselves (DCR).", "", ""); fail_rest(r, 4, "not run: no client id"); r["status"] = json!("failed"); put_run(ctx, id, r).await; return };
            let body = json!({"client_name": c.client_name, "redirect_uris": [cb], "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"], "token_endpoint_auth_method": "none"});
            let res = h.post(reg).json(&body).send().await;
            let (st, t) = match res { Ok(x) => (x.status().as_u16(), x.text().await.unwrap_or_default()), Err(e) => (0, e.to_string()) };
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
            match v["client_id"].as_str() {
                Some(cid) if st < 300 => { set(r, 3, Some(true), &format!("POST {} → client_id", reg.replace(&origin(reg), "")), &format!("POST {reg}\n{}", serde_json::to_string_pretty(&body).unwrap_or_default()), &format!("{st} {t}")); cid.to_string() }
                _ => { set(r, 3, Some(false), &format!("Registration refused ({st})."), &format!("POST {reg}"), &format!("{st} {t}")); fail_rest(r, 4, "not run: no client id"); r["status"] = json!("failed"); put_run(ctx, id, r).await; return }
            }
        }
        "metadata_url" => {
            let url = metadata_url(ctx, &c.id);
            let adv = asm["client_id_metadata_document_supported"].as_bool().unwrap_or(false);
            set(r, 3, Some(true), &format!("client_id is {url}{}", if adv { "" } else { " (the AS does not advertise client_id_metadata_document_supported; trying anyway)" }), &format!("GET {url} (the AS fetches it)"), &serde_json::to_string_pretty(&metadata(ctx, c)).unwrap_or_default());
            url
        }
        _ => { set(r, 3, Some(true), "pre-registered client id", "", ""); c.client_id.clone() }
    };
    r["clientId"] = json!(client_id);
    put_run(ctx, id, r).await;
    // 4. A request without code_challenge must be refused.
    let scope = r["scopes"].as_array().map(|s| s.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "));
    let base_q = |extra: &[(&str, String)]| {
        let mut q: Vec<(String, String)> = vec![("response_type".into(), "code".into()), ("client_id".into(), client_id.clone()), ("redirect_uri".into(), cb.clone()), ("resource".into(), resource.to_string())];
        if let Some(s) = &scope { q.push(("scope".into(), s.clone())); }
        for (k, v) in extra { q.push((k.to_string(), v.clone())); }
        format!("{auth_ep}?{}", q.iter().map(|(k, v)| format!("{k}={}", crate::flow::enc(v))).collect::<Vec<_>>().join("&"))
    };
    let no_pkce = base_q(&[("state", "nopkce".into())]);
    let res = h.get(&no_pkce).send().await;
    let (st, loc, body) = match res { Ok(x) => (x.status().as_u16(), x.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("").to_string(), x.text().await.unwrap_or_default()), Err(e) => (0, String::new(), e.to_string()) };
    let refused = st == 400 || (loc.starts_with(&cb) && loc.contains("error="));
    set(r, 4, Some(refused), if refused { "a request without code_challenge is refused" } else { "The authorization server accepted a request without code_challenge. Any client could then skip PKCE." },
        &format!("GET {no_pkce}"), &format!("{st}{}{}", if loc.is_empty() { String::new() } else { format!("\nLocation: {loc}") }, if body.is_empty() { String::new() } else { format!("\n{}", body.chars().take(400).collect::<String>()) }));
    put_run(ctx, id, r).await;
    // 5–6. With PKCE and the loopback redirect: the person signs in on the app's page.
    let verifier = rand_b64(32);
    let challenge = B64U.encode(Sha256::digest(verifier.as_bytes()));
    let url = base_q(&[("state", id.to_string()), ("code_challenge", challenge), ("code_challenge_method", "S256".into())]);
    let res = h.get(&url).send().await;
    let (st, loc) = match res { Ok(x) => (x.status().as_u16(), x.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("").to_string()), Err(e) => (0, e.to_string()) };
    let loop_refused = st == 400 || (loc.starts_with(&cb) && loc.contains("error="));
    // Check 4 means something only if the same request with PKCE is accepted.
    if loop_refused && r["checks"][3]["ok"] == json!(true) {
        r["checks"][3]["ok"] = Value::Null;
        r["checks"][3]["detail"] = json!("can't tell: the request with PKCE was refused too");
    }
    // A URL client id served by a local Stuntman (http, loopback) is refused by a careful AS (https only,
    // no private addresses: SSRF). That's the environment, not the app.
    if loop_refused && c.kind == "metadata_url" && (client_id.starts_with("http://") || client_id.contains("localhost") || client_id.contains("127.0.0.1")) {
        set(r, 3, None, &format!("client_id is {client_id}: the AS refused it. The spec requires an https URL the AS can reach; refusing http and private addresses is right (SSRF). Publish the metadata document at a public https URL (or point the client id to one) to run this client."),
            &format!("GET {url}"), &format!("{st}{}", if loc.is_empty() { String::new() } else { format!("\nLocation: {loc}") }));
        set(r, 5, None, "not checked: the client id URL was refused first", "", "");
        fail_rest(r, 6, "not run: the client id URL needs public https");
        r["status"] = json!("blocked");
        put_run(ctx, id, r).await;
        return;
    }
    set(r, 5, Some(!loop_refused), &if loop_refused { format!("The redirect {cb} was refused") } else { format!("{cb}") }, &format!("GET {url}"), &format!("{st}{}", if loc.is_empty() { String::new() } else { format!("\nLocation: {loc}") }));
    if loop_refused { fail_rest(r, 6, "not run: the redirect was refused"); r["status"] = json!("failed"); put_run(ctx, id, r).await; return; }
    r["verifier"] = json!(verifier);
    r["authorizeUrl"] = json!(url);
    r["status"] = json!("waiting");
    set(r, 6, None, "Waiting: open the sign-in page and sign in at the app (a person, or sign_in_as on the app's identity provider).", &format!("GET {url}"), "");
    put_run(ctx, id, r).await;
}

/// The app sent the browser back with a code (or an error): checks 6–10.
pub async fn resume(ctx: &Ctx, run_id: &str, code: Option<String>, error: Option<String>) -> Result<(), ApiError> {
    let (mut r, _, _) = store::get(ctx, "run", run_id).await?.ok_or_else(|| ApiError::not_found("No such run"))?;
    if r["status"] != json!("waiting") { return Err(ApiError::bad("This run is not waiting for a sign-in")); }
    let Some(code) = code else {
        set(&mut r, 6, Some(false), &format!("The app answered error={}", error.unwrap_or_default()), "", "");
        fail_rest(&mut r, 7, "not run: no code"); r["status"] = json!("failed"); put_run(ctx, run_id, &r).await; return Ok(());
    };
    set(&mut r, 6, Some(true), "signed in at the app; the code came back to the loopback redirect", "", &format!("code={}…", code.chars().take(12).collect::<String>()));
    r["status"] = json!("running");
    put_run(ctx, run_id, &r).await;
    let (ctx2, id) = (ctx.clone(), run_id.to_string());
    tokio::spawn(async move { second_half(&ctx2, &id, &mut r, code).await; });
    Ok(())
}

async fn token_call(h: &reqwest::Client, ep: &str, form: &[(&str, String)]) -> (u16, Value, String) {
    match h.post(ep).form(form).send().await {
        Ok(x) => { let st = x.status().as_u16(); let t = x.text().await.unwrap_or_default(); (st, serde_json::from_str(&t).unwrap_or(Value::Null), t) }
        Err(e) => (0, Value::Null, e.to_string()),
    }
}

async fn second_half(ctx: &Ctx, id: &str, r: &mut Value, code: String) {
    let h = http();
    let ep = r["tokenEndpoint"].as_str().unwrap_or("").to_string();
    let (cid, res, ver) = (r["clientId"].as_str().unwrap_or("").to_string(), r["resource"].as_str().unwrap_or("").to_string(), r["verifier"].as_str().unwrap_or("").to_string());
    // 7. Token exchange.
    let form = vec![("grant_type", "authorization_code".to_string()), ("code", code), ("redirect_uri", callback(ctx)), ("client_id", cid.clone()), ("code_verifier", ver), ("resource", res.clone())];
    let (st, v, t) = token_call(&h, &ep, &form).await;
    let sent = format!("POST {ep}\ngrant_type=authorization_code&code=…&code_verifier=…&resource={res}");
    let access = v["access_token"].as_str().unwrap_or("").to_string();
    if st != 200 || access.is_empty() { set(r, 7, Some(false), &format!("The token endpoint answered {st}"), &sent, &t); fail_rest(r, 8, "not run: no token"); r["status"] = json!("failed"); put_run(ctx, id, r).await; return; }
    let refresh = v["refresh_token"].as_str().unwrap_or("").to_string();
    set(r, 7, Some(true), &format!("POST /token with verifier → access{}", if refresh.is_empty() { "" } else { " + refresh" }), &sent, &redact(&t));
    put_run(ctx, id, r).await;
    // 8. Audience (RFC 8707): readable only when the token is a JWT.
    match token::decode(&access) {
        Some((_, c)) => {
            let aud: Vec<String> = match &c["aud"] { Value::String(s) => vec![s.clone()], Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(), _ => vec![] };
            let norm = |s: &str| s.trim_end_matches('/').to_string();
            let ok = aud.iter().any(|a| norm(a) == norm(&res));
            let detail = if aud.is_empty() { "aud missing: any API would accept this token".to_string() } else if ok { format!("aud = {}", aud.join(", ")) } else { format!("aud is {}, not {res}: the token can be used on another API", aud.join(", ")) };
            set(r, 8, Some(ok), &detail, &sent, &format!("decoded: {}", serde_json::to_string_pretty(&c).unwrap_or_default()));
        }
        None => set(r, 8, None, "The access token is opaque (not a JWT): its audience can't be read from outside. Check that the server binds tokens to the resource.", &sent, "opaque token"),
    }
    put_run(ctx, id, r).await;
    // 9. MCP call with the token.
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "stuntman", "version": "1"}}});
    match mcp(&h, &res, &access, None, init).await {
        Ok((200, _, sid)) => {
            let _ = mcp(&h, &res, &access, sid.as_deref(), json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await;
            match mcp(&h, &res, &access, sid.as_deref(), json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})).await {
                Ok((st, v, _)) if st == 200 => { let n = v["result"]["tools"].as_array().map(|a| a.len()).unwrap_or(0); set(r, 9, Some(true), &format!("tools/list → {st}, {n} tools"), &format!("POST {res} initialize, tools/list (Bearer)"), &format!("{st}")); }
                Ok((st, v, _)) => set(r, 9, Some(false), &format!("tools/list → {st}"), &format!("POST {res}"), &v.to_string()),
                Err(e) => set(r, 9, Some(false), &e, &format!("POST {res}"), ""),
            }
        }
        Ok((st, v, _)) => set(r, 9, Some(false), &format!("initialize with the token → {st}"), &format!("POST {res} (Bearer)"), &v.to_string()),
        Err(e) => set(r, 9, Some(false), &e, &format!("POST {res}"), ""),
    }
    put_run(ctx, id, r).await;
    // 10. Refresh rotation: the new one works, the old one is refused after use.
    if refresh.is_empty() { set(r, 10, None, "No refresh token was issued (offline_access not asked or not granted).", "", ""); }
    else {
        let f = |rt: &str| vec![("grant_type", "refresh_token".to_string()), ("refresh_token", rt.to_string()), ("client_id", cid.clone()), ("resource", res.clone())];
        let (st1, v1, t1) = token_call(&h, &ep, &f(&refresh)).await;
        let (st2, _, t2) = token_call(&h, &ep, &f(&refresh)).await;
        let rotated = v1["refresh_token"].as_str().map(|x| x != refresh).unwrap_or(false);
        // Many servers allow a short reuse leeway (OpenIddict: 30 s) for concurrent refreshes: try again after it.
        let (st3, t3) = if st1 == 200 && st2 < 400 {
            set(r, 10, None, "The old refresh token worked again right after use; trying once more after a 35 s reuse leeway…", "", "");
            put_run(ctx, id, r).await;
            tokio::time::sleep(std::time::Duration::from_secs(35)).await;
            let (s, _, t) = token_call(&h, &ep, &f(&refresh)).await;
            (Some(s), t)
        } else { (None, String::new()) };
        let ok = st1 == 200 && (st2 >= 400 || st3.map(|s| s >= 400).unwrap_or(false));
        let detail = if st1 != 200 { format!("refresh → {st1}") }
            else if st2 >= 400 { format!("old refresh token refused after use{}", if rotated { "" } else { " (no new refresh token returned)" }) }
            else if ok { "old refresh token accepted again within a short reuse leeway, refused after it".into() }
            else { "the old refresh token still works 35 s after use: a stolen one keeps working".into() };
        set(r, 10, Some(ok), &detail, &format!("POST {ep} grant_type=refresh_token (the same token again at once{})", if st3.is_some() { " and after 35 s" } else { "" }),
            &format!("first: {st1} {}\nagain at once: {st2} {}{}", redact(&t1), redact(&t2), st3.map(|s| format!("\nafter 35 s: {s} {}", redact(&t3))).unwrap_or_default()));
    }
    let failed = r["checks"].as_array().map(|a| a.iter().any(|c| c["ok"] == json!(false))).unwrap_or(false);
    r["status"] = json!(if failed { "failed" } else { "passed" });
    r["finished"] = json!(util::now_ms());
    put_run(ctx, id, r).await;
}

fn redact(t: &str) -> String {
    let mut v: Value = serde_json::from_str(t).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() { for k in ["access_token", "refresh_token", "id_token"] { if let Some(x) = o.get_mut(k) { if let Some(s) = x.as_str() { *x = json!(format!("{}…", s.chars().take(6).collect::<String>())); } } } }
    if v.is_null() { t.chars().take(400).collect() } else { serde_json::to_string_pretty(&v).unwrap_or_default() }
}

/// Runs, newest first (for a client).
pub async fn runs(ctx: &Ctx, client: Option<&str>) -> Result<Vec<Value>, ApiError> {
    let c = client.map(String::from);
    let rows: Vec<(String, String)> = ctx.db.call(move |cn| cn.prepare("SELECT id, data FROM id_state WHERE kind='run' ORDER BY at DESC LIMIT 50")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()).await?;
    Ok(rows.into_iter().filter_map(|(id, d)| { let mut v: Value = serde_json::from_str(&d).ok()?; v["id"] = json!(id); if v.get("verifier").is_some() { v.as_object_mut()?.remove("verifier"); } Some(v) })
        .filter(|v| c.as_ref().map(|c| v["client"] == json!(c)).unwrap_or(true)).collect())
}
