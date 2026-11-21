//! A17: the RabbitMQ management HTTP API (its own port, 15672) for apps and tools that create
//! vhosts, queues, exchanges, bindings, policies, users and permissions over HTTP before they
//! connect (MassTransit setups, provisioning scripts, rabbitmqadmin): overview, whoami,
//! vhosts, users, permissions, policies, queues, exchanges, bindings, health checks, aliveness.
//! Vhosts are the RabbitMQ namespaces of the default environment ("/" is the one named default, else the first).
//!
//! Users declared here (rabbitmqadmin `users declare`) are real: a wrong password gets 401 here
//! and ACCESS_REFUSED on AMQP, and a vhost without a permission for them refuses them. Anyone
//! else gets in as before (the namespace's “Accept any key”).

use std::collections::BTreeMap;

use axum::{
    extract::{Path, Query, Request, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use stuntman_core::{env::Envs, util, Ctx};

use crate::{
    config::{self, Permission, RabbitPolicy, RABBIT},
    rabbit,
};

const NODE: &str = "stuntman@localhost";

fn vh(v: &str) -> String {
    let d = v.replace("%2F", "/").replace("%2f", "/");
    if d == "/" || d.is_empty() { "/".into() } else { d.trim_start_matches('/').to_string() }
}

fn ns_of(envs: &Envs, vhost: &str) -> Option<(Ctx, config::Namespace)> {
    rabbit::find_existing(envs, if vhost == "/" { "" } else { vhost })
}

fn err(code: StatusCode, error: &str, reason: &str) -> Response {
    (code, Json(json!({"error": error, "reason": reason}))).into_response()
}

fn not_found() -> Response {
    err(StatusCode::NOT_FOUND, "not_found", "Not Found")
}

fn bad(reason: &str) -> Response {
    err(StatusCode::BAD_REQUEST, "bad_request", reason)
}

fn created(was_new: bool) -> Response {
    if was_new { StatusCode::CREATED } else { StatusCode::NO_CONTENT }.into_response()
}

fn saved(r: Result<config::Namespace, stuntman_core::ApiError>, was_new: bool) -> Response {
    match r {
        Ok(_) => created(was_new),
        Err(e) if e.status == 404 => not_found(),
        Err(e) => bad(&e.message),
    }
}

// ---------------------------------------------------------------- users

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct User {
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Users live in messaging/_rabbitmq-users.yaml (no namespace name starts with "_"); their
/// password hashes (rabbit_password_hashing_sha256) in secrets.yaml.
fn users_file(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("messaging").join("_rabbitmq-users.yaml")
}

pub fn users(ctx: &Ctx) -> BTreeMap<String, User> {
    std::fs::read_to_string(users_file(ctx)).ok().and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default()
}

fn save_users(ctx: &Ctx, u: &BTreeMap<String, User>) -> std::io::Result<()> {
    std::fs::create_dir_all(ctx.path("messaging"))?;
    std::fs::write(users_file(ctx), serde_yaml::to_string(u).unwrap_or_default())
}

fn secret(user: &str) -> String {
    format!("messaging.rabbitmq-users.{user}")
}

/// RabbitMQ's password hash: base64(salt ‖ sha256(salt ‖ password)), a 4-byte salt.
pub fn hash(password: &str) -> String {
    let salt: [u8; 4] = rand::random();
    let mut v = salt.to_vec();
    v.extend(Sha256::digest([&salt[..], password.as_bytes()].concat()));
    B64.encode(v)
}

fn verify(hash: &str, password: &str) -> bool {
    match B64.decode(hash) {
        Ok(b) if b.len() == 36 => Sha256::digest([&b[..4], password.as_bytes()].concat())[..] == b[4..],
        _ => false,
    }
}

pub enum Login {
    /// Not a declared user: the namespace's own rules apply.
    Unknown,
    Refused,
    Ok(User),
}

pub fn login(ctx: &Ctx, user: &str, password: &str) -> Login {
    match users(ctx).remove(user) {
        None => Login::Unknown,
        Some(u) if verify(&util::resolve_secret(&format!("secret://{}", secret(user)), &ctx.data_dir), password) => Login::Ok(u),
        Some(_) => Login::Refused,
    }
}

fn tags_of(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => s.split(',').map(str::trim).filter(|t| !t.is_empty()).map(String::from).collect(),
        Value::Array(a) => a.iter().filter_map(|t| t.as_str().map(String::from)).collect(),
        _ => vec![],
    }
}

fn user_json(name: &str, u: &User) -> Value {
    json!({"name": name, "tags": u.tags, "hashing_algorithm": "rabbit_password_hashing_sha256", "has_password": true, "limits": {}})
}

fn basic(h: &HeaderMap) -> (String, String) {
    let raw = h.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Basic ")).and_then(|b| B64.decode(b.trim()).ok()).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default();
    let (u, p) = raw.split_once(':').unwrap_or((&raw, ""));
    (if u.is_empty() { "guest".into() } else { u.to_string() }, p.to_string())
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())) {
            (b'%', Some(x)) => {
                out.push(x);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into()
}

/// A declared user: right password, and outside the administrator tag only its own vhosts
/// (404 for the others) and no vhost, user, permission or policy changes (401), as RabbitMQ.
async fn auth(State(e): State<Envs>, req: Request, next: Next) -> Response {
    // A namespace set to down (or close) is a stopped broker: its vhost's endpoints answer 503
    // (the port is shared with the other vhosts, so it cannot refuse the connection).
    let segs: Vec<String> = req.uri().path().trim_start_matches("/api/").split('/').map(decode).collect();
    let scoped = matches!(segs[0].as_str(), "queues" | "exchanges" | "bindings" | "policies" | "aliveness-test" | "vhosts" | "permissions");
    if scoped && segs.len() > 1 && !segs[1].is_empty() && ns_of(&e, &vh(&segs[1])).map(|(_, n)| config::is_down(&n.behaviour.reachability)).unwrap_or(false) {
        return err(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", "the broker is down (Stuntman: the namespace is set to down)");
    }
    let Some(ctx) = e.default_env() else { return next.run(req).await };
    let (user, pass) = basic(req.headers());
    let u = match login(&ctx, &user, &pass) {
        Login::Unknown => return next.run(req).await,
        Login::Refused => return err(StatusCode::UNAUTHORIZED, "not_authorized", "Login failed"),
        Login::Ok(u) => u,
    };
    if u.tags.iter().any(|t| t == "administrator") {
        return next.run(req).await;
    }
    let write = !matches!(*req.method(), Method::GET | Method::HEAD);
    let coll = segs[0].as_str();
    let policymaker = u.tags.iter().any(|t| t == "policymaker");
    if coll == "users" || (write && matches!(coll, "vhosts" | "permissions")) || (write && coll == "policies" && !policymaker) {
        return err(StatusCode::UNAUTHORIZED, "not_authorised", "Not administrator user");
    }
    if scoped && segs.len() > 1 && !segs[1].is_empty() {
        let allowed = ns_of(&e, &vh(&segs[1])).map(|(_, n)| n.access.permissions.contains_key(&user)).unwrap_or(false);
        if !allowed {
            return not_found();
        }
    }
    next.run(req).await
}

/// Every change through the management API (users, passwords, permissions, vhosts, policies,
/// connections closed) is a Timeline row: who (the management user), from where, and with what.
async fn audit(State(e): State<Envs>, req: Request, next: Next) -> Response {
    let (method, path) = (req.method().clone(), req.uri().path().to_string());
    let segs: Vec<String> = path.trim_start_matches("/api/").split('/').map(decode).collect();
    if matches!(method, Method::GET | Method::HEAD) || !matches!(segs[0].as_str(), "users" | "permissions" | "vhosts" | "policies" | "connections") {
        return next.run(req).await;
    }
    let (user, _) = basic(req.headers());
    let agent = req.headers().get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let from = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip().to_string()).unwrap_or_default();
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 1 << 20).await else { return bad("body too large") };
    let b: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
    let res = next.run(Request::from_parts(parts, axum::body::Body::from(bytes))).await;
    let arg = |i: usize| segs.get(i).map(|x| vh(x)).unwrap_or_default();
    let what = match (segs[0].as_str(), method == Method::DELETE) {
        ("users", true) => format!("user {} deleted", arg(1)),
        ("users", false) if b.get("password").is_some() || b.get("password_hash").is_some() => format!("user {}: password set", arg(1)),
        ("users", false) => format!("user {}: tags set to {}", arg(1), tags_of(&b["tags"]).join(",")),
        ("permissions", true) => format!("permission of {} on vhost {} removed", arg(2), arg(1)),
        ("permissions", false) => format!("permission of {} on vhost {} set (configure {}, write {}, read {})", arg(2), arg(1), b["configure"].as_str().unwrap_or(""), b["write"].as_str().unwrap_or(""), b["read"].as_str().unwrap_or("")),
        ("vhosts", true) => format!("vhost {} deleted", arg(1)),
        ("vhosts", false) => format!("vhost {} declared", arg(1)),
        ("policies", true) => format!("policy {} on vhost {} deleted", arg(2), arg(1)),
        ("policies", false) => format!("policy {} on vhost {} set", arg(2), arg(1)),
        _ => format!("connection {} closed", arg(1)),
    };
    let st = res.status();
    if let Some(ctx) = e.default_env() {
        let label = if st.is_success() { what } else { format!("{what} — refused ({})", st.as_u16()) };
        let who = if user.is_empty() { "management API (no user)".to_string() } else { user };
        let detail = format!("{method} {path} · management API{}{}", if from.is_empty() { String::new() } else { format!(" · from {from}") }, if agent.is_empty() { String::new() } else { format!(" · {agent}") });
        let _ = stuntman_core::trips::record(&ctx, stuntman_core::trips::Trip { id: &stuntman_core::util::id("mgmt"), module: "messaging", label: &label, status: if st.is_success() { "ok" } else { "error" },
            trace_id: None, link: "", host: "rabbitmq management", actor: &who, outcome: if st.is_success() { "ok" } else { "failed" }, detail: &detail }).await;
    }
    res
}

// ---------------------------------------------------------------- rows

fn vhost_json(name: &str, n: Option<&config::Namespace>) -> Value {
    let dqt = n.map(|n| n.default_queue_type.as_str()).filter(|t| !t.is_empty()).unwrap_or("classic");
    json!({"name": name, "description": "", "tags": [], "default_queue_type": dqt, "metadata": {"description": "", "tags": [], "default_queue_type": dqt}, "tracing": false, "cluster_state": {NODE: "running"}})
}

async fn queue_row(ctx: &Ctx, n: &config::Namespace, vhost: &str, name: &str) -> Option<Value> {
    let q = n.entities.get(name)?.queue.as_ref()?;
    let rq = n.rabbit_queue(name)?;
    let c = crate::broker::counts(ctx, &n.namespace, name).await;
    let ready = c["active"].as_i64().unwrap_or(0) + c["scheduled"].as_i64().unwrap_or(0);
    let unacked = c["locked"].as_i64().unwrap_or(0);
    let mut args: Map<String, Value> = q.arguments.clone().into_iter().collect();
    if !q.dead_letter_exchange.is_empty() {
        args.insert("x-dead-letter-exchange".into(), json!(q.dead_letter_exchange));
    }
    Some(json!({"name": name, "vhost": vhost, "durable": true, "auto_delete": false, "exclusive": false, "arguments": args, "type": rq.kind, "state": "running", "node": NODE,
        "policy": if rq.policy.is_empty() { Value::Null } else { json!(rq.policy) }, "effective_policy_definition": rq.definition,
        "messages": ready + unacked, "messages_ready": ready, "messages_unacknowledged": unacked, "consumers": rabbit::active_consumers(ctx, &n.namespace, name).await}))
}

async fn queue_rows(ctx: &Ctx, n: &config::Namespace, vhost: &str) -> Vec<Value> {
    let mut out = vec![];
    for name in n.entities.keys() {
        if let Some(r) = queue_row(ctx, n, vhost, name).await {
            out.push(r);
        }
    }
    out
}

fn exchanges_of(n: &config::Namespace, vhost: &str) -> Vec<Value> {
    let mut out = vec![json!({"name": "", "vhost": vhost, "type": "direct", "durable": true, "auto_delete": false, "internal": false, "arguments": {}})];
    for (name, e) in &n.entities {
        if let Some(t) = &e.topic {
            let policy = n.policy_for(name, None).map(|(p, _)| json!(p)).unwrap_or(Value::Null);
            out.push(json!({"name": name, "vhost": vhost, "type": if t.kind.is_empty() { "topic" } else { t.kind.as_str() }, "durable": true, "auto_delete": false, "internal": false, "arguments": {}, "policy": policy}));
        }
    }
    out
}

fn bindings_of(n: &config::Namespace, vhost: &str) -> Vec<Value> {
    let row = |src: &str, dst: &str, kind: &str, key: &str| json!({"source": src, "vhost": vhost, "destination": dst, "destination_type": kind, "routing_key": key, "arguments": {}, "properties_key": if key.is_empty() { "~".to_string() } else { key.to_string() }});
    // The default exchange binds every queue by its name.
    let mut out: Vec<Value> = n.entities.iter().filter(|(_, e)| e.queue.is_some()).map(|(q, _)| row("", q, "queue", q)).collect();
    for (name, e) in &n.entities {
        if let Some(t) = &e.topic {
            for (kind, map) in [("queue", &t.subscriptions), ("exchange", &t.exchanges)] {
                for (d, s) in map {
                    out.extend(s.all_keys().iter().map(|k| row(name, d, kind, k)));
                }
            }
        }
    }
    out
}

fn policy_json(vhost: &str, name: &str, p: &RabbitPolicy) -> Value {
    json!({"vhost": vhost, "name": name, "pattern": p.pattern, "apply-to": p.apply_to, "definition": p.definition, "priority": p.priority})
}

/// `?columns=a,b`: only those fields (clients that only want the names ask for columns=name).
fn columns(v: Vec<Value>, q: &BTreeMap<String, String>) -> Value {
    let Some(cols) = q.get("columns") else { return Value::Array(v) };
    let cols: Vec<&str> = cols.split(',').map(str::trim).collect();
    Value::Array(v.into_iter().map(|r| Value::Object(cols.iter().filter_map(|c| r.get(*c).map(|x| (c.to_string(), x.clone()))).collect())).collect())
}

fn flag(q: &BTreeMap<String, String>, k: &str) -> bool {
    q.get(k).map(|v| v == "true").unwrap_or(false)
}

/// Every RabbitMQ namespace of the default environment.
fn all_vhosts(e: &Envs) -> Vec<(Ctx, config::Namespace)> {
    e.default_env().map(|c| config::list(&c).into_iter().filter(|n| n.kind == RABBIT).map(|n| (c.clone(), n)).collect()).unwrap_or_default()
}

fn perm_json(user: &str, vhost: &str, p: &Permission) -> Value {
    json!({"user": user, "vhost": vhost, "configure": p.configure, "write": p.write, "read": p.read})
}

// ---------------------------------------------------------------- handlers

/// A request body as JSON; empty (curl -X PUT with a JSON content type, as scripts send it) is none:
/// RabbitMQ takes that, axum's Json would answer 400.
fn js(raw: &axum::body::Bytes) -> Option<Json<Value>> {
    if raw.iter().all(|b| b.is_ascii_whitespace()) { None } else { serde_json::from_slice(raw).ok().map(Json) }
}

async fn put_vhost(State(e): State<Envs>, Path(v): Path<String>, headers: HeaderMap, raw: axum::body::Bytes) -> Response {
    let body = js(&raw);
    let name = vh(&v);
    let Some(ctx) = e.default_env() else { return StatusCode::SERVICE_UNAVAILABLE.into_response() };
    // RabbitMQ: a PUT without default_queue_type sets it back to classic.
    let dqt = body.as_ref().and_then(|b| b["default_queue_type"].as_str()).unwrap_or("").replace("classic", "");
    let (user, _) = basic(&headers);
    let declared = users(&ctx).contains_key(&user);
    let existing = ns_of(&e, &name);
    let was_new = existing.is_none();
    let target = match existing {
        Some((_, n)) => n.namespace,
        None => match config::create(&ctx, RABBIT, &name) {
            Ok(n) => {
                ctx.live.send("messaging.namespaces", &name, "created");
                n.namespace
            }
            Err(err) => return bad(&err.message),
        },
    };
    let r = config::change(&ctx, &target, |n| {
        n.default_queue_type = dqt.clone();
        // The user who creates a vhost gets full permissions on it, as RabbitMQ does.
        if was_new && declared {
            n.access.permissions.insert(user.clone(), Permission { configure: ".*".into(), write: ".*".into(), read: ".*".into() });
        }
        Ok(())
    });
    saved(r, was_new)
}

async fn delete_vhost(State(e): State<Envs>, Path(v): Path<String>) -> Response {
    let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found() };
    if std::fs::remove_file(config::file(&ctx, &n.namespace)).is_err() {
        return not_found();
    }
    let _ = crate::broker::purge(&ctx, &n.namespace, "", None).await;
    // RabbitMQ closes the vhost's connections; their consumers see the close, not silence.
    crate::rabbit::force_close(&ctx.env, None, Some(&n.namespace), &format!("vhost '{}' is deleted", vh(&v)));
    ctx.live.send("messaging.namespace", &n.namespace, "deleted");
    StatusCode::NO_CONTENT.into_response()
}

async fn put_policy(State(e): State<Envs>, Path((v, name)): Path<(String, String)>, raw: axum::body::Bytes) -> Response {
    let body = js(&raw);
    let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found() };
    let Some(Json(b)) = body else { return bad("Bad Request") };
    let Some(pattern) = b["pattern"].as_str() else { return bad("[{key_missing,pattern}]") };
    let Some(definition) = b["definition"].as_object() else { return bad("[{key_missing,definition}]") };
    let p = RabbitPolicy {
        pattern: pattern.into(),
        apply_to: b["apply-to"].as_str().unwrap_or("all").into(),
        priority: b["priority"].as_i64().or_else(|| b["priority"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0),
        definition: definition.clone().into_iter().collect(),
    };
    if !config::APPLY_TO.contains(&p.apply_to.as_str()) {
        return bad(&format!("Validation failed\n\n'{}' is not a valid apply-to value\n", p.apply_to));
    }
    let was_new = !n.policies.contains_key(&name);
    saved(config::change(&ctx, &n.namespace, |n| {
        n.policies.insert(name.clone(), p);
        Ok(())
    }), was_new)
}

async fn put_user(State(e): State<Envs>, Path(u): Path<String>, raw: axum::body::Bytes) -> Response {
    let body = js(&raw);
    let Some(ctx) = e.default_env() else { return StatusCode::SERVICE_UNAVAILABLE.into_response() };
    let b = body.map(|Json(b)| b).unwrap_or(json!({}));
    let mut all = users(&ctx);
    let was_new = !all.contains_key(&u);
    // rabbitmqadmin v2 sends password_hash; curl scripts send password.
    let h = match (b["password"].as_str(), b["password_hash"].as_str()) {
        (Some(p), _) => Some(hash(p)),
        (None, Some(h)) => Some(h.to_string()),
        _ if was_new => return bad("Mandatory fields missing: password or password_hash"),
        _ => None,
    };
    if let Some(h) = h {
        if util::set_secret(&ctx.data_dir, &secret(&u), &h).is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    all.insert(u.clone(), User { tags: tags_of(&b["tags"]) });
    if save_users(&ctx, &all).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    ctx.live.send("messaging.users", &u, "updated");
    created(was_new)
}

async fn delete_user(State(e): State<Envs>, Path(u): Path<String>) -> Response {
    let Some(ctx) = e.default_env() else { return StatusCode::SERVICE_UNAVAILABLE.into_response() };
    let mut all = users(&ctx);
    if all.remove(&u).is_none() || save_users(&ctx, &all).is_err() {
        return not_found();
    }
    for (c, n) in all_vhosts(&e) {
        if n.access.permissions.contains_key(&u) {
            let _ = config::change(&c, &n.namespace, |n| {
                n.access.permissions.remove(&u);
                Ok(())
            });
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn put_permission(State(e): State<Envs>, Path((v, u)): Path<(String, String)>, raw: axum::body::Bytes) -> Response {
    let body = js(&raw);
    let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return bad("vhost_or_user_not_found") };
    let b = body.map(|Json(b)| b).unwrap_or(json!({}));
    let s = |k: &str| b[k].as_str().unwrap_or("").to_string();
    let was_new = !n.access.permissions.contains_key(&u);
    saved(config::change(&ctx, &n.namespace, |n| {
        n.access.permissions.insert(u.clone(), Permission { configure: s("configure"), write: s("write"), read: s("read") });
        Ok(())
    }), was_new)
}

async fn put_queue(State(e): State<Envs>, Path((v, q)): Path<(String, String)>, raw: axum::body::Bytes) -> Response {
    let body = js(&raw);
    let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found() };
    let b = body.map(|Json(b)| b).unwrap_or(json!({}));
    let args = b["arguments"].as_object().cloned().unwrap_or_default();
    let durable_shared = b["durable"].as_bool().unwrap_or(true) && !b["exclusive"].as_bool().unwrap_or(false);
    let was_new = n.entities.get(&q).and_then(|e| e.queue.as_ref()).is_none();
    match rabbit::declare_queue(&ctx, &n.namespace, "management API", &q, false, durable_shared, args) {
        Ok(()) => {
            ctx.live.send("messaging.entities", &q, "created");
            created(was_new)
        }
        Err((_, text)) => bad(&text),
    }
}

async fn delete_queue(State(e): State<Envs>, Path((v, q)): Path<(String, String)>, Query(qs): Query<BTreeMap<String, String>>) -> Response {
    let name = vh(&v);
    let Some((ctx, n)) = ns_of(&e, &name) else { return not_found() };
    let Some(rq) = n.rabbit_queue(&q) else { return not_found() };
    let (if_unused, if_empty) = (flag(&qs, "if-unused"), flag(&qs, "if-empty"));
    if rq.kind == "quorum" && (if_unused || if_empty) {
        let f = if if_unused { "if-unused" } else { "if-empty" };
        return bad(&format!("cannot delete queue '{q}' in vhost '{name}'. queue.delete operations with {f} flag set are not supported by quorum queues"));
    }
    if let Err(text) = rabbit::can_delete_queue(&ctx, &n.namespace, &q, if_unused, if_empty).await {
        return bad(&text.replace(&format!("vhost '{}'", n.namespace), &format!("vhost '{name}'")));
    }
    let _ = crate::broker::purge(&ctx, &n.namespace, &q, None).await;
    let _ = config::change(&ctx, &n.namespace, |ns| {
        rabbit::drop_queue(ns, &q);
        Ok(())
    });
    StatusCode::NO_CONTENT.into_response()
}

async fn put_exchange(State(e): State<Envs>, Path((v, x)): Path<(String, String)>, raw: axum::body::Bytes) -> Response {
    let body = js(&raw);
    let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found() };
    let kind = body.as_ref().and_then(|b| b["type"].as_str().map(String::from)).unwrap_or("direct".into());
    let was_new = n.entities.get(&x).and_then(|e| e.topic.as_ref()).is_none();
    match rabbit::declare_exchange(&ctx, &n.namespace, "management API", &x, &kind, false) {
        Ok(()) => created(was_new),
        Err((_, text)) => bad(&text),
    }
}

async fn delete_exchange(State(e): State<Envs>, Path((v, x)): Path<(String, String)>, Query(qs): Query<BTreeMap<String, String>>) -> Response {
    let name = vh(&v);
    let Some((ctx, n)) = ns_of(&e, &name) else { return not_found() };
    if n.entities.get(&x).and_then(|e| e.topic.as_ref()).is_none() {
        return not_found();
    }
    if flag(&qs, "if-unused") && rabbit::exchange_in_use(&n, &x) {
        return bad(&format!("exchange '{x}' in vhost '{name}' in use"));
    }
    let _ = config::change(&ctx, &n.namespace, |ns| {
        rabbit::drop_exchange(ns, &x);
        Ok(())
    });
    StatusCode::NO_CONTENT.into_response()
}

async fn post_binding(e: Envs, v: String, x: String, d: String, to_exchange: bool, body: Option<Json<Value>>) -> Response {
    let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found() };
    let key = body.as_ref().and_then(|b| b["routing_key"].as_str().map(String::from)).unwrap_or_default();
    let headers = body.as_ref().and_then(|b| b["arguments"].as_object()).map(|a| a.iter().map(|(k, v)| format!("{k}={}", v.as_str().map(String::from).unwrap_or(v.to_string()))).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    match config::change(&ctx, &n.namespace, |ns| rabbit::bind(ns, &x, &d, to_exchange, &key, &headers)) {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(err) if err.status == 404 => not_found(),
        Err(err) => bad(&err.message),
    }
}

async fn delete_binding(e: Envs, v: String, x: String, d: String, to_exchange: bool, props: String) -> Response {
    let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found() };
    let key = if props == "~" { String::new() } else { props };
    let _ = config::change(&ctx, &n.namespace, |ns| {
        rabbit::unbind(ns, &x, &d, to_exchange, &key);
        Ok(())
    });
    StatusCode::NO_CONTENT.into_response()
}

fn bindings_between(n: &config::Namespace, vhost: &str, x: &str, d: &str, kind: &str) -> Vec<Value> {
    bindings_of(n, vhost).into_iter().filter(|b| b["source"] == json!(x) && b["destination"] == json!(d) && b["destination_type"] == json!(kind)).collect()
}

/// Resource alarms: a namespace set to busy (connection.blocked on AMQP) is a memory alarm.
fn alarms(e: &Envs) -> Response {
    let busy: Vec<Value> = all_vhosts(e).iter().filter(|(_, n)| n.behaviour.reachability == "busy").map(|(_, n)| json!({"node": NODE, "resource": "memory", "vhost": n.namespace})).collect();
    if busy.is_empty() {
        Json(json!({"status": "ok"})).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"status": "failed", "reason": "resource alarm(s) in effect in the cluster", "alarms": busy}))).into_response()
    }
}

pub fn router(envs: Envs) -> Router {
    Router::new()
        .route("/api/overview", get(|State(e): State<Envs>| async move {
            let (q, x) = all_vhosts(&e).iter().fold((0, 0), |(q, x), (_, n)| (q + n.queues(), x + n.topics()));
            Json(json!({"management_version": "3.13.7", "rabbitmq_version": "3.13.7", "product_name": "RabbitMQ (Stuntman)", "product_version": "3.13.7", "cluster_name": NODE, "node": NODE,
                "object_totals": {"queues": q, "exchanges": x, "connections": 0, "channels": 0, "consumers": 0}}))
        }))
        .route("/api/whoami", get(|State(e): State<Envs>, h: HeaderMap| async move {
            let (user, _) = basic(&h);
            let tags = e.default_env().and_then(|c| users(&c).remove(&user)).map(|u| u.tags).unwrap_or_else(|| vec!["administrator".into()]);
            Json(json!({"name": user, "tags": tags}))
        }))
        .route("/api/healthchecks/node", get(|| async { Json(json!({"status": "ok"})) }))
        .route("/api/health/checks/alarms", get(|State(e): State<Envs>| async move { alarms(&e) }))
        .route("/api/health/checks/local-alarms", get(|State(e): State<Envs>| async move { alarms(&e) }))
        .route("/api/aliveness-test/{vhost}", get(|State(e): State<Envs>, Path(v): Path<String>| async move {
            match ns_of(&e, &vh(&v)) { Some(_) => Json(json!({"status": "ok"})).into_response(), None => not_found() }
        }))
        .route("/api/vhosts", get(|State(e): State<Envs>| async move {
            let mut v: Vec<Value> = all_vhosts(&e).iter().map(|(_, n)| vhost_json(&n.namespace, Some(n))).collect();
            v.insert(0, vhost_json("/", None));
            Json(v)
        }))
        .route("/api/vhosts/{vhost}", get(|State(e): State<Envs>, Path(v): Path<String>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((_, n)) => Json(vhost_json(&name, Some(&n))).into_response(), None => not_found() }
        }).put(put_vhost).delete(delete_vhost))
        .route("/api/vhosts/{vhost}/permissions", get(|State(e): State<Envs>, Path(v): Path<String>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((_, n)) => Json(n.access.permissions.iter().map(|(u, p)| perm_json(u, &name, p)).collect::<Vec<_>>()).into_response(), None => not_found() }
        }))
        .route("/api/users", get(|State(e): State<Envs>| async move {
            Json(e.default_env().map(|c| users(&c).iter().map(|(n, u)| user_json(n, u)).collect::<Vec<_>>()).unwrap_or_default())
        }))
        .route("/api/users/{user}", get(|State(e): State<Envs>, Path(u): Path<String>| async move {
            match e.default_env().and_then(|c| users(&c).remove(&u)) {
                Some(x) => Json(user_json(&u, &x)).into_response(),
                None if u == "guest" => Json(user_json(&u, &User { tags: vec!["administrator".into()] })).into_response(),
                None => not_found(),
            }
        }).put(put_user).delete(delete_user))
        .route("/api/users/{user}/permissions", get(|State(e): State<Envs>, Path(u): Path<String>| async move {
            Json(all_vhosts(&e).iter().filter_map(|(_, n)| n.access.permissions.get(&u).map(|p| perm_json(&u, &n.namespace, p))).collect::<Vec<_>>())
        }))
        .route("/api/permissions", get(|State(e): State<Envs>| async move {
            Json(all_vhosts(&e).iter().flat_map(|(_, n)| n.access.permissions.iter().map(|(u, p)| perm_json(u, &n.namespace, p)).collect::<Vec<_>>()).collect::<Vec<_>>())
        }))
        .route("/api/permissions/{vhost}/{user}", get(|State(e): State<Envs>, Path((v, u)): Path<(String, String)>| async move {
            let name = vh(&v);
            match ns_of(&e, &name).and_then(|(_, n)| n.access.permissions.get(&u).cloned()) { Some(p) => Json(perm_json(&u, &name, &p)).into_response(), None => not_found() }
        }).put(put_permission).delete(|State(e): State<Envs>, Path((v, u)): Path<(String, String)>| async move {
            let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found() };
            if !n.access.permissions.contains_key(&u) {
                return not_found();
            }
            saved(config::change(&ctx, &n.namespace, |n| {
                n.access.permissions.remove(&u);
                Ok(())
            }), false)
        }))
        .route("/api/policies", get(|State(e): State<Envs>| async move {
            Json(all_vhosts(&e).iter().flat_map(|(_, n)| n.policies.iter().map(|(k, p)| policy_json(&n.namespace, k, p)).collect::<Vec<_>>()).collect::<Vec<_>>())
        }))
        .route("/api/policies/{vhost}", get(|State(e): State<Envs>, Path(v): Path<String>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((_, n)) => Json(n.policies.iter().map(|(k, p)| policy_json(&name, k, p)).collect::<Vec<_>>()).into_response(), None => not_found() }
        }))
        .route("/api/policies/{vhost}/{name}", get(|State(e): State<Envs>, Path((v, p)): Path<(String, String)>| async move {
            let name = vh(&v);
            match ns_of(&e, &name).and_then(|(_, n)| n.policies.get(&p).cloned()) { Some(x) => Json(policy_json(&name, &p, &x)).into_response(), None => not_found() }
        }).put(put_policy).delete(|State(e): State<Envs>, Path((v, p)): Path<(String, String)>| async move {
            let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found() };
            if !n.policies.contains_key(&p) {
                return not_found();
            }
            saved(config::change(&ctx, &n.namespace, |n| {
                n.policies.remove(&p);
                Ok(())
            }), false)
        }))
        .route("/api/queues", get(|State(e): State<Envs>, Query(qs): Query<BTreeMap<String, String>>| async move {
            let mut out = vec![];
            for (ctx, n) in all_vhosts(&e) {
                out.extend(queue_rows(&ctx, &n, &n.namespace).await);
            }
            Json(columns(out, &qs))
        }))
        .route("/api/queues/{vhost}", get(|State(e): State<Envs>, Path(v): Path<String>, Query(qs): Query<BTreeMap<String, String>>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((ctx, n)) => Json(columns(queue_rows(&ctx, &n, &name).await, &qs)).into_response(), None => not_found() }
        }))
        .route("/api/queues/{vhost}/{name}", get(|State(e): State<Envs>, Path((v, q)): Path<(String, String)>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((ctx, n)) => match queue_row(&ctx, &n, &name, &q).await { Some(r) => Json(r).into_response(), None => not_found() }, None => not_found() }
        }).put(put_queue).delete(delete_queue))
        .route("/api/queues/{vhost}/{name}/contents", axum::routing::delete(|State(e): State<Envs>, Path((v, q)): Path<(String, String)>| async move {
            let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found() };
            if n.rabbit_queue(&q).is_none() {
                return not_found();
            }
            let _ = crate::broker::purge(&ctx, &n.namespace, &q, Some("active".into())).await;
            StatusCode::NO_CONTENT.into_response()
        }))
        .route("/api/exchanges", get(|State(e): State<Envs>| async move { Json(all_vhosts(&e).iter().flat_map(|(_, n)| exchanges_of(n, &n.namespace)).collect::<Vec<_>>()) }))
        .route("/api/exchanges/{vhost}", get(|State(e): State<Envs>, Path(v): Path<String>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((_, n)) => Json(exchanges_of(&n, &name)).into_response(), None => not_found() }
        }))
        .route("/api/exchanges/{vhost}/{name}", get(|State(e): State<Envs>, Path((v, x)): Path<(String, String)>| async move {
            let name = vh(&v);
            match ns_of(&e, &name).and_then(|(_, n)| exchanges_of(&n, &name).into_iter().find(|r| r["name"] == json!(x))) { Some(r) => Json(r).into_response(), None => not_found() }
        }).put(put_exchange).delete(delete_exchange))
        .route("/api/bindings", get(|State(e): State<Envs>| async move { Json(all_vhosts(&e).iter().flat_map(|(_, n)| bindings_of(n, &n.namespace)).collect::<Vec<_>>()) }))
        .route("/api/bindings/{vhost}", get(|State(e): State<Envs>, Path(v): Path<String>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((_, n)) => Json(bindings_of(&n, &name)).into_response(), None => not_found() }
        }))
        .route("/api/bindings/{vhost}/e/{ex}/q/{q}", post(|State(e): State<Envs>, Path((v, x, q)): Path<(String, String, String)>, raw: axum::body::Bytes| post_binding(e, v, x, q, false, js(&raw)))
            .get(|State(e): State<Envs>, Path((v, x, q)): Path<(String, String, String)>| async move {
                let name = vh(&v);
                match ns_of(&e, &name) { Some((_, n)) => Json(bindings_between(&n, &name, &x, &q, "queue")).into_response(), None => not_found() }
            }))
        .route("/api/bindings/{vhost}/e/{ex}/e/{dest}", post(|State(e): State<Envs>, Path((v, x, d)): Path<(String, String, String)>, raw: axum::body::Bytes| post_binding(e, v, x, d, true, js(&raw)))
            .get(|State(e): State<Envs>, Path((v, x, d)): Path<(String, String, String)>| async move {
                let name = vh(&v);
                match ns_of(&e, &name) { Some((_, n)) => Json(bindings_between(&n, &name, &x, &d, "exchange")).into_response(), None => not_found() }
            }))
        .route("/api/bindings/{vhost}/e/{ex}/q/{q}/{props}", axum::routing::delete(|State(e): State<Envs>, Path((v, x, q, p)): Path<(String, String, String, String)>| delete_binding(e, v, x, q, false, p)))
        .route("/api/bindings/{vhost}/e/{ex}/e/{dest}/{props}", axum::routing::delete(|State(e): State<Envs>, Path((v, x, d, p)): Path<(String, String, String, String)>| delete_binding(e, v, x, d, true, p)))
        .route("/api/connections", get(|State(e): State<Envs>| async move { Json(e.default_env().map(|c| crate::rabbit::connections(Some(&c.env), None)).unwrap_or_default()) }))
        .route("/api/vhosts/{vhost}/connections", get(|State(e): State<Envs>, Path(v): Path<String>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((c, n)) => Json(crate::rabbit::connections(Some(&c.env), Some(&n.namespace))).into_response(), None => not_found() }
        }))
        .route("/api/connections/{name}", get(|State(e): State<Envs>, Path(c): Path<String>| async move {
            let name = decode(&c);
            match e.default_env().and_then(|x| crate::rabbit::connections(Some(&x.env), None).into_iter().find(|r| r["name"] == json!(name))) { Some(r) => Json(r).into_response(), None => not_found() }
        }).delete(|State(e): State<Envs>, Path(c): Path<String>, h: HeaderMap| async move {
            let Some(ctx) = e.default_env() else { return not_found() };
            // rabbitmqadmin sends the reason in X-Reason, as the management UI does.
            let why = h.get("x-reason").and_then(|v| v.to_str().ok()).filter(|s| !s.is_empty()).unwrap_or("Closed via management plugin").to_string();
            if crate::rabbit::force_close(&ctx.env, Some(&decode(&c)), None, &why) == 0 { not_found() } else { StatusCode::NO_CONTENT.into_response() }
        }))
        .layer(middleware::from_fn_with_state(envs.clone(), auth))
        .layer(middleware::from_fn_with_state(envs.clone(), audit))
        .with_state(envs)
}

pub fn serve(envs: Envs, port: u16) {
    tokio::spawn(async move {
        let Ok(l) = tokio::net::TcpListener::bind(("0.0.0.0", port)).await else {
            eprintln!("RabbitMQ management: port {port} is taken");
            return;
        };
        let _ = axum::serve(l, router(envs).into_make_service_with_connect_info::<std::net::SocketAddr>()).await;
    });
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn password_hash_round_trips_like_rabbitmq() {
        let h = hash("acme-pass-1");
        assert!(verify(&h, "acme-pass-1"));
        assert!(!verify(&h, "wrong"));
        // rabbitmqadmin v2 sends hashes made by RabbitMQ's own algorithm: salt ‖ sha256(salt ‖ pw)
        let salt = [1u8, 2, 3, 4];
        let theirs = B64.encode([&salt[..], &Sha256::digest([&salt[..], b"secret-1"].concat())[..]].concat());
        assert!(verify(&theirs, "secret-1"));
        assert_eq!(decode("acme%2Dadmin"), "acme-admin");
        assert_eq!(tags_of(&json!("management, policymaker")), vec!["management", "policymaker"]);
    }
}
