//! The Kinesis Data Streams endpoint (AWS_ENDPOINT_URL_KINESIS=http://localhost:4567): AWS JSON
//! 1.1 (X-Amz-Target: Kinesis_20131202.<Operation>) and the CBOR the Java SDK sends, SigV4 accepted
//! with any key (the region comes from its credential scope), errors as the SDKs parse them, and
//! SubscribeToShard as an event stream over HTTP/1.1 or HTTP/2. One port for every environment.

use std::time::Duration;

use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use serde_json::{json, Value};
use stuntman_core::{clock, env::Envs, netserve, Ctx};

use crate::{cbor, config, limits::Limiter, store::{self, kerr, KErr}};

const JSON: &str = "application/x-amz-json-1.1";
const CBOR: &str = "application/x-amz-cbor-1.1";

fn status(code: &str) -> StatusCode {
    match code {
        "InternalFailure" => StatusCode::INTERNAL_SERVER_ERROR,
        "ServiceUnavailable" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_REQUEST,
    }
}

fn encode(v: &Value, cbor_ct: bool) -> Vec<u8> {
    if cbor_ct { cbor::encode(v) } else { serde_json::to_vec(v).unwrap() }
}

fn answer(r: Result<Value, KErr>, cbor_ct: bool) -> Response {
    let ct = if cbor_ct { CBOR } else { JSON };
    match r {
        Ok(v) => (StatusCode::OK, [("content-type", ct)], encode(&v, cbor_ct)).into_response(),
        Err(e) => {
            let b = json!({"__type": e.code, "message": e.msg});
            (status(e.code), [("content-type", ct), ("x-amzn-errortype", e.code)], encode(&b, cbor_ct)).into_response()
        }
    }
}

/// us-east-1 unless the SigV4 credential scope says otherwise (any key is accepted).
fn region(h: &HeaderMap) -> String {
    h.get("authorization").and_then(|v| v.to_str().ok()).and_then(|a| a.split("Credential=").nth(1)).and_then(|c| c.split('/').nth(2)).unwrap_or("us-east-1").to_string()
}

fn ops() -> &'static [&'static str] {
    &["CreateStream", "DeleteStream", "DescribeStream", "DescribeStreamSummary", "ListStreams", "ListShards", "PutRecord", "PutRecords", "GetShardIterator", "GetRecords",
      "MergeShards", "SplitShard", "UpdateShardCount", "IncreaseStreamRetentionPeriod", "DecreaseStreamRetentionPeriod", "AddTagsToStream", "RemoveTagsFromStream",
      "ListTagsForStream", "TagResource", "UntagResource", "ListTagsForResource", "RegisterStreamConsumer", "ListStreamConsumers", "DescribeStreamConsumer",
      "DeregisterStreamConsumer", "SubscribeToShard", "DescribeLimits", "UpdateStreamMode", "StartStreamEncryption", "StopStreamEncryption",
      "EnableEnhancedMonitoring", "DisableEnhancedMonitoring"]
}

fn mutates(op: &str) -> Option<&'static str> {
    match op {
        "PutRecord" | "PutRecords" => Some("kinesis.records"),
        "CreateStream" | "DeleteStream" | "MergeShards" | "SplitShard" | "UpdateShardCount" | "IncreaseStreamRetentionPeriod" | "DecreaseStreamRetentionPeriod" | "AddTagsToStream"
        | "RemoveTagsFromStream" | "TagResource" | "UntagResource" | "RegisterStreamConsumer" | "DeregisterStreamConsumer" | "UpdateStreamMode" | "StartStreamEncryption"
        | "StopStreamEncryption" | "EnableEnhancedMonitoring" | "DisableEnhancedMonitoring" => Some("kinesis.streams"),
        _ => None,
    }
}

/// One operation. `provoke`: the app's calls meet the provoked failure; tools do not.
/// The real limits apply to the app's calls only (provoke), unless set_kinesis limits=off.
pub async fn call(ctx: &Ctx, op: &str, region: &str, b: Value, producer: &str, provoke: bool) -> Result<Value, KErr> {
    let stream = store::named(op, &b).unwrap_or_default();
    let cfg = config::load(ctx);
    let mut partial = false;
    if provoke {
        let put = op == "PutRecord" || op == "PutRecords";
        let named = !stream.is_empty() && op != "CreateStream" && op != "ListStreams";
        // KMS errors only reach encrypted streams.
        let kms = cfg.provoke.starts_with("kms") && (put || op == "GetRecords") && {
            let s2 = stream.clone();
            ctx.db.call(move |cn| Ok(store::encrypted(cn, &s2))).await.unwrap_or(false)
        };
        let mut kinds = vec![];
        if named { kinds.push("not_found") }
        if put { kinds.extend(["throughput_puts", "slow_puts"]) }
        if put && kms { kinds.extend(["kms_throttling", "kms_disabled"]) }
        if op == "PutRecords" { kinds.push("partial_failures") }
        if op == "GetRecords" { kinds.extend(["throughput_gets", "expired_iterator"]) }
        if op == "GetRecords" && kms { kinds.push("kms_throttling") }
        if let Some(c) = config::take(ctx, &stream, &kinds) {
            match c.provoke.as_str() {
                "not_found" => return Err(kerr("ResourceNotFoundException", format!("Stream {stream} under account {} not found.", store::ACCOUNT))),
                "throughput_puts" | "throughput_gets" => return Err(kerr("ProvisionedThroughputExceededException", format!("Rate exceeded for stream {stream} under account {}.", store::ACCOUNT))),
                "expired_iterator" => return Err(kerr("ExpiredIteratorException", "Iterator expired. The iterator was created more than 300000 milliseconds ago.")),
                "kms_throttling" => return Err(kerr("KMSThrottlingException", "The request was denied due to request throttling by AWS KMS. Retry the request.")),
                "kms_disabled" => return Err(kerr("KMSDisabledException", format!("The KMS key of stream {stream} under account {} is disabled.", store::ACCOUNT))),
                "slow_puts" => tokio::time::sleep(Duration::from_millis(c.delay_ms)).await,
                "partial_failures" => partial = true,
                _ => {}
            }
        }
    }
    let now = clock::now(ctx);
    let wall = stuntman_core::util::now_ms();
    let ready = wall + cfg.transition_ms as i64;
    let lim = Limiter { env: ctx.env.to_string(), on: provoke && cfg.limits, now: wall };
    let (o, r, p, st) = (op.to_string(), region.to_string(), producer.to_string(), stream.clone());
    let res = ctx.db.call(move |cn| Ok((|| {
        if !st.is_empty() && o != "CreateStream" { store::gate(cn, &o, &st)? }
        let v = match o.as_str() {
        "CreateStream" => store::create(cn, &b, &r, now, ready),
        "DeleteStream" => store::delete(cn, &b, ready),
        "DescribeStream" => store::describe(cn, &b, false),
        "DescribeStreamSummary" => store::describe(cn, &b, true),
        "ListStreams" => store::list_streams(cn, &b),
        "ListShards" => store::list_shards(cn, &b, now),
        "PutRecord" => store::put_record(cn, &b, now, &p, &lim),
        "PutRecords" => store::put_records(cn, &b, now, &p, |i| partial && i % 2 == 1, &lim),
        "GetShardIterator" => store::get_shard_iterator(cn, &b, now),
        "GetRecords" => store::get_records(cn, &b, now, &lim),
        "SplitShard" => store::split(cn, &b),
        "MergeShards" => store::merge(cn, &b),
        "UpdateShardCount" => store::update_shard_count(cn, &b),
        "IncreaseStreamRetentionPeriod" => store::retention(cn, &b, true),
        "DecreaseStreamRetentionPeriod" => store::retention(cn, &b, false),
        "AddTagsToStream" | "RemoveTagsFromStream" | "ListTagsForStream" | "TagResource" | "UntagResource" | "ListTagsForResource" => store::tags(cn, &o, &b),
        "RegisterStreamConsumer" | "ListStreamConsumers" | "DescribeStreamConsumer" | "DeregisterStreamConsumer" => store::consumers(cn, &o, &b, now),
        "DescribeLimits" => store::limits(cn),
        "UpdateStreamMode" => store::update_mode(cn, &b),
        "StartStreamEncryption" => store::encryption(cn, &b, true),
        "StopStreamEncryption" => store::encryption(cn, &b, false),
        "EnableEnhancedMonitoring" => store::monitoring(cn, &b, true),
        "DisableEnhancedMonitoring" => store::monitoring(cn, &b, false),
        _ => Err(kerr("UnknownOperationException", format!("Kinesis_20131202.{o} is not an operation Stuntman knows"))),
        }?;
        // Resharding, mode and encryption changes: UPDATING for a while, as Kinesis does.
        if matches!(o.as_str(), "SplitShard" | "MergeShards" | "UpdateShardCount" | "UpdateStreamMode" | "StartStreamEncryption" | "StopStreamEncryption") {
            store::transition(cn, &st, "UPDATING", ready)?;
        }
        Ok(v)
    })())).await.map_err(|e| kerr("InternalFailure", e.message))?;
    if res.is_ok() {
        if let Some(t) = mutates(op) { ctx.live.send(t, &stream, "updated"); }
    }
    res
}

async fn handle(State(envs): State<Envs>, mut req: Request) -> Response {
    let ctx = match netserve::route(&envs, &mut req) { Ok(r) => r.ctx, Err(e) => return e };
    let h = req.headers().clone();
    let cbor_ct = h.get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|c| c.contains("cbor"));
    let target = h.get("x-amz-target").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let Some(op) = target.strip_prefix("Kinesis_20131202.").filter(|o| ops().contains(o)) else {
        return answer(Err(kerr("UnknownOperationException", format!("Unknown X-Amz-Target “{target}”: Stuntman's Kinesis speaks Kinesis_20131202.*"))), cbor_ct);
    };
    if ctx.state.paused() {
        return answer(Err(kerr("ServiceUnavailable", format!("{} is paused in Stuntman", ctx.env))), cbor_ct);
    }
    let raw: Bytes = match axum::body::to_bytes(req.into_body(), 12 << 20).await { Ok(b) => b, Err(_) => return answer(Err(kerr("SerializationException", "body too large")), cbor_ct) };
    let body = if raw.is_empty() { Ok(json!({})) } else if cbor_ct { cbor::decode(&raw) } else { serde_json::from_slice::<Value>(&raw).map_err(|e| e.to_string()) };
    let body = match body { Ok(b) => b, Err(e) => return answer(Err(kerr("SerializationException", e)), cbor_ct) };
    let ua = h.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("app").split(' ').next().unwrap_or("app").to_string();
    ctx.live.seen(&ua, "Kinesis");
    if op == "SubscribeToShard" {
        return subscribe(ctx, body, cbor_ct).await;
    }
    answer(call(&ctx, op, &region(&h), body, "app", true).await, cbor_ct)
}

// ---------------------------------------------------------------- SubscribeToShard (enhanced fan-out)

/// One event stream message: prelude, string headers, payload, CRCs.
pub fn event(kind: &str, payload: &[u8], ct: &str) -> Vec<u8> {
    let mut hd = vec![];
    for (k, v) in [(":message-type", "event"), (":event-type", kind), (":content-type", ct)] {
        hd.push(k.len() as u8);
        hd.extend(k.as_bytes());
        hd.push(7);
        hd.extend((v.len() as u16).to_be_bytes());
        hd.extend(v.as_bytes());
    }
    let total = 12 + hd.len() + payload.len() + 4;
    let mut m = Vec::with_capacity(total);
    m.extend((total as u32).to_be_bytes());
    m.extend((hd.len() as u32).to_be_bytes());
    m.extend(crc32fast::hash(&m).to_be_bytes());
    m.extend(hd);
    m.extend(payload);
    m.extend(crc32fast::hash(&m).to_be_bytes());
    m
}

async fn subscribe(ctx: Ctx, b: Value, cbor_ct: bool) -> Response {
    let b2 = b.clone();
    let start = ctx.db.call(move |cn| Ok((|| {
        let (s, _, _, _) = store::consumer(cn, &b2)?;
        store::gate(cn, "SubscribeToShard", &s.name)?;
        let sh = s.shard(b2["ShardId"].as_str().unwrap_or(""))?.clone();
        let p = &b2["StartingPosition"];
        let from = store::start_at(cn, &s, &sh, p["Type"].as_str().unwrap_or(""), p["SequenceNumber"].as_str(), p["Timestamp"].as_f64())?;
        Ok::<_, KErr>((s.name.clone(), sh.id, from))
    })())).await;
    let (stream, shard, from) = match start { Ok(Ok(x)) => x, Ok(Err(e)) => return answer(Err(e), cbor_ct), Err(e) => return answer(Err(kerr("InternalFailure", e.message)), cbor_ct) };
    let ct = if cbor_ct { CBOR } else { JSON };
    // A subscription lasts 5 minutes; an event at least every 5 s, at once when records arrive.
    let end = tokio::time::Instant::now() + Duration::from_secs(300);
    let st = futures::stream::unfold((ctx, from, 0u8, tokio::time::Instant::now()), move |(ctx, from, phase, last)| {
        let (stream, shard) = (stream.clone(), shard.clone());
        async move {
            if phase == 0 {
                return Some((Ok::<_, std::io::Error>(Bytes::from(event("initial-response", &encode(&json!({}), cbor_ct), ct))), (ctx, from, 1, tokio::time::Instant::now() - Duration::from_secs(10))));
            }
            if phase == 2 || tokio::time::Instant::now() > end || ctx.state.gone() { return None }
            loop {
                let now = clock::now(&ctx);
                let (s2, sh2) = (stream.clone(), shard.clone());
                let r = ctx.db.call(move |cn| Ok((|| { let s = store::load(cn, &s2)?; let sh = s.shard(&sh2)?.clone(); store::read(cn, &s, &sh, from, 10_000, now) })())).await;
                let batch = match r { Ok(Ok(b)) => b, _ => return None };
                let idle = tokio::time::Instant::now().duration_since(last) >= Duration::from_secs(5);
                if !batch.records.is_empty() || batch.next.is_none() || idle {
                    let cont = batch.last.unwrap_or(from - 1);
                    let mut ev = json!({"Records": batch.records, "ContinuationSequenceNumber": store::seq_str(cont), "MillisBehindLatest": batch.behind});
                    if !batch.children.is_empty() { ev["ChildShards"] = json!(batch.children); }
                    let next = batch.next.unwrap_or(from);
                    let phase = if batch.next.is_none() { 2 } else { 1 };
                    return Some((Ok(Bytes::from(event("SubscribeToShardEvent", &encode(&ev, cbor_ct), ct))), (ctx, next, phase, tokio::time::Instant::now())));
                }
                if tokio::time::Instant::now() > end { return None }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    });
    Response::builder().status(200).header("content-type", "application/vnd.amazon.eventstream").header("x-amzn-requestid", stuntman_core::util::uuid()).body(Body::from_stream(st)).unwrap()
}

pub fn serve(envs: Envs, port: u16) {
    tokio::spawn(async move {
        let Ok(l) = tokio::net::TcpListener::bind(("0.0.0.0", port)).await else {
            eprintln!("Kinesis: port {port} is taken");
            return;
        };
        let app = Router::new().fallback(handle).with_state(envs);
        let _ = axum::serve(l, app).await;
    });
}

#[cfg(test)]
mod t {
    #[test]
    fn event_stream_frame() {
        let m = super::event("SubscribeToShardEvent", b"{}", "application/x-amz-json-1.1");
        let total = u32::from_be_bytes(m[0..4].try_into().unwrap()) as usize;
        assert_eq!(total, m.len());
        assert_eq!(u32::from_be_bytes(m[8..12].try_into().unwrap()), crc32fast::hash(&m[..8]));
        assert_eq!(u32::from_be_bytes(m[total - 4..].try_into().unwrap()), crc32fast::hash(&m[..total - 4]));
    }
}
