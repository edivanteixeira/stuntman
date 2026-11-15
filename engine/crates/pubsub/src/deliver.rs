//! What Pub/Sub delivers by itself: push subscriptions (an HTTP POST per message, wrapped or not,
//! with an OIDC token when asked; 2xx acks, anything else retries with backoff), BigQuery
//! subscriptions (a row per message in a table Stuntman keeps) and Cloud Storage subscriptions
//! (batched text or Avro files, in Stuntman's storage when an account has that bucket), Bigtable
//! subscriptions (a row per message in a table Stuntman keeps); subscription message transforms;
//! idle subscriptions expiring on the environment clock; topics ingesting from their source. One
//! loop for every environment, woken by the bell.

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
    let now = stuntman_core::clock::now(ctx);
    for s in store::subs(ctx, "", None).await? {
        // Expiration policy: idle (no pull, ack or successful delivery) longer than the ttl.
        if let Some(ttl) = s.ttl_ms() {
            if now - store::last_seen(ctx, &s.name).await? > ttl {
                store::delete_sub(ctx, &s.name).await?;
                continue;
            }
        }
        if s.pb.detached { continue }
        match s.kind() {
            "push" => push(ctx, &s).await?,
            "bigquery" => bigquery(ctx, &s).await?,
            "cloud_storage" => storage(ctx, &s).await?,
            "bigtable" => bigtable(ctx, &s).await?,
            _ => {}
        }
    }
    for t in store::topics(ctx, "").await? {
        if t.ingestion_data_source_settings.is_some() {
            if let Err(e) = crate::ingest::poll(ctx, &t).await { eprintln!("Pub/Sub ingestion into {}: {}", t.name, e.message) }
        }
    }
    Ok(())
}

/// Messages for a subscriber: pulled, then through the subscription's JavaScript transforms (null:
/// acked without delivery; an error: nacked, back after the backoff). Marks the subscription active.
pub async fn take(ctx: &Ctx, s: &Sub, max: usize, secs: i32, max_bytes: i64) -> Result<Vec<Got>, ApiError> {
    let got = store::pull_bytes(ctx, s, max, secs, max_bytes).await?;
    let steps = crate::udf::steps(&s.pb.message_transforms);
    if steps.is_empty() || got.is_empty() { return Ok(got) }
    let mut out = vec![];
    for mut g in got {
        let meta = (g.msg.id.to_string(), rfc3339(g.msg.published), g.msg.ordering_key.clone());
        match crate::udf::apply(ctx, &steps, g.msg.data.clone(), g.msg.attributes.clone(), meta).await {
            Ok(crate::udf::Out::Keep(d, a)) => { g.msg.data = d; g.msg.attributes = a; out.push(g) }
            Ok(crate::udf::Out::Drop) => { store::ack(ctx, &s.name, vec![g.ack_id], false, false).await?; }
            Err(e) => {
                // Retried by the retry policy; past max delivery attempts the original message goes to the
                // dead-letter topic with CloudPubSubDeadLetterSourceSMTErrorMessage.
                store::set_error(ctx, &s.name, &format!("{SMT_ERROR}{}: {e}", g.msg.id)).await?;
                // A pull subscription's retry policy delays it from the deadline on (store::pull_bytes).
                let wait = if s.kind() == "pull" && s.pb.retry_policy.is_some() { 0 } else { backoff_ms(s, g.attempt).max(100) };
                store::retry_after(ctx, &s.name, &g.ack_id, wait).await?;
            }
        }
    }
    Ok(out)
}

/// How a subscription transform's failure reads in lastError (and what dead-lettering looks for).
pub const SMT_ERROR: &str = "message transform failed on ";

async fn failed(ctx: &Ctx, s: &Sub, g: &Got, e: &str) -> Result<(), ApiError> {
    store::set_error(ctx, &s.name, e).await?;
    store::retry_after(ctx, &s.name, &g.ack_id, backoff_ms(s, g.attempt)).await
}

async fn written(ctx: &Ctx, s: &Sub, ids: Vec<String>) -> Result<(), ApiError> {
    store::ack(ctx, &s.name, ids, dup(ctx), false).await?;
    store::seen(ctx, &s.name).await?;
    store::set_error(ctx, &s.name, "").await
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
    for g in take(ctx, s, room.min(20) as usize, s.ack_deadline, 0).await? {
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
        let _ = written(ctx, s, vec![g.ack_id]).await;
    } else {
        let _ = failed(ctx, s, &g, &format!("push to {} answered {}", pc.push_endpoint, if status == 0 { "nothing".to_string() } else { status.to_string() })).await;
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

/// A row against a table schema ([{name, type, mode}]): extra fields dropped (drop_unknown_fields) or
/// refused, required columns present, each value of its column's type.
pub fn fit_row(row: &mut Map<String, Value>, cols: &[Value], drop_unknown: bool) -> Result<(), String> {
    let col = |n: &str| cols.iter().find(|c| c["name"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(n)));
    let extra: Vec<String> = row.keys().filter(|k| col(k).is_none()).cloned().collect();
    for k in extra {
        if drop_unknown { row.remove(&k); } else { return Err(format!("field {k} is not a column of the table (set drop_unknown_fields to drop it)")) }
    }
    for c in cols {
        let n = c["name"].as_str().unwrap_or("");
        let mode = c["mode"].as_str().unwrap_or("NULLABLE").to_uppercase();
        let v = row.get(n).cloned().unwrap_or(Value::Null);
        if v.is_null() {
            if mode == "REQUIRED" { return Err(format!("required column {n} is missing")) }
            continue;
        }
        let vals = if mode == "REPEATED" { v.as_array().cloned().ok_or_else(|| format!("column {n} is REPEATED: expected an array"))? } else { vec![v] };
        let ty = c["type"].as_str().unwrap_or("STRING").to_uppercase();
        for x in &vals {
            let ok = match ty.as_str() {
                "STRING" | "BYTES" | "GEOGRAPHY" => x.is_string(),
                "INTEGER" | "INT64" => x.as_i64().is_some() || x.as_str().is_some_and(|t| t.parse::<i64>().is_ok()),
                "FLOAT" | "FLOAT64" | "NUMERIC" | "BIGNUMERIC" => x.is_number() || x.as_str().is_some_and(|t| t.parse::<f64>().is_ok()),
                "BOOLEAN" | "BOOL" => x.is_boolean(),
                "TIMESTAMP" | "DATE" | "DATETIME" | "TIME" => x.is_string() || x.is_number(),
                "RECORD" | "STRUCT" => x.is_object(),
                _ => true,
            };
            if !ok { return Err(format!("column {n} is {ty}: {x} does not fit")) }
        }
    }
    Ok(())
}

async fn bigquery(ctx: &Ctx, s: &Sub) -> Result<(), ApiError> {
    let got = take(ctx, s, 100, 60, 0).await?;
    if got.is_empty() { return Ok(()) }
    let bq = s.pb.bigquery_config.clone().unwrap_or_default();
    let topic = if bq.use_topic_schema { store::topic(ctx, &s.topic).await? } else { None };
    let table = store::bq_table(ctx, &bq.table).await?;
    let cols = table.as_ref().and_then(|v| v.as_array().cloned());
    for g in got {
        let mut row = Map::new();
        let fields: Result<Map<String, Value>, String> = if bq.use_topic_schema {
            match &topic { Some(t) => by_schema(ctx, t, &g).await.map(|v| match v { Value::Object(o) => o, v => Map::from_iter([("data".to_string(), v)]) }), None => Err("the topic was deleted".to_string()) }
        } else if bq.use_table_schema {
            if cols.is_none() { Err(format!("use_table_schema: table {} has no schema (set_pubsub_bigquery_table)", bq.table)) }
            else { serde_json::from_slice::<Value>(&g.msg.data).ok().and_then(|v| v.as_object().cloned()).ok_or_else(|| "use_table_schema: the data is not a JSON object".to_string()) }
        } else {
            Ok(Map::from_iter([("data".to_string(), data_text(&g.msg.data))]))
        };
        let mut fields = match fields { Ok(f) => f, Err(e) => { failed(ctx, s, &g, &e).await?; continue } };
        row.append(&mut fields);
        if bq.write_metadata {
            row.insert("subscription_name".into(), json!(s.name));
            row.insert("message_id".into(), json!(g.msg.id.to_string()));
            row.insert("publish_time".into(), json!(rfc3339(g.msg.published)));
            row.insert("attributes".into(), json!(json!(g.msg.attributes).to_string()));
        }
        if let Some(c) = &cols {
            if let Err(e) = fit_row(&mut row, c, bq.drop_unknown_fields && (bq.use_table_schema || bq.use_topic_schema)) {
                failed(ctx, s, &g, &format!("BigQuery {}: {e}", bq.table)).await?;
                continue;
            }
        }
        store::add_bq_row(ctx, &s.name, &bq.table, &Value::Object(row)).await?;
        written(ctx, s, vec![g.ack_id]).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------- Bigtable

/// Pub/Sub's row key: subscription#salt#message_id (the salt spreads keys; here two hex digits of
/// the message id's hash), or the delimited key from JSON fields of the message.
async fn bigtable(ctx: &Ctx, s: &Sub) -> Result<(), ApiError> {
    use crate::v1::bigtable_config::{column_family_mapping::KeyDefinition, MessageMapping};
    let got = take(ctx, s, 100, 60, 0).await?;
    let bt = s.pb.bigtable_config.clone().unwrap_or_default();
    for g in got {
        let id = g.msg.id.to_string();
        let key = match &bt.message_mapping {
            Some(MessageMapping::ColumnFamilyMapping(m)) => match &m.key_definition {
                Some(KeyDefinition::DelimitedKey(k)) => {
                    let v: Value = serde_json::from_slice(&g.msg.data).unwrap_or(Value::Null);
                    let parts: Option<Vec<String>> = k.key_fields.iter().map(|f| v.get(f).map(|x| x.as_str().map(String::from).unwrap_or(x.to_string()))).collect();
                    match parts {
                        Some(p) => p.join(&String::from_utf8_lossy(&k.delimiter)),
                        // A key field missing: the message stays in the backlog.
                        None => { failed(ctx, s, &g, &format!("row key fields {:?} are not all in message {id}", k.key_fields)).await?; continue }
                    }
                }
                Some(KeyDefinition::RowKeySchema(_)) => {
                    let schema = store::bq_table(ctx, &format!("bigtable:{}", bt.table)).await?;
                    let v: Value = serde_json::from_slice(&g.msg.data).unwrap_or(Value::Null);
                    match schema.ok_or_else(|| format!("table {} has no row key schema (set_pubsub_bigtable_table)", bt.table)).and_then(|sc| row_key(&sc, &v)) {
                        Ok(k) => show_key(&k),
                        Err(e) => { failed(ctx, s, &g, &format!("row key of message {id}: {e}")).await?; continue }
                    }
                }
                None => format!("{}#{:02x}#{id}", s.name, salt(&id)),
            },
            None => format!("{}#{:02x}#{id}", s.name, salt(&id)),
        };
        let mut cells = json!({"data": {"": data_text(&g.msg.data)}});
        if bt.write_metadata {
            cells["pubsub_metadata"] = json!({"subscription_name": s.name, "message_id": id, "publish_time": rfc3339(g.msg.published),
                "attributes": json!(g.msg.attributes).to_string(), "ordering_key": g.msg.ordering_key});
        }
        store::add_bt_row(ctx, &s.name, &bt.table, &key, &cells).await?;
        written(ctx, s, vec![g.ack_id]).await?;
    }
    Ok(())
}

/// A row key by the table's row key schema: {fields: [{name, type: STRING|INT64|BYTES}], encoding:
/// singleton | delimited_bytes (delimiter) | ordered_code_bytes}. STRING is UTF-8, BYTES base64 (or
/// the text), INT64 8 bytes big-endian (sign bit flipped under ordered code, so keys sort).
pub fn row_key(schema: &Value, msg: &Value) -> Result<Vec<u8>, String> {
    let fields = schema["fields"].as_array().ok_or("the row key schema has no fields")?;
    let enc = schema["encoding"].as_str().unwrap_or("delimited_bytes");
    let ordered = enc == "ordered_code_bytes";
    let mut parts = vec![];
    for f in fields {
        let n = f["name"].as_str().unwrap_or("");
        let v = msg.get(n).filter(|v| !v.is_null()).ok_or_else(|| format!("the message has no field {n}"))?;
        let b = match f["type"].as_str().unwrap_or("STRING").to_uppercase().as_str() {
            "INT64" => {
                let i = v.as_i64().or_else(|| v.as_str().and_then(|x| x.parse().ok())).ok_or_else(|| format!("{n} is INT64: {v} is not"))?;
                (if ordered { (i as u64) ^ (1 << 63) } else { i as u64 }).to_be_bytes().to_vec()
            }
            "BYTES" => { let t = v.as_str().ok_or_else(|| format!("{n} is BYTES: {v} is not a string"))?; B64.decode(t).unwrap_or_else(|_| t.as_bytes().to_vec()) }
            _ => v.as_str().map(|t| t.as_bytes().to_vec()).unwrap_or_else(|| v.to_string().into_bytes()),
        };
        parts.push(b);
    }
    Ok(match enc {
        "singleton" => { if parts.len() != 1 { return Err("a singleton row key has one field".into()) } parts.remove(0) }
        "ordered_code_bytes" => {
            let mut out = vec![];
            for (i, p) in parts.iter().enumerate() {
                if i > 0 { out.extend([0x00, 0x01]); }
                for b in p { match b { 0x00 => out.extend([0x00, 0xff]), x => out.push(*x) } }
            }
            out
        }
        _ => parts.join(schema["delimiter"].as_str().unwrap_or("#").as_bytes()),
    })
}

/// A row key as text: printable bytes as they are, the rest \xNN.
fn show_key(k: &[u8]) -> String {
    k.iter().map(|b| if (0x20..0x7f).contains(b) && *b != b'\\' { (*b as char).to_string() } else { format!("\\x{b:02x}") }).collect()
}

fn salt(id: &str) -> u8 {
    id.bytes().fold(0u8, |h, b| h.wrapping_mul(31).wrapping_add(b))
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
pub fn avro_file(s: &Sub, got: &[Got], meta: bool, typed: Option<&(Value, Vec<Vec<u8>>)>) -> Vec<u8> {
    let mut fields = vec![];
    if meta {
        fields.extend([json!({"name": "subscription_name", "type": "string"}), json!({"name": "message_id", "type": "string"}),
            json!({"name": "publish_time", "type": {"type": "long", "logicalType": "timestamp-micros"}}), json!({"name": "attributes", "type": {"type": "map", "values": "string"}})]);
    }
    fields.push(json!({"name": "data", "type": typed.map(|t| t.0.clone()).unwrap_or(json!("bytes"))}));
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
    for (i, g) in got.iter().enumerate() {
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
        match typed { Some((_, recs)) => block.extend(&recs[i]), None => abytes(&mut block, &g.msg.data) }
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
    let n_take = if c.max_messages > 0 { c.max_messages.min(n) } else { n.min(10_000) } as usize;
    let mut got = take(ctx, s, n_take, 60, 0).await?;
    if c.max_bytes > 0 {
        // A file stops at max_bytes; the rest waits for the next one.
        let (mut sum, mut keep) = (0i64, 0);
        for g in &got { if keep > 0 && sum + g.msg.data.len() as i64 > c.max_bytes { break } sum += g.msg.data.len() as i64; keep += 1; }
        let rest: Vec<(String, i32)> = got.split_off(keep).into_iter().map(|g| (g.ack_id, 0)).collect();
        store::modack(ctx, s, rest).await?;
    }
    if got.is_empty() { return Ok(()) }
    let typed = match &c.output_format {
        Some(cloud_storage_config::OutputFormat::AvroConfig(a)) if a.use_topic_schema => match typed_records(ctx, s, &mut got).await {
            Ok(t) => t,
            Err(e) => { for g in &got { failed(ctx, s, g, &e).await?; } return Ok(()) }
        },
        _ => None,
    };
    let (data, format, ctype) = match &c.output_format {
        Some(cloud_storage_config::OutputFormat::AvroConfig(a)) => (avro_file(s, &got, a.write_metadata, typed.as_ref()), "avro", "application/octet-stream"),
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
    written(ctx, s, got.into_iter().map(|g| g.ack_id).collect()).await
}

/// use_topic_schema: the topic's Avro schema (the revision of the first message; messages of another
/// revision wait for the next file) and each message as an Avro record of it.
async fn typed_records(ctx: &Ctx, s: &Sub, got: &mut Vec<Got>) -> Result<Option<(Value, Vec<Vec<u8>>)>, String> {
    let t = store::topic(ctx, &s.topic).await.map_err(|e| e.message)?.ok_or("the topic was deleted")?;
    let ss = t.schema_settings.clone().filter(|x| !x.schema.is_empty()).ok_or("use_topic_schema: the topic has no schema")?;
    let revs = store::revs(ctx, &ss.schema).await.map_err(|e| e.message)?;
    let rev_of = |g: &Got| g.msg.attributes.get("googclient_schemarevisionid").cloned().or_else(|| revs.first().map(|r| r.revision.clone())).unwrap_or_default();
    let first = rev_of(&got[0]);
    let keep = got.iter().take_while(|g| rev_of(g) == first).count();
    let rest: Vec<(String, i32)> = got.split_off(keep).into_iter().map(|g| (g.ack_id, 0)).collect();
    let r = revs.iter().find(|r| r.revision == first).ok_or("the message's schema revision was deleted")?;
    if r.kind != schema::AVRO { return Err("use_topic_schema in Avro files needs an Avro topic schema".into()) }
    let def: Value = serde_json::from_str(&r.definition).map_err(|e| e.to_string())?;
    let enc = if ss.encoding == 0 { schema::JSON } else { ss.encoding };
    let mut recs = vec![];
    for g in got.iter() {
        let v = schema::validate(r.kind, &r.definition, enc, &g.msg.data)?;
        recs.push(stuntman_streams::avro::encode(&def, &v)?);
    }
    let _ = store::modack(ctx, s, rest).await;
    Ok(Some((def, recs)))
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn row_key_schemas() {
        let m = json!({"region": "us-east", "seq": 5, "order": "ORD-1"});
        let sc = json!({"fields": [{"name": "region", "type": "STRING"}, {"name": "seq", "type": "INT64"}], "encoding": "delimited_bytes", "delimiter": "#"});
        assert_eq!(show_key(&row_key(&sc, &m).unwrap()), "us-east#\\x00\\x00\\x00\\x00\\x00\\x00\\x00\\x05");
        let oc = json!({"fields": [{"name": "region"}, {"name": "order"}], "encoding": "ordered_code_bytes"});
        assert_eq!(row_key(&oc, &m).unwrap(), b"us-east\x00\x01ORD-1".to_vec());
        assert_eq!(row_key(&json!({"fields": [{"name": "order"}], "encoding": "singleton"}), &m).unwrap(), b"ORD-1");
        assert!(row_key(&sc, &json!({"region": "x"})).unwrap_err().contains("seq"));
        let neg = json!({"fields": [{"name": "n", "type": "INT64"}], "encoding": "ordered_code_bytes"});
        assert!(row_key(&neg, &json!({"n": -1})).unwrap() < row_key(&neg, &json!({"n": 1})).unwrap(), "ordered code sorts");
    }

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
