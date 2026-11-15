//! A9 · Recording the real provider: calls go to its sandbox and back, the app gets the real
//! answer, and each pair is kept with secrets out. The review groups the calls into routes
//! (/shipments/{id}) and answers, turns ids and dates into templates, echoes what the app sent and
//! suggests the rule that tells the answers apart; make_plugin writes it as a normal plugin.
//! ponytail: recordings are calls in the store (answer "recorded"), the settings live in the
//! plugin's `record:` block; no streaming bodies (64 MB cap, files over 5 MB keep only headers).

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
    time::Duration,
};

use axum::response::Response;
use serde_json::{json, Value};
use sha2::Digest;
use stuntman_core::{util, ApiError, Ctx, Tool};

use crate::{
    engine::{self, Outcome, Req},
    openapi::{status_name, template, uniq_answer},
    plugin::{self, Answer, Decide, Plugin, Record, Route},
    store,
};

pub fn folder(ctx: &Ctx, service: &str) -> std::path::PathBuf {
    plugin::dir(ctx).join(format!("{service}.record"))
}

pub fn files_dir(ctx: &Ctx, service: &str) -> std::path::PathBuf {
    plugin::dir(ctx).join(format!("{service}.files"))
}

/// Recording now: not stopped, not paused, not past its time.
pub fn live(rc: &Record) -> bool {
    rc.stopped == 0 && !rc.paused && (rc.until == 0 || util::now_ms() < rc.until)
}

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(30)).build().unwrap())
}

// ---------------------------------------------------------------- what is never kept

const SECRET_FIELDS: [&str; 15] = ["password", "passwd", "pass", "secret", "client_secret", "api_key", "apikey", "token", "access_token", "refresh_token", "id_token", "authorization", "pin", "cvv", "cvc"];

fn secret_header(k: &str) -> bool {
    matches!(k, "authorization" | "proxy-authorization" | "cookie" | "set-cookie") || ["token", "secret", "api-key", "apikey", "password"].iter().any(|x| k.contains(x)) || k.ends_with("-key")
}

fn re(i: usize) -> &'static regex::Regex {
    static R: OnceLock<[regex::Regex; 3]> = OnceLock::new();
    &R.get_or_init(|| {
        [
            regex::Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap(),
            regex::Regex::new(r"\b\d(?:[ -]?\d){12,18}\b").unwrap(),
            regex::Regex::new(r"\b\d{3}\.\d{3}\.\d{3}-\d{2}\b|\b\d{11}\b").unwrap(),
        ]
    })[i]
}

fn luhn(d: &str) -> bool {
    let ds: Vec<u32> = d.chars().filter_map(|c| c.to_digit(10)).collect();
    ds.len() >= 13 && ds.iter().rev().enumerate().map(|(i, x)| if i % 2 == 1 { let y = x * 2; if y > 9 { y - 9 } else { y } } else { *x }).sum::<u32>() % 10 == 0
}

fn h(s: &str) -> Vec<u8> {
    sha2::Sha256::digest(s.as_bytes()).to_vec()
}

/// A CPF with right check digits, the same for the same input (so echoes still match).
fn synthetic_cpf(seed: &str, formatted: bool) -> String {
    let b = h(seed);
    let mut d: Vec<u32> = (0..9).map(|i| b[i] as u32 % 10).collect();
    d[0] = 9;
    for n in [9usize, 10] {
        let s: u32 = (0..n).map(|i| d[i] * (n as u32 + 1 - i as u32)).sum();
        let r = (s * 10) % 11;
        d.push(if r == 10 { 0 } else { r });
    }
    let c: String = d.iter().map(|x| x.to_string()).collect();
    if formatted { format!("{}.{}.{}-{}", &c[..3], &c[3..6], &c[6..9], &c[9..]) } else { c }
}

/// Masks secrets (by field and header name), e-mails, card numbers and CPFs in what is kept.
pub struct Masker {
    service: String,
    extra: Vec<String>,
    pub count: usize,
}

impl Masker {
    pub fn new(service: &str, extra: &[String]) -> Self {
        Masker { service: service.into(), extra: extra.iter().map(|x| x.to_lowercase()).collect(), count: 0 }
    }

    fn secret(&mut self, name: &str) -> String {
        self.count += 1;
        format!("secret://{}-{}", self.service, name.to_lowercase().replace('_', "-"))
    }

    pub fn headers(&mut self, h: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        h.iter().map(|(k, v)| (k.clone(), if secret_header(k) { self.secret(k) } else { self.text(v) })).collect()
    }

    pub fn text(&mut self, s: &str) -> String {
        let mut n = 0;
        let a = re(0).replace_all(s, |c: &regex::Captures| {
            let e = &c[0];
            if e.ends_with("@example.com") || e.ends_with("@example.org") || e.ends_with("@ejemplo.com") {
                return e.to_string();
            }
            n += 1;
            format!("user-{}@example.com", hex::encode(&h(e)[..2]))
        });
        let b = re(1).replace_all(&a, |c: &regex::Captures| if luhn(&c[0]) && !c[0].starts_with("4242") { n += 1; "4242424242424242".to_string() } else { c[0].to_string() });
        let out = re(2).replace_all(&b, |c: &regex::Captures| {
            let x = &c[0];
            if crate::expr::cpf_ok(x) && !x.starts_with('9') { n += 1; synthetic_cpf(x, x.contains('.')) } else { x.to_string() }
        }).into_owned();
        self.count += n;
        out
    }

    fn value(&mut self, v: &mut Value) {
        match v {
            Value::Object(o) => {
                for (k, x) in o.iter_mut() {
                    let kl = k.to_lowercase();
                    if (SECRET_FIELDS.contains(&kl.as_str()) || self.extra.contains(&kl)) && !x.is_null() && !x.is_object() && !x.is_array() {
                        *x = json!(self.secret(k));
                    } else {
                        self.value(x);
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| self.value(x)),
            Value::String(s) => *s = self.text(s),
            _ => {}
        }
    }

    /// A body as kept: JSON with secret fields masked, form fields alike, text with its patterns.
    pub fn body(&mut self, raw: &[u8], ct: &str) -> Vec<u8> {
        let t = String::from_utf8_lossy(raw);
        if let Ok(mut v) = serde_json::from_str::<Value>(&t) {
            self.value(&mut v);
            return serde_json::to_vec(&v).unwrap_or_default();
        }
        if ct.contains("x-www-form-urlencoded") {
            let parts: Vec<String> = t.split('&').map(|kv| match kv.split_once('=') {
                Some((k, _)) if SECRET_FIELDS.contains(&k.to_lowercase().as_str()) || self.extra.contains(&k.to_lowercase()) => format!("{k}={}", self.secret(k)),
                _ => self.text(kv),
            }).collect();
            return parts.join("&").into_bytes();
        }
        self.text(&t).into_bytes()
    }
}

fn texty(ct: &str) -> bool {
    let c = ct.to_lowercase();
    c.is_empty() || c.starts_with("text/") || ["json", "xml", "javascript", "x-www-form-urlencoded", "yaml", "csv"].iter().any(|x| c.contains(x))
}

// ---------------------------------------------------------------- forwarding

/// Sends the call to the provider, keeps the pair (masked) and gives the app the real answer.
#[allow(clippy::too_many_arguments)]
pub async fn forward(ctx: &Ctx, p: &Plugin, rc: &Record, method: &str, path: &str, inner: &str, query: &str, headers: &BTreeMap<String, String>, rq: &Req, t0: std::time::Instant, kill: Option<stuntman_core::netserve::Kill>) -> Response {
    let url = format!("{}{inner}{}", rc.upstream.trim_end_matches('/'), if query.is_empty() { String::new() } else { format!("?{query}") });
    let mut r = client().request(method.parse().unwrap_or(reqwest::Method::GET), &url);
    for (k, v) in headers {
        if !matches!(k.as_str(), "host" | "content-length" | "connection" | "accept-encoding" | "transfer-encoding" | "keep-alive" | "upgrade") && !k.starts_with("x-stuntman") && !k.starts_with("x-duble") {
            r = r.header(k, v);
        }
    }
    if !rq.raw_body.is_empty() {
        r = r.body(rq.raw_body.clone());
    }
    let host = rc.upstream.split("://").nth(1).unwrap_or(&rc.upstream).split('/').next().unwrap_or("").to_string();
    let (status, rh, body, err) = match r.send().await {
        Ok(res) => {
            let st = res.status().as_u16();
            let hh: Vec<(String, String)> = res.headers().iter().filter(|(k, _)| !matches!(k.as_str(), "transfer-encoding" | "connection" | "content-length" | "keep-alive")).map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())).collect();
            match res.bytes().await {
                Ok(b) => (st, hh, b.to_vec(), None),
                Err(e) => (502, vec![], vec![], Some(format!("the answer broke off ({e})"))),
            }
        }
        Err(e) => (502, vec![("content-type".into(), "application/json".into())], vec![], Some(if e.is_timeout() { "no answer in 30 s".to_string() } else if e.is_connect() { format!("{host} refused the connection") } else { e.to_string() })),
    };
    let body = if let Some(e) = &err { serde_json::to_vec(&json!({"error": "bad_gateway", "stuntman": format!("Recording: {e}")})).unwrap() } else { body };
    let ms = t0.elapsed().as_millis() as u64;
    // What is kept.
    let id = util::id("call");
    let mut m = Masker::new(&p.service.name, &rc.mask);
    let kept_headers = m.headers(headers);
    let ct_in = headers.get("content-type").cloned().unwrap_or_default();
    let mut kept = rq.clone();
    kept.raw_body = m.body(&rq.raw_body, &ct_in);
    let ct_out = rh.iter().find(|(k, _)| k == "content-type").map(|x| x.1.clone()).unwrap_or_default();
    let mut file = String::new();
    let kept_body = if texty(&ct_out) {
        m.body(&body, &ct_out)
    } else {
        if body.len() <= 5 << 20 {
            let dir = folder(ctx, &p.service.name);
            if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(dir.join(format!("{id}.bin")), &body).is_ok() {
                file = format!("{id}.bin");
            }
        }
        vec![]
    };
    let kept_rh: Vec<(String, String)> = rh.iter().map(|(k, v)| (k.clone(), if secret_header(k) { m.secret(k) } else { v.clone() })).collect();
    let masked = m.count;
    let text = match &err {
        Some(e) => format!("Sent to {host}: {e}; the app got 502"),
        None => format!("Sent to {host} · {status} · {ms} ms{}{}", if masked > 0 { format!(" · {masked} masked") } else { String::new() }, if file.is_empty() && !texty(&ct_out) && !body.is_empty() { format!(" · {} KB, too big to keep", body.len() / 1024) } else if !file.is_empty() { format!(" · {} KB kept as a file", body.len() / 1024) } else { String::new() }),
    };
    let o = Outcome { status, headers: kept_rh, body: kept_body, answer: "recorded".into(), sentence: format!("{} {method} {inner} → {status} (recorded)", rq.caller), latency_ms: ms,
        steps: vec![json!({"step": "record", "text": text, "ok": err.is_none(), "masked": masked, "file": file, "failed": err.is_some(), "size": body.len()})], ..Default::default() };
    engine::log_call(ctx, p, &id, false, &format!("{method} {inner}"), method, path, inner, query, &kept_headers, &kept, &o, t0).await;
    engine::respond(Outcome { status, headers: rh, body, ..Default::default() }, kill).await
}

// ---------------------------------------------------------------- the review

fn idlike(seg: &str) -> bool {
    let version = seg.len() <= 4 && seg.starts_with('v') && seg[1..].chars().all(|c| c.is_ascii_digit() || c == '.');
    !version && (seg.chars().any(|c| c.is_ascii_digit()) || seg.len() >= 24)
}

fn template_path(path: &str) -> (String, Vec<(String, String)>) {
    let mut n = 0;
    let mut params = vec![];
    let segs: Vec<String> = path.split('/').map(|s| {
        if !s.is_empty() && idlike(s) {
            n += 1;
            let name = if n == 1 { "id".to_string() } else { format!("id{n}") };
            params.push((name.clone(), s.to_string()));
            format!("{{{name}}}")
        } else {
            s.to_string()
        }
    }).collect();
    (segs.join("/"), params)
}

fn leaves(v: &Value, prefix: &str, out: &mut Vec<(String, Value)>) {
    match v {
        Value::Object(o) => o.iter().for_each(|(k, x)| leaves(x, &if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") }, out)),
        Value::Array(_) => {}
        x => out.push((prefix.to_string(), x.clone())),
    }
}

/// What the app sent, as fields: body.a.b and query.x.
fn request_fields(c: &Value) -> Vec<(String, Value)> {
    let mut out = vec![];
    let b: Value = serde_json::from_str(c["request"]["body"].as_str().unwrap_or("")).unwrap_or(Value::Null);
    leaves(&b, "body", &mut out);
    for kv in c["request"]["query"].as_str().unwrap_or("").split('&').filter(|x| !x.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        out.push((format!("query.{k}"), json!(v)));
    }
    out
}

fn iso_time(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() >= 19 && b[4] == b'-' && b[7] == b'-' && (b[10] == b'T' || b[10] == b' ') && b[13] == b':'
}

fn id_key(k: &str) -> bool {
    let l = k.to_lowercase();
    l == "id" || l.ends_with("_id") || k.ends_with("Id") || l == "uuid" || l == "token" || l == "protocol" || l == "number"
}

/// One answer body: ids that change become random, dates become now(), values the app sent are
/// echoed, path parameters come from the path.
fn templ(v: &Value, key: &str, ptr: &str, req: &[(String, Value)], params: &[(String, String)], others: &[Value]) -> Value {
    match v {
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), templ(x, k, &format!("{ptr}/{k}"), req, params, others))).collect()),
        Value::Array(a) => Value::Array(a.iter().enumerate().map(|(i, x)| templ(x, key, &format!("{ptr}/{i}"), req, params, others)).collect()),
        Value::String(s) if s.len() >= 2 => {
            if let Some((f, _)) = req.iter().find(|(_, rv)| rv == v) {
                return crate::openapi::echo(f);
            }
            if let Some((n, _)) = params.iter().find(|(_, pv)| pv == s) {
                return json!(format!("{{{{ path.{n} }}}}"));
            }
            if iso_time(s) {
                return json!("{{ now() }}");
            }
            let varies = others.iter().any(|o| o.pointer(ptr).map(|x| x != v).unwrap_or(false));
            if id_key(key) && varies {
                let prefix: String = s.chars().take_while(|c| !c.is_ascii_digit()).collect();
                let prefix = if prefix.len() > 6 || prefix.len() == s.len() { String::new() } else { prefix };
                return json!(format!("{prefix}{{{{ random_id() }}}}"));
            }
            v.clone()
        }
        Value::Number(n) if n.as_f64().map(|x| x.abs() > 1.0).unwrap_or(false) => {
            if let Some((f, _)) = req.iter().find(|(_, rv)| rv == v) {
                return crate::openapi::echo(f);
            }
            v.clone()
        }
        x => x.clone(),
    }
}

fn lit(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{}'", s.replace('\'', "\\'")),
        x => x.to_string(),
    }
}

/// The rule that tells an error answer from the others: a field the error calls lack, or a value
/// only they have.
fn rule_for(err: &[&Value], rest: &[&Value]) -> Option<(String, String)> {
    if err.is_empty() || rest.is_empty() {
        return None;
    }
    let ef: Vec<BTreeMap<String, Value>> = err.iter().map(|c| request_fields(c).into_iter().collect()).collect();
    let rf: Vec<BTreeMap<String, Value>> = rest.iter().map(|c| request_fields(c).into_iter().collect()).collect();
    let empty = |m: &BTreeMap<String, Value>, f: &str| m.get(f).map(|v| v.is_null() || v == &json!("")).unwrap_or(true);
    let all: BTreeSet<String> = rf.iter().chain(ef.iter()).flat_map(|m| m.keys().cloned()).collect();
    let n = err.len();
    // The field the error answer names ("postal_code invalid") wins; then fields whose parent the
    // error calls still sent.
    let said: String = err.iter().map(|c| c["response"]["body"].as_str().unwrap_or("").to_lowercase()).collect();
    let rank = |f: &str| {
        let leaf = f.rsplit('.').next().unwrap_or(f).to_lowercase();
        let parent = f.rsplit_once('.').map(|x| x.0).filter(|p| p.contains('.'));
        (!said.contains(&leaf), !parent.map(|p| ef.iter().all(|m| m.keys().any(|k| k.starts_with(&format!("{p}."))))).unwrap_or(false))
    };
    let mut cands: Vec<(String, String, String)> = vec![];
    for f in &all {
        if ef.iter().all(|m| empty(m, f)) && rf.iter().all(|m| !empty(m, f)) {
            cands.push((f.clone(), format!("!{f}"), format!("{} {} had no {}.", if n == 1 { "The".to_string() } else { format!("All {n}") }, if n == 1 { "call" } else { "calls" }, f.rsplit('.').next().unwrap_or(f))));
        }
    }
    for f in &all {
        let v = ef[0].get(f).cloned();
        if let Some(v) = v.filter(|v| !v.is_null()) {
            if ef.iter().all(|m| m.get(f) == Some(&v)) && rf.iter().all(|m| m.get(f) != Some(&v)) {
                cands.push((f.clone(), format!("{f} == {}", lit(&v)), format!("{} had {} {}; the others did not.", if n == 1 { "The call".to_string() } else { format!("All {n} calls") }, f.rsplit('.').next().unwrap_or(f), lit(&v))));
            }
        }
    }
    cands.sort_by_key(|c| rank(&c.0));
    if let Some((_, w, why)) = cands.into_iter().next() {
        return Some((w, why));
    }
    None
}

/// The routes, answers and rules a set of recorded calls makes, named against `existing`.
pub struct Analysis {
    pub routes: Vec<Value>,
    pub plugin_routes: Vec<Route>,
    pub answers: Vec<Answer>,
    /// (answer, kept file, content type)
    pub files: Vec<(String, String, String)>,
    pub grouped: Vec<Value>,
}

pub fn analyse(calls: &[Value], existing: &Plugin) -> Analysis {
    // Group by method and path template.
    let mut groups: Vec<(String, String, Vec<&Value>)> = vec![];
    for c in calls {
        let (t, _) = template_path(c["path"].as_str().unwrap_or("/"));
        let m = c["method"].as_str().unwrap_or("GET").to_string();
        match groups.iter_mut().find(|g| g.0 == m && g.1 == t) {
            Some(g) => g.2.push(c),
            None => groups.push((m, t, vec![c])),
        }
    }
    let mut answers: Vec<Answer> = existing.answers.clone();
    let mut new_answers = vec![];
    let mut files = vec![];
    let mut out = vec![];
    let mut routes = vec![];
    let mut grouped = vec![];
    for (m, t, cs) in &groups {
        let paths: BTreeSet<&str> = cs.iter().map(|c| c["path"].as_str().unwrap_or("")).collect();
        if paths.len() > 1 || paths.iter().any(|p| *p != t) {
            grouped.push(json!({"route": format!("{m} {t}"), "paths": paths.iter().take(3).collect::<Vec<_>>(), "more": paths.len().saturating_sub(3)}));
        }
        let failed: Vec<&&Value> = cs.iter().filter(|c| c["steps"][0]["failed"] == true).collect();
        let ok_calls: Vec<&Value> = cs.iter().copied().filter(|c| c["steps"][0]["failed"] != true).collect();
        let mut by_status: BTreeMap<u16, Vec<&Value>> = BTreeMap::new();
        for c in &ok_calls {
            by_status.entry(c["status"].as_u64().unwrap_or(0) as u16).or_default().push(c);
        }
        let res = t.split('/').filter(|s| !s.is_empty() && !s.starts_with('{') && !(s.starts_with('v') && s[1..].chars().all(|c| c.is_ascii_digit()))).last().unwrap_or("call").to_string();
        let mut named: Vec<(String, u16, Vec<&Value>)> = vec![];
        for (st, list) in &by_status {
            let last = list.last().unwrap();
            let (_, params) = template_path(last["path"].as_str().unwrap_or(""));
            let req = request_fields(last);
            let rh: Vec<(String, String)> = last["response"]["headers"].as_array().cloned().unwrap_or_default().iter().map(|x| (x[0].as_str().unwrap_or("").to_lowercase(), x[1].as_str().unwrap_or("").to_string())).collect();
            let ct = rh.iter().find(|x| x.0 == "content-type").map(|x| x.1.clone()).unwrap_or_default();
            let mut headers = BTreeMap::new();
            for (k, v) in &rh {
                if matches!(k.as_str(), "content-type" | "location" | "retry-after") {
                    headers.insert(k.clone(), v.clone());
                }
            }
            let raw = last["response"]["body"].as_str().unwrap_or("");
            let others: Vec<Value> = list.iter().map(|c| serde_json::from_str(c["response"]["body"].as_str().unwrap_or("")).unwrap_or(Value::Null)).collect();
            let file = last["steps"][0]["file"].as_str().unwrap_or("").to_string();
            let (body, rawflag) = match serde_json::from_str::<Value>(raw) {
                Ok(v) if !raw.trim().is_empty() => (template(&templ(&v, "", "", &req, &params, &others)), false),
                _ => (raw.to_string(), !raw.is_empty()),
            };
            let a = Answer { name: String::new(), status: *st, headers, body: if file.is_empty() { body } else { String::new() }, raw: rawflag && file.is_empty(),
                note: format!("recorded: {} call{} to {m} {t}", list.len(), if list.len() == 1 { "" } else { "s" }), ..Default::default() };
            let sn = status_name(*st);
            let tries = vec![sn.clone(), format!("{}_{sn}", crate::soap::snake(&res)), format!("{}_{}_{sn}", m.to_lowercase(), crate::soap::snake(&res))];
            let (name, shared) = uniq_answer(&answers, &tries, &a);
            if !shared {
                answers.push(Answer { name: name.clone(), ..a.clone() });
                new_answers.push(Answer { name: name.clone(), ..a });
                if !file.is_empty() {
                    files.push((name.clone(), file, ct));
                }
            }
            named.push((name, *st, list.clone()));
        }
        // The default: the most frequent success (or the most frequent answer).
        let def = named.iter().filter(|n| n.1 < 400).max_by_key(|n| n.2.len()).or(named.iter().max_by_key(|n| n.2.len())).map(|n| n.0.clone());
        let mut decide = vec![];
        let mut rules = vec![];
        for (name, st, list) in named.iter().filter(|n| Some(&n.0) != def.as_ref()) {
            let rest: Vec<&Value> = named.iter().filter(|o| &o.0 != name).flat_map(|o| o.2.iter().copied()).collect();
            if let Some((when, why)) = rule_for(list, &rest) {
                rules.push(json!({"when": when, "answer": name, "why": why, "status": st}));
                decide.push(Decide { when: Some(when), answer: name.clone() });
            }
        }
        if let Some(d) = &def {
            decide.push(Decide { when: None, answer: d.clone() });
        }
        let route = format!("{m} {t}");
        let exists = existing.routes.iter().any(|r| r.route.eq_ignore_ascii_case(&route));
        out.push(json!({"route": route, "calls": cs.len(), "exists": exists, "default": def,
            "answers": named.iter().map(|(n, st, l)| json!({"name": n, "status": st, "calls": l.len(), "file": files.iter().any(|f| &f.0 == n)})).collect::<Vec<_>>(),
            "failed": failed.iter().map(|c| c["steps"][0]["text"].clone()).collect::<Vec<_>>(), "rules": rules,
            "example": new_answers.iter().find(|a| Some(&a.name) == def.as_ref()).map(|a| a.body.clone()).or_else(|| answers.iter().find(|a| Some(&a.name) == def.as_ref()).map(|a| a.body.clone()))}));
        routes.push(Route { route, decide, ..Default::default() });
    }
    Analysis { routes: out, plugin_routes: routes, answers: new_answers, files, grouped }
}

// ---------------------------------------------------------------- tools (A9 "API & MCP")

async fn status(ctx: &Ctx, p: &Plugin, with_review: bool) -> Result<Value, ApiError> {
    let Some(rc) = &p.record else { return Ok(json!({"service": p.service.name, "recording": false})) };
    let calls = store::count_recorded(ctx, &p.service.name, rc.started).await?;
    let now = util::now_ms();
    let state = if rc.stopped > 0 { "stopped" } else if rc.paused { "paused" } else if rc.until > 0 && now >= rc.until { "time is up" } else if rc.max > 0 && calls >= rc.max as i64 { "limit reached" } else { "recording" };
    let mut v = json!({"service": p.service.name, "recording": state == "recording", "state": state, "upstream": rc.upstream, "mode": rc.mode, "started": rc.started,
        "until": rc.until, "leftMs": if rc.until > 0 { (rc.until - now).max(0) } else { 0 }, "max": rc.max, "calls": calls, "mask": rc.mask});
    if with_review || state != "recording" {
        let list = store::recorded(ctx, &p.service.name, rc.started, if rc.stopped > 0 { rc.stopped } else { now }).await?;
        let a = analyse(&list, p);
        v["masked"] = json!(list.iter().map(|c| c["steps"][0]["masked"].as_u64().unwrap_or(0)).sum::<u64>());
        v["review"] = json!({"routes": a.routes, "grouped": a.grouped, "answers": a.answers.len()});
    }
    Ok(v)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("start_recording", "Records the real provider: each call to the service goes to upstream (its sandbox) and back; the app gets the real answer and Stuntman keeps the pair with secrets, card numbers, CPFs and e-mails masked. mode=unknown on an existing service: its routes answer and only unknown calls go. Creates the service if it does not exist. Use a sandbox key, never a production one.", |ctx, a| async move {
            let s = a.need("service")?;
            let up = a.need("upstream")?;
            let mut p = match plugin::get(&ctx, &s) {
                Ok(p) => p,
                Err(_) if !plugin::file(&ctx, &s).exists() => {
                    plugin::valid_name(&s)?;
                    Plugin { service: plugin::Service { name: s.clone(), title: a.str("title").unwrap_or_else(|| s.clone()), origin: "yours".into(), base_url: up.trim_end_matches('/').into(), ..Default::default() }, ..Default::default() }
                }
                Err(e) => return Err(e),
            };
            let now = util::now_ms();
            let keep = p.record.as_ref().filter(|r| r.upstream == up).map(|r| r.started).unwrap_or(now);
            let mode = a.str("mode").unwrap_or(if p.routes.is_empty() { "every".into() } else { "unknown".into() });
            p.record = Some(Record { upstream: up, mode, started: keep, until: a.ms("stop_after")?.map(|m| now + m as i64).unwrap_or(now + 30 * 60_000), max: a.u64("max").unwrap_or(500) as u32,
                mask: a.get("mask").and_then(|m| m.as_array()).map(|x| x.iter().filter_map(|y| y.as_str().map(String::from)).collect()).unwrap_or_default(), ..Default::default() });
            plugin::save(&ctx, &p)?;
            let mut v = status(&ctx, &p, false).await?;
            v["urls"] = json!({"local": format!("http://127.0.0.1:{}{}/{}", crate::port(), ctx.prefix(), s)});
            Ok(v)
        })
        .req("service", "the service (new or existing)").req("upstream", "the provider's sandbox base URL, https://…").param("mode", "every (default for a new service) | unknown (routes answer; the rest go)")
        .param("stop_after", "default 30m").param("max", "stop after this many calls (default 500)").param("mask", "more body fields to mask, e.g. [\"document\"]").param("title", "shown name of a new service")
        .route("POST", "/plugins/{service}/recording"),
        Tool::new("recording_status", "A recording: recording | paused | time is up | limit reached | stopped, calls kept, time left; once it stops (or review=true), the routes and answers it found and the rules it suggests.", |ctx, a| async move {
            let p = plugin::get(&ctx, &a.need("service")?)?;
            status(&ctx, &p, a.bool("review").unwrap_or(false)).await
        })
        .req("service", "the service").param("review", "true: the review now, while recording")
        .route("GET", "/plugins/{service}/recording"),
        Tool::new("pause_recording", "Pauses a recording (calls get 404 “no route”, or the routes' answers) or, with resume=true, goes on.", |ctx, a| async move {
            let mut p = plugin::get(&ctx, &a.need("service")?)?;
            let rc = p.record.as_mut().ok_or_else(|| ApiError::bad("Not recording"))?;
            rc.paused = !a.bool("resume").unwrap_or(false);
            plugin::save(&ctx, &p)?;
            status(&ctx, &p, false).await
        })
        .req("service", "the service").param("resume", "true: record again")
        .route("POST", "/plugins/{service}/recording/pause"),
        Tool::new("stop_recording", "Stops a recording and returns the review: calls grouped into routes, the answers each gave, ids and dates made templates, and the rules that tell the answers apart. make_plugin writes it.", |ctx, a| async move {
            let mut p = plugin::get(&ctx, &a.need("service")?)?;
            let rc = p.record.as_mut().ok_or_else(|| ApiError::bad("Not recording"))?;
            if rc.stopped == 0 {
                rc.stopped = util::now_ms();
                plugin::save(&ctx, &p)?;
            }
            status(&ctx, &p, true).await
        })
        .req("service", "the service")
        .route("DELETE", "/plugins/{service}/recording"),
        Tool::new("make_plugin", "Writes the plugin from a recording: the routes it found (routes= only these, “METHOD /path”), their answers (binary ones as files) and the suggested rules (rules=false: each route gives its usual answer). Routes the service has stay as they are. The recording ends; the provider is no longer needed.", |ctx, a| async move {
            let s = a.need("service")?;
            let mut p = plugin::get(&ctx, &s)?;
            let rc = p.record.clone().ok_or_else(|| ApiError::bad("Nothing recorded: start_recording first"))?;
            let list = store::recorded(&ctx, &s, rc.started, if rc.stopped > 0 { rc.stopped } else { util::now_ms() }).await?;
            let an = analyse(&list, &p);
            let only: Option<Vec<String>> = a.get("routes").and_then(|r| r.as_array()).map(|x| x.iter().filter_map(|y| y.as_str().map(|s| s.to_uppercase())).collect());
            let rules = a.bool("rules").unwrap_or(true);
            let mut added = vec![];
            let mut used = BTreeSet::new();
            for mut r in an.plugin_routes {
                if only.as_ref().map(|o| !o.contains(&r.route.to_uppercase())).unwrap_or(false) || p.routes.iter().any(|x| x.route.eq_ignore_ascii_case(&r.route)) {
                    continue;
                }
                if !rules {
                    r.decide.retain(|d| d.when.is_none());
                }
                let mine = an.routes.iter().find(|x| x["route"] == json!(r.route)).cloned().unwrap_or_default();
                used.extend(mine["answers"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x["name"].as_str().map(String::from)));
                added.push(r.route.clone());
                p.routes.push(r);
            }
            let mut answers_added = 0;
            for mut ans in an.answers.into_iter().filter(|x| used.contains(&x.name)) {
                if let Some((_, f, ct)) = an.files.iter().find(|f| f.0 == ans.name) {
                    let ext = ct.split('/').nth(1).unwrap_or("bin").split([';', '+']).next().unwrap_or("bin").trim().to_string();
                    let name = format!("{}.{ext}", ans.name);
                    let dir = files_dir(&ctx, &s);
                    std::fs::create_dir_all(&dir).map_err(|e| ApiError::internal(e.to_string()))?;
                    if std::fs::copy(folder(&ctx, &s).join(f), dir.join(&name)).is_ok() {
                        ans.file = name;
                    }
                }
                p.answers.push(ans);
                answers_added += 1;
            }
            if p.service.base_url.is_empty() {
                p.service.base_url = rc.upstream.trim_end_matches('/').to_string();
            }
            p.record = None;
            let v = plugin::save(&ctx, &p)?;
            let _ = std::fs::remove_dir_all(folder(&ctx, &s));
            Ok(json!({"service": s, "version": v, "routes": added, "answers": answers_added, "rules": rules, "file": plugin::rel(&s),
                "note": "A normal plugin now: the provider is no longer called. Edit it like any other."}))
        })
        .req("service", "the recorded service").param("routes", "[\"POST /v2/shipments\", …] (default: all)").param("rules", "false: no suggested rules")
        .route("POST", "/plugins/{service}/recording/plugin"),
    ]
}

#[cfg(test)]
mod t {
    use super::*;
    use serde_json::Map;

    fn call(method: &str, path: &str, req: Value, status: u16, resp: Value) -> Value {
        json!({"method": method, "path": path, "status": status, "steps": [{"step": "record", "ok": true}],
            "request": {"body": req.to_string(), "query": ""}, "response": {"status": status, "headers": [["content-type", "application/json"]], "body": resp.to_string()}})
    }

    #[test]
    fn masks_and_reviews() {
        let mut m = Masker::new("globex", &[]);
        let h = m.headers(&BTreeMap::from([("authorization".to_string(), "Bearer sk_live".to_string()), ("accept".into(), "application/json".into())]));
        assert_eq!(h["authorization"], "secret://globex-authorization");
        let b = String::from_utf8(m.body(br#"{"email": "ana.souza@mail.globex.test", "card": "4111 1111 1111 1111", "cpf": "111.444.777-35", "client_secret": "abc", "n": 3}"#, "application/json")).unwrap();
        assert!(!b.contains("globex.test") && !b.contains("4111") && !b.contains("111.444") && b.contains("secret://globex-client-secret"), "{b}");
        let v: Value = serde_json::from_str(&b).unwrap();
        assert!(crate::expr::cpf_ok(v["cpf"].as_str().unwrap()));
        assert_eq!(m.count, 5);

        let calls = vec![
            call("POST", "/v2/shipments", json!({"destination": {"city": "Porto", "postal_code": "4000"}, "weight": 2.4}), 201, json!({"id": "SHP-88229", "destination": {"city": "Porto", "postal_code": "4000"}, "created_at": "2026-11-14T14:50:12Z"})),
            call("GET", "/v2/shipments/SHP-88229", json!(null), 200, json!({"id": "SHP-88229", "status": "in_transit"})),
            call("POST", "/v2/shipments", json!({"destination": {"city": "Lisbon", "postal_code": "1000"}, "weight": 1.0}), 201, json!({"id": "SHP-88231", "destination": {"city": "Lisbon", "postal_code": "1000"}, "created_at": "2026-11-14T14:52:10Z"})),
            call("POST", "/v2/shipments", json!({"destination": {"city": "Faro"}, "weight": 1.0}), 422, json!({"error": "postal_code invalid"})),
            call("GET", "/v2/shipments/SHP-00000", json!(null), 404, json!({"error": "not found"})),
            call("GET", "/v2/shipments/SHP-88231", json!(null), 200, json!({"id": "SHP-88231", "status": "created"})),
        ];
        let a = analyse(&calls, &Plugin::default());
        let routes: Vec<&str> = a.plugin_routes.iter().map(|r| r.route.as_str()).collect();
        assert_eq!(routes, vec!["POST /v2/shipments", "GET /v2/shipments/{id}"]);
        let post = &a.plugin_routes[0];
        assert_eq!(post.decide[0].when.as_deref(), Some("!body.destination.postal_code"));
        assert_eq!(post.decide[0].answer, "invalid");
        assert_eq!(post.decide.last().unwrap().answer, "created");
        let created = a.answers.iter().find(|x| x.name == "created").unwrap();
        assert!(created.body.contains("SHP-{{ random_id() }}") && created.body.contains("{{ now() }}") && created.body.contains("{{ body.destination.city | json }}"), "{}", created.body);
        let ok = a.answers.iter().find(|x| x.status == 200).unwrap();
        assert!(ok.body.contains("{{ path.id }}"), "{}", ok.body);
        // the plugin it makes is valid and renders
        let p = Plugin { service: plugin::Service { name: "globex".into(), ..Default::default() }, routes: a.plugin_routes.clone(), answers: a.answers.clone(), ..Default::default() };
        let t = serde_yaml::to_string(&p).unwrap();
        assert!(plugin::validate(&plugin::parse(&t).unwrap(), &t).is_empty(), "{t}");
        let mut sc = Map::new();
        sc.insert("body".into(), json!({"destination": {"city": "Braga", "postal_code": "4700"}}));
        let out: Value = serde_json::from_str(&crate::expr::render(&created.body, &sc).unwrap()).unwrap();
        assert_eq!(out["destination"]["city"], "Braga");
        assert!(crate::expr::test("!body.destination.postal_code", &{ let mut s = Map::new(); s.insert("body".into(), json!({"destination": {"city": "x"}})); s }).unwrap());
    }
}
