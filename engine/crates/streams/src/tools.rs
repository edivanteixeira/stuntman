//! The Streams tools (A17): topics, messages (decoded when they carry a registry schema), producing
//! as the partner would, consumer groups with their lag, rebalances, schemas, broker settings.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx, Tool};

use crate::{avro, broker, config, registry, store, wire::Rec};

/// A value as people read it: Confluent-framed Avro decoded with its schema, JSON, text, or hex.
pub async fn show(ctx: &Ctx, hexv: &str) -> Value {
    let b = hex::decode(hexv).unwrap_or_default();
    if b.len() > 5 && b[0] == 0 {
        let id = u32::from_be_bytes([b[1], b[2], b[3], b[4]]) as i64;
        return match registry::by_id(ctx, id).await {
            Some((s, t)) if t == "AVRO" => match serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()).and_then(|sc| avro::decode(&sc, &b[5..])) {
                Ok(v) => json!({"schemaId": id, "format": "avro", "value": v}),
                Err(e) => json!({"schemaId": id, "format": "avro", "error": format!("does not match schema {id}: {e}"), "hex": hexv}),
            },
            Some((_, t)) => json!({"schemaId": id, "format": t.to_lowercase(), "value": serde_json::from_slice::<Value>(&b[5..]).unwrap_or(json!(String::from_utf8_lossy(&b[5..])))}),
            None => json!({"schemaId": id, "error": format!("schema {id} is not in the registry"), "hex": hexv}),
        };
    }
    match serde_json::from_slice::<Value>(&b) {
        Ok(v) => json!({"format": "json", "value": v}),
        Err(_) => match String::from_utf8(b.clone()) { Ok(t) => json!({"format": "text", "value": t}), Err(_) => json!({"format": "binary", "hex": hexv}) },
    }
}

pub fn all() -> Vec<Tool> {
    vec![
        Tool::new("list_topics", "The Kafka topics of this environment with their partitions, messages and high watermarks; how to connect.", |ctx, _a| async move {
            let mut out = vec![];
            for (t, n) in store::topics(&ctx).await? {
                let mut parts = vec![];
                let mut total = 0;
                for p in 0..n {
                    let (s, hw) = store::marks(&ctx, &t, p).await?;
                    total += hw - s;
                    parts.push(json!({"partition": p, "start": s, "highWatermark": hw}));
                }
                out.push(json!({"name": t, "partitions": n, "messages": total, "offsets": parts}));
            }
            let cfg = config::load(&ctx);
            Ok(json!({"topics": out, "bootstrap": format!("{}:{}", cfg.advertised_host, crate::kafka_port()), "schemaRegistry": format!("http://localhost:{}", crate::registry_port()),
                "sasl": {"required": cfg.sasl.required, "mechanisms": cfg.sasl.mechanisms, "users": cfg.sasl.users.iter().map(|u| u.user.clone()).collect::<Vec<_>>()}, "provoke": cfg.provoke,
                "provocations": config::PROVOKE.iter().map(|p| json!({"kind": p.0, "label": p.1})).collect::<Vec<_>>()}))
        })
        .route("GET", "/streams/topics"),
        Tool::new("create_topic", "Creates a topic with N partitions.", |ctx, a| async move {
            let n = a.need("name")?;
            let made = store::create_topic(&ctx, &n, a.u64("partitions").unwrap_or(1) as i32).await?;
            Ok(json!({"name": n, "created": made}))
        })
        .req("name", "shipments.events").param("partitions", "1")
        .route("PUT", "/streams/topics/{name}"),
        Tool::new("delete_topic", "Deletes a topic and its messages.", |ctx, a| async move {
            let n = a.need("name")?;
            store::delete_topic(&ctx, &n).await?;
            Ok(json!({"deleted": n}))
        })
        .req("name", "the topic")
        .route("DELETE", "/streams/topics/{name}"),
        Tool::new("list_stream_messages", "The newest messages of a topic: partition, offset, key, headers and the value decoded (Avro with its registry schema, JSON or text).", |ctx, a| async move {
            let t = a.need("topic")?;
            let rows = store::messages(&ctx, &t, a.u64("partition").map(|x| x as i32), a.u64("limit").unwrap_or(50) as i64).await?;
            let mut out = vec![];
            for mut r in rows {
                let hx = r["_value"].as_str().unwrap_or("").to_string();
                r["value"] = show(&ctx, &hx).await;
                r.as_object_mut().unwrap().remove("_value");
                out.push(r);
            }
            Ok(json!({"topic": t, "messages": out}))
        })
        .req("topic", "the topic").param("partition", "one partition").param("limit", "50")
        .route("GET", "/streams/topics/{topic}/messages"),
        Tool::new("produce", "Produces a message as the partner would: key, value (JSON or text), headers, partition; with subject (or schema_id) the value is Avro-encoded with that registry schema in Confluent framing. bad_schema=true sends one the schema rejects (an unknown schema id).", |ctx, a| async move {
            let t = a.need("topic")?;
            let parts = match store::topic(&ctx, &t).await? { Some(p) => p, None => { store::create_topic(&ctx, &t, config::load(&ctx).default_partitions).await?; config::load(&ctx).default_partitions.max(1) } };
            let key = a.str("key");
            let p = a.u64("partition").map(|x| x as i32).unwrap_or_else(|| key.as_ref().map(|k| (k.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % parts as u32) as i32).unwrap_or(0));
            let v = a.get("value").cloned().unwrap_or(Value::Null);
            let value: Vec<u8> = if a.bool("bad_schema").unwrap_or(false) {
                let mut b = vec![0u8, 0, 1, 0x86, 0x9f];
                b.extend(b"not what the schema says");
                b
            } else if let Some(sid) = match (a.str("subject"), a.u64("schema_id")) { (_, Some(i)) => Some(i as i64), (Some(s), None) => registry::version(&ctx, &s, "latest").await.and_then(|x| x["id"].as_i64()), _ => None } {
                let (sc, ty) = registry::by_id(&ctx, sid).await.ok_or_else(|| ApiError::not_found(format!("No schema {sid}")))?;
                let mut b = vec![0u8];
                b.extend((sid as u32).to_be_bytes());
                if ty == "AVRO" {
                    b.extend(avro::encode(&serde_json::from_str(&sc).map_err(|e| ApiError::bad(e.to_string()))?, &v).map_err(ApiError::bad)?);
                } else {
                    b.extend(v.to_string().into_bytes());
                }
                b
            } else {
                match &v { Value::String(s) => s.clone().into_bytes(), o => o.to_string().into_bytes() }
            };
            let headers: Vec<(String, Option<Vec<u8>>)> = a.get("headers").and_then(|h| h.as_object()).map(|o| o.iter().map(|(k, v)| (k.clone(), Some(v.as_str().map(String::from).unwrap_or(v.to_string()).into_bytes()))).collect()).unwrap_or_default();
            let off = store::append(&ctx, &t, p, vec![Rec { offset: 0, ts: util::now_ms(), key: key.map(|k| k.into_bytes()), value: Some(value), headers }], "you").await?;
            Ok(json!({"topic": t, "partition": p, "offset": off}))
        })
        .req("topic", "the topic").param("key", "the key").param("value", "JSON or text").param("headers", "{name: value}").param("partition", "default: by key").param("subject", "registry subject (Avro)").param("schema_id", "or the schema id").param("bad_schema", "bool")
        .route("POST", "/streams/topics/{topic}/produce"),
        Tool::new("list_consumer_groups", "Consumer groups: state, generation, members (client, host, assigned partitions) and the lag of each partition (committed offset vs high watermark).", |ctx, _a| async move {
            let mut names: Vec<String> = store::groups(&ctx).await?;
            for g in broker::group_names(&ctx) { if !names.contains(&g) { names.push(g) } }
            let mut out = vec![];
            for g in names {
                let live = broker::group_view(&ctx, &g);
                let mut lag = vec![];
                let mut total = 0;
                for (t, p, off, _) in store::committed(&ctx, &g).await? {
                    let (_, hw) = store::marks(&ctx, &t, p).await?;
                    total += (hw - off).max(0);
                    lag.push(json!({"topic": t, "partition": p, "committed": off, "highWatermark": hw, "lag": (hw - off).max(0)}));
                }
                out.push(json!({"group": g, "state": live.as_ref().map(|x| x.state.clone()).unwrap_or("Empty".into()), "generation": live.as_ref().map(|x| x.generation).unwrap_or(0),
                    "members": live.as_ref().map(|x| x.members.values().map(|m| json!({"id": m.id, "client": m.client, "host": m.host, "leader": m.id == x.leader})).collect::<Vec<_>>()).unwrap_or_default(),
                    "lag": total, "partitions": lag}));
            }
            Ok(json!({"groups": out}))
        })
        .route("GET", "/streams/groups"),
        Tool::new("rebalance", "Starts a rebalance of a consumer group: members get REBALANCE_IN_PROGRESS on their next heartbeat and must join again.", |ctx, a| async move {
            let g = a.need("group")?;
            if !broker::rebalance(&ctx, &g) { return Err(ApiError::not_found(format!("No live members in {g}"))) }
            Ok(json!({"group": g, "state": "PreparingRebalance"}))
        })
        .req("group", "the consumer group")
        .route("POST", "/streams/groups/{group}/rebalance"),
        Tool::new("set_streams", "Broker settings: default_partitions, auto_create, advertised_host, sasl_required, sasl_users [{user, password}], and a provocation: leader_not_available, sasl_refused, slow_fetch, duplicate, registry_down (none clears).", |ctx, a| async move {
            let mut c = config::load(&ctx);
            if let Some(x) = a.u64("default_partitions") { c.default_partitions = x as i32; }
            if let Some(x) = a.bool("auto_create") { c.auto_create = x; }
            if let Some(x) = a.str("advertised_host") { c.advertised_host = x; }
            if let Some(x) = a.bool("sasl_required") { c.sasl.required = x; }
            if let Some(u) = a.get("sasl_users") { c.sasl.users = serde_json::from_value(u.clone()).map_err(|e| ApiError::bad(format!("sasl_users: {e}")))?; }
            if let Some(p) = a.str("provoke") {
                if p != "none" && !config::PROVOKE.iter().any(|x| x.0 == p) { return Err(ApiError::bad(format!("provoke: {} | none", config::PROVOKE.iter().map(|x| x.0).collect::<Vec<_>>().join(" | ")))) }
                c.provoke = if p == "none" { String::new() } else { p };
            }
            config::save(&ctx, &c)?;
            ctx.live.send("streams.config", "", "updated");
            Ok(json!({"config": {"defaultPartitions": c.default_partitions, "autoCreate": c.auto_create, "advertisedHost": c.advertised_host, "saslRequired": c.sasl.required, "users": c.sasl.users.iter().map(|u| u.user.clone()).collect::<Vec<_>>(), "provoke": c.provoke}}))
        })
        .param("default_partitions", "1").param("auto_create", "bool").param("advertised_host", "localhost").param("sasl_required", "bool").param("sasl_users", "[{user, password}]").param("provoke", "leader_not_available | sasl_refused | slow_fetch | duplicate | registry_down | none")
        .route("PUT", "/streams/config"),
        Tool::new("list_schemas", "The Schema Registry's subjects with their versions and ids.", |ctx, _a| async move {
            Ok(json!({"subjects": registry::subjects(&ctx).await, "url": format!("http://localhost:{}", crate::registry_port())}))
        })
        .route("GET", "/streams/schemas"),
        Tool::new("register_schema", "Registers a schema under a subject (Avro by default; JSON or PROTOBUF as schema_type). The same schema keeps its id.", |ctx, a| async move {
            let s = a.need("subject")?;
            let sc = match a.get("schema") { Some(Value::String(x)) => x.clone(), Some(v) => v.to_string(), None => return Err(ApiError::bad("schema")) };
            let (id, v) = registry::register(&ctx, &s, &sc, &a.str("schema_type").unwrap_or_default()).await.map_err(ApiError::bad)?;
            Ok(json!({"subject": s, "id": id, "version": v}))
        })
        .req("subject", "shipments.events-value").req("schema", "the schema (JSON)").param("schema_type", "AVRO | JSON | PROTOBUF")
        .route("POST", "/streams/schemas"),
    ]
}

#[allow(dead_code)]
fn _b(x: &[u8]) -> String {
    B64.encode(x)
}
