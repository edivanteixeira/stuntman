//! A17: the RabbitMQ management HTTP API (its own port, 15672) for apps and tools that create
//! vhosts, queues, exchanges and bindings over HTTP before they connect (MassTransit, scripts):
//! overview, whoami, vhosts, permissions, queues, exchanges, bindings, aliveness. Vhosts are the
//! RabbitMQ namespaces of the default environment ("/" is the first one).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use stuntman_core::{env::Envs, Ctx};

use crate::config::{self, Entity, Queue, Subscription, Topic, RABBIT};

fn vh(v: &str) -> String {
    let d = v.replace("%2F", "/").replace("%2f", "/");
    if d == "/" || d.is_empty() { "/".into() } else { d.trim_start_matches('/').to_string() }
}

fn ns_of(envs: &Envs, vhost: &str) -> Option<(Ctx, config::Namespace)> {
    crate::rabbit::find(envs, if vhost == "/" { "" } else { vhost })
}

fn not_found(what: &str) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": "Object Not Found", "reason": format!("{what} not found")}))).into_response()
}

async fn queue_rows(ctx: &Ctx, n: &config::Namespace, vhost: &str) -> Vec<Value> {
    let mut out = vec![];
    for (name, e) in &n.entities {
        if e.queue.is_some() {
            let c = crate::broker::counts(ctx, &n.namespace, name).await;
            let ready = c["active"].as_i64().unwrap_or(0) + c["scheduled"].as_i64().unwrap_or(0);
            let unacked = c["locked"].as_i64().unwrap_or(0);
            out.push(json!({"name": name, "vhost": vhost, "durable": true, "auto_delete": false, "exclusive": false, "arguments": {}, "type": "classic", "state": "running",
                "messages": ready + unacked, "messages_ready": ready, "messages_unacknowledged": unacked, "consumers": crate::broker::consumers(ctx, &n.namespace, name).await.map(|x| x.len()).unwrap_or(0)}));
        }
    }
    out
}

fn exchanges_of(n: &config::Namespace, vhost: &str) -> Vec<Value> {
    let mut out = vec![json!({"name": "", "vhost": vhost, "type": "direct", "durable": true, "auto_delete": false, "internal": false, "arguments": {}})];
    for (name, e) in &n.entities {
        if let Some(t) = &e.topic {
            out.push(json!({"name": name, "vhost": vhost, "type": if t.kind.is_empty() { "topic" } else { t.kind.as_str() }, "durable": true, "auto_delete": false, "internal": false, "arguments": {}}));
        }
    }
    out
}

fn bindings_of(n: &config::Namespace, vhost: &str) -> Vec<Value> {
    let mut out = vec![];
    for (name, e) in &n.entities {
        if let Some(t) = &e.topic {
            for (q, s) in &t.subscriptions {
                out.push(json!({"source": name, "vhost": vhost, "destination": q, "destination_type": "queue", "routing_key": s.filter, "arguments": {}, "properties_key": if s.filter.is_empty() { "~".to_string() } else { s.filter.clone() }}));
            }
        }
    }
    out
}

pub fn router(envs: Envs) -> Router {
    Router::new()
        .route("/api/overview", get(|State(e): State<Envs>| async move {
            let ctx = e.default_env();
            let (q, x) = ctx.as_ref().map(|c| config::list(c).into_iter().filter(|n| n.kind == RABBIT).fold((0, 0), |(q, x), n| (q + n.entities.values().filter(|e| e.queue.is_some()).count(), x + n.entities.values().filter(|e| e.topic.is_some()).count()))).unwrap_or((0, 0));
            Json(json!({"management_version": "3.13.7", "rabbitmq_version": "3.13.7", "product_name": "RabbitMQ (Stuntman)", "product_version": "3.13.7", "cluster_name": "stuntman@localhost", "node": "stuntman@localhost",
                "object_totals": {"queues": q, "exchanges": x, "connections": 0, "channels": 0, "consumers": 0}}))
        }))
        .route("/api/whoami", get(|| async { Json(json!({"name": "guest", "tags": ["administrator"]})) }))
        .route("/api/healthchecks/node", get(|| async { Json(json!({"status": "ok"})) }))
        .route("/api/aliveness-test/{vhost}", get(|State(e): State<Envs>, Path(v): Path<String>| async move {
            match ns_of(&e, &vh(&v)) { Some(_) => Json(json!({"status": "ok"})).into_response(), None => not_found("vhost") }
        }))
        .route("/api/vhosts", get(|State(e): State<Envs>| async move {
            let ctx = e.default_env();
            let mut v: Vec<Value> = ctx.as_ref().map(|c| config::list(c).into_iter().filter(|n| n.kind == RABBIT).map(|n| json!({"name": n.namespace, "tracing": false, "description": "", "tags": []})).collect()).unwrap_or_default();
            v.insert(0, json!({"name": "/", "tracing": false, "description": "the default vhost", "tags": []}));
            Json(v)
        }))
        .route("/api/vhosts/{vhost}", get(|State(e): State<Envs>, Path(v): Path<String>| async move {
            let name = vh(&v);
            let Some(ctx) = e.default_env() else { return StatusCode::SERVICE_UNAVAILABLE.into_response() };
            if name == "/" || config::get(&ctx, &name).map(|n| n.kind == RABBIT).unwrap_or(false) { Json(json!({"name": name, "tracing": false})).into_response() } else { not_found("vhost") }
        }).put(|State(e): State<Envs>, Path(v): Path<String>| async move {
            let name = vh(&v);
            let Some(ctx) = e.default_env() else { return StatusCode::SERVICE_UNAVAILABLE.into_response() };
            if name == "/" || config::get(&ctx, &name).is_ok() { return StatusCode::NO_CONTENT.into_response() }
            match config::create(&ctx, RABBIT, &name) {
                Ok(_) => { ctx.live.send("messaging.namespaces", &name, "created"); StatusCode::CREATED.into_response() }
                Err(err) => (StatusCode::BAD_REQUEST, Json(json!({"error": "bad_request", "reason": err.message}))).into_response(),
            }
        }).delete(|| async { StatusCode::NO_CONTENT }))
        .route("/api/permissions/{vhost}/{user}", put(|| async { StatusCode::CREATED }).get(|Path((v, u)): Path<(String, String)>| async move {
            Json(json!({"user": u, "vhost": vh(&v), "configure": ".*", "write": ".*", "read": ".*"}))
        }))
        .route("/api/users/{user}", put(|| async { StatusCode::CREATED }).get(|Path(u): Path<String>| async move { Json(json!({"name": u, "tags": ["administrator"]})) }))
        .route("/api/queues", get(|State(e): State<Envs>| async move {
            let mut out = vec![];
            if let Some(ctx) = e.default_env() {
                for n in config::list(&ctx).into_iter().filter(|n| n.kind == RABBIT) { out.extend(queue_rows(&ctx, &n, &n.namespace).await); }
            }
            Json(out)
        }))
        .route("/api/queues/{vhost}", get(|State(e): State<Envs>, Path(v): Path<String>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((ctx, n)) => Json(queue_rows(&ctx, &n, &name).await).into_response(), None => not_found("vhost") }
        }))
        .route("/api/queues/{vhost}/{name}", get(|State(e): State<Envs>, Path((v, q)): Path<(String, String)>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((ctx, n)) => match queue_rows(&ctx, &n, &name).await.into_iter().find(|r| r["name"] == json!(q)) { Some(r) => Json(r).into_response(), None => not_found("queue") }, None => not_found("vhost") }
        }).put(|State(e): State<Envs>, Path((v, q)): Path<(String, String)>, body: Option<Json<Value>>| async move {
            let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found("vhost") };
            let dlx = body.as_ref().and_then(|b| b["arguments"]["x-dead-letter-exchange"].as_str().map(String::from)).unwrap_or_default();
            let created = !n.entities.contains_key(&q);
            let r = config::change(&ctx, &n.namespace, |ns| { ns.entities.entry(q.clone()).or_insert(Entity { queue: Some(Queue { lock: "30s".into(), max_delivery: 10, dead_letter_exchange: dlx, created_by: "management API".into(), ..Default::default() }), topic: None }); Ok(()) });
            match r { Ok(_) => { ctx.live.send("messaging.entities", &q, "created"); if created { StatusCode::CREATED } else { StatusCode::NO_CONTENT }.into_response() } Err(err) => (StatusCode::BAD_REQUEST, err.message).into_response() }
        }).delete(|State(e): State<Envs>, Path((v, q)): Path<(String, String)>| async move {
            let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found("vhost") };
            let _ = config::change(&ctx, &n.namespace, |ns| { ns.entities.remove(&q); Ok(()) });
            StatusCode::NO_CONTENT.into_response()
        }))
        .route("/api/exchanges/{vhost}", get(|State(e): State<Envs>, Path(v): Path<String>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((_, n)) => Json(exchanges_of(&n, &name)).into_response(), None => not_found("vhost") }
        }))
        .route("/api/exchanges/{vhost}/{name}", put(|State(e): State<Envs>, Path((v, x)): Path<(String, String)>, body: Option<Json<Value>>| async move {
            let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found("vhost") };
            let kind = body.as_ref().and_then(|b| b["type"].as_str().map(String::from)).unwrap_or("topic".into());
            let created = !n.entities.contains_key(&x);
            let r = config::change(&ctx, &n.namespace, |ns| { ns.entities.entry(x.clone()).or_insert(Entity { queue: None, topic: Some(Topic { kind, created_by: "management API".into(), ..Default::default() }) }); Ok(()) });
            match r { Ok(_) => if created { StatusCode::CREATED.into_response() } else { StatusCode::NO_CONTENT.into_response() }, Err(err) => (StatusCode::BAD_REQUEST, err.message).into_response() }
        }).get(|State(e): State<Envs>, Path((v, x)): Path<(String, String)>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((_, n)) => match exchanges_of(&n, &name).into_iter().find(|r| r["name"] == json!(x)) { Some(r) => Json(r).into_response(), None => not_found("exchange") }, None => not_found("vhost") }
        }).delete(|State(e): State<Envs>, Path((v, x)): Path<(String, String)>| async move {
            let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found("vhost") };
            let _ = config::change(&ctx, &n.namespace, |ns| { ns.entities.remove(&x); Ok(()) });
            StatusCode::NO_CONTENT.into_response()
        }))
        .route("/api/bindings/{vhost}", get(|State(e): State<Envs>, Path(v): Path<String>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((_, n)) => Json(bindings_of(&n, &name)).into_response(), None => not_found("vhost") }
        }))
        .route("/api/bindings/{vhost}/e/{ex}/q/{q}", post(|State(e): State<Envs>, Path((v, x, q)): Path<(String, String, String)>, body: Option<Json<Value>>| async move {
            let Some((ctx, n)) = ns_of(&e, &vh(&v)) else { return not_found("vhost") };
            let key = body.as_ref().and_then(|b| b["routing_key"].as_str().map(String::from)).unwrap_or_default();
            let r = config::change(&ctx, &n.namespace, |ns| {
                let t = ns.entities.entry(x.clone()).or_insert(Entity { queue: None, topic: Some(Topic { kind: "topic".into(), ..Default::default() }) });
                if let Some(t) = t.topic.as_mut() { t.subscriptions.insert(q.clone(), Subscription { filter: key.clone(), lock: "30s".into(), max_delivery: 10, created_by: "management API".into(), ..Default::default() }); }
                Ok(())
            });
            match r { Ok(_) => StatusCode::CREATED.into_response(), Err(err) => (StatusCode::BAD_REQUEST, err.message).into_response() }
        }).get(|State(e): State<Envs>, Path((v, x, q)): Path<(String, String, String)>| async move {
            let name = vh(&v);
            match ns_of(&e, &name) { Some((_, n)) => Json(bindings_of(&n, &name).into_iter().filter(|b| b["source"] == json!(x) && b["destination"] == json!(q)).collect::<Vec<_>>()).into_response(), None => not_found("vhost") }
        }))
        .with_state(envs)
}

pub fn serve(envs: Envs, port: u16) {
    tokio::spawn(async move {
        let Ok(l) = tokio::net::TcpListener::bind(("0.0.0.0", port)).await else {
            eprintln!("RabbitMQ management: port {port} is taken");
            return;
        };
        let _ = axum::serve(l, router(envs)).await;
    });
}
