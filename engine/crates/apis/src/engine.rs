//! The APIs port: finds the service (host <service>.<base>, the provider's real host, or a
//! /<service>/ path prefix), matches a route and runs its steps: receive → let → keep → wait →
//! decide → respond → send. A request no route takes is answered 404 "no route" and listed in
//! Unmatched with the closest route.

use std::{collections::BTreeMap, time::Duration};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use stuntman_core::{
    env::Envs,
    netserve::{self, Kill, Remote},
    util, Ctx,
};
use serde_json::{json, Map, Value};

use crate::{
    expr,
    plugin::{self, Answer, Plugin, Route},
    store,
};

/// One request as the steps see it.
#[derive(Clone, Debug, Default)]
pub struct Req {
    /// The client certificate the app presented over HTTPS (mTLS), by name.
    pub client_cert: Option<String>,
    /// The CNPJ in an ICP-Brasil style client certificate (A18).
    pub client_cnpj: Option<String>,
    pub method: String,
    pub path: String,
    pub query: Map<String, Value>,
    /// The query string as received (WireMock templates read every value of a repeated parameter).
    pub raw_query: String,
    /// http or https, as received.
    pub scheme: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
    pub raw_body: Vec<u8>,
    pub files: Vec<Value>,
    pub caller: String,
}

/// What a route answered (also what Try it shows).
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub answer: String,
    pub sentence: String,
    pub steps: Vec<Value>,
    pub provoked: Option<String>,
    /// reset | empty | timeout: the connection itself fails.
    pub network: Option<String>,
    pub sent: Value,
    pub answered: Value,
    pub latency_ms: u64,
    pub scope: Map<String, Value>,
    /// A streamed answer (A10): each part after its wait, in ms.
    pub parts: Vec<(u64, Vec<u8>)>,
    /// The stream is cut on purpose after the parts: the connection breaks.
    pub cut: bool,
}

fn caller(h: &BTreeMap<String, String>) -> String {
    if let Some(a) = h.get("x-stuntman-app").or(h.get("x-duble-app")).filter(|s| !s.is_empty()) {
        return a.clone();
    }
    let ua = h.get("user-agent").cloned().unwrap_or_default();
    if ua.starts_with("Mozilla") {
        return "browser".into();
    }
    ua.split([' ', '/']).next().filter(|s| !s.is_empty()).unwrap_or("app").to_string()
}

/// "/a/{id}/b" against "/a/42/b" → {id: 42}
pub fn match_path(pattern: &str, path: &str) -> Option<Map<String, Value>> {
    match_path_case(pattern, path, false)
}

pub fn match_path_case(pattern: &str, path: &str, ignore_case: bool) -> Option<Map<String, Value>> {
    // "~/abc(/v[0-9]+)?/x": a regex over the whole path (imported mocks); named groups are params.
    if let Some(re) = pattern.strip_prefix('~') {
        use std::sync::{Mutex, OnceLock};
        static C: OnceLock<Mutex<std::collections::HashMap<String, Option<fancy_regex::Regex>>>> = OnceLock::new();
        let r = C.get_or_init(Default::default).lock().unwrap().entry(format!("{ignore_case}{re}")).or_insert_with(|| fancy_regex::Regex::new(&format!("{}^(?:{re})$", if ignore_case { "(?i)" } else { "" })).ok()).clone()?;
        let caps = r.captures(path).ok().flatten()?;
        let mut m = Map::new();
        for n in r.capture_names().flatten() {
            if let Some(v) = caps.name(n) {
                m.insert(n.to_string(), json!(v.as_str()));
            }
        }
        return Some(m);
    }
    let (p, q): (Vec<&str>, Vec<&str>) = (pattern.trim_matches('/').split('/').collect(), path.trim_matches('/').split('/').collect());
    if p.len() != q.len() && !pattern.ends_with("{*}") {
        return None;
    }
    let mut m = Map::new();
    for (i, seg) in p.iter().enumerate() {
        if *seg == "{*}" {
            m.insert("rest".into(), json!(q[i..].join("/")));
            return Some(m);
        }
        let got = q.get(i)?;
        if seg.starts_with('{') && seg.ends_with('}') {
            m.insert(seg[1..seg.len() - 1].to_string(), json!(netserve_decode(got)));
        } else if seg != got && !(ignore_case && seg.eq_ignore_ascii_case(got)) {
            return None;
        }
    }
    Some(m)
}

pub(crate) fn netserve_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The route of a plugin closest to this request (A2): same method, fewest different segments.
pub fn closest(plugins: &[Plugin], service: &str, method: &str, path: &str) -> Value {
    let q: Vec<&str> = path.trim_matches('/').split('/').collect();
    let mut best: Option<(usize, Value)> = None;
    for p in plugins.iter().filter(|p| service.is_empty() || p.service.name == service) {
        for r in &p.routes {
            let (m, pat) = r.method_path();
            let s: Vec<&str> = pat.trim_matches('/').split('/').collect();
            let len_diff = (s.len() as i64 - q.len() as i64).unsigned_abs() as usize;
            let diffs: Vec<(String, String)> = s.iter().zip(q.iter()).filter(|(a, b)| !(a.starts_with('{') && a.ends_with('}')) && a != b).map(|(a, b)| (b.to_string(), a.to_string())).collect();
            let score = diffs.len() * 2 + len_diff * 3 + if m == method { 0 } else { 2 };
            if best.as_ref().map(|b| score < b.0).unwrap_or(true) {
                let text = if len_diff == 0 && diffs.len() == 1 {
                    format!("Differs in one segment: {} here, {} in the plugin.", diffs[0].0, diffs[0].1)
                } else if len_diff == 0 && diffs.is_empty() && m != method {
                    format!("Same path, but the plugin takes {m}.")
                } else {
                    format!("Differs in {} segment(s).", diffs.len() + len_diff)
                };
                best = Some((score, json!({"service": p.service.name, "route": r.route, "differs": diffs, "text": text, "score": score})));
            }
        }
    }
    match best {
        Some((s, v)) if s <= 8 => v,
        _ => Value::Null,
    }
}

fn parse_multipart(ct: &str, body: &[u8]) -> (Map<String, Value>, Vec<Value>) {
    let mut fields = Map::new();
    let mut files = vec![];
    let Some(b) = ct.split("boundary=").nth(1).map(|b| b.trim_matches('"').to_string()) else { return (fields, files) };
    let delim = format!("--{b}");
    let text = body;
    let mut start = 0;
    let mut part_no = 0;
    let find = |hay: &[u8], needle: &[u8], from: usize| hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from);
    while let Some(i) = find(text, delim.as_bytes(), start) {
        let part_start = i + delim.len() + 2;
        let Some(next) = find(text, delim.as_bytes(), part_start) else { break };
        if part_start >= next {
            break;
        }
        let part = &text[part_start..next.saturating_sub(2)];
        if let Some(h_end) = find(part, b"\r\n\r\n", 0) {
            let head = String::from_utf8_lossy(&part[..h_end]).to_string();
            let data = &part[h_end + 4..];
            let name = head.split("name=\"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("").to_string();
            let ctype = head.lines().find(|l| l.to_lowercase().starts_with("content-type:")).map(|l| l[13..].trim().to_string()).unwrap_or_default();
            // multipart/related and /mixed (Drive, Gmail uploads): parts have no name. A JSON part is
            // the metadata (its fields go to the body); the others are files named part1, part2…
            if name.is_empty() && !head.to_lowercase().contains("content-disposition") {
                part_no += 1;
                let n = part_no;
                match serde_json::from_slice::<Value>(data) {
                    Ok(Value::Object(m)) if ctype.to_lowercase().contains("json") => fields.extend(m),
                    _ => files.push(json!({"field": format!("part{n}"), "filename": "", "size": data.len(), "contentType": ctype,
                        "base64": if data.len() <= 1 << 20 { json!(B64.encode(data)) } else { Value::Null }})),
                }
                start = next;
                continue;
            }
            match head.split("filename=\"").nth(1).and_then(|r| r.split('"').next()) {
                Some(fname) => {
                    files.push(json!({"field": name, "filename": fname, "size": data.len(), "contentType": ctype, "sha256": expr::eval("sha256(x)", &json!({"x": String::from_utf8_lossy(data)}).as_object().unwrap().clone()).unwrap_or(Value::Null),
                        "base64": if data.len() <= 1 << 20 { json!(B64.encode(data)) } else { Value::Null }}));
                    fields.insert(name, json!({"filename": fname, "size": data.len()}));
                }
                None => {
                    fields.insert(name, json!(String::from_utf8_lossy(data)));
                }
            }
        }
        start = next;
    }
    (fields, files)
}

pub fn read_request(method: &str, path: &str, query: &str, headers: BTreeMap<String, String>, raw: Vec<u8>) -> Req {
    // A gzip body (Sentry SDKs, many HTTP clients) is read as the provider would: decompressed.
    let raw = if headers.get("content-encoding").map(|e| e.eq_ignore_ascii_case("gzip")).unwrap_or(false) {
        let mut out = vec![];
        match std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&raw[..]), &mut out) { Ok(_) => out, Err(_) => raw }
    } else { raw };
    let q: Map<String, Value> = query.split('&').filter(|x| !x.is_empty()).map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (netserve_decode(&k.replace('+', " ")), json!(netserve_decode(&v.replace('+', " "))))
    }).collect();
    let ct = headers.get("content-type").cloned().unwrap_or_default().to_lowercase();
    let (body, files) = if ct.contains("multipart/") {
        let (f, files) = parse_multipart(headers.get("content-type").map(String::as_str).unwrap_or(""), &raw);
        (Value::Object(f), files)
    } else if ct.contains("x-www-form-urlencoded") {
        let s = String::from_utf8_lossy(&raw).to_string();
        (Value::Object(s.split('&').filter(|x| !x.is_empty()).map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (netserve_decode(&k.replace('+', " ")), json!(netserve_decode(&v.replace('+', " "))))
        }).collect()), vec![])
    } else if raw.is_empty() {
        (Value::Null, vec![])
    } else {
        (serde_json::from_slice(&raw).unwrap_or_else(|_| json!(String::from_utf8_lossy(&raw))), vec![])
    };
    let c = caller(&headers);
    Req { client_cert: None, client_cnpj: None, method: method.into(), path: path.into(), query: q, raw_query: query.into(), scheme: String::new(), headers, body, raw_body: raw, files, caller: c }
}

fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |acc, k| acc.get(k)).filter(|x| !x.is_null())
}

/// "selfie.jpg · 182 KB" for a long base64 field; the value itself otherwise.
fn show_field(v: &Value) -> Value {
    match v {
        Value::String(s) if s.len() > 200 => json!(format!("{} KB of data", s.len() * 3 / 4 / 1024)),
        Value::Object(o) if o.contains_key("filename") => json!(format!("{} · {} KB", o["filename"].as_str().unwrap_or(""), o["size"].as_u64().unwrap_or(0) / 1024)),
        o => o.clone(),
    }
}

/// The first leaves of an answer body, for "Stuntman answered".
pub fn leaves(v: &Value, prefix: &str, out: &mut Vec<(String, Value)>) {
    if out.len() >= 10 {
        return;
    }
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                leaves(x, &if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") }, out);
            }
        }
        Value::Array(a) if !a.is_empty() => leaves(&a[0], &format!("{prefix}[0]"), out),
        x => out.push((prefix.to_string(), x.clone())),
    }
}

fn generic(name: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
    match name {
        // A gateway refusing the credential before the API: no body at all.
        "401" => (401, vec![("www-authenticate".into(), "Bearer".into())], vec![]),
        "403" => (403, vec![], vec![]),
        "500" => (500, vec![("content-type".into(), "application/json".into())], br#"{"error":"Internal Server Error"}"#.to_vec()),
        "503" => (503, vec![("content-type".into(), "application/json".into()), ("retry-after".into(), "5".into())], br#"{"message":"Service Unavailable"}"#.to_vec()),
        "429" => (429, vec![("content-type".into(), "application/json".into()), ("retry-after".into(), "1".into())], br#"{"message":"Too Many Requests"}"#.to_vec()),
        "html" => (200, vec![("content-type".into(), "text/html".into())], b"<html><body><h1>Service temporarily unavailable</h1><p>Please try again later.</p></body></html>".to_vec()),
        // A proxy or load balancer in front of the API: 503 with its HTML page.
        "503_html" => (503, vec![("content-type".into(), "text/html".into()), ("retry-after".into(), "5".into())], b"<html><head><title>503 Service Temporarily Unavailable</title></head><body><center><h1>503 Service Temporarily Unavailable</h1></center></body></html>".to_vec()),
        _ => (502, vec![], vec![]),
    }
}

fn rate_ok(env: &str, service: &str, rate: &str) -> bool {
    use std::sync::{Mutex, OnceLock};
    static W: OnceLock<Mutex<std::collections::HashMap<String, Vec<i64>>>> = OnceLock::new();
    let Some((n, window)) = plugin::per(rate) else { return true };
    let now = util::now_ms();
    let mut m = W.get_or_init(Default::default).lock().unwrap();
    let v = m.entry(format!("{env}/{service}")).or_default();
    v.retain(|t| *t > now - window);
    if v.len() as u64 >= n {
        return false;
    }
    v.push(now);
    true
}

fn status_answer(status: u16, body: Value) -> (u16, Vec<(String, String)>, Vec<u8>) {
    (status, vec![("content-type".into(), "application/json".into())], serde_json::to_vec(&body).unwrap_or_default())
}

/// Runs a route. `dry` (Try it): nothing is kept, counted or billed, no latency, no webhooks.
pub async fn run(ctx: &Ctx, p: &Plugin, r: &Route, params: Map<String, Value>, req: &Req, dry: bool) -> Outcome {
    let mut o = Outcome::default();
    let (m, pat) = r.method_path();
    let route_name = format!("{m} {pat}");
    let mut steps: Vec<Value> = vec![];
    let svc = &p.service;
    // Scope the expressions read.
    let state = if dry { json!({}) } else { store::state(ctx, &svc.name).await.unwrap_or(json!({})) };
    let mut sc = Map::new();
    sc.insert("body".into(), req.body.clone());
    sc.insert("query".into(), Value::Object(req.query.clone()));
    sc.insert("headers".into(), json!(req.headers));
    sc.insert("path".into(), Value::Object(params.clone()));
    sc.insert("params".into(), Value::Object(params));
    sc.insert("files".into(), json!(req.files));
    sc.insert("data".into(), json!(p.data));
    sc.insert("state".into(), state);
    sc.insert("caller".into(), json!(req.caller));
    sc.insert("client_cert".into(), json!(req.client_cert));
    sc.insert("client_cnpj".into(), json!(req.client_cnpj));
    sc.insert("auth".into(), Value::Null);
    sc.insert("__now".into(), json!(stuntman_core::clock::now(ctx)));
    sc.insert("raw".into(), json!(String::from_utf8_lossy(&req.raw_body)));
    sc.insert("__path".into(), json!(req.path));
    sc.insert("request_path".into(), json!(req.path));
    sc.insert("__query".into(), json!(req.raw_query));
    sc.insert("__scheme".into(), json!(if req.scheme.is_empty() { "http" } else { &req.scheme }));
    sc.insert("__method".into(), json!(req.method));
    sc.insert("scenario".into(), sc["state"].pointer("/__scenario/state/name").cloned().unwrap_or(json!("Started")));
    if !r.graphql.is_empty() {
        sc.insert("variables".into(), req.body.get("variables").cloned().unwrap_or(json!({})));
        sc.insert("operation".into(), json!(r.graphql));
    }
    // Sent fields, for the detail.
    let mut sent = Map::new();
    for f in &r.receive.body {
        if let Some(v) = get_path(&req.body, f) {
            sent.insert(f.rsplit('.').next().unwrap_or(f).to_string(), show_field(v));
        }
    }
    o.sent = Value::Object(sent);

    // 1. Service limits and provoked answers come before the steps.
    let mut chosen: Option<String> = None;
    let mut stream_force: Option<String> = None;
    if !dry && !svc.limits.rate.is_empty() && !rate_ok(&ctx.env, &svc.name, &svc.limits.rate) {
        chosen = Some("429".into());
        o.provoked = Some(format!("rate limit {}", svc.limits.rate));
    }
    if chosen.is_none() && !dry && svc.limits.fail_share > 0.0 && rand::random::<f64>() < svc.limits.fail_share {
        chosen = Some("503".into());
        o.provoked = Some(format!("fail a share ({:.0} %)", svc.limits.fail_share * 100.0));
    }
    // A forced named answer is the provider's business answer: the token endpoint's only when the
    // force names it (route=POST /token: "credentials refused").
    if chosen.is_none() && !dry {
        if let Ok(Some((a, by))) = store::take_force(ctx, &svc.name, &route_name, r.token).await {
            o.provoked = Some(format!("forced by {}", by.trim_start_matches("api:").trim_start_matches("mcp:")));
            if a == "cut" || a == "slow" {
                stream_force = Some(a);
            } else {
                chosen = Some(a);
            }
        }
    }
    if chosen.is_none() && !r.fail.is_empty() {
        chosen = Some(r.fail.clone());
        o.provoked = Some(format!("route fails on purpose ({})", r.fail));
    }

    // 2. Receive: the token route, auth, required fields.
    if chosen.is_none() && !r.grant.is_empty() {
        let mut g = crate::steps::grant(ctx, p, r, req, dry).await;
        g.sent = o.sent.clone();
        return g;
    }
    if chosen.is_none() {
        if r.token {
            let oauth = svc.auth.oauth2_client_credentials.clone().unwrap_or_default();
            let (mut cid, mut csec) = (req.body.get("client_id").and_then(|v| v.as_str()).unwrap_or("").to_string(), req.body.get("client_secret").and_then(|v| v.as_str()).unwrap_or("").to_string());
            if let Some(b) = req.headers.get("authorization").and_then(|h| h.strip_prefix("Basic ")).and_then(|b| B64.decode(b.trim()).ok()).map(|b| String::from_utf8_lossy(&b).to_string()) {
                if let Some((a, b)) = b.split_once(':') {
                    cid = a.to_string();
                    csec = b.to_string();
                }
            }
            let want_id = util::resolve_secret(&oauth.client_id, &ctx.data_dir);
            let want_sec = util::resolve_secret(&oauth.client_secret, &ctx.data_dir);
            let ok = oauth.any_client || (!want_id.is_empty() && cid == want_id && csec == want_sec);
            let grant = req.body.get("grant_type").and_then(|v| v.as_str()).unwrap_or("client_credentials");
            if !ok || grant != "client_credentials" {
                steps.push(json!({"step": "receive", "text": format!("Token request from {} refused: invalid_client", if cid.is_empty() { "an unknown client".into() } else { cid.clone() }), "ok": false}));
                let (s, h, b) = status_answer(401, json!({"error": if grant != "client_credentials" { "unsupported_grant_type" } else { "invalid_client" }, "error_description": "Bad credentials"}));
                o.status = s;
                o.headers = h;
                o.body = b;
                o.answer = "invalid_client".into();
                o.sentence = format!("Token refused for {}: invalid_client", if cid.is_empty() { "a client".into() } else { cid });
                o.steps = steps;
                return o;
            }
            let ttl = util::parse_ms(&oauth.token_ttl).unwrap_or(3_600_000) as i64;
            let tok = if dry { "dbl_try_it".to_string() } else { store::issue_token(ctx, &svc.name, &cid, ttl).await.unwrap_or_default() };
            let who = if cid.is_empty() { req.caller.clone() } else { cid };
            steps.push(json!({"step": "receive", "text": format!("Client credentials from {who}"), "ok": true}));
            let (s, h, b) = status_answer(200, json!({"access_token": tok, "token_type": "Bearer", "expires_in": ttl / 1000, "scope": "default"}));
            o.status = s;
            o.headers = h;
            o.body = b;
            o.answer = "token".into();
            o.sentence = format!("Token issued to {who} (client credentials)");
            o.answered = json!({"expires_in": ttl / 1000});
            o.steps = steps;
            return o;
        }
        if r.login {
            let sess = svc.auth.session.clone().unwrap_or_default();
            let g = |k: &[&str]| k.iter().find_map(|x| req.body.get(*x).and_then(|v| v.as_str())).unwrap_or("").to_string();
            let (user, pw) = (g(&["username", "user", "login", "usuario"]), g(&["password", "senha", "pass"]));
            let ok = !user.is_empty() && (sess.any_user || sess.users.get(&user).map(|s| util::resolve_secret(s, &ctx.data_dir) == pw).unwrap_or(false));
            if !ok {
                steps.push(json!({"step": "receive", "text": format!("Login refused for {}", if user.is_empty() { "an empty user".to_string() } else { user.clone() }), "ok": false}));
                let (s, h, b) = status_answer(401, json!({"message": "Invalid user or password"}));
                o.status = s;
                o.headers = h;
                o.body = b;
                o.answer = "login_refused".into();
                o.sentence = format!("Login refused for {}", if user.is_empty() { "an empty user".to_string() } else { user });
                o.steps = steps;
                return o;
            }
            let ttl = util::parse_ms(&sess.ttl).unwrap_or(1_800_000) as i64;
            let id = if dry { "ses_try_it".to_string() } else { store::issue_token(ctx, &svc.name, &format!("session:{user}"), ttl).await.unwrap_or_default() };
            steps.push(json!({"step": "receive", "text": format!("Login of {user}: session cookie {} for {}", sess.cookie, sess.ttl), "ok": true}));
            sc.insert("user".into(), json!(user));
            let cookie = format!("{}={id}; Path=/; HttpOnly; Max-Age={}", sess.cookie, ttl / 1000);
            match r.decide.first().and_then(|d| p.answers.iter().find(|a| a.name == d.answer)) {
                Some(a) => {
                    let (status, mut headers, body, answered) = render_answer(a, &sc);
                    headers.push(("set-cookie".into(), cookie));
                    o.status = status;
                    o.headers = headers;
                    o.body = body;
                    o.answered = answered;
                    o.answer = a.name.clone();
                }
                None => {
                    let (s, mut h, b) = status_answer(200, json!({"ok": true}));
                    h.push(("set-cookie".into(), cookie));
                    o.status = s;
                    o.headers = h;
                    o.body = b;
                    o.answer = "logged_in".into();
                }
            }
            o.sentence = format!("{user} logged in (session for {})", sess.ttl);
            o.steps = steps;
            return o;
        }
        let auth = if r.receive.auth.is_empty() { if svc.auth.basic.is_some() { "basic" } else { "none" } } else { r.receive.auth.as_str() };
        let mut refuse: Option<(u16, Value, String, Vec<(String, String)>)> = None;
        match auth {
            "bearer" => {
                let tok = req.headers.get("authorization").and_then(|h| h.strip_prefix("Bearer ").or(h.strip_prefix("bearer "))).unwrap_or("").trim().to_string();
                if !dry {
                    match store::token(ctx, &svc.name, &tok).await.ok().flatten() {
                        None => refuse = Some((401, json!({"message": "Invalid token"}), "token invalid".into(), vec![])),
                        Some((_, issued, exp)) if exp < util::now_ms() => refuse = Some((401, json!({"message": "Token expired"}), format!("token from {} expired", stuntman_core::util::fmt_ms(((util::now_ms() - issued) / 60_000 * 60_000).max(60_000) as u64)), vec![])),
                        Some((client, _, _)) => {
                            // A token from a person grant carries the person.
                            let person = client.strip_prefix("person:").map(|c| crate::steps::find_person(p, c)).unwrap_or(Value::Null);
                            sc.insert("auth".into(), json!({"client": client, "person": person}));
                        }
                    }
                }
            }
            "session" => {
                let sess = svc.auth.session.clone().unwrap_or_default();
                let cookie = req.headers.get("cookie").and_then(|c| c.split(';').find_map(|kv| kv.trim().strip_prefix(&format!("{}=", sess.cookie)).map(String::from))).unwrap_or_default();
                if !dry {
                    match store::token(ctx, &svc.name, &cookie).await.ok().flatten() {
                        None => refuse = Some((401, json!({"message": "Not logged in"}), "no session cookie: log in first".into(), vec![])),
                        Some((_, _, exp)) if exp < util::now_ms() => refuse = Some((401, json!({"message": "Session expired"}), "session expired: log in again".into(), vec![])),
                        _ => {}
                    }
                }
            }
            "basic" => {
                let b = svc.auth.basic.clone().unwrap_or_default();
                let cred = req.headers.get("authorization").and_then(|h| h.strip_prefix("Basic ")).and_then(|x| B64.decode(x.trim()).ok()).map(|x| String::from_utf8_lossy(&x).to_string());
                let challenge = vec![("www-authenticate".to_string(), format!("Basic realm=\"{}\"", svc.name))];
                match cred.and_then(|c| c.split_once(':').map(|(u, p)| (u.to_string(), p.to_string()))) {
                    None if b.challenge || !b.any_user => refuse = Some((401, json!({"message": "Authentication required"}), "Basic challenge".into(), challenge)),
                    Some((u, pw)) if !b.any_user && b.users.get(&u).map(|s| util::resolve_secret(s, &ctx.data_dir) != pw).unwrap_or(true) => {
                        refuse = Some((401, json!({"message": "Invalid user or password"}), format!("wrong password for {u}"), challenge))
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        if refuse.is_none() {
            let missing: Vec<&String> = r.receive.body.iter().filter(|f| get_path(&req.body, f).is_none()).collect();
            if let Some(f) = missing.first() {
                if !r.receive.missing.is_empty() {
                    chosen = Some(r.receive.missing.clone());
                    steps.push(json!({"step": "receive", "text": format!("“{f}” is missing → {}", r.receive.missing), "ok": false}));
                } else {
                    refuse = Some((400, json!({"message": format!("{f} is required")}), format!("“{f}” missing"), vec![]));
                }
            }
        }
        // The body against the spec's schema (A8).
        if refuse.is_none() && chosen.is_none() && !r.receive.schema.is_empty() {
            if let Some(schema) = p.spec.as_ref().filter(|sp| sp.validate).and_then(|sp| sp.schemas.get(&r.receive.schema)) {
                let ct = req.headers.get("content-type").cloned().unwrap_or_default();
                let lenient = ct.contains("form");
                if let Err(e) = crate::openapi::check(schema, &req.body, "", lenient) {
                    sc.insert("invalid".into(), json!(e));
                    if !r.receive.invalid.is_empty() {
                        chosen = Some(r.receive.invalid.clone());
                        steps.push(json!({"step": "receive", "text": format!("Body breaks {}: {e} → {}", r.receive.schema, r.receive.invalid), "ok": false}));
                    } else {
                        refuse = Some((400, json!({"message": e}), format!("body breaks {}: {e}", r.receive.schema), vec![]));
                    }
                }
            }
        }
        if let Some((st, b, why, h)) = refuse {
            steps.push(json!({"step": "receive", "text": format!("Refused: {why}"), "ok": false}));
            let (s, mut hh, body) = status_answer(st, b);
            hh.extend(h);
            o.status = s;
            o.headers = hh;
            o.body = body;
            o.answer = format!("{st}");
            o.sentence = format!("Refused: {why} ({st})");
            o.steps = steps;
            return o;
        }
        if chosen.is_none() {
            steps.push(json!({"step": "receive", "text": format!("{} from {}{}{}{}", if auth == "bearer" { "Bearer token" } else if auth == "basic" { "Basic auth" } else if auth == "session" { "Session cookie" } else { "Request" }, req.caller,
                req.client_cert.as_ref().map(|c| format!(" (client certificate {c})")).unwrap_or_default(),
                if r.receive.body.is_empty() { String::new() } else { format!(" · body: {}", r.receive.body.join(", ")) },
                if r.receive.schema.is_empty() || !p.spec.as_ref().map(|x| x.validate).unwrap_or(false) { String::new() } else { format!(" · body matches {}", r.receive.schema) }), "ok": true}));
        }
    }

    // 3. A native step (in code) picks the answer; then step (a push the app polls), let, keep, wait, decide.
    if chosen.is_none() && !r.native.is_empty() {
        if dry {
            steps.push(json!({"step": "native", "text": format!("{} is not run in Try it", r.native), "ok": false}));
        } else {
            let n = crate::restpki::run(ctx, p, &r.native, &sc["params"], &req.body).await;
            steps.push(json!({"step": "native", "text": format!("{} → {}", r.native, n.answer), "ok": true}));
            sc.insert("result".into(), n.result);
            sc.insert("call".into(), n.note.clone());
            o.sent = n.note;
            chosen = Some(n.answer.into());
        }
    }
    // step (a push the app polls), let, keep, wait, decide.
    if chosen.is_none() && !r.step.is_empty() {
        let code = expr::eval(&r.step, &sc).map(|v| expr::text(&v)).unwrap_or_default();
        let st = if dry || code.is_empty() { None } else { crate::steps::get(ctx, &code).await.ok().flatten().filter(|s| s["service"] == json!(svc.name)) };
        steps.push(json!({"step": "step", "text": match &st { Some(s) => format!("Push {code}: {}", s["status"].as_str().unwrap_or("")), None => format!("No push “{code}”") }, "ok": st.is_some()}));
        sc.insert("step".into(), st.unwrap_or(Value::Null));
    }
    let mut keep = None;
    if chosen.is_none() {
        for (k, e) in expr::let_order(&r.lets) {
            match expr::eval(e, &sc) {
                Ok(v) => {
                    sc.insert(k.clone(), v);
                }
                Err(x) => steps.push(json!({"step": "let", "text": format!("{k}: {x}"), "ok": false})),
            }
        }
        keep = r.keep.as_ref();
        if let Some(w) = &r.wait {
            let key = expr::eval(&w.key, &sc).map(|v| expr::text(&v)).unwrap_or_default();
            let n = if dry { 1 } else { store::wait_count(ctx, &svc.name, &route_name, &key).await.unwrap_or(1) };
            let a = if n as u32 <= w.times { w.answer.clone() } else { w.then.clone() };
            steps.push(json!({"step": "wait", "text": format!("Call {n} for {key}: “{a}” ({} times, then “{}”)", w.times, w.then), "ok": true}));
            chosen = Some(a);
        }
    }
    if chosen.is_none() {
        for (i, d) in r.decide.iter().enumerate() {
            let hit = match &d.when {
                None => true,
                Some(w) => match expr::test(w, &sc) {
                    Ok(b) => b,
                    Err(x) => {
                        steps.push(json!({"step": "decide", "text": format!("{w}: {x}"), "ok": false}));
                        false
                    }
                },
            };
            if hit {
                steps.push(json!({"step": "decide", "text": format!("{} → {}{}", d.when.clone().unwrap_or("otherwise".into()), d.answer, if d.scenario.is_empty() { String::new() } else { format!(" · scenario → {}", d.scenario) }), "ok": true, "rule": i}));
                if !d.scenario.is_empty() && !dry {
                    let _ = store::keep(ctx, &svc.name, "__scenario", "state", json!({"name": d.scenario})).await;
                }
                chosen = Some(d.answer.clone());
                break;
            }
        }
    }
    let Some(name) = chosen else {
        steps.push(json!({"step": "decide", "text": "No rule matched and there is no “otherwise”", "ok": false}));
        let (s, h, b) = status_answer(500, json!({"error": "Stuntman: the route decided no answer"}));
        o.status = s;
        o.headers = h;
        o.body = b;
        o.answer = "no answer".into();
        o.sentence = "No answer decided (the plugin needs an “otherwise”)".into();
        o.steps = steps;
        return o;
    };

    // 4. Respond.
    let (lo, hi) = plugin::latency(if r.respond.latency.is_empty() { &svc.limits.latency } else { &r.respond.latency });
    o.latency_ms = if hi > lo { lo + rand::random::<u64>() % (hi - lo + 1) } else { lo };
    match p.answers.iter().find(|a| a.name == name) {
        Some(a) => {
            let (status, headers, mut body, answered) = render_answer(a, &sc);
            if !a.file.is_empty() {
                body = std::fs::read(crate::record::files_dir(ctx, &svc.name).join(&a.file)).unwrap_or_default();
            }
            o.status = status;
            o.headers = headers;
            o.body = body;
            o.answered = answered;
            o.answer = a.name.clone();
            if !a.stream.is_empty() {
                stream_parts(&mut o, a, &sc, stream_force.as_deref());
            }
            if let Some(ms) = util::parse_ms(&a.latency) {
                o.latency_ms = ms;
            }
            o.sentence = if a.sentence.is_empty() { format!("{m} {} → {}", req.path, a.name) } else { expr::render(&a.sentence, &sc).unwrap_or_else(|e| format!("(sentence: {e})")) };
            steps.push(json!({"step": "respond", "text": format!("{} {}  ·  latency {} ms", a.name, status, o.latency_ms), "ok": status < 400}));
        }
        None => {
            if ["reset", "empty", "timeout"].contains(&name.as_str()) {
                o.network = Some(name.clone());
                o.status = 0;
            } else {
                let (s, h, b) = generic(&name);
                o.status = s;
                o.headers = h;
                o.body = b;
            }
            o.answer = name.clone();
            o.sentence = format!("{} {}: {}", m, req.path, plugin::GENERIC.iter().find(|g| g.0 == name).map(|g| g.1.to_string()).unwrap_or(name.clone()));
            steps.push(json!({"step": "respond", "text": o.sentence.clone(), "ok": false}));
        }
    }
    // keep saves only what was accepted: a refused call (4xx/5xx) leaves the state as it was.
    if let Some(k) = keep {
        let key = expr::eval(&k.key, &sc).map(|v| expr::text(&v)).unwrap_or_default();
        let ok = o.status > 0 && o.status < 400;
        if !dry && ok {
            let _ = store::keep(ctx, &svc.name, &k.state, &key, json!({"at": sc.get("__now").cloned().unwrap_or_else(|| json!(util::now_ms())), "body": req.body})).await;
        }
        steps.push(json!({"step": "keep", "text": if ok { format!("Saved to state “{}” by {}{}", k.state, key, if dry { " (not kept: Try it)" } else { "" }) } else { format!("Not saved to state “{}”: the answer is {}", k.state, o.status) }, "ok": true}));
    }
    if !r.send.is_empty() {
        steps.push(json!({"step": "send", "text": format!("{} webhook(s) after responding{}", r.send.len(), if dry { " (not sent: Try it)" } else { "" }), "ok": true}));
    }
    sc.insert("answer".into(), json!(o.answer));
    sc.insert("__response".into(), json!({"status": o.status, "body": String::from_utf8_lossy(&o.body).chars().take(256 * 1024).collect::<String>(),
        "headers": o.headers.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<Map<String, Value>>()}));
    o.scope = sc;
    o.steps = steps;
    o
}

/// The parts of a streamed answer, rendered; `force` cut keeps half and breaks the connection,
/// slow waits 2 s more before each part.
pub fn stream_parts(o: &mut Outcome, a: &Answer, sc: &Map<String, Value>, force: Option<&str>) {
    let fmt = if a.stream_format.is_empty() { "sse" } else { a.stream_format.as_str() };
    let mut parts = vec![];
    for pt in &a.stream {
        let data = expr::render(&pt.data, sc).unwrap_or_else(|e| format!("template error: {e}"));
        let bytes = match fmt {
            "sse" => {
                let mut t = String::new();
                if !pt.event.is_empty() {
                    t.push_str(&format!("event: {}\n", pt.event));
                }
                for l in data.lines() {
                    t.push_str(&format!("data: {l}\n"));
                }
                if data.is_empty() {
                    t.push_str("data: \n");
                }
                t.push('\n');
                t
            }
            "ndjson" => format!("{}\n", data.replace('\n', " ")),
            _ => data,
        };
        let wait = stuntman_core::util::parse_ms(&pt.after).unwrap_or(0) + if force == Some("slow") { 2000 } else { 0 };
        parts.push((wait, bytes.into_bytes()));
    }
    if force == Some("cut") {
        parts.truncate((parts.len() / 2).max(1));
        o.cut = true;
    }
    o.headers.retain(|(k, _)| k != "content-type");
    o.headers.push(("content-type".into(), match fmt { "sse" => "text/event-stream", "ndjson" => "application/x-ndjson", _ => "text/plain; charset=utf-8" }.into()));
    o.headers.push(("cache-control".into(), "no-cache".into()));
    o.body = parts.iter().flat_map(|p| p.1.clone()).collect();
    o.parts = parts;
}

/// A GraphQL request names this operation: operationName, the operation's name in the query, or
/// the first field it selects ("{ account(id: 1) { … } }" → account).
pub fn gql_matches(body: &Value, name: &str) -> bool {
    if body.get("operationName").and_then(|v| v.as_str()) == Some(name) {
        return true;
    }
    let Some(q) = body.get("query").and_then(|v| v.as_str()) else { return false };
    let q: String = q.lines().map(|l| l.split('#').next().unwrap_or("")).collect::<Vec<_>>().join(" ");
    let head = q[..q.find('{').unwrap_or(q.len())].trim();
    let mut w = head.split(|c: char| c.is_whitespace() || c == '(' );
    if matches!(w.next(), Some("query" | "mutation" | "subscription")) && w.next() == Some(name) {
        return true;
    }
    let Some(i) = q.find('{') else { return false };
    let rest = q[i + 1..].trim_start();
    let id: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    let after = rest[id.len()..].trim_start();
    let field = if let Some(a) = after.strip_prefix(':') { a.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect() } else { id };
    field == name
}

/// Status, headers, body and the first answered fields of a named answer.
pub fn render_answer(a: &Answer, sc: &Map<String, Value>) -> (u16, Vec<(String, String)>, Vec<u8>, Value) {
    // A WireMock template renders its headers as templates too (Location: http://localhost:{{request.port}}/x).
    let hb = a.template == "handlebars";
    let mut headers: Vec<(String, String)> = a.headers.iter().map(|(k, v)| (k.to_lowercase(), if hb { crate::hbs::render(v, sc).unwrap_or_else(|_| v.clone()) } else { expr::render(v, sc).unwrap_or_else(|_| v.clone()) })).collect();
    let mut status = a.status;
    let body = if a.raw {
        a.body.clone()
    } else if a.template == "handlebars" {
        // A template that fails answers 500, as WireMock does.
        crate::hbs::render(&a.body, sc).unwrap_or_else(|e| { status = 500; format!("Stuntman: template error: {e}") })
    } else {
        expr::render(&a.body, sc).unwrap_or_else(|e| format!("{{\"stuntman\": \"template error: {e}\"}}"))
    };
    if !headers.iter().any(|(k, _)| k == "content-type") && !body.is_empty() {
        let t = body.trim_start();
        headers.push(("content-type".into(), if t.starts_with('<') { if t.to_lowercase().starts_with("<html") || t.to_lowercase().starts_with("<!doctype") { "text/html" } else { "application/xml" } } else { "application/json" }.into()));
    }
    let mut lv = vec![];
    if let Ok(v) = serde_json::from_str::<Value>(&body) {
        leaves(&v, "", &mut lv);
    }
    (status, headers, body.into_bytes(), Value::Object(lv.into_iter().collect()))
}

/// Checks an answer against the route's schema with a sample request (A4 "must be a number").
pub fn check_answer(r: &Route, a: &Answer, p: &Plugin) -> Option<String> {
    if a.raw {
        return None;
    }
    let mut sc = Map::new();
    sc.insert("body".into(), json!({}));
    sc.insert("data".into(), json!(p.data));
    sc.insert("state".into(), json!({}));
    for (k, _) in &r.lets {
        sc.insert(k.clone(), Value::Null);
    }
    // The lets are null here: a function that needs real data (cert_pem(person)) fails, and the
    // holes already parsed in validate. Only a body that renders is checked against the schema.
    if a.template == "handlebars" {
        return None;
    }
    let Ok(body) = expr::render(&a.body, &sc) else { return None };
    let ct = a.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-type")).map(|x| x.1.clone()).unwrap_or_default();
    if body.trim().is_empty() || ct.contains("xml") || ct.contains("html") || body.trim_start().starts_with('<') {
        return None;
    }
    let v: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return Some(format!("the body is not valid JSON ({e}); mark it “malformed on purpose” to send it anyway")),
    };
    for (path, ty) in &r.schema {
        let got = path.split('.').try_fold(&v, |acc, k| acc.get(k));
        let ok = match (got, ty.as_str()) {
            (None, _) => continue,
            (Some(x), "number") => x.is_number() || x.is_null(),
            (Some(x), "string") => x.is_string() || x.is_null(),
            (Some(x), "bool" | "boolean") => x.is_boolean() || x.is_null(),
            (Some(x), "object") => x.is_object(),
            (Some(x), "array") => x.is_array(),
            _ => true,
        };
        if !ok {
            return Some(format!("“{}” must be a {ty}", path.rsplit('.').next().unwrap_or(path)));
        }
    }
    None
}

/// Webhooks of a route, after responding: at once, or kept until the environment's clock reaches
/// their time (delay, A10c).
pub fn send_webhooks(ctx: &Ctx, call_id: &str, p: &Plugin, r: &Route, sc: Map<String, Value>) {
    let wh = p.service.webhooks.clone();
    let prov = wh.as_ref().map(|w| w.provoke.clone()).unwrap_or_default();
    let source = if p.service.base_url.is_empty() { format!("/{}", p.service.name) } else { p.service.base_url.clone() };
    let n = r.send.len();
    for (i, s) in r.send.iter().enumerate() {
        if !s.when.is_empty() && !expr::test(&s.when, &sc).unwrap_or(false) {
            continue;
        }
        let mut delay = util::parse_ms(&s.delay).unwrap_or(0);
        if prov.iter().any(|x| x == "out_of_order") && n > 1 && i == 0 {
            delay += 2000;
        }
        let times = if prov.iter().any(|x| x == "twice") { 2 } else { 1 };
        let job = json!({"call": call_id, "send": s, "webhooks": wh, "scope": sc, "source": source});
        for _ in 0..times {
            if delay == 0 {
                let (ctx, job) = (ctx.clone(), job.clone());
                tokio::spawn(async move { fire(&ctx, job).await });
            } else {
                let (ctx, job, due) = (ctx.clone(), job.clone(), stuntman_core::clock::now(ctx) + delay as i64);
                tokio::spawn(async move {
                    let _ = store::schedule(&ctx, due, job).await;
                });
            }
        }
    }
}

/// Sends whose time has come on the environment's clock.
pub async fn fire_due(ctx: &Ctx) {
    for job in store::take_due(ctx, stuntman_core::clock::now(ctx)).await.unwrap_or_default() {
        let ctx = ctx.clone();
        tokio::spawn(async move { fire(&ctx, job).await });
    }
}

/// One webhook: rendered, signed in the service's format, sent, kept on the call.
async fn fire(ctx: &Ctx, job: Value) {
    let Ok(s) = serde_json::from_value::<plugin::Send>(job["send"].clone()) else { return };
    let wh: Option<plugin::Webhooks> = serde_json::from_value(job["webhooks"].clone()).ok().flatten();
    let sc: Map<String, Value> = job["scope"].as_object().cloned().unwrap_or_default();
    let id = job["call"].as_str().unwrap_or("").to_string();
    let hb = s.template == "handlebars";
    let tpl = |t: &str| if hb { crate::hbs::render(t, &sc).unwrap_or_default() } else { expr::render(t, &sc).unwrap_or_default() };
    let url = tpl(&s.url);
    let body = tpl(&s.body);
    // A SOAP callback: the body is the payload, inside an envelope of the call's version.
    if !s.operation.is_empty() {
        let v = sc.get("soap_version").and_then(|v| v.as_str()).unwrap_or("1.2").to_string();
        let t0 = std::time::Instant::now();
        let (status, err) = match crate::soap::post(ctx, &url, &v, &s.operation, &body).await { Ok((st, _)) => (Some(st), String::new()), Err(e) => (None, e) };
        let _ = store::add_send(ctx, &id, json!({"at": util::now_ms(), "url": url, "operation": s.operation, "status": status, "error": err, "ms": t0.elapsed().as_millis() as i64})).await;
        return;
    }
    let w = wh.clone().unwrap_or_default();
    let sign = if !s.sign.is_empty() { s.sign.clone() } else if wh.is_some() { w.sign.clone() } else if !s.secret.is_empty() { "hex".into() } else { String::new() };
    // The secret may be a template: each subscription signs with its own key ("{{ state.sub.signing_key }}").
    let secret = util::resolve_secret(&expr::render(if s.secret.is_empty() { &w.secret } else { &s.secret }, &sc).unwrap_or_default(), &ctx.data_dir);
    let header = if s.header.is_empty() { w.header.clone() } else { s.header.clone() };
    let tol = if wh.is_some() { w.tolerance } else { 300 };
    let mut headers: Vec<(String, String)> = vec![("content-type".into(), "application/json".into())];
    // Header names are templates too; a name that renders empty is left out.
    // "*": the value renders to "Name: value" lines, one header each (headers a subscriber chose).
    for (k, v) in &s.headers {
        let k = if hb { k.clone() } else { expr::render(k, &sc).unwrap_or_default() };
        let v = tpl(v);
        if k.trim() == "*" {
            for line in v.lines() {
                if let Some((n, x)) = line.split_once(':').filter(|(n, _)| !n.trim().is_empty()) {
                    headers.push((n.trim().to_string(), x.trim().to_string()));
                }
            }
        } else if !k.trim().is_empty() {
            headers.push((k.trim().to_string(), v));
        }
    }
    // A send that names its own Content-Type (form-encoded callbacks) replaces the JSON default.
    if headers.iter().skip(1).any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
        headers.remove(0);
    }
    let evid = format!("evt_{}", util::uuid().replace('-', "")[..20].to_string());
    let method = s.method.to_uppercase();
    headers.extend(sign_headers(&sign, &header, &secret, &body, stuntman_core::clock::now(ctx) / 1000, tol, &w.provoke, &s.event, job["source"].as_str().unwrap_or(""), &evid, &method, &url));
    let mut req = ctx.http.request(method.parse().unwrap_or(reqwest::Method::POST), &url).body(body.clone());
    for (k, v) in &headers {
        req = req.header(k, v);
    }
    let t0 = std::time::Instant::now();
    let r = req.send().await;
    let (status, err) = match &r { Ok(x) => (Some(x.status().as_u16()), String::new()), Err(e) => (None, if e.is_connect() { "connection refused".into() } else { e.to_string() }) };
    let _ = store::add_send(ctx, &id, json!({"at": util::now_ms(), "url": url, "status": status, "error": err, "ms": t0.elapsed().as_millis() as i64, "sign": sign, "event": s.event,
        "provoked": w.provoke, "headers": headers.iter().filter(|(k, _)| k != "content-type").map(|(k, v)| json!([k, v])).collect::<Vec<_>>()})).await;
}

fn hmac256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    use hmac::Mac;
    let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).unwrap();
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

/// The signature headers of one webhook in a provider's format; `provoke` breaks them on purpose.
#[allow(clippy::too_many_arguments)]
pub fn sign_headers(sign: &str, header: &str, secret: &str, body: &str, ts: i64, tolerance: u32, provoke: &[String], event: &str, source: &str, id: &str, method: &str, url: &str) -> Vec<(String, String)> {
    let has = |x: &str| provoke.iter().any(|p| p == x);
    let ts = if has("old_timestamp") { ts - tolerance as i64 - 60 } else { ts };
    let secret = if has("wrong_signature") { format!("{secret}-wrong") } else { secret.to_string() };
    let h = |d: &str| if header.is_empty() { d.to_string() } else { header.to_string() };
    let mut out: Vec<(String, String)> = vec![];
    let skip = has("no_signature");
    match sign {
        "hex" if !skip => out.push((h("X-Signature"), hex::encode(hmac256(secret.as_bytes(), body.as_bytes())))),
        "hex_ts" => {
            out.push(("X-Webhook-Timestamp".into(), ts.to_string()));
            if !skip {
                out.push((h("X-Webhook-Signature"), hex::encode(hmac256(secret.as_bytes(), format!("{ts}.{body}").as_bytes()))));
            }
        }
        "stripe" if !skip => out.push((h("Stripe-Signature"), format!("t={ts},v1={}", hex::encode(hmac256(secret.as_bytes(), format!("{ts}.{body}").as_bytes()))))),
        "svix" => {
            let key = secret.strip_prefix("whsec_").and_then(|k| B64.decode(k).ok()).unwrap_or_else(|| secret.as_bytes().to_vec());
            let msg_id = format!("msg_{}", id.trim_start_matches("evt_"));
            out.push(("svix-id".into(), msg_id.clone()));
            out.push(("svix-timestamp".into(), ts.to_string()));
            if !skip {
                out.push((h("svix-signature"), format!("v1,{}", B64.encode(hmac256(&key, format!("{msg_id}.{ts}.{body}").as_bytes())))));
            }
        }
        "github" if !skip => out.push((h("X-Hub-Signature-256"), format!("sha256={}", hex::encode(hmac256(secret.as_bytes(), body.as_bytes()))))),
        // Twilio: HMAC-SHA1 of the URL followed by every form parameter, sorted, as name+value; base64.
        "twilio" if !skip => {
            let mut params: Vec<(String, String)> = body.split('&').filter(|x| !x.is_empty()).map(|kv| {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                (netserve_decode(&k.replace('+', " ")), netserve_decode(&v.replace('+', " ")))
            }).collect();
            params.sort();
            let msg = params.iter().fold(url.to_string(), |acc, (k, v)| acc + k + v);
            use hmac::Mac;
            let mut m = hmac::Hmac::<sha1::Sha1>::new_from_slice(secret.as_bytes()).unwrap();
            m.update(msg.as_bytes());
            out.push((h("X-Twilio-Signature"), B64.encode(m.finalize().into_bytes())));
        }
        // Slack: v0= HMAC-SHA256 of "v0:{timestamp}:{body}", hex, with X-Slack-Request-Timestamp.
        "slack" => {
            out.push(("X-Slack-Request-Timestamp".into(), ts.to_string()));
            if !skip {
                out.push((h("X-Slack-Signature"), format!("v0={}", hex::encode(hmac256(secret.as_bytes(), format!("v0:{ts}:{body}").as_bytes())))));
            }
        }
        // Shopify: HMAC-SHA256 of the raw body, base64.
        "shopify" if !skip => out.push((h("X-Shopify-Hmac-Sha256"), B64.encode(hmac256(secret.as_bytes(), body.as_bytes())))),
        // HubSpot v3: HMAC-SHA256 of method + URL + body + timestamp (milliseconds), base64.
        "hubspot" => {
            let ms = ts * 1000;
            out.push(("X-HubSpot-Request-Timestamp".into(), ms.to_string()));
            if !skip {
                out.push((h("X-HubSpot-Signature-v3"), B64.encode(hmac256(secret.as_bytes(), format!("{method}{url}{body}{ms}").as_bytes()))));
            }
        }
        "cloudevents" => {
            out.push(("ce-specversion".into(), "1.0".into()));
            out.push(("ce-type".into(), if event.is_empty() { "com.stuntman.event".into() } else { event.to_string() }));
            out.push(("ce-source".into(), source.to_string()));
            out.push(("ce-id".into(), id.to_string()));
            out.push(("ce-time".into(), expr::iso(ts * 1000, None)));
            if !skip && !secret.is_empty() && !header.is_empty() {
                out.push((header.to_string(), hex::encode(hmac256(secret.as_bytes(), body.as_bytes()))));
            }
        }
        _ => {}
    }
    out
}

/// Which plugin a request is for, and the path inside it.
pub fn find_service<'a>(plugins: &'a [Plugin], host: &str, path: &str) -> Option<(&'a Plugin, String)> {
    let h = host.split(':').next().unwrap_or("").to_lowercase();
    let first = h.split('.').next().unwrap_or("");
    let base_host = |u: &str| u.split("://").nth(1).unwrap_or(u).split('/').next().unwrap_or("").to_lowercase();
    if let Some(p) = plugins.iter().find(|p| (!p.service.real_host.is_empty() && p.service.real_host.matches(&h)) || (!p.service.base_url.is_empty() && base_host(&p.service.base_url) == h) || (h.contains('.') && p.service.name == first)) {
        return Some((p, path.to_string()));
    }
    let seg = path.trim_start_matches('/').split('/').next().unwrap_or("");
    plugins.iter().find(|p| p.service.name == seg).map(|p| (p, format!("/{}", path.trim_start_matches('/').splitn(2, '/').nth(1).unwrap_or(""))))
}

pub async fn handle(State(envs): State<Envs>, mut req: Request) -> Response {
    let kill = req.extensions().get::<Kill>().cloned();
    let _remote = req.extensions().get::<Remote>().map(|r| r.0.ip().to_string()).unwrap_or_default();
    let client_cert = req.extensions().get::<crate::tls::ClientCert>().map(|c| c.0.clone());
    let scheme = if req.extensions().get::<crate::tls::Tls>().is_some() { "https" } else { "http" };
    let client_cnpj = req.extensions().get::<crate::tls::ClientCertDer>().and_then(|c| crate::ca::cnpj_of(&c.0));
    let routed = match netserve::route(&envs, &mut req) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let ctx = routed.ctx;
    if ctx.state.paused() {
        return (StatusCode::SERVICE_UNAVAILABLE, format!("{} is paused in Stuntman", ctx.env)).into_response();
    }
    let headers = netserve::lower_headers(req.headers());
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let raw = axum::body::to_bytes(req.into_body(), 64 << 20).await.map(|b| b.to_vec()).unwrap_or_default();
    let host = headers.get("host").cloned().unwrap_or_default();
    let plugins = plugin::shared(&ctx);
    let t0 = std::time::Instant::now();
    let found = find_service(&plugins, &host, &path);
    let (p, inner) = match found {
        Some(x) => x,
        None => {
            // Not answered is still a call to HTTP APIs (the ones a service took count in live.seen below).
            stuntman_core::usage::hit(&ctx, "apis");
            return unmatched(&ctx, &plugins, "", &method, &path, &path, &host, read_request(&method, &path, &query, headers, raw)).await;
        }
    };
    // "/acme/v0/…" reached by the path prefix: the pages post back under the same prefix.
    let base = format!("{}{}", ctx.prefix(), path[..path.len() - inner.len().min(path.len())].trim_end_matches('/'));
    let mut rq = read_request(&method, &inner, &query, headers.clone(), raw);
    rq.scheme = scheme.into();
    rq.client_cert = client_cert;
    rq.client_cnpj = client_cnpj;
    if !headers.contains_key("x-stuntman-app") && !headers.contains_key("x-duble-app") {
        if let Some(c) = &rq.client_cert {
            rq.caller = c.clone();
        }
    }
    ctx.live.seen(&rq.caller, "APIs");
    if p.service.cors && method == "OPTIONS" {
        return (StatusCode::NO_CONTENT, [("access-control-allow-origin", "*"), ("access-control-allow-headers", "*"), ("access-control-allow-methods", "GET, POST, PUT, PATCH, DELETE, OPTIONS")]).into_response();
    }
    // A9: recording the real provider; mode unknown only for calls nothing here answers.
    if let Some(rc) = p.record.as_ref().filter(|rc| crate::record::live(rc)) {
        let known = inner.starts_with("/__stuntman/")
            || p.routes.iter().any(|r| { let (m, pat) = r.method_path(); (m == method || (m == "GET" && method == "HEAD")) && match_path_case(&pat, &inner, p.service.ignore_case).is_some() })
            || ((method == "GET" || method == "HEAD") && p.pages.iter().any(|pg| match_path(&pg.method_path().1, &inner).is_some()))
            || p.soap.as_ref().map(|sp| inner.trim_end_matches('/').eq_ignore_ascii_case(sp.endpoint.trim_end_matches('/'))).unwrap_or(false);
        let full = rc.max > 0 && store::count_recorded(&ctx, &p.service.name, rc.started).await.unwrap_or(0) >= rc.max as i64;
        if !full && (rc.mode == "every" && !inner.starts_with("/__stuntman/") || rc.mode == "unknown" && !known) {
            return crate::record::forward(&ctx, p, rc, &method, &path, &inner, &query, &headers, &rq, t0, kill).await;
        }
    }
    // A8: the spec a service was made from, served back.
    if p.spec.is_some() && (method == "GET" || method == "HEAD") {
        let taken = p.routes.iter().any(|r| { let (m, pat) = r.method_path(); m == "GET" && match_path_case(&pat, &inner, p.service.ignore_case).is_some() });
        if !taken {
            if let Some(r) = crate::openapi::serve_file(&ctx, p, &inner) {
                return r;
            }
        }
    }
    // Browser steps: the pages Stuntman draws, and what people do on them.
    if let Some(rest) = inner.strip_prefix("/__stuntman/") {
        let parts: Vec<&str> = rest.split('/').collect();
        match (method.as_str(), parts.as_slice()) {
            ("GET", ["preview", i]) => {
                let Some(pg) = i.parse::<usize>().ok().and_then(|i| p.pages.get(i)) else { return StatusCode::NOT_FOUND.into_response() };
                let mut sc = Map::new();
                sc.insert("query".into(), json!({"client_id": "acme-api", "redirect_uri": "https://acme.example.com/callback", "state": "58213"}));
                sc.insert("data".into(), json!(p.data));
                return ([("content-type", "text/html; charset=utf-8")], crate::steps::page_html(p, pg, None, &base, &sc, None, true)).into_response();
            }
            ("GET", ["steps", id]) => {
                let o = crate::steps::show(&ctx, p, id, &base).await;
                return respond(o, kill).await;
            }
            ("POST", ["steps", id]) => {
                let form = rq.body.as_object().cloned().unwrap_or_default();
                let by = if rq.caller == "browser" { "the person".to_string() } else { rq.caller.clone() };
                let o = match crate::steps::act(&ctx, p, id, &form, &by, &base).await {
                    Ok(o) => o,
                    Err(e) => {
                        let step = crate::steps::get(&ctx, id).await.ok().flatten();
                        let Some(pg) = step.as_ref().and_then(|s| p.pages.get(s["page"].as_u64().unwrap_or(0) as usize)) else { return (StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST), e.message).into_response() };
                        let mut sc = Map::new();
                        sc.insert("query".into(), step.as_ref().map(|s| s["params"].clone()).unwrap_or(Value::Null));
                        sc.insert("data".into(), json!(p.data));
                        let html = crate::steps::page_html(p, pg, step.as_ref(), &base, &sc, Some(&e.message), false);
                        Outcome { status: e.status, headers: vec![("content-type".into(), "text/html; charset=utf-8".into())], body: html.into_bytes(), answer: "refused".into(), sentence: format!("{}: {}", pg.title, e.message), ..Default::default() }
                    }
                };
                return finish(&ctx, p, None, "POST /__stuntman/steps", &method, &path, &inner, &query, &headers, &rq, o, t0, kill).await;
            }
            _ => return StatusCode::NOT_FOUND.into_response(),
        }
    }
    if method == "GET" || method == "HEAD" {
        if let Some(idx) = p.pages.iter().position(|pg| match_path(&pg.method_path().1, &inner).is_some()) {
            let o = crate::steps::open(&ctx, p, idx, &rq, &base).await;
            let label = p.pages[idx].route.clone();
            return finish(&ctx, p, None, &label, &method, &path, &inner, &query, &headers, &rq, o, t0, kill).await;
        }
    }
    // A SOAP service (A7): ?wsdl and its XSDs, and envelopes on the endpoint.
    if let Some(sp) = &p.soap {
        if method == "GET" || method == "HEAD" {
            if let Some(r) = crate::soap::serve_file(&ctx, p, &inner, &query) {
                return r;
            }
        }
        if method == "POST" && inner.trim_end_matches('/').eq_ignore_ascii_case(sp.endpoint.trim_end_matches('/')) {
            let (r, o) = crate::soap::serve(&ctx, p, &rq, false).await;
            if o.latency_ms > 0 {
                tokio::time::sleep(Duration::from_millis(o.latency_ms)).await;
            }
            let label = r.as_ref().map(|r| format!("SOAP {}", r.operation)).unwrap_or_else(|| "SOAP".into());
            return finish(&ctx, p, r.as_ref(), &label, &method, &path, &inner, &query, &headers, &rq, o, t0, kill).await;
        }
    }
    // A10b: the service as an MCP server.
    if let Some(m) = &p.mcp {
        if inner.trim_end_matches('/').eq_ignore_ascii_case(m.path.trim_end_matches('/')) {
            let (r, o) = crate::mcp::serve(&ctx, p, m, &method, &rq).await;
            let label = r.map(|r| format!("MCP {}", r.tool)).unwrap_or_else(|| "MCP".into());
            return finish(&ctx, p, r, &label, &method, &path, &inner, &query, &headers, &rq, o, t0, kill).await;
        }
    }
    // GraphQL routes first (they share a path and differ by operation), then the others.
    let candidates: Vec<(&Route, Map<String, Value>)> = p.routes.iter().filter(|r| !r.graphql.is_empty()).chain(p.routes.iter().filter(|r| r.graphql.is_empty())).filter_map(|r| {
        let (m, pat) = r.method_path();
        // ANY: every method (imported WireMock mappings).
        if m != method && m != "ANY" && !(m == "GET" && method == "HEAD") {
            return None;
        }
        if !r.graphql.is_empty() && !gql_matches(&rq.body, &r.graphql) {
            return None;
        }
        match_path_case(&pat, &inner, p.service.ignore_case).map(|pp| (r, pp))
    }).collect();
    let Some((r, params)) = candidates.first().cloned() else {
        return unmatched(&ctx, &plugins, &p.service.name, &method, &path, &inner, &host, rq).await;
    };
    // A10c: a key seen before gets the first answer again; with another body, 409.
    let idem = p.service.idempotency.as_ref().filter(|_| ["POST", "PUT", "PATCH", "DELETE"].contains(&method.as_str())).and_then(|i| headers.get(&i.header.to_lowercase()).map(|k| (i.clone(), k.clone())));
    let hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&rq.raw_body));
    if let Some((i, k)) = &idem {
        let ttl = util::parse_ms(&i.ttl).unwrap_or(86_400_000) as i64;
        if let Ok(Some((h, status, hs, body))) = store::idem_get(&ctx, &p.service.name, k, util::now_ms() - ttl).await {
            let (m, pat) = r.method_path();
            let o = if h == hash {
                let mut hs: Vec<(String, String)> = hs;
                hs.push(("idempotent-replayed".into(), "true".into()));
                Outcome { status, headers: hs, body, answer: "replayed".into(), sentence: format!("{m} {pat}: replayed the first answer for {} {k}", i.header), provoked: None, ..Default::default() }
            } else {
                let (s, h, b) = status_answer(409, json!({"error": {"type": "idempotency_error", "message": format!("Keys for idempotent requests can only be used with the same parameters they were first used with. Try using a key other than '{k}'.")}}));
                Outcome { status: s, headers: h, body: b, answer: "idempotency_conflict".into(), sentence: format!("{m} {pat}: {} {k} reused with another body (409)", i.header), ..Default::default() }
            };
            return finish(&ctx, p, None, &format!("{m} {pat}"), &method, &path, &inner, &query, &headers, &rq, o, t0, kill).await;
        }
    }
    let mut o = run(&ctx, p, r, params, &rq, false).await;
    // Imported mocks: no mapping of this route matched, try the next route that takes the path.
    let mut r = r;
    for (next, pp) in candidates.iter().skip(1) {
        if !(r.fallthrough && o.answer == "not_matched") {
            break;
        }
        o = run(&ctx, p, next, pp.clone(), &rq, false).await;
        r = next;
    }
    if let Some((_, k)) = &idem {
        if o.status > 0 && o.status < 500 && o.network.is_none() {
            let _ = store::idem_put(&ctx, &p.service.name, k, &hash, o.status, &o.headers, &o.body).await;
        }
    }
    if o.latency_ms > 0 {
        tokio::time::sleep(Duration::from_millis(o.latency_ms)).await;
    }
    let (m, pat) = r.method_path();
    finish(&ctx, p, Some(r), &format!("{m} {pat}"), &method, &path, &inner, &query, &headers, &rq, o, t0, kill).await
}

/// Records the call (A1, Timeline), sends the route's webhooks and answers.
#[allow(clippy::too_many_arguments)]
async fn finish(ctx: &Ctx, p: &Plugin, r: Option<&Route>, route: &str, method: &str, path: &str, inner: &str, query: &str, headers: &BTreeMap<String, String>, rq: &Req, mut o: Outcome, t0: std::time::Instant, kill: Option<Kill>) -> Response {
    if p.service.cors {
        o.headers.push(("access-control-allow-origin".into(), "*".into()));
    }
    let id = util::id("call");
    let billable = r.map(|r| r.billable).unwrap_or(false);
    log_call(ctx, p, &id, billable, route, method, path, inner, query, headers, rq, &o, t0).await;
    if let Some(r) = r {
        if o.status > 0 && o.status < 500 {
            send_webhooks(ctx, &id, p, r, o.scope.clone());
        }
    }
    respond(o, kill).await
}

/// Keeps one call in the history and on the timeline.
#[allow(clippy::too_many_arguments)]
pub async fn log_call(ctx: &Ctx, p: &Plugin, id: &str, billable: bool, route: &str, method: &str, path: &str, inner: &str, query: &str, headers: &BTreeMap<String, String>, rq: &Req, o: &Outcome, t0: std::time::Instant) {
    let id = id.to_string();
    let trace = headers.get("traceparent").and_then(|t| t.split('-').nth(1)).map(|s| s.to_lowercase());
    let billable = billable && p.service.billing.enabled && o.status > 0 && !o.answer.is_empty() && o.answer != "token";
    let req_json = json!({"method": method, "path": path, "query": query, "headers": headers, "body": String::from_utf8_lossy(&rq.raw_body).chars().take(64 * 1024).collect::<String>(), "size": rq.raw_body.len(), "files": rq.files.iter().map(|f| json!({"field": f["field"], "filename": f["filename"], "size": f["size"], "contentType": f["contentType"]})).collect::<Vec<_>>()});
    let resp_json = json!({"status": o.status, "headers": o.headers.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(), "body": String::from_utf8_lossy(&o.body).chars().take(64 * 1024).collect::<String>(), "size": o.body.len()});
    let _ = store::record(ctx, store::Call {
        id: id.clone(), service: p.service.name.clone(), route: route.to_string(), method: method.to_string(), path: inner.to_string(), status: o.status, answer: o.answer.clone(),
        sentence: o.sentence.clone(), billable, caller: rq.caller.clone(), trace_id: trace.clone(), ms: t0.elapsed().as_millis() as i64, provoked: o.provoked.clone(),
        sent: o.sent.clone(), answered: o.answered.clone(), request: req_json, response: resp_json, steps: json!(o.steps),
    }).await;
    let _ = stuntman_core::trips::record(ctx, stuntman_core::trips::Trip { id: &id, module: "apis", label: &o.sentence, status: &o.answer, trace_id: trace.as_deref(), link: &format!("/apis?call={id}"), host: &p.service.name,
        actor: &rq.caller, outcome: if o.provoked.is_some() { "provoked" } else if o.status >= 400 || o.status == 0 { "failed" } else { "ok" },
        detail: &format!("{}{}  ·  {}  ·  {} ms", o.provoked.as_ref().map(|x| format!("{x}  ·  ")).unwrap_or_default(), p.service.name, if o.status == 0 { "no answer".to_string() } else { o.status.to_string() }, t0.elapsed().as_millis()) }).await;
}

pub async fn respond(o: Outcome, kill: Option<Kill>) -> Response {
    match o.network.as_deref() {
        Some("timeout") => {
            tokio::time::sleep(Duration::from_secs(600)).await;
            StatusCode::GATEWAY_TIMEOUT.into_response()
        }
        Some(n) => {
            if let Some(k) = kill {
                k.fire(n == "reset");
                std::future::pending::<()>().await;
            }
            StatusCode::BAD_GATEWAY.into_response()
        }
        None if !o.parts.is_empty() => {
            let cut = o.cut;
            let st = futures::stream::unfold((o.parts.into_iter(), false), move |(mut it, done)| async move {
                if done {
                    return None;
                }
                match it.next() {
                    Some((wait, b)) => {
                        if wait > 0 {
                            tokio::time::sleep(Duration::from_millis(wait)).await;
                        }
                        Some((Ok::<_, std::io::Error>(bytes::Bytes::from(b)), (it, false)))
                    }
                    None if cut => {
                        // Let the parts sent so far reach the app before the connection breaks.
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        Some((Err(std::io::Error::other("stream cut on purpose")), (it, true)))
                    }
                    None => None,
                }
            });
            let mut resp = Response::new(Body::from_stream(st));
            *resp.status_mut() = StatusCode::from_u16(o.status).unwrap_or(StatusCode::OK);
            for (k, v) in o.headers {
                if let (Ok(k), Ok(v)) = (HeaderName::try_from(k), HeaderValue::from_str(&v)) {
                    resp.headers_mut().append(k, v);
                }
            }
            resp
        }
        None => {
            let mut resp = Response::new(Body::from(o.body));
            *resp.status_mut() = StatusCode::from_u16(o.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            for (k, v) in o.headers {
                if let (Ok(k), Ok(v)) = (HeaderName::try_from(k), HeaderValue::from_str(&v)) {
                    resp.headers_mut().append(k, v);
                }
            }
            resp
        }
    }
}

/// `full`: the path as received; `path`: relative to the service (the same when there is none).
async fn unmatched(ctx: &Ctx, plugins: &[Plugin], service: &str, method: &str, full: &str, path: &str, host: &str, rq: Req) -> Response {
    let c = closest(plugins, service, method, path);
    let req_json = json!({"method": method, "path": full, "servicePath": path, "query": rq.raw_query, "headers": rq.headers, "body": String::from_utf8_lossy(&rq.raw_body).chars().take(16 * 1024).collect::<String>()});
    let _ = store::unmatched_add(ctx, method, path, host, service, &rq.caller, req_json, c).await;
    let msg = if service.is_empty() { format!("Stuntman: no plugin answers {host}{path}") } else { format!("Stuntman: {service} has no route for {method} {path}") };
    let mut r = (StatusCode::NOT_FOUND, axum::Json(json!({"error": "no route", "stuntman": msg}))).into_response();
    r.headers_mut().insert("x-stuntman", HeaderValue::from_static("no route"));
    r
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn paths_and_closest() {
        assert_eq!(match_path("/orders/{id}/cancel", "/orders/58213/cancel").unwrap()["id"], "58213");
        assert!(match_path("/a/b", "/a/c").is_none());
        let p: Plugin = serde_yaml::from_str("service: {name: serpro}\nroutes:\n  - route: POST /datavalid/v5/pessoa-fisica/validacao\n").unwrap();
        let c = closest(&[p], "", "POST", "/datavalid/v4/pessoa-fisica/validacao");
        assert_eq!(c["text"], "Differs in one segment: v4 here, v5 in the plugin.");
    }

    #[test]
    fn handlebars_headers() {
        let a: crate::plugin::Answer = serde_yaml::from_str("name: go\nstatus: 302\nheaders: {Location: 'http://localhost:{{request.port}}/acme/x'}\nbody: '{{request.baseUrl}}'\ntemplate: handlebars\n").unwrap();
        let mut sc = Map::new();
        sc.insert("headers".into(), json!({"host": "localhost:18084"}));
        let (st, h, b, _) = render_answer(&a, &sc);
        assert_eq!((st, h.iter().find(|x| x.0 == "location").map(|x| x.1.as_str()), b.as_slice()), (302, Some("http://localhost:18084/acme/x"), b"http://localhost:18084".as_slice()));
    }

    #[test]
    fn generic_503_html() {
        let (st, h, b) = generic("503_html");
        assert_eq!((st, h[0].1.as_str()), (503, "text/html"));
        assert!(String::from_utf8_lossy(&b).contains("<html>"));
    }

    #[test]
    fn real_host_list() {
        let one: Plugin = serde_yaml::from_str("service: {name: acme, real_host: api.acme.example.com}\n").unwrap();
        let many: Plugin = serde_yaml::from_str("service: {name: globex, real_host: [a.globex.example.com, b.globex.example.com]}\n").unwrap();
        let ps = [one, many];
        let svc = |h: &str| find_service(&ps, h, "/v1/x").map(|(p, inner)| (p.service.name.clone(), inner));
        assert_eq!(svc("api.acme.example.com:443"), Some(("acme".into(), "/v1/x".into())));
        assert_eq!(svc("A.globex.example.com"), Some(("globex".into(), "/v1/x".into())));
        assert_eq!(svc("b.globex.example.com"), Some(("globex".into(), "/v1/x".into())));
        // One name is written back as a string, several as a list.
        assert!(serde_yaml::to_string(&ps[0].service).unwrap().contains("real_host: api.acme.example.com"));
        assert!(serde_yaml::to_string(&ps[1].service).unwrap().contains("real_host:\n- a.globex.example.com\n- b.globex.example.com"));
    }

    #[test]
    fn provider_signatures() {
        let one = |sign: &str, body: &str, provoke: &[String]| sign_headers(sign, "", "k", body, 1_700_000_000, 300, provoke, "", "", "evt_1", "POST", "https://app.example.com/hook?x=1");
        let get = |v: &[(String, String)], n: &str| v.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone()).unwrap_or_default();
        // Twilio's documented example: the auth token 12345 over its sample URL and parameters.
        let t = sign_headers("twilio", "", "12345", "CallSid=CA1234567890ABCDE&Caller=%2B12349013030&Digits=1234&From=%2B12349013030&To=%2B18005551212", 0, 300, &[], "", "", "", "POST", "https://mycompany.com/myapp.php?foo=1&bar=2");
        assert_eq!(get(&t, "X-Twilio-Signature"), "0/KCTR6DLpKmkAf8muzZqo1nDgQ=");
        let s = one("slack", "{}", &[]);
        assert_eq!(get(&s, "X-Slack-Request-Timestamp"), "1700000000");
        assert_eq!(get(&s, "X-Slack-Signature"), format!("v0={}", hex::encode(hmac256(b"k", b"v0:1700000000:{}"))));
        assert_eq!(get(&one("shopify", "{}", &[]), "X-Shopify-Hmac-Sha256"), B64.encode(hmac256(b"k", b"{}")));
        let hs = one("hubspot", "{}", &[]);
        assert_eq!(get(&hs, "X-HubSpot-Signature-v3"), B64.encode(hmac256(b"k", b"POSThttps://app.example.com/hook?x=1{}1700000000000")));
        assert!(one("hubspot", "{}", &["no_signature".into()]).iter().all(|(k, _)| k != "X-HubSpot-Signature-v3"));
    }

    #[test]
    fn multipart() {
        let body = b"--XX\r\nContent-Disposition: form-data; name=\"cpf\"\r\n\r\n12345678909\r\n--XX\r\nContent-Disposition: form-data; name=\"doc\"; filename=\"a.pdf\"\r\nContent-Type: application/pdf\r\n\r\n%PDF-1\r\n--XX--\r\n";
        let r = read_request("POST", "/x", "", BTreeMap::from([("content-type".to_string(), "multipart/form-data; boundary=XX".to_string())]), body.to_vec());
        assert_eq!(r.body["cpf"], "12345678909");
        assert_eq!(r.files[0]["filename"], "a.pdf");
        assert_eq!(r.files[0]["size"], 6);
        let rel = b"--YY\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{\"name\": \"Q4 plan.pdf\", \"parents\": [\"f1\"]}\r\n--YY\r\nContent-Type: application/pdf\r\n\r\n%PDF-1\r\n--YY--\r\n";
        let r = read_request("POST", "/x", "", BTreeMap::from([("content-type".to_string(), "multipart/related; boundary=YY".to_string())]), rel.to_vec());
        assert_eq!((r.body["name"].as_str(), r.files[0]["field"].as_str(), r.files[0]["size"].as_u64()), (Some("Q4 plan.pdf"), Some("part2"), Some(6)));
    }
}
