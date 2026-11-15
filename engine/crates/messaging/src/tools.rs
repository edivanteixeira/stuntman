//! M1–M3 tools: one vocabulary for Service Bus and RabbitMQ. The rule tools (S2) and the account
//! tools (S5) are shared with Storage: here they are messaging_*; the server answers the shared
//! names with them when service is azure-service-bus or rabbitmq.

use axum::{
    extract::{Path, State},
    http::header,
    response::IntoResponse,
    routing::get,
    Router,
};
use serde_json::{json, Map, Value};
use stuntman_core::{tools::Args, util, ApiError, Ctx, Tool};

use crate::{broker, config, rules};

fn ns_of(a: &Args) -> Result<String, ApiError> {
    a.str("namespace").or(a.str("ns")).or(a.str("account")).ok_or_else(|| ApiError::bad("“namespace” is required"))
}

fn entity_of(a: &Args) -> Result<String, ApiError> {
    a.str("entity").or(a.str("queue")).ok_or_else(|| ApiError::bad("“entity” is required: a queue, a topic or <topic>/subscriptions/<name>"))
}

fn by(a: &Args) -> String {
    match a.str("_by").as_deref() {
        Some("you") | None => "you".into(),
        Some(b) => b.trim_start_matches("mcp:").trim_start_matches("api:").to_string(),
    }
}

fn via(a: &Args) -> String {
    match a.str("_by").as_deref() {
        Some(b) if b.starts_with("mcp:") => "mcp".into(),
        Some(b) if b.starts_with("api:") => "api".into(),
        _ => "you".into(),
    }
}

/// Where an app connects: a connection string in the provider's own format.
pub fn connection(ctx: &Ctx, n: &config::Namespace) -> String {
    let port = crate::port();
    let default = ctx.prefix().is_empty();
    let policy = n.access.policies.keys().next().cloned().unwrap_or_default();
    let key = config::key_value(ctx, &n.namespace, &policy);
    if n.kind == config::SB {
        // <ns>.localhost resolves to loopback without a hosts entry; the host names the namespace.
        let host = if default { format!("{}.localhost", n.namespace) } else { format!("{}.{}.localhost", n.namespace, ctx.env) };
        format!("Endpoint=sb://{host}:{port};SharedAccessKeyName={policy};SharedAccessKey={key};UseDevelopmentEmulator=true")
    } else {
        let vhost = if default { n.namespace.clone() } else { format!("{}%2F{}", ctx.env, n.namespace) };
        format!("amqp://{policy}:{}@localhost:{port}/{vhost}", key.replace('/', "%2F").replace('+', "%2B").replace('=', "%3D"))
    }
}

pub async fn ns_view(ctx: &Ctx, n: &config::Namespace) -> Value {
    let mut entities = vec![];
    for (name, e) in &n.entities {
        if let Some(q) = &e.queue {
            let c = broker::counts(ctx, &n.namespace, name).await;
            entities.push(json!({"name": name, "kind": "queue", "counts": c, "lock": q.lock, "maxDelivery": q.max_delivery, "sessions": q.sessions, "createdBy": q.created_by,
                "deadLetterExchange": q.dead_letter_exchange, "next": broker::next_of(ctx, &n.namespace, name).await}));
        }
        if let Some(t) = &e.topic {
            let mut subs = vec![];
            for (s, sub) in &t.subscriptions {
                let holder = if n.kind == config::SB { format!("{name}/subscriptions/{s}") } else { s.clone() };
                let c = if n.kind == config::SB { broker::counts(ctx, &n.namespace, &holder).await } else { json!({}) };
                subs.push(json!({"name": s, "holder": holder, "filter": sub.filter, "counts": c, "lock": sub.lock, "maxDelivery": sub.max_delivery}));
            }
            entities.push(json!({"name": name, "kind": "topic", "exchangeType": t.kind, "subscriptions": subs, "createdBy": t.created_by,
                "counts": if n.kind == config::SB { broker::counts(ctx, &n.namespace, name).await } else { json!({}) }}));
        }
    }
    let keys: Vec<Value> = n.access.policies.iter().map(|(p, pol)| json!({"name": p, "rights": pol.rights, "masked": config::mask(&config::key_value(ctx, &n.namespace, p))})).collect();
    json!({"namespace": n.namespace, "kind": n.kind, "service": n.kind, "label": if n.kind == config::SB { "Azure Service Bus" } else { "RabbitMQ" },
        "queues": n.queues(), "topics": n.topics(), "entities": entities, "connection": connection(ctx, n), "keys": keys,
        "behaviour": n.behaviour, "access": {"anyKey": n.access.any_key, "managedIdentity": n.access.managed_identity},
        "file": config::rel_file(&n.namespace), "yaml": config::text(ctx, &n.namespace), "settings": n, "port": crate::port(),
        "bindError": crate::amqp::BIND_ERROR.lock().unwrap().clone()})
}

fn service(a: &Args) -> Result<String, ApiError> {
    let s = a.str("service").or(a.str("name")).ok_or_else(|| ApiError::bad("“service” is required: azure-service-bus or rabbitmq"))?;
    if s != config::SB && s != config::RABBIT {
        return Err(ApiError::not_found(format!("No messaging service “{s}”: azure-service-bus or rabbitmq")));
    }
    Ok(s)
}

pub fn is_messaging(a: &Value) -> bool {
    let s = a.get("service").or(a.get("name")).and_then(|v| v.as_str()).unwrap_or("");
    s == config::SB || s == config::RABBIT
}

fn wanted(a: &Args) -> Option<String> {
    a.str("state").filter(|s| s != "all")
}

pub fn all() -> Vec<Tool> {
    let mut v = queues();
    v.extend(rules_tools());
    v.extend(namespaces());
    v.extend(crate::openwire::tools());
    v
}

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route("/api/messaging/{ns}/yaml", get(|State(ctx): State<Ctx>, Path(ns): Path<String>| async move { ([(header::CONTENT_TYPE, "application/yaml")], config::text(&ctx, &ns)).into_response() }))
        .route("/api/messaging/rules/{service}", get(|State(ctx): State<Ctx>, Path(s): Path<String>| async move {
            ([(header::CONTENT_TYPE, "application/yaml")], std::fs::read_to_string(rules::file_path(&ctx, s.trim_end_matches(".yaml"))).unwrap_or_default()).into_response()
        }))
}

/// Seeds load on reset (nothing to do on start: environments begin empty, tests/seed_messaging.py fills a demo).
pub fn seed_if_empty(ctx: &Ctx) {
    for n in config::list(ctx).into_iter().filter(|n| n.behaviour.seed_on_reset && !n.seed.is_empty()) {
        let dir = ctx.path(&n.seed);
        if !dir.exists() {
            continue;
        }
        let (ctx, n) = (ctx.clone(), n.clone());
        tokio::spawn(async move {
            let have = broker::counts(&ctx, &n.namespace, "").await;
            if STATES_SUM(&have) > 0 {
                return;
            }
            for e in std::fs::read_dir(ctx.path(&n.seed)).into_iter().flatten().flatten() {
                let entity = e.file_name().to_string_lossy().to_string();
                let mut files: Vec<_> = std::fs::read_dir(e.path()).into_iter().flatten().flatten().map(|f| f.path()).collect();
                files.sort();
                for f in files {
                    let Ok(t) = std::fs::read_to_string(&f) else { continue };
                    let v: Value = serde_json::from_str(&t).unwrap_or(json!({"body": t}));
                    let body = v.get("body").cloned().unwrap_or(v.clone());
                    let o = broker::Outgoing { subject: v["subject"].as_str().unwrap_or("").into(), content_type: "application/json".into(), body: if let Value::String(s) = &body { s.clone().into_bytes() } else { body.to_string().into_bytes() },
                        props: v["properties"].as_object().cloned().unwrap_or_default(), sender: "seed".into(), via: "seed".into(), ..Default::default() };
                    let _ = broker::publish(&ctx, &n.namespace, &entity, o).await;
                }
            }
        });
    }
}

#[allow(non_snake_case)]
fn STATES_SUM(v: &Value) -> i64 {
    broker::STATES.iter().map(|s| v[*s].as_i64().unwrap_or(0)).sum()
}

fn find_holder(n: &config::Namespace, entity: &str) -> Result<String, ApiError> {
    let e = entity.trim_end_matches("/$DeadLetterQueue").trim_end_matches("/$deadletterqueue");
    if n.holders().iter().any(|h| h == e) || n.entities.contains_key(e) {
        return Ok(e.to_string());
    }
    Err(ApiError::not_found(format!("No queue, topic or subscription “{entity}” in {}", n.namespace)))
}

fn queues() -> Vec<Tool> {
    vec![
        Tool::new("list_namespaces", "Namespaces with kind (azure-service-bus, rabbitmq), entities (queues, topics and their subscriptions or bindings), counters per state and the connection string.", |ctx, _a| async move {
            let mut out = vec![];
            for n in config::list(&ctx) {
                out.push(ns_view(&ctx, &n).await);
            }
            Ok(json!({"namespaces": out, "port": crate::port()}))
        })
        .route("GET", "/messaging/namespaces"),
        Tool::new("list_queue_messages", "Messages of a queue, topic or subscription, newest first; state = active | scheduled | locked | dead_letter | completed.", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let n = config::get(&ctx, &ns)?;
            let h = find_holder(&n, &entity_of(&a)?)?;
            let rows = broker::list(&ctx, &ns, &h, wanted(&a), a.str("q"), a.u64("last").unwrap_or(50) as u32).await?;
            Ok(json!({"namespace": ns, "entity": h, "counts": broker::counts(&ctx, &ns, &h).await, "messages": rows, "next": broker::next_of(&ctx, &ns, &h).await,
                "limits": {"lock": n.limits(&h).lock_ms / 1000, "maxDelivery": n.limits(&h).max_delivery}}))
        })
        .req("namespace", "namespace").req("entity", "queue, topic, or <topic>/subscriptions/<name>").param("state", "active | scheduled | locked | dead_letter | completed").param("q", "message id, subject, #seq or text in the body").param("last", "default 50")
        .route("GET", "/messaging/{namespace}/{entity}/messages"),
        Tool::new("get_queue_message", "One message: properties, body and the delivery history (published, delivered, abandoned, completed, dead-lettered…).", |ctx, a| async move {
            let ns = ns_of(&a)?;
            broker::get(&ctx, &ns, a.need("seq")?.trim_start_matches('#').parse().map_err(|_| ApiError::bad("seq: a number"))?).await
        })
        .req("namespace", "namespace").param("entity", "entity (not needed: seq is unique)").req("seq", "sequence number")
        .route("GET", "/messaging/{namespace}/{entity}/messages/{seq}"),
        Tool::new("wait_for_queue_message", "Waits for the first message published by the app (not by you or an agent) that matches: what an agent waits for. 408 after the timeout.", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let n = config::get(&ctx, &ns)?;
            let h = find_holder(&n, &entity_of(&a)?)?;
            let since = a.u64("since").map(|x| x as i64).unwrap_or_else(|| util::now_ms() - 2_000);
            let wh: Map<String, Value> = match a.get("where") {
                Some(Value::Object(o)) => o.clone(),
                Some(Value::String(s)) => serde_json::from_str(s).unwrap_or_default(),
                _ => Map::new(),
            };
            let end = util::now_ms() + a.ms("timeout")?.unwrap_or(30_000) as i64;
            let bell = broker::bell(&ctx, &ns, &h);
            loop {
                let rows = broker::list(&ctx, &ns, &h, None, None, 200).await?;
                if let Some(m) = rows.iter().rev().find(|m| m["enqueuedAt"].as_i64().unwrap_or(0) >= since && m["via"] == "amqp" && wh.iter().all(|(k, v)| {
                    let want = v.as_str().map(String::from).unwrap_or(v.to_string());
                    let have = m.get(k).or(m["properties"].get(k)).or(m["system"].get(k)).map(|x| x.as_str().map(String::from).unwrap_or(x.to_string())).unwrap_or_default();
                    have == want
                })) {
                    let seq = m["seq"].as_i64().unwrap_or(0);
                    let mut full = broker::get(&ctx, &ns, seq).await?;
                    if let Ok(b) = serde_json::from_str::<Value>(full["body"].as_str().unwrap_or("")) {
                        full["body"] = b;
                    }
                    return Ok(full);
                }
                if util::now_ms() > end {
                    return Err(ApiError::new(408, format!("No matching message on {ns}/{h} yet")));
                }
                tokio::select! { _ = bell.notified() => {} _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {} }
            }
        })
        .req("namespace", "namespace").req("entity", "entity").param("where", "{\"subject\": \"OrderCreated\"} — message fields, properties or system properties").param("timeout", "default 30s").param("since", "ms; default 2 s ago")
        .route("GET", "/messaging/{namespace}/{entity}/wait"),
        Tool::new("publish", "Publishes a message as a test (recorded as sent by you or the agent, never as the app). Topics fan out to their subscriptions.", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let entity = entity_of(&a)?;
            let body = match a.get("body") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            let mut sys = Map::new();
            for k in ["correlation_id", "session_id", "reply_to", "routing_key"] {
                if let Some(v) = a.str(k) {
                    let camel: String = k.split('_').enumerate().map(|(i, p)| if i == 0 { p.to_string() } else { p[..1].to_uppercase() + &p[1..] }).collect();
                    sys.insert(camel, json!(v));
                }
            }
            let schedule = match a.get("schedule_at") {
                Some(Value::Number(n)) => n.as_i64(),
                Some(Value::String(s)) => util::parse_ms(s).map(|d| util::now_ms() + d as i64),
                _ => None,
            };
            let o = broker::Outgoing { subject: a.str("subject").unwrap_or_default(), content_type: a.str("content_type").unwrap_or("application/json".into()), body: body.into_bytes(),
                props: a.get("properties").and_then(|p| p.as_object().cloned()).unwrap_or_default(), sys, scheduled_at: schedule, sender: by(&a), via: via(&a), message_id: a.str("message_id").unwrap_or_default() };
            let seqs = broker::publish(&ctx, &ns, &entity, o).await.map_err(|e| ApiError::new(match e { broker::Refusal::NotFound(_) => 404, broker::Refusal::Unauthorized(_) => 401, broker::Refusal::Quota(_) => 403, _ => 503 }, e.text().to_string()))?;
            Ok(json!({"namespace": ns, "entity": entity, "seqs": seqs, "copies": seqs.len()}))
        })
        .req("namespace", "namespace").req("entity", "queue or topic").param("body", "JSON or text").param("subject", "subject / label").param("properties", "{key: value} application properties (headers)")
        .param("schedule_at", "epoch ms or a delay like 5m").param("correlation_id", "").param("session_id", "").param("routing_key", "RabbitMQ: the routing key").param("message_id", "")
        .route("POST", "/messaging/{namespace}/{entity}/messages"),
        Tool::new("set_next_message_delivery", "What the next message published to this entity does: lose_lock | duplicate | hold | drop | dead_letter | out_of_order | normal. Once (times=1) then normal; times=0 every time.", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let n = config::get(&ctx, &ns)?;
            let h = find_holder(&n, &entity_of(&a)?)?;
            let b = a.str("behavior").or(a.str("behaviour")).unwrap_or("normal".into());
            broker::set_next(&ctx, &ns, &h, &b, a.u64("times").map(|x| x as i64).unwrap_or(1), &if via(&a) == "you" { String::new() } else { by(&a) }).await?;
            Ok(json!({"namespace": ns, "entity": h, "next": broker::next_of(&ctx, &ns, &h).await}))
        })
        .req("namespace", "namespace").req("entity", "entity").req("behavior", "lose_lock | duplicate | hold | drop | dead_letter | out_of_order | normal").param("times", "1 (default): once, then normal; 0: every time")
        .route("PUT", "/messaging/{namespace}/{entity}/next-delivery"),
        Tool::new("resubmit", "Puts dead-lettered messages back on their queue, with a note in their history: one seq, or all_dead_letters.", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let seqs: Vec<i64> = if a.bool("all_dead_letters").unwrap_or(false) || a.get("seq").is_none() {
                let n = config::get(&ctx, &ns)?;
                let h = find_holder(&n, &entity_of(&a)?)?;
                broker::list(&ctx, &ns, &h, Some("dead_letter".into()), None, 1000).await?.iter().filter_map(|m| m["seq"].as_i64()).collect()
            } else {
                vec![a.need("seq")?.trim_start_matches('#').parse().map_err(|_| ApiError::bad("seq: a number"))?]
            };
            let n = broker::resubmit(&ctx, &ns, seqs, &by(&a)).await?;
            Ok(json!({"resubmitted": n}))
        })
        .req("namespace", "namespace").param("entity", "entity (with all_dead_letters)").param("seq", "one message").param("all_dead_letters", "true: every dead-lettered message of the entity")
        .route("POST", "/messaging/{namespace}/{entity}/resubmit"),
        Tool::new("dead_letter_message", "Moves a waiting message to dead-letter by hand (“dead-lettered by you”).", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let seq = a.need("seq")?.trim_start_matches('#').parse().map_err(|_| ApiError::bad("seq: a number"))?;
            broker::dead_letter(&ctx, &ns, seq, &by(&a)).await?;
            Ok(json!({"seq": seq, "state": "dead_letter"}))
        })
        .req("namespace", "namespace").param("entity", "entity").req("seq", "sequence number")
        .route("POST", "/messaging/{namespace}/{entity}/messages/{seq}/dead-letter"),
        Tool::new("delete_queue_message", "Deletes one message and its history.", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let seq = a.need("seq")?.trim_start_matches('#').parse().map_err(|_| ApiError::bad("seq: a number"))?;
            broker::delete(&ctx, &ns, seq).await?;
            Ok(json!({"deleted": seq}))
        })
        .req("namespace", "namespace").param("entity", "entity").req("seq", "sequence number")
        .route("DELETE", "/messaging/{namespace}/{entity}/messages/{seq}"),
        Tool::new("purge", "Removes messages of an entity (a state, or all). People confirm on the screen; agents do not.", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let n = config::get(&ctx, &ns)?;
            let h = find_holder(&n, &entity_of(&a)?)?;
            let gone = broker::purge(&ctx, &ns, &h, wanted(&a)).await?;
            Ok(json!({"purged": gone}))
        })
        .req("namespace", "namespace").req("entity", "entity").param("state", "dead_letter, active… (default all)")
        .route("DELETE", "/messaging/{namespace}/{entity}/messages"),
        Tool::new("list_consumers", "Connections receiving from this entity: client id, link, receive mode, prefetch, locks held, completed today; disconnected ones of the last day.", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let h = a.str("entity").filter(|e| e != "*").unwrap_or_default();
            Ok(json!({"namespace": ns, "entity": h, "consumers": broker::consumers(&ctx, &ns, &h).await?}))
        })
        .req("namespace", "namespace").param("entity", "entity (empty: all)")
        .route("GET", "/messaging/{namespace}/{entity}/consumers"),
        Tool::new("get_consumer", "One consumer: connection, the locks it holds now and its last messages.", |ctx, a| async move {
            let id = a.need("id")?;
            let ns = ns_of(&a)?;
            let all = broker::consumers(&ctx, &ns, "").await?;
            let c = all.into_iter().find(|c| c["id"] == json!(id)).ok_or_else(|| ApiError::not_found(format!("No consumer {id}")))?;
            let client = c["client"].as_str().unwrap_or("").to_string();
            let n = config::get(&ctx, &ns)?;
            Ok(json!({"consumer": c, "locks": broker::consumer_locks(&ctx, &id).await, "last": broker::consumer_last(&ctx, &client).await, "lock": n.limits(c["entity"].as_str().unwrap_or("")).lock_ms / 1000}))
        })
        .req("namespace", "namespace").req("id", "consumer id")
        .route("GET", "/messaging/{namespace}/consumers/{id}"),
        Tool::new("disconnect_consumer", "Forces a consumer off (RabbitMQ: CONNECTION_FORCED; Service Bus: amqp:link:detach-forced). Its locks are released: tests the app's reconnect logic.", |ctx, a| async move {
            let id = a.need("id")?;
            // Taking it out of the map is the signal (Service Bus links look for it); the notify wakes them.
            let k = broker::kills().lock().unwrap().remove(&id);
            let Some(k) = k else { return Err(ApiError::new(409, format!("{id} is not connected"))) };
            k.notify_waiters();
            k.notify_one();
            broker::consumer_close(&ctx, &id, &format!("disconnected by {}", by(&a))).await;
            Ok(json!({"disconnected": id}))
        })
        .req("namespace", "namespace").req("id", "consumer id")
        .route("POST", "/messaging/{namespace}/consumers/{id}/disconnect"),
        Tool::new("expire_consumer_locks", "Expires the locks a consumer holds now: its next complete fails with MessageLockLost and the messages are delivered again.", |ctx, a| async move {
            let id = a.need("id")?;
            let n = broker::expire_locks(&ctx, &id).await;
            Ok(json!({"expired": n}))
        })
        .req("namespace", "namespace").req("id", "consumer id")
        .route("POST", "/messaging/{namespace}/consumers/{id}/expire-locks"),
        Tool::new("set_namespace_reachability", "The whole namespace: ok | refuse (connection refused) | timeout (no answer) | 401 (key rejected) | busy (ServerBusy / connection.blocked).", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let mode = a.need("mode")?;
            if !config::REACH.contains(&mode.as_str()) {
                return Err(ApiError::bad(format!("mode: {}", config::REACH.join(" | "))));
            }
            let n = config::change(&ctx, &ns, |n| {
                n.behaviour.reachability = mode.clone();
                Ok(())
            })?;
            Ok(json!({"namespace": ns, "reachability": n.behaviour.reachability}))
        })
        .req("namespace", "namespace").req("mode", "ok | refuse | timeout | 401 | busy")
        .route("PUT", "/messaging/{namespace}/reachability"),
    ]
}

// ---------------------------------------------------------------- M2 rules (shared S2 names)

fn rule_name(a: &Args) -> Option<String> {
    a.str("rule").or_else(|| a.str("name").filter(|n| n != config::SB && n != config::RABBIT))
}

fn rule_from(a: &Args, base: rules::Rule) -> Result<rules::Rule, ApiError> {
    let mut r = base;
    if let Some(n) = rule_name(a) {
        if r.name.is_empty() {
            r.name = n;
        }
    }
    if let Some(n) = a.str("new_name") {
        r.name = n;
    }
    if let Some(w) = a.get("when") {
        r.when = serde_json::from_value(w.clone()).map_err(|e| ApiError::bad(format!("when: {e}")))?;
    }
    if let Some(t) = a.get("instead").or(a.get("then")) {
        r.then = serde_json::from_value(t.clone()).map_err(|e| ApiError::bad(format!("instead: {e}")))?;
    }
    if let Some(h) = a.get("how_often") {
        r.how_often = match h {
            Value::String(s) if s == "every" || s == "every_time" => rules::HowOften { every: true, ..Default::default() },
            h => serde_json::from_value(h.clone()).map_err(|e| ApiError::bad(format!("how_often: {e}")))?,
        };
    }
    if let Some(e) = a.bool("enabled") {
        r.enabled = e;
    }
    r.validate().map_err(ApiError::bad)?;
    Ok(r)
}

async fn last_deliveries(ctx: &Ctx, service: &str, n: u32) -> Vec<Value> {
    let mut out = vec![];
    for ns in config::list(ctx).into_iter().filter(|x| x.kind == service) {
        out.extend(broker::list(ctx, &ns.namespace, "", None, None, n).await.unwrap_or_default());
    }
    out.sort_by_key(|m| -m["seq"].as_i64().unwrap_or(0));
    out.truncate(n as usize);
    out
}

async fn rules_view(ctx: &Ctx, service: &str) -> Result<Value, ApiError> {
    let f = rules::load(ctx, service)?;
    let hits = rules::hits(ctx, service).await?;
    let last = last_deliveries(ctx, service, 50).await;
    let out: Vec<Value> = f.rules.iter().map(|r| {
        let h = hits.get(&r.name).cloned().unwrap_or((0, None));
        let mut v = json!(r);
        v["meta"] = json!(r.meta());
        v["hitsToday"] = json!(h.0);
        v["lastHit"] = json!(h.1);
        v["warnings"] = rules::test(r, &f.rules, &last, 30)["warnings"].clone();
        v
    }).collect();
    Ok(json!({"service": service, "rules": out, "file": format!("messaging/rules/{service}.yaml"), "yaml": std::fs::read_to_string(rules::file_path(ctx, service)).unwrap_or_default()}))
}

fn rules_tools() -> Vec<Tool> {
    vec![
        Tool::new("messaging_list_rule_options", "", |ctx, a| async move {
            let s = service(&a)?;
            let mut v = rules::options(&s);
            v["namespaces"] = json!(config::list(&ctx).into_iter().filter(|n| n.kind == s).map(|n| json!({"namespace": n.namespace, "entities": n.entities.keys().collect::<Vec<_>>()})).collect::<Vec<_>>());
            Ok(v)
        }),
        Tool::new("messaging_list_rules", "", |ctx, a| async move { rules_view(&ctx, &service(&a)?).await }),
        Tool::new("messaging_test_rule", "", |ctx, a| async move {
            let s = service(&a)?;
            let f = rules::load(&ctx, &s)?;
            let name = rule_name(&a).unwrap_or("draft".into());
            let base = f.rules.iter().find(|r| r.name == name).cloned().unwrap_or(rules::Rule { name: name.clone(), enabled: true, ..Default::default() });
            let r = rule_from(&a, base)?;
            let last = last_deliveries(&ctx, &s, a.u64("last").unwrap_or(50) as u32).await;
            let lock = config::list(&ctx).into_iter().find(|n| n.namespace == r.when.namespace).map(|n| n.limits(&r.when.entity).lock_ms / 1000).unwrap_or(30);
            let mut others = f.rules.clone();
            if !others.iter().any(|x| x.name == r.name) {
                others.push(r.clone());
            }
            Ok(rules::test(&r, &others, &last, lock))
        }),
        Tool::new("messaging_create_rule", "", |ctx, a| async move {
            let s = service(&a)?;
            let mut f = rules::load(&ctx, &s)?;
            let r = rule_from(&a, rules::Rule { enabled: true, ..Default::default() })?;
            if f.rules.iter().any(|x| x.name == r.name) {
                return Err(ApiError::new(409, format!("A rule “{}” exists", r.name)));
            }
            f.rules.push(r.clone());
            rules::save(&ctx, &s, &f)?;
            rules::reset_count(&ctx, &s, &r.name).await?;
            let v = rules_view(&ctx, &s).await?;
            Ok(v["rules"].as_array().and_then(|x| x.iter().find(|y| y["name"] == json!(r.name)).cloned()).unwrap_or(Value::Null))
        }),
        Tool::new("messaging_update_rule", "", |ctx, a| async move {
            let s = service(&a)?;
            let n = rule_name(&a).ok_or_else(|| ApiError::bad("“rule” (name) is required"))?;
            let mut f = rules::load(&ctx, &s)?;
            let i = f.rules.iter().position(|r| r.name == n).ok_or_else(|| ApiError::not_found(format!("No rule “{n}”")))?;
            let r = rule_from(&a, f.rules[i].clone())?;
            f.rules[i] = r.clone();
            rules::save(&ctx, &s, &f)?;
            rules::reset_count(&ctx, &s, &r.name).await?;
            let v = rules_view(&ctx, &s).await?;
            Ok(v["rules"].as_array().and_then(|x| x.iter().find(|y| y["name"] == json!(r.name)).cloned()).unwrap_or(Value::Null))
        }),
        Tool::new("messaging_set_rule_enabled", "", |ctx, a| async move {
            let s = service(&a)?;
            let n = rule_name(&a).ok_or_else(|| ApiError::bad("rule name required"))?;
            let mut f = rules::load(&ctx, &s)?;
            let r = f.rules.iter_mut().find(|r| r.name == n).ok_or_else(|| ApiError::not_found(format!("No rule “{n}”")))?;
            r.enabled = a.bool("enabled").unwrap_or(true);
            rules::save(&ctx, &s, &f)?;
            rules::reset_count(&ctx, &s, &n).await?;
            Ok(json!({"rule": n, "enabled": a.bool("enabled").unwrap_or(true)}))
        }),
        Tool::new("messaging_move_rule", "", |ctx, a| async move {
            let s = service(&a)?;
            let n = rule_name(&a).ok_or_else(|| ApiError::bad("rule name required"))?;
            let mut f = rules::load(&ctx, &s)?;
            let i = f.rules.iter().position(|r| r.name == n).ok_or_else(|| ApiError::not_found(format!("No rule “{n}”")))?;
            let r = f.rules.remove(i);
            let p = (a.u64("position").unwrap_or(1).max(1) as usize - 1).min(f.rules.len());
            f.rules.insert(p, r);
            rules::save(&ctx, &s, &f)?;
            Ok(json!({"order": f.rules.iter().map(|r| r.name.clone()).collect::<Vec<_>>()}))
        }),
        Tool::new("messaging_delete_rule", "", |ctx, a| async move {
            let s = service(&a)?;
            let n = rule_name(&a).ok_or_else(|| ApiError::bad("rule name required"))?;
            let mut f = rules::load(&ctx, &s)?;
            let before = f.rules.len();
            f.rules.retain(|r| r.name != n);
            if f.rules.len() == before {
                return Err(ApiError::not_found(format!("No rule “{n}”")));
            }
            rules::save(&ctx, &s, &f)?;
            Ok(json!({"deleted": n}))
        }),
        Tool::new("messaging_put_rules_yaml", "", |ctx, a| async move {
            let s = service(&a)?;
            let f: rules::File = serde_yaml::from_str(&a.need("yaml")?).map_err(|e| ApiError::bad(format!("YAML: {e}")))?;
            rules::save(&ctx, &s, &f)?;
            rules_view(&ctx, &s).await
        }),
    ]
}

// ---------------------------------------------------------------- M3 namespaces (shared S5 names + extras)

fn patch(n: &mut config::Namespace, p: &Value) -> Result<(), ApiError> {
    let mut cur = serde_json::to_value(&*n).map_err(|e| ApiError::internal(e.to_string()))?;
    fn merge(t: &mut Value, p: &Value) {
        match (t, p) {
            (Value::Object(t), Value::Object(p)) => {
                for (k, v) in p {
                    if v.is_null() {
                        t.remove(k);
                    } else {
                        merge(t.entry(k.clone()).or_insert(Value::Null), v);
                    }
                }
            }
            (t, p) => *t = p.clone(),
        }
    }
    merge(&mut cur, p);
    *n = serde_json::from_value(cur).map_err(|e| ApiError::bad(e.to_string()))?;
    Ok(())
}

/// Over HTTP the path's {name} is the service; the entity comes as "entity" (or "subscription").
fn entity_name(a: &Args, key: &str) -> Result<String, ApiError> {
    a.str(key).or_else(|| a.str("name").filter(|n| n != config::SB && n != config::RABBIT)).ok_or_else(|| ApiError::bad(format!("“{key}” is required")))
}

fn kind_ok(n: &config::Namespace, s: &str) -> Result<(), ApiError> {
    if n.kind != s {
        return Err(ApiError::not_found(format!("{} is a {} namespace", n.namespace, n.kind)));
    }
    Ok(())
}

fn etag(t: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    format!("\"{:016x}\"", h.finish())
}

fn namespaces() -> Vec<Tool> {
    vec![
        Tool::new("messaging_get_service_schema", "", |_ctx, a| async move { Ok(config::schema(&service(&a)?)) }),
        Tool::new("messaging_list_service_accounts", "", |ctx, a| async move {
            let s = service(&a)?;
            let mut out = vec![];
            for n in config::list(&ctx).into_iter().filter(|n| n.kind == s) {
                out.push(ns_view(&ctx, &n).await);
            }
            Ok(json!({"service": s, "accounts": out}))
        }),
        Tool::new("messaging_get_account", "", |ctx, a| async move {
            let s = service(&a)?;
            let n = config::get(&ctx, &ns_of(&a)?)?;
            kind_ok(&n, &s)?;
            let mut v = ns_view(&ctx, &n).await;
            v["etag"] = json!(etag(&config::text(&ctx, &n.namespace)));
            Ok(v)
        }),
        Tool::new("messaging_create_account", "", |ctx, a| async move {
            let s = service(&a)?;
            let name = ns_of(&a)?;
            config::valid_name(&name).map_err(ApiError::bad)?;
            let mut n = config::create(&ctx, &s, &name)?;
            if let Some(p) = a.get("settings") {
                patch(&mut n, p)?;
                config::save(&ctx, &n)?;
            }
            Ok(ns_view(&ctx, &n).await)
        }),
        Tool::new("messaging_update_account", "", |ctx, a| async move {
            let s = service(&a)?;
            let name = ns_of(&a)?;
            let text = config::text(&ctx, &name);
            if let Some(m) = a.str("if_match") {
                if m != etag(&text) {
                    return Err(ApiError::new(412, format!("{} changed since you read it", config::rel_file(&name))).with(json!({"yaml": text, "etag": etag(&text)})));
                }
            }
            let p = a.get("patch").or(a.get("settings")).cloned().ok_or_else(|| ApiError::bad("“patch” is required"))?;
            let n = config::change(&ctx, &name, |n| {
                kind_ok(n, &s)?;
                patch(n, &p)?;
                if n.namespace != name {
                    return Err(ApiError::bad("Renaming a namespace: make a new one (the name is in every connection string)"));
                }
                Ok(())
            })?;
            let mut v = ns_view(&ctx, &n).await;
            v["etag"] = json!(etag(&config::text(&ctx, &n.namespace)));
            Ok(v)
        }),
        Tool::new("messaging_rotate_key", "", |ctx, a| async move {
            let s = service(&a)?;
            let n = config::get(&ctx, &ns_of(&a)?)?;
            kind_ok(&n, &s)?;
            let k = a.str("key").or(a.str("policy")).ok_or_else(|| ApiError::bad("“key” (policy) is required"))?;
            if !n.access.policies.contains_key(&k) {
                return Err(ApiError::not_found(format!("No policy “{k}” in {}", n.namespace)));
            }
            let v = config::new_key(&ctx, &n.namespace, &k)?;
            ctx.live.send("messaging.namespace", &n.namespace, "updated");
            Ok(json!({"key": k, "value": v, "masked": config::mask(&v), "connection": connection(&ctx, &n)}))
        }),
        Tool::new("messaging_delete_account", "", |ctx, a| async move {
            let s = service(&a)?;
            let n = config::get(&ctx, &ns_of(&a)?)?;
            kind_ok(&n, &s)?;
            std::fs::remove_file(config::file(&ctx, &n.namespace)).map_err(|e| ApiError::internal(e.to_string()))?;
            broker::purge(&ctx, &n.namespace, "", None).await?;
            ctx.live.send("messaging.namespace", &n.namespace, "deleted");
            Ok(json!({"deleted": n.namespace}))
        }),
        Tool::new("add_entity", "Adds a queue or a topic to a namespace (or marks one the app created as declared: “Keep it in the YAML”).", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let name = entity_name(&a, "entity")?;
            config::valid_entity(&name).map_err(ApiError::bad)?;
            let kind = a.str("kind").unwrap_or("queue".into());
            let n = config::change(&ctx, &ns, |n| {
                if let Some(e) = n.entities.get_mut(&name) {
                    // Already there: keeping it clears "created by the app".
                    if let Some(q) = e.queue.as_mut() { q.created_by.clear(); }
                    if let Some(t) = e.topic.as_mut() { t.created_by.clear(); }
                    return Ok(());
                }
                let e = if kind == "topic" || kind == "exchange" {
                    config::Entity { queue: None, topic: Some(config::Topic { kind: if n.kind == config::RABBIT { a.str("exchange_type").unwrap_or("topic".into()) } else { String::new() }, ..Default::default() }) }
                } else {
                    config::Entity { queue: Some(config::Queue { lock: a.str("lock").unwrap_or("30s".into()), max_delivery: a.u64("max_delivery").unwrap_or(10) as u32, sessions: a.bool("sessions").unwrap_or(false), ..Default::default() }), topic: None }
                };
                n.entities.insert(name.clone(), e);
                Ok(())
            })?;
            Ok(ns_view(&ctx, &n).await)
        })
        .req("namespace", "namespace").req("entity", "queue or topic name").param("kind", "queue (default) | topic").param("lock", "30s").param("max_delivery", "10").param("sessions", "true | false").param("exchange_type", "RabbitMQ: topic | direct | fanout | headers")
        .route("POST", "/services/{name}/accounts/{account}/entities"),
        Tool::new("remove_entity", "Removes a queue or a topic and its messages.", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let name = entity_name(&a, "entity")?;
            let n = config::change(&ctx, &ns, |n| {
                n.entities.remove(&name).ok_or_else(|| ApiError::not_found(format!("No entity “{name}”")))?;
                for e in n.entities.values_mut() {
                    if let Some(t) = e.topic.as_mut() {
                        t.subscriptions.remove(&name);
                    }
                }
                Ok(())
            })?;
            let gone = broker::purge(&ctx, &ns, &name, None).await?;
            let mut v = ns_view(&ctx, &n).await;
            v["messagesRemoved"] = json!(gone);
            Ok(v)
        })
        .req("namespace", "namespace").req("name", "entity")
        .route("DELETE", "/services/{name}/accounts/{account}/entities/{entity}"),
        Tool::new("add_subscription", "Adds a subscription to a topic (RabbitMQ: binds a queue to the exchange; filter is the binding key).", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let topic = a.need("topic")?;
            let name = entity_name(&a, "subscription")?;
            config::valid_entity(&name).map_err(ApiError::bad)?;
            let filter = a.str("filter").unwrap_or_default();
            let n = config::change(&ctx, &ns, |n| {
                let rabbit = n.kind == config::RABBIT;
                if rabbit && !n.entities.get(&name).map(|e| e.queue.is_some()).unwrap_or(false) {
                    n.entities.insert(name.clone(), config::Entity { queue: Some(config::Queue { lock: "30s".into(), max_delivery: 10, ..Default::default() }), topic: None });
                }
                let t = n.entities.get_mut(&topic).and_then(|e| e.topic.as_mut()).ok_or_else(|| ApiError::not_found(format!("No topic “{topic}”")))?;
                t.subscriptions.insert(name.clone(), config::Subscription { lock: a.str("lock").unwrap_or("30s".into()), max_delivery: a.u64("max_delivery").unwrap_or(10) as u32, filter: filter.clone(), ..Default::default() });
                Ok(())
            })?;
            Ok(ns_view(&ctx, &n).await)
        })
        .req("namespace", "namespace").req("topic", "topic (exchange)").req("subscription", "subscription (RabbitMQ: the queue)").param("filter", "Service Bus: subject = X or key = value; RabbitMQ: binding key").param("lock", "30s").param("max_delivery", "10")
        .route("POST", "/services/{name}/accounts/{account}/topics/{topic}/subscriptions"),
        Tool::new("rotate_policy_key", "Makes a new key for a shared access policy (RabbitMQ: a user's password); senders still using the old one get 401.", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let p = a.need("policy")?;
            let n = config::get(&ctx, &ns)?;
            if !n.access.policies.contains_key(&p) {
                return Err(ApiError::not_found(format!("No policy “{p}”")));
            }
            let v = config::new_key(&ctx, &ns, &p)?;
            ctx.live.send("messaging.namespace", &ns, "updated");
            Ok(json!({"policy": p, "value": v, "masked": config::mask(&v), "connection": connection(&ctx, &n)}))
        })
        .req("namespace", "namespace").req("policy", "policy (user)")
        .route("POST", "/services/{name}/accounts/{account}/policies/{policy}/rotate"),
        Tool::new("set_throughput", "Messages a second the namespace takes before ServerBusy; none, a number, or throttle_now (every send ServerBusy).", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let v = a.str("per_second").or(a.str("throughput")).unwrap_or("none".into());
            let n = config::change(&ctx, &ns, |n| {
                n.behaviour.throughput = v.clone();
                Ok(())
            })?;
            Ok(json!({"namespace": ns, "throughput": n.behaviour.throughput}))
        })
        .req("namespace", "namespace").req("per_second", "none | <n> | throttle_now")
        .route("PUT", "/messaging/{namespace}/throughput"),
        Tool::new("set_auto_create", "On: queues and topics the app declares or sends to are created. Off: they answer 404 (test a missing entity).", |ctx, a| async move {
            let ns = ns_of(&a)?;
            let on = a.bool("on").unwrap_or(true);
            config::change(&ctx, &ns, |n| {
                n.behaviour.auto_create = on;
                Ok(())
            })?;
            Ok(json!({"namespace": ns, "autoCreate": on}))
        })
        .req("namespace", "namespace").req("on", "true | false")
        .route("PUT", "/messaging/{namespace}/auto-create"),
    ]
}
