//! What Pub/Sub delivers by itself: push subscriptions (an HTTP POST per message, wrapped or not,
//! with an OIDC token when asked; 2xx acks, anything else retries with backoff), BigQuery
//! subscriptions (a row per message in a table Stuntman keeps) and Cloud Storage subscriptions
//! (batched text or Avro files, in Stuntman's storage when an account has that bucket). One loop
//! for every environment, woken by the bell.

use std::time::Duration;

use base64::{engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64U}, Engine};
use serde_json::{json, Map, Value};
use stuntman_core::{env::Envs, util, ApiError, Ctx};

use crate::{
    config, schema,
    store::{self, dur_ms, Got, Sub},
    v1::{push_config, cloud_storage_config, Topic},
};

pub fn run(envs: Envs) {
    tokio::spawn(async move {
        loop {
            let bell = crate::bell().notified();
            for ctx in envs.all() {
                if ctx.state.paused() || ctx.state.gone() { continue }
                if let Err(e) = tick(&ctx).await { eprintln!("Pub/Sub delivery in {}: {}", ctx.env, e.message) }
            }
            tokio::select! { _ = bell => {} _ = tokio::time::sleep(Duration::from_millis(250)) => {} }
        }
    });
}

pub async fn tick(ctx: &Ctx) -> Result<(), ApiError> {
    for s in store::subs(ctx, "", None).await? {
        if s.pb.detached { continue }
        match s.kind() {
            "push" => push(ctx, &s).await?,
            "bigquery" => bigquery(ctx, &s).await?,
            "cloud_storage" => storage(ctx, &s).await?,
            _ => {}
        }
    }
    Ok(())
}

fn dup(ctx: &Ctx) -> bool {
    config::load(ctx).provoke == "duplicate"
}

/// RFC 3339 in UTC with milliseconds, as Pub/Sub writes publish times.
pub fn rfc3339(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(ms);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{:03}Z", ms.rem_euclid(1000))
}

fn civil(ms: i64) -> (i64, i64, i64, i64, i64, i64) {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

// ---------------------------------------------------------------- push

/// The push body Pub/Sub sends by default (both spellings of the ids, as it does).
pub fn wrapped(s: &Sub, g: &Got) -> Value {
    let mut m = Map::new();
    if !g.msg.attributes.is_empty() { m.insert("attributes".into(), json!(g.msg.attributes)); }
    if !g.msg.data.is_empty() { m.insert("data".into(), json!(B64.encode(&g.msg.data))); }
    let (id, at) = (g.msg.id.to_string(), rfc3339(g.msg.published));
    m.insert("messageId".into(), json!(id));
    m.insert("message_id".into(), json!(id));
    m.insert("publishTime".into(), json!(at));
    m.insert("publish_time".into(), json!(at));
    if !g.msg.ordering_key.is_empty() { m.insert("orderingKey".into(), json!(g.msg.ordering_key)); }
    let mut v = json!({"message": m, "subscription": s.name});
    if !s.dead_letter.is_empty() { v["deliveryAttempt"] = json!(g.attempt); }
    v
}

/// How long a failed push waits: the retry policy's backoff, else Pub/Sub's push backoff (100 ms
/// doubling to 60 s).
pub fn backoff_ms(s: &Sub, attempt: i32) -> i64 {
    let (lo, hi) = match &s.pb.retry_policy {
        Some(r) => (dur_ms(&r.minimum_backoff).unwrap_or(10_000), dur_ms(&r.maximum_backoff).unwrap_or(600_000)),
        None => (100, 60_000),
    };
    lo.saturating_mul(1 << (attempt - 1).clamp(0, 20)).min(hi)
}

/// A token as Google signs for push: the identity provider of this environment signs it (its JWKS
/// verifies it); with none, an unsigned test token (alg none, iss https://stuntman.invalid/pubsub-test-token).
async fn oidc(ctx: &Ctx, o: &push_config::OidcToken, endpoint: &str) -> String {
    use stuntman_identity::{config as idc, token};
    let now = util::now_ms() / 1000;
    let mut claims = json!({"aud": if o.audience.is_empty() { endpoint } else { &o.audience }, "azp": "stuntman-pubsub-push", "sub": "stuntman-pubsub-push",
        "email": o.service_account_email, "email_verified": true, "iat": now, "exp": now + 3600});
    let provs = idc::list(ctx);
    if let Some(mut p) = provs.iter().find(|p| p.flavour == "google").or(provs.first()).cloned() {
        let ready = { let _g = stuntman_identity::WRITE.lock().await; token::ensure_keys(ctx, &mut p).await.is_ok() };
        if ready {
            claims["iss"] = json!(token::issuer(ctx, &p));
            if let Ok(t) = token::issue(ctx, &p, &claims, token::Twist::default()).await { return t }
        }
    }
    claims["iss"] = json!("https://stuntman.invalid/pubsub-test-token");
    format!("{}.{}.", B64U.encode(r#"{"alg":"none","typ":"JWT"}"#), B64U.encode(claims.to_string()))
}

async fn push(ctx: &Ctx, s: &Sub) -> Result<(), ApiError> {
    let room = 100 - store::outstanding(ctx, &s.name).await?;
    if room <= 0 { return Ok(()) }
    for g in store::pull(ctx, s, room.min(20) as usize, s.ack_deadline).await? {
        let (ctx, s) = (ctx.clone(), s.clone());
        tokio::spawn(async move { push_one(&ctx, &s, g).await });
    }
    Ok(())
}

async fn push_one(ctx: &Ctx, s: &Sub, g: Got) {
    let pc = s.pb.push_config.clone().unwrap_or_default();
    let mut req = ctx.http.post(&pc.push_endpoint).timeout(Duration::from_secs(s.ack_deadline as u64))
        .header("user-agent", "APIs-Google; (+https://developers.google.com/webmasters/APIs-Google.html)");
    match &pc.wrapper {
        Some(push_config::Wrapper::NoWrapper(nw)) => {
            req = req.body(g.msg.data.clone());
            if nw.write_metadata {
                req = req.header("x-goog-pubsub-subscription-name", &s.name).header("x-goog-pubsub-message-id", g.msg.id.to_string())
                    .header("x-goog-pubsub-publish-time", rfc3339(g.msg.published));
                if !s.dead_letter.is_empty() { req = req.header("x-goog-pubsub-delivery-attempt", g.attempt.to_string()) }
                if !g.msg.ordering_key.is_empty() { req = req.header("x-goog-pubsub-ordering-key", &g.msg.ordering_key) }
                for (k, v) in &g.msg.attributes { req = req.header(k.as_str(), v.as_str()) }
            }
        }
        _ => req = req.json(&wrapped(s, &g)),
    }
    if let Some(push_config::AuthenticationMethod::OidcToken(o)) = &pc.authentication_method {
        req = req.bearer_auth(oidc(ctx, o, &pc.push_endpoint).await);
    }
    let status = match req.send().await { Ok(r) => r.status().as_u16(), Err(_) => 0 };
    ctx.live.send("pubsub.push", &s.name, &status.to_string());
    if (200..300).contains(&status) {
        let _ = store::ack(ctx, &s.name, vec![g.ack_id], dup(ctx), false).await;
    } else {
        let _ = store::retry_after(ctx, &s.name, &g.ack_id, backoff_ms(s, g.attempt)).await;
    }
}

// ---------------------------------------------------------------- BigQuery

/// The message as its topic's schema reads it (the revision it was published under).
async fn by_schema(ctx: &Ctx, t: &Topic, g: &Got) -> Result<Value, String> {
    let ss = t.schema_settings.as_ref().filter(|x| !x.schema.is_empty()).ok_or("the topic has no schema")?;
    let revs = store::revs(ctx, &ss.schema).await.map_err(|e| e.message)?;
    let want = g.msg.attributes.get("googclient_schemarevisionid");
    let r = revs.iter().find(|r| Some(&r.revision) == want).or(revs.first()).ok_or("the topic's schema was deleted")?;
    schema::validate(r.kind, &r.definition, if ss.encoding == 0 { schema::JSON } else { ss.encoding }, &g.msg.data)
}

fn data_text(d: &[u8]) -> Value {
    match std::str::from_utf8(d) { Ok(t) => json!(t), Err(_) => json!(B64.encode(d)) }
}

async fn bigquery(ctx: &Ctx, s: &Sub) -> Result<(), ApiError> {
    let got = store::pull(ctx, s, 100, 60).await?;
    if got.is_empty() { return Ok(()) }
    let bq = s.pb.bigquery_config.clone().unwrap_or_default();
    let topic = if bq.use_topic_schema { store::topic(ctx, &s.topic).await? } else { None };
    for g in got {
        let mut row = Map::new();
        if bq.use_topic_schema {
            let r = match &topic { Some(t) => by_schema(ctx, t, &g).await, None => Err("the topic was deleted".to_string()) };
            match r {
                Ok(Value::Object(o)) => row.extend(o),
                Ok(v) => { row.insert("data".into(), v); }
                Err(_) => { store::retry_after(ctx, &s.name, &g.ack_id, backoff_ms(s, g.attempt).max(10_000)).await?; continue }
            }
        } else {
            row.insert("data".into(), data_text(&g.msg.data));
        }
        if bq.write_metadata {
            row.insert("subscription_name".into(), json!(s.name));
            row.insert("message_id".into(), json!(g.msg.id.to_string()));
            row.insert("publish_time".into(), json!(rfc3339(g.msg.published)));
            row.insert("attributes".into(), json!(json!(g.msg.attributes).to_string()));
        }
        store::add_bq_row(ctx, &s.name, &bq.table, &Value::Object(row)).await?;
        store::ack(ctx, &s.name, vec![g.ack_id], dup(ctx), false).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------- Cloud Storage

/// filename_datetime_format: YYYY, MM, DD, hh, mm, ss (default YYYY-MM-DD/hh_mm_ssZ).
fn when(fmt: &str, ms: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(ms);
    let f = if fmt.is_empty() { "YYYY-MM-DD/hh_mm_ssZ" } else { fmt };
    f.replace("YYYY", &format!("{y:04}")).replace("MM", &format!("{mo:02}")).replace("DD", &format!("{d:02}")).replace("hh", &format!("{h:02}"))
        .replace("mm", &format!("{mi:02}")).replace("ss", &format!("{s:02}"))
}

fn zz(w: &mut Vec<u8>, v: i64) {
    let mut u = ((v << 1) ^ (v >> 63)) as u64;
    loop {
        let b = (u & 0x7f) as u8;
        u >>= 7;
        if u == 0 { w.push(b); return }
        w.push(b | 0x80);
    }
}

fn abytes(w: &mut Vec<u8>, b: &[u8]) {
    zz(w, b.len() as i64);
    w.extend(b);
}

/// An Avro object container file of the messages: data, plus subscription_name, message_id,
/// publish_time and attributes with write_metadata.
pub fn avro_file(s: &Sub, got: &[Got], meta: bool) -> Vec<u8> {
    let mut fields = vec![];
    if meta {
        fields.extend([json!({"name": "subscription_name", "type": "string"}), json!({"name": "message_id", "type": "string"}),
            json!({"name": "publish_time", "type": {"type": "long", "logicalType": "timestamp-micros"}}), json!({"name": "attributes", "type": {"type": "map", "values": "string"}})]);
    }
    fields.push(json!({"name": "data", "type": "bytes"}));
    let schema = json!({"type": "record", "name": "PubsubMessage", "fields": fields}).to_string();
    let sync: [u8; 16] = rand::random();
    let mut w = b"Obj\x01".to_vec();
    zz(&mut w, 2);
    abytes(&mut w, b"avro.schema");
    abytes(&mut w, schema.as_bytes());
    abytes(&mut w, b"avro.codec");
    abytes(&mut w, b"null");
    zz(&mut w, 0);
    w.extend(sync);
    let mut block = vec![];
    for g in got {
        if meta {
            abytes(&mut block, s.name.as_bytes());
            abytes(&mut block, g.msg.id.to_string().as_bytes());
            zz(&mut block, g.msg.published * 1000);
            if !g.msg.attributes.is_empty() {
                zz(&mut block, g.msg.attributes.len() as i64);
                for (k, v) in &g.msg.attributes { abytes(&mut block, k.as_bytes()); abytes(&mut block, v.as_bytes()); }
            }
            zz(&mut block, 0);
        }
        abytes(&mut block, &g.msg.data);
    }
    zz(&mut w, got.len() as i64);
    zz(&mut w, block.len() as i64);
    w.extend(block);
    w.extend(sync);
    w
}

async fn storage(ctx: &Ctx, s: &Sub) -> Result<(), ApiError> {
    let c = s.pb.cloud_storage_config.clone().unwrap_or_default();
    let (n, bytes, oldest) = store::backlog(ctx, s).await?;
    if n == 0 { return Ok(()) }
    let max_dur = dur_ms(&c.max_duration).unwrap_or(300_000);
    let due = (c.max_messages > 0 && n >= c.max_messages) || (c.max_bytes > 0 && bytes >= c.max_bytes) || oldest.is_some_and(|o| util::now_ms() - o >= max_dur);
    if !due { return Ok(()) }
    let take = if c.max_messages > 0 { c.max_messages.min(n) } else { n.min(10_000) } as usize;
    let mut got = store::pull(ctx, s, take, 60).await?;
    if c.max_bytes > 0 {
        // A file stops at max_bytes; the rest waits for the next one.
        let (mut sum, mut keep) = (0i64, 0);
        for g in &got { if keep > 0 && sum + g.msg.data.len() as i64 > c.max_bytes { break } sum += g.msg.data.len() as i64; keep += 1; }
        let rest: Vec<(String, i32)> = got.split_off(keep).into_iter().map(|g| (g.ack_id, 0)).collect();
        store::modack(ctx, &s.name, rest, false).await?;
    }
    if got.is_empty() { return Ok(()) }
    let (data, format, ctype) = match &c.output_format {
        Some(cloud_storage_config::OutputFormat::AvroConfig(a)) => (avro_file(s, &got, a.write_metadata), "avro", "application/octet-stream"),
        _ => (got.iter().flat_map(|g| g.msg.data.iter().copied().chain([b'\n'])).collect::<Vec<u8>>(), "text", "text/plain"),
    };
    let object = format!("{}{}_{}{}", c.filename_prefix, when(&c.filename_datetime_format, util::now_ms()), util::uuid(), c.filename_suffix);
    let mut stored = String::new();
    if let Some(a) = stuntman_storage::config::list(ctx).into_iter().find(|a| a.containers.contains_key(&c.bucket)) {
        let w = stuntman_storage::ops::Write { container: c.bucket.clone(), path: object.clone(), data: data.clone(), ctype: ctype.into(), meta: Default::default(), headers: Default::default(),
            by: "pubsub".into(), via: format!("Pub/Sub {}", s.name.rsplit('/').next().unwrap_or("")), etag: None, truncated: false };
        if let Ok(b) = stuntman_storage::ops::write(ctx, &a, w).await {
            stuntman_storage::ops::emit(ctx, &a, &b, true, "PutBlob", "pubsub", None).await;
            stored = format!("{}/{}", a.account, c.bucket);
        }
    }
    store::add_file(ctx, &s.name, &c.bucket, &object, data, got.len() as i64, format, &stored).await?;
    store::ack(ctx, &s.name, got.into_iter().map(|g| g.ack_id).collect(), false, false).await?;
    Ok(())
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn times_and_backoff() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_794_830_405_123), "2026-11-16T12:00:05.123Z");
        assert_eq!(when("", 1_794_830_405_123), "2026-11-16/12_00_05Z");
        let mut s = Sub::default();
        assert_eq!((backoff_ms(&s, 1), backoff_ms(&s, 2), backoff_ms(&s, 30)), (100, 200, 60_000));
        s.pb.retry_policy = Some(crate::v1::RetryPolicy { minimum_backoff: Some(store::dur(1000)), maximum_backoff: Some(store::dur(3000)) });
        assert_eq!((backoff_ms(&s, 1), backoff_ms(&s, 3)), (1000, 3000));
    }
}
