//! The Kinesis tools (A21): streams with their shards, records as people read them, putting a
//! record as another producer would, creating a stream, waiting for the app's record, provocations.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx, Tool};

use crate::{api, config, store::{self, KErr}};

fn e(k: KErr) -> ApiError {
    let st = match k.code { "ResourceNotFoundException" => 404, "ResourceInUseException" => 409, "InternalFailure" => 500, _ => 400 };
    ApiError::new(st, format!("{}: {}", k.code, k.msg))
}

async fn db<T: Send + 'static>(ctx: &Ctx, f: impl FnOnce(&mut rusqlite::Connection) -> Result<T, KErr> + Send + 'static) -> Result<T, ApiError> {
    ctx.db.call(move |cn| Ok(f(cn))).await?.map_err(e)
}

fn provoked(c: &config::Config) -> Value {
    if c.provoke.is_empty() { Value::Null } else { json!({"kind": c.provoke, "times": c.times, "stream": c.stream, "delayMs": c.delay_ms}) }
}

fn endpoint() -> String {
    format!("http://localhost:{}", crate::port())
}

pub fn all() -> Vec<Tool> {
    vec![
        Tool::new("list_kinesis_streams", "The Kinesis streams of this environment: status (CREATING, UPDATING, ACTIVE, DELETING), mode, encryption (KMS key), shard-level metrics, retention, shards (hash key range, sequence range, parent shards, open or closed, records), enhanced fan-out consumers, tags; how to connect; whether the throughput limits are enforced; the provocation.", |ctx, _a| async move {
            let streams = db(&ctx, |cn| store::overview(cn)).await?;
            let c = config::load(&ctx);
            Ok(json!({"streams": streams, "endpoint": endpoint(), "connect": {"AWS_ENDPOINT_URL_KINESIS": endpoint(), "AWS_REGION": "us-east-1", "AWS_ACCESS_KEY_ID": "any", "AWS_SECRET_ACCESS_KEY": "any"},
                "limits": if c.limits { "on" } else { "off" }, "transitionMs": c.transition_ms, "provoke": provoked(&c),
                "provocations": config::PROVOKE.iter().map(|p| json!({"kind": p.0, "label": p.1})).collect::<Vec<_>>()}))
        })
        .route("GET", "/kinesis/streams"),
        Tool::new("create_kinesis_stream", "Creates a stream: shards = N for provisioned mode (hash key space split evenly), or mode = on_demand (4 shards). It is CREATING for transition_ms (1 s), then ACTIVE.", |ctx, a| async move {
            let n = a.need("name")?;
            let mut b = json!({"StreamName": n});
            if a.str("mode").is_some_and(|m| m.eq_ignore_ascii_case("on_demand")) { b["StreamModeDetails"] = json!({"StreamMode": "ON_DEMAND"}) } else { b["ShardCount"] = json!(a.u64("shards").unwrap_or(1)) }
            api::call(&ctx, "CreateStream", "us-east-1", b, "", false).await.map_err(e)?;
            let all = db(&ctx, |cn| store::overview(cn)).await?;
            Ok(all.into_iter().find(|s| s["name"] == json!(n)).unwrap_or(Value::Null))
        })
        .req("name", "shipments").param("shards", "2").param("mode", "provisioned | on_demand")
        .route("PUT", "/kinesis/streams/{name}"),
        Tool::new("list_kinesis_records", "The newest records of a stream (or one shard): shard, sequence number, partition key, data (JSON or text), who put it, when.", |ctx, a| async move {
            let s = a.need("stream")?;
            let (sh, lim, since) = (a.str("shard"), a.u64("limit").unwrap_or(50) as i64, a.u64("since").unwrap_or(0) as i64);
            let s2 = s.clone();
            let rows = db(&ctx, move |cn| store::newest(cn, &s2, sh.as_deref(), since, lim)).await?;
            Ok(json!({"stream": s, "records": rows}))
        })
        .req("stream", "shipments").param("shard", "shardId-000000000000").param("limit", "50").param("since", "ms epoch")
        .route("GET", "/kinesis/streams/{stream}/records"),
        Tool::new("kinesis_put", "Puts a record as another producer would: data (JSON or text), partition_key (MD5 of it picks the shard), explicit_hash_key to choose the shard. The app's consumers read it.", |ctx, a| async move {
            let s = a.need("stream")?;
            let data = match a.get("data") { Some(Value::String(t)) => t.clone().into_bytes(), Some(v) => v.to_string().into_bytes(), None => return Err(ApiError::bad("“data” is required")) };
            let mut b = json!({"StreamName": s, "PartitionKey": a.str("partition_key").unwrap_or_else(|| util::id("pk")), "Data": B64.encode(data)});
            if let Some(h) = a.str("explicit_hash_key") { b["ExplicitHashKey"] = json!(h) }
            let by = a.str("_by").unwrap_or("you".into());
            let r = api::call(&ctx, "PutRecord", "us-east-1", b, &by, false).await.map_err(e)?;
            Ok(json!({"stream": s, "shardId": r["ShardId"], "sequenceNumber": r["SequenceNumber"]}))
        })
        .req("stream", "shipments").req("data", "{\"shipment\": \"SHP-1042\", \"status\": \"DELIVERED\"}").param("partition_key", "SHP-1042").param("explicit_hash_key", "a 128-bit decimal")
        .route("POST", "/kinesis/streams/{stream}/records"),
        Tool::new("wait_for_kinesis_record", "Waits for the first record the app put (not you or an agent) on a stream that matches: text in the data, partition key. Looks back 15 s by default; 408 after the timeout.", |ctx, a| async move {
            let s = a.need("stream")?;
            let since = a.u64("since").map(|x| x as i64).unwrap_or_else(|| stuntman_core::clock::now(&ctx) - 15_000);
            let (contains, pk) = (a.str("contains"), a.str("partition_key"));
            let end = util::now_ms() + a.ms("timeout")?.unwrap_or(30_000) as i64;
            loop {
                let s2 = s.clone();
                let rows = db(&ctx, move |cn| store::newest(cn, &s2, None, since, 1000)).await?;
                if let Some(r) = rows.iter().rev().find(|r| r["producer"] == json!("app")
                    && pk.as_ref().is_none_or(|k| r["partitionKey"].as_str() == Some(k))
                    && contains.as_ref().is_none_or(|c| { let d = &r["data"]["value"]; d.as_str().map(String::from).unwrap_or(d.to_string()).contains(c.as_str()) })) {
                    return Ok(r.clone());
                }
                if util::now_ms() > end {
                    return Err(ApiError::new(408, format!("No matching record on {s} yet")).with(json!({"seen": rows.len()})));
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        })
        .req("stream", "shipments").param("contains", "text in the data").param("partition_key", "SHP-1042").param("since", "ms epoch").param("timeout", "30s")
        .route("GET", "/kinesis/streams/{stream}/wait"),
        Tool::new("set_kinesis", "The Kinesis endpoint's settings and provocations. limits = on | off: the real throughput limits (per provisioned shard 1 MiB/s or 1,000 records/s in, 2 MiB/s and 5 GetRecords/s out; on-demand streams 4 MiB/s or 4,000 records/s in) answer ProvisionedThroughputExceededException; on by default. transition_ms: how long CREATING, UPDATING and DELETING last (1000; 0 = at once). provoke: throughput_puts, throughput_gets (ProvisionedThroughputExceededException), not_found (ResourceNotFoundException), expired_iterator, kms_throttling (KMSThrottlingException), kms_disabled (KMSDisabledException on puts; both KMS ones only on encrypted streams), slow_puts (delay_ms), partial_failures (PutRecords fails every second record); times = for the next N calls (0 = until cleared), stream = only that stream; none clears. reshard splits the stream's widest open shard now.", |ctx, a| async move {
            let mut c = config::load(&ctx);
            if let Some(l) = a.str("limits") {
                c.limits = match l.as_str() { "on" | "true" | "1" => true, "off" | "false" | "0" => false, _ => return Err(ApiError::bad("limits: on | off")) };
            }
            if let Some(t) = a.u64("transition_ms") { c.transition_ms = t.min(600_000) }
            if let Some(p) = a.str("provoke") {
                if p == "reshard" {
                    let s = a.need("stream")?;
                    let ready = util::now_ms() + c.transition_ms as i64;
                    let r = db(&ctx, move |cn| store::reshard(cn, &s, ready)).await?;
                    ctx.live.send("kinesis.streams", "", "updated");
                    return Ok(r);
                }
                if p != "none" && !config::PROVOKE.iter().any(|x| x.0 == p) { return Err(ApiError::bad(format!("provoke: {} | none", config::PROVOKE.iter().map(|x| x.0).collect::<Vec<_>>().join(" | ")))) }
                c.clear();
                if p != "none" {
                    c.provoke = p;
                    c.times = a.u64("times").unwrap_or(0) as u32;
                    c.stream = a.str("stream").unwrap_or_default();
                }
                if let Some(d) = a.u64("delay_ms") { c.delay_ms = d }
            } else if a.get("limits").is_none() && a.get("transition_ms").is_none() {
                return Err(ApiError::bad("give provoke, limits or transition_ms"));
            }
            config::save(&ctx, &c)?;
            ctx.live.send("kinesis.config", "", "updated");
            Ok(json!({"provoke": provoked(&c), "limits": if c.limits { "on" } else { "off" }, "transitionMs": c.transition_ms}))
        })
        .param("provoke", "throughput_puts | throughput_gets | not_found | expired_iterator | kms_throttling | kms_disabled | slow_puts | partial_failures | reshard | none").param("times", "0 = until cleared").param("stream", "only this stream").param("delay_ms", "3000")
        .param("limits", "on | off").param("transition_ms", "1000")
        .route("PUT", "/kinesis/config"),
    ]
}
