//! The emulator port: google.pubsub.v1 Publisher, Subscriber and SchemaService over plaintext gRPC,
//! and on the same port the REST publish (POST /v1/projects/{p}/topics/{t}:publish). gRPC has no
//! path to carry an environment: the x-stuntman-env metadata names it, else the default one answers.

use std::{collections::HashMap, time::Duration};

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use prost::Message;
use serde_json::{json, Value};
use stuntman_core::{env::Envs, util, ApiError, Ctx};
use tonic::{metadata::MetadataMap, Request, Response as Resp, Status, Streaming};

use crate::{
    config, filter, schema,
    store::{self, dur_ms, Got, Labels, Rev, Snap, Sub, DELETED_SCHEMA, DELETED_TOPIC},
    v1::{
        publisher_server::{Publisher, PublisherServer},
        schema_service_server::{SchemaService, SchemaServiceServer},
        streaming_pull_response::{AcknowledgeConfirmation, ModifyAckDeadlineConfirmation, SubscriptionProperties},
        subscriber_server::{Subscriber, SubscriberServer},
        *,
    },
};

#[derive(Clone)]
pub struct Svc(pub Envs);

fn env_of(envs: &Envs, name: Option<&str>) -> Result<Ctx, Status> {
    let ctx = match name {
        Some(n) => envs.get(n).ok_or_else(|| Status::not_found(format!("No Stuntman environment {n}")))?,
        None => envs.default_env().ok_or_else(|| Status::unavailable("no environment"))?,
    };
    if ctx.state.paused() {
        return Err(Status::unavailable(format!("{} is paused in Stuntman", ctx.env)));
    }
    Ok(ctx)
}

impl Svc {
    fn ctx<T>(&self, r: &Request<T>) -> Result<Ctx, Status> {
        env_of(&self.0, r.metadata().get("x-stuntman-env").and_then(|v| v.to_str().ok()))
    }
}

fn int(e: ApiError) -> Status {
    Status::internal(e.message)
}

/// "app:gl-python/3.12.1" from x-goog-api-client (every official client sends it), else the user agent.
fn client(m: &MetadataMap) -> String {
    let v = m.get("x-goog-api-client").or(m.get("user-agent")).and_then(|v| v.to_str().ok()).unwrap_or("client");
    format!("app:{}", v.split_whitespace().next().unwrap_or("client"))
}

fn check(name: &str, kind: &str) -> Result<(), Status> {
    let p: Vec<&str> = name.split('/').collect();
    if p.len() == 4 && p[0] == "projects" && !p[1].is_empty() && p[2] == kind && !p[3].is_empty() {
        Ok(())
    } else {
        Err(Status::invalid_argument(format!("Invalid [{kind}] name: (name={name})")))
    }
}

fn short(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn not_found(name: &str) -> Status {
    Status::not_found(format!("Resource not found (resource={}).", short(name)))
}

fn ts(ms: i64) -> prost_types::Timestamp {
    prost_types::Timestamp { seconds: ms.div_euclid(1000), nanos: ms.rem_euclid(1000) as i32 * 1_000_000 }
}

fn ts_ms(t: &prost_types::Timestamp) -> i64 {
    t.seconds * 1000 + t.nanos as i64 / 1_000_000
}

/// One page of a list: page_size (default 100, at most 1000) from page_token (an offset).
fn page<T>(v: Vec<T>, size: i32, token: &str) -> Result<(Vec<T>, String), Status> {
    let start: usize = if token.is_empty() { 0 } else { token.parse().map_err(|_| Status::invalid_argument(format!("Invalid page token: {token}")))? };
    let size = if size <= 0 { 100 } else { (size as usize).min(1000) };
    let total = v.len();
    let out = v.into_iter().skip(start).take(size).collect();
    Ok((out, if start + size < total { (start + size).to_string() } else { String::new() }))
}

fn mask(m: Option<prost_types::FieldMask>) -> Result<Vec<String>, Status> {
    let p = m.map(|m| m.paths).unwrap_or_default();
    if p.is_empty() { return Err(Status::invalid_argument("The update_mask in the request must be set and non-empty.")) }
    Ok(p)
}

fn sub_pb(s: &Sub) -> Subscription {
    let mut pb = s.pb.clone();
    pb.name = s.name.clone();
    pb.topic = s.topic.clone();
    pb.ack_deadline_seconds = s.ack_deadline;
    pb.enable_message_ordering = s.ordering;
    pb.labels = s.labels.clone().into_iter().collect();
    pb.dead_letter_policy = (!s.dead_letter.is_empty()).then(|| DeadLetterPolicy { dead_letter_topic: s.dead_letter.clone(), max_delivery_attempts: s.max_attempts });
    pb.message_retention_duration.get_or_insert(store::dur(7 * store::DAY_MS));
    pb.expiration_policy.get_or_insert(ExpirationPolicy { ttl: Some(store::dur(31 * store::DAY_MS)) });
    pb.state = 1;
    if let Some(b) = pb.bigquery_config.as_mut() { b.state = 1 }
    if let Some(c) = pb.cloud_storage_config.as_mut() { c.state = 1 }
    if let Some(c) = pb.bigtable_config.as_mut() { c.state = 1 }
    pb
}

fn received(s: &Sub, g: Got) -> ReceivedMessage {
    ReceivedMessage {
        ack_id: g.ack_id,
        // delivery_attempt is only filled when a dead-letter policy is set, as Pub/Sub does.
        delivery_attempt: if s.dead_letter.is_empty() { 0 } else { g.attempt },
        message: Some(PubsubMessage {
            data: g.msg.data,
            attributes: g.msg.attributes.into_iter().collect(),
            message_id: g.msg.id.to_string(),
            publish_time: Some(ts(g.msg.published)),
            ordering_key: g.msg.ordering_key,
        }),
    }
}

async fn need_sub(ctx: &Ctx, name: &str) -> Result<Sub, Status> {
    check(name, "subscriptions")?;
    store::sub(ctx, name).await.map_err(int)?.ok_or_else(|| not_found(name))
}

async fn need_topic(ctx: &Ctx, name: &str) -> Result<Topic, Status> {
    check(name, "topics")?;
    store::topic(ctx, name).await.map_err(int)?.ok_or_else(|| not_found(name))
}

/// Pull and StreamingPull: only from a pull (or push) subscription that is not detached.
fn pullable(s: &Sub) -> Result<(), Status> {
    if s.pb.detached { return Err(Status::failed_precondition(format!("Subscription {} is detached.", s.name))) }
    if matches!(s.kind(), "bigquery" | "cloud_storage" | "bigtable") { return Err(Status::failed_precondition(format!("Subscription {} delivers to {}; it cannot be pulled.", s.name, s.kind()))) }
    Ok(())
}

// ---------------------------------------------------------------- schemas on topics

/// Validates data against the topic's schema (the revisions it allows, newest first) and stamps the
/// googclient_ attributes subscribers read.
pub async fn conform(ctx: &Ctx, t: &Topic, data: &[u8], attrs: &mut Labels) -> Result<(), String> {
    let Some(ss) = t.schema_settings.as_ref().filter(|s| !s.schema.is_empty()) else { return Ok(()) };
    if ss.schema == DELETED_SCHEMA { return Err("the topic's schema was deleted".into()) }
    let revs = store::revs(ctx, &ss.schema).await.map_err(|e| e.message)?;
    if revs.is_empty() { return Err(format!("schema {} does not exist", ss.schema)) }
    let hi = revs.iter().position(|r| r.revision == ss.last_revision_id).unwrap_or(0);
    let lo = revs.iter().position(|r| r.revision == ss.first_revision_id).unwrap_or(revs.len() - 1);
    let enc = if ss.encoding == 0 { schema::JSON } else { ss.encoding };
    let mut first_err = None;
    for r in revs.get(hi..=lo.max(hi)).unwrap_or(&[]) {
        match schema::validate(r.kind, &r.definition, enc, data) {
            Ok(_) => {
                attrs.insert("googclient_schemaname".into(), ss.schema.clone());
                attrs.insert("googclient_schemaencoding".into(), if enc == schema::BINARY { "BINARY" } else { "JSON" }.into());
                attrs.insert("googclient_schemarevisionid".into(), r.revision.clone());
                return Ok(());
            }
            Err(e) => { first_err.get_or_insert(e); }
        }
    }
    Err(first_err.unwrap_or_else(|| "no schema revision in range".into()))
}

/// Publish as the app does (gRPC or REST): provocations first, then the topic, the schema, then the messages.
pub async fn publish(ctx: &Ctx, topic: &str, msgs: Vec<PubsubMessage>, by: &str) -> Result<Vec<String>, Status> {
    check(topic, "topics")?;
    let provoked = config::take(ctx, topic);
    if let Some((kind, _)) = &provoked {
        if kind == "kms_disabled" {
            let t = store::topic(ctx, topic).await.map_err(int)?.ok_or_else(|| not_found(topic))?;
            if !t.kms_key_name.is_empty() {
                return Err(Status::failed_precondition(format!("The Cloud KMS key {} is disabled or its permission was revoked. (provoked by Stuntman)", t.kms_key_name)));
            }
        }
    }
    if let Some((kind, delay)) = provoked {
        match kind.as_str() {
            "unavailable" => return Err(Status::unavailable("The service is currently unavailable. (provoked by Stuntman)")),
            "deadline_exceeded" => return Err(Status::deadline_exceeded("Deadline exceeded (provoked by Stuntman)")),
            "resource_exhausted" => return Err(Status::resource_exhausted("Quota exceeded for quota metric 'Publish requests' (provoked by Stuntman)")),
            "not_found" => return Err(Status::not_found(format!("Resource not found (resource={}). (provoked by Stuntman)", short(topic)))),
            "slow_publish" => tokio::time::sleep(Duration::from_millis(delay)).await,
            _ => {}
        }
    }
    let t = store::topic(ctx, topic).await.map_err(int)?.ok_or_else(|| not_found(topic))?;
    accept(ctx, &t, msgs, by).await
}

/// What follows the provocations (also for ingestion): the topic's JavaScript transforms (null
/// drops a message but it still gets an id; an error fails the whole request), then its schema.
pub async fn accept(ctx: &Ctx, t: &Topic, msgs: Vec<PubsubMessage>, by: &str) -> Result<Vec<String>, Status> {
    let topic = t.name.as_str();
    if msgs.is_empty() {
        return Err(Status::invalid_argument("The request contains no messages."));
    }
    if let Some(m) = msgs.iter().find(|m| m.data.is_empty() && m.attributes.is_empty()) {
        return Err(Status::invalid_argument(format!("Some messages contain neither data nor attributes (message_id={}).", m.message_id)));
    }
    let steps = crate::udf::steps(&t.message_transforms);
    let now = crate::deliver::rfc3339(util::now_ms());
    // Some(kept) or None (filtered), in request order.
    let mut out: Vec<Option<(Vec<u8>, Labels, String)>> = vec![];
    for m in msgs {
        let a: Labels = m.attributes.into_iter().collect();
        let (d, mut a) = if steps.is_empty() { (m.data, a) } else {
            let r = crate::udf::apply(ctx, &steps, m.data, a, (String::new(), now.clone(), m.ordering_key.clone())).await;
            match r.map_err(|e| match crate::udf::http_of(&e) {
                429 => Status::resource_exhausted(e),
                500..=599 => Status::unavailable(e),
                _ => Status::invalid_argument(format!("Message transformation failed: {e}")),
            })? {
                crate::udf::Out::Keep(d, a) => (d, a),
                crate::udf::Out::Drop => { out.push(None); continue }
            }
        };
        conform(ctx, t, &d, &mut a).await.map_err(|e| Status::invalid_argument(format!("Invalid data in message: Message failed schema validation. {e}")))?;
        out.push(Some((d, a, m.ordering_key)));
    }
    if by.starts_with("app:") { ctx.live.seen(by.trim_start_matches("app:"), "Pub/Sub"); }
    let kept: Vec<_> = out.iter().flatten().cloned().collect();
    let mut ids = store::publish(ctx, topic, kept, by).await.map_err(int)?.into_iter();
    let mut gone = store::reserve_ids(ctx, topic, out.iter().filter(|x| x.is_none()).count()).await.map_err(int)?.into_iter();
    Ok(out.iter().map(|x| if x.is_some() { ids.next() } else { gone.next() }.unwrap_or_default().to_string()).collect())
}

/// A topic as kept: retention in bounds, the schema existing, the encoding set.
/// A Cloud KMS key name, as Pub/Sub checks it: projects/*/locations/*/keyRings/*/cryptoKeys/*.
fn kms_ok(k: &str) -> bool {
    let p: Vec<&str> = k.split('/').collect();
    p.len() == 8 && p[0] == "projects" && p[2] == "locations" && p[4] == "keyRings" && p[6] == "cryptoKeys" && p.iter().all(|x| !x.is_empty())
}

/// A Google Cloud region name: us-central1, europe-west4, southamerica-east1, asia-northeast1…
fn region_ok(r: &str) -> bool {
    let mut parts = r.split('-');
    let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else { return false };
    !a.is_empty() && a.chars().all(|c| c.is_ascii_lowercase()) && b.len() >= 2
        && b.chars().last().is_some_and(|c| c.is_ascii_digit()) && b.trim_end_matches(|c: char| c.is_ascii_digit()).chars().all(|c| c.is_ascii_lowercase())
}

async fn topic_settings(ctx: &Ctx, mut t: Topic) -> Result<Topic, Status> {
    if !t.kms_key_name.is_empty() && !kms_ok(&t.kms_key_name) {
        return Err(Status::invalid_argument(format!("Invalid kms_key_name: {}. Expected projects/{{project}}/locations/{{location}}/keyRings/{{key_ring}}/cryptoKeys/{{key}}.", t.kms_key_name)));
    }
    if let Some(p) = &t.message_storage_policy {
        if let Some(bad) = p.allowed_persistence_regions.iter().find(|r| !region_ok(r)) {
            return Err(Status::invalid_argument(format!("Invalid region in allowed_persistence_regions: {bad}.")));
        }
        if p.enforce_in_transit && p.allowed_persistence_regions.is_empty() {
            return Err(Status::invalid_argument("enforce_in_transit needs at least one allowed persistence region."));
        }
    }
    if let Some(ms) = dur_ms(&t.message_retention_duration) {
        if !(600_000..=31 * store::DAY_MS).contains(&ms) { return Err(Status::invalid_argument("message_retention_duration must be between 10 minutes and 31 days.")) }
    }
    if let Some(ss) = t.schema_settings.as_mut().filter(|s| !s.schema.is_empty()) {
        if ss.schema != DELETED_SCHEMA {
            check(&ss.schema, "schemas")?;
            let revs = store::revs(ctx, &ss.schema).await.map_err(int)?;
            if revs.is_empty() { return Err(Status::not_found(format!("Schema {} not found.", ss.schema))) }
            for r in [&ss.first_revision_id, &ss.last_revision_id] {
                if !r.is_empty() && !revs.iter().any(|x| &x.revision == r) { return Err(Status::invalid_argument(format!("Revision {r} of {} not found.", ss.schema))) }
            }
        }
        if ss.encoding == 0 { ss.encoding = schema::JSON }
    }
    if t.schema_settings.as_ref().is_some_and(|s| s.schema.is_empty()) { t.schema_settings = None }
    if t.ingestion_data_source_settings.as_ref().is_some_and(|i| i.source.is_none()) { t.ingestion_data_source_settings = None }
    crate::ingest::set_state(ctx, &mut t).await.map_err(int)?;
    crate::udf::check(&t.message_transforms).map_err(|e| Status::invalid_argument(format!("Invalid message transform: {e}")))?;
    t.state = 1;
    Ok(t)
}

/// A subscription as kept, checked as Pub/Sub checks it.
pub async fn sub_settings(ctx: &Ctx, mut pb: Subscription, new: bool) -> Result<Sub, Status> {
    check(&pb.name, "subscriptions")?;
    if new {
        check(&pb.topic, "topics")?;
        need_topic(ctx, &pb.topic).await?;
    }
    if pb.push_config.as_ref().is_some_and(|p| p.push_endpoint.is_empty()) { pb.push_config = None }
    let kinds = pb.push_config.is_some() as u8 + pb.bigquery_config.is_some() as u8 + pb.cloud_storage_config.is_some() as u8 + pb.bigtable_config.is_some() as u8;
    if kinds > 1 { return Err(Status::invalid_argument("A subscription is push, BigQuery, Cloud Storage or Bigtable, not more than one.")) }
    if let Some(b) = &pb.bigtable_config {
        let p: Vec<&str> = b.table.split('/').collect();
        if !(p.len() == 6 && p[0] == "projects" && p[2] == "instances" && p[4] == "tables") { return Err(Status::invalid_argument(format!("Invalid Bigtable table: \"{}\" (projects/<p>/instances/<i>/tables/<t>).", b.table))) }
        use crate::v1::bigtable_config::{column_family_mapping::KeyDefinition, MessageMapping};
        if let Some(MessageMapping::ColumnFamilyMapping(m)) = &b.message_mapping {
            match &m.key_definition {
                Some(KeyDefinition::DelimitedKey(k)) if !k.key_fields.is_empty() => {}
                Some(KeyDefinition::DelimitedKey(_)) => return Err(Status::invalid_argument("delimited_key needs key_fields.")),
                Some(KeyDefinition::RowKeySchema(_)) => {}
                None => return Err(Status::invalid_argument("column_family_mapping needs a key definition.")),
            }
        }
    }
    crate::udf::check(&pb.message_transforms).map_err(|e| Status::invalid_argument(format!("Invalid message transform: {e}")))?;
    if let Some(ms) = pb.expiration_policy.as_ref().and_then(|e| dur_ms(&e.ttl)) {
        if ms < store::DAY_MS { return Err(Status::invalid_argument("expiration_policy.ttl must be at least 1 day.")) }
    }
    if let Some(r) = &pb.retry_policy {
        let (lo, hi) = (dur_ms(&r.minimum_backoff).unwrap_or(10_000), dur_ms(&r.maximum_backoff).unwrap_or(600_000));
        if lo > hi || hi > 600_000 { return Err(Status::invalid_argument("retry_policy: 0 ≤ minimum_backoff ≤ maximum_backoff ≤ 600 s.")) }
    }
    if let Some(p) = &pb.push_config {
        if !(p.push_endpoint.starts_with("http://") || p.push_endpoint.starts_with("https://")) { return Err(Status::invalid_argument(format!("Invalid push_endpoint: {}", p.push_endpoint))) }
        if pb.enable_exactly_once_delivery { return Err(Status::invalid_argument("Exactly-once delivery is not supported on push subscriptions.")) }
    }
    if let Some(b) = &pb.bigquery_config {
        if b.table.split(['.', ':']).filter(|x| !x.is_empty()).count() != 3 { return Err(Status::invalid_argument(format!("Invalid BigQuery table: \"{}\" (project.dataset.table).", b.table))) }
        if b.use_topic_schema && b.use_table_schema { return Err(Status::invalid_argument("use_topic_schema and use_table_schema cannot both be set.")) }
        if b.use_topic_schema && new && need_topic(ctx, &pb.topic).await?.schema_settings.is_none() { return Err(Status::invalid_argument("use_topic_schema needs a topic with a schema.")) }
    }
    if let Some(c) = pb.cloud_storage_config.as_mut() {
        if c.bucket.is_empty() || c.bucket.starts_with("gs://") { return Err(Status::invalid_argument("cloud_storage_config.bucket: the bucket name, without gs://")) }
        c.max_duration.get_or_insert(store::dur(300_000));
        c.output_format.get_or_insert(cloud_storage_config::OutputFormat::TextConfig(cloud_storage_config::TextConfig {}));
    }
    if !pb.filter.is_empty() { filter::parse(&pb.filter).map_err(|e| Status::invalid_argument(format!("Invalid filter: {e}")))?; }
    if let Some(ms) = dur_ms(&pb.message_retention_duration) {
        if !(600_000..=31 * store::DAY_MS).contains(&ms) { return Err(Status::invalid_argument("message_retention_duration must be between 10 minutes and 31 days.")) }
    }
    if let Some(d) = &pb.dead_letter_policy { if !d.dead_letter_topic.is_empty() { check(&d.dead_letter_topic, "topics")?; } }
    Ok(Sub::from_pb(pb))
}

// ---------------------------------------------------------------- exactly-once ack errors

#[derive(Clone, PartialEq, prost::Message)]
struct RpcStatus {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
    #[prost(message, repeated, tag = "3")]
    details: Vec<prost_types::Any>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ErrorInfo {
    #[prost(string, tag = "1")]
    reason: String,
    #[prost(string, tag = "2")]
    domain: String,
    #[prost(map = "string, string", tag = "3")]
    metadata: HashMap<String, String>,
}

/// What Pub/Sub answers an ack or modack with expired ack ids under exactly-once delivery:
/// INVALID_ARGUMENT with a google.rpc.ErrorInfo whose metadata maps each id to
/// PERMANENT_FAILURE_INVALID_ACK_ID (the clients read it to fail that message's ack future).
fn ack_failure(bad: &[String], why: &str) -> Status {
    let msg = if why.starts_with("TRANSIENT") { "Some acknowledgement ids in the request failed temporarily; retry them." }
        else { "Some acknowledgement ids in the request were invalid. This could be because the acknowledgement ids have expired or the acknowledgement ids were malformed." };
    let info = ErrorInfo { reason: "EXACTLY_ONCE_ACKID_FAILURE".into(), domain: "pubsub.googleapis.com".into(), metadata: bad.iter().map(|a| (a.clone(), why.into())).collect() };
    let st = RpcStatus { code: tonic::Code::InvalidArgument as i32, message: msg.into(), details: vec![prost_types::Any { type_url: "type.googleapis.com/google.rpc.ErrorInfo".into(), value: info.encode_to_vec() }] };
    Status::with_details(tonic::Code::InvalidArgument, msg, Bytes::from(st.encode_to_vec()))
}

struct Acked {
    bad_acks: Vec<String>,
    bad_modacks: Vec<String>,
    /// ack_transient provoked: nothing applied, every id to retry.
    transient: bool,
}

/// Applies acks and modacks; answers the ids that matched nothing. Marks the subscription active.
async fn apply_acks(ctx: &Ctx, s: &Sub, acks: Vec<String>, modacks: Vec<(String, i32)>) -> Result<Acked, Status> {
    if acks.is_empty() && modacks.is_empty() { return Ok(Acked { bad_acks: vec![], bad_modacks: vec![], transient: false }) }
    store::seen(ctx, &s.name).await.map_err(int)?;
    if s.exactly_once() && config::take_ack(ctx) {
        return Ok(Acked { bad_acks: acks, bad_modacks: modacks.into_iter().map(|m| m.0).collect(), transient: true });
    }
    let dup = config::load(ctx).provoke == "duplicate";
    let a = store::ack(ctx, &s.name, acks, dup, s.exactly_once()).await.map_err(int)?;
    let m = store::modack(ctx, s, modacks).await.map_err(int)?;
    Ok(Acked { bad_acks: a, bad_modacks: m, transient: false })
}

/// The answer to an exactly-once ack or modack with failed ids. When only some failed, the code is
/// ABORTED: the clients complete the failed ids from the ErrorInfo and send the others again (an
/// already-acked id then succeeds), instead of failing the good ones.
fn ack_error(s: &Sub, bad: &[String], total: usize, transient: bool) -> Result<(), Status> {
    if s.exactly_once() && !bad.is_empty() {
        let st = ack_failure(bad, if transient { "TRANSIENT_FAILURE_UNORDERED_ACK_ID" } else { "PERMANENT_FAILURE_INVALID_ACK_ID" });
        if bad.len() < total {
            let d = RpcStatus::decode(st.details()).map(|mut r| { r.code = tonic::Code::Aborted as i32; r.encode_to_vec() }).unwrap_or_default();
            return Err(Status::with_details(tonic::Code::Aborted, st.message(), Bytes::from(d)));
        }
        return Err(st);
    }
    Ok(())
}

// ---------------------------------------------------------------- Publisher

fn copy_topic(cur: &mut Topic, new: &Topic, path: &str) -> Result<(), Status> {
    match path.split('.').next().unwrap_or("") {
        "labels" => cur.labels = new.labels.clone(),
        "message_storage_policy" => cur.message_storage_policy = new.message_storage_policy.clone(),
        "kms_key_name" => cur.kms_key_name = new.kms_key_name.clone(),
        "schema_settings" => cur.schema_settings = new.schema_settings.clone(),
        "message_retention_duration" => cur.message_retention_duration = new.message_retention_duration,
        "message_transforms" => cur.message_transforms = new.message_transforms.clone(),
        "ingestion_data_source_settings" => cur.ingestion_data_source_settings = new.ingestion_data_source_settings.clone(),
        "name" => return Err(Status::invalid_argument("A topic's name cannot be updated.")),
        p => return Err(Status::invalid_argument(format!("Invalid update_mask provided in the UpdateTopicRequest: the field '{p}' cannot be updated or does not exist."))),
    }
    Ok(())
}

#[tonic::async_trait]
impl Publisher for Svc {
    async fn create_topic(&self, r: Request<Topic>) -> Result<Resp<Topic>, Status> {
        let ctx = self.ctx(&r)?;
        let t = r.into_inner();
        check(&t.name, "topics")?;
        let t = topic_settings(&ctx, t).await?;
        if !store::create_topic(&ctx, &t).await.map_err(int)? {
            return Err(Status::already_exists("Topic already exists"));
        }
        Ok(Resp::new(t))
    }

    async fn get_topic(&self, r: Request<GetTopicRequest>) -> Result<Resp<Topic>, Status> {
        let ctx = self.ctx(&r)?;
        Ok(Resp::new(need_topic(&ctx, &r.into_inner().topic).await?))
    }

    /// The fields the update mask names: labels, schema settings, retention, storage policy, KMS key.
    async fn update_topic(&self, r: Request<UpdateTopicRequest>) -> Result<Resp<Topic>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let new = q.topic.ok_or_else(|| Status::invalid_argument("topic is required"))?;
        let mut cur = need_topic(&ctx, &new.name).await?;
        for p in mask(q.update_mask)? { copy_topic(&mut cur, &new, &p)?; }
        let cur = topic_settings(&ctx, cur).await?;
        store::save_topic(&ctx, &cur).await.map_err(int)?;
        Ok(Resp::new(cur))
    }

    async fn publish(&self, r: Request<PublishRequest>) -> Result<Resp<PublishResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let by = client(r.metadata());
        let req = r.into_inner();
        Ok(Resp::new(PublishResponse { message_ids: publish(&ctx, &req.topic, req.messages, &by).await? }))
    }

    async fn list_topics(&self, r: Request<ListTopicsRequest>) -> Result<Resp<ListTopicsResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let (topics, next_page_token) = page(store::topics(&ctx, &format!("{}/topics/", q.project)).await.map_err(int)?, q.page_size, &q.page_token)?;
        Ok(Resp::new(ListTopicsResponse { topics, next_page_token }))
    }

    async fn list_topic_subscriptions(&self, r: Request<ListTopicSubscriptionsRequest>) -> Result<Resp<ListTopicSubscriptionsResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        need_topic(&ctx, &q.topic).await?;
        let all = store::subs(&ctx, "", Some(&q.topic)).await.map_err(int)?.into_iter().filter(|s| !s.pb.detached).map(|s| s.name).collect();
        let (subscriptions, next_page_token) = page(all, q.page_size, &q.page_token)?;
        Ok(Resp::new(ListTopicSubscriptionsResponse { subscriptions, next_page_token }))
    }

    async fn list_topic_snapshots(&self, r: Request<ListTopicSnapshotsRequest>) -> Result<Resp<ListTopicSnapshotsResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        need_topic(&ctx, &q.topic).await?;
        let all = store::snapshots(&ctx, "", Some(&q.topic)).await.map_err(int)?.into_iter().map(|s| s.name).collect();
        let (snapshots, next_page_token) = page(all, q.page_size, &q.page_token)?;
        Ok(Resp::new(ListTopicSnapshotsResponse { snapshots, next_page_token }))
    }

    async fn delete_topic(&self, r: Request<DeleteTopicRequest>) -> Result<Resp<()>, Status> {
        let ctx = self.ctx(&r)?;
        let t = r.into_inner().topic;
        check(&t, "topics")?;
        if !store::delete_topic(&ctx, &t).await.map_err(int)? {
            return Err(not_found(&t));
        }
        Ok(Resp::new(()))
    }

    /// The subscription keeps its name but gets nothing more; what it held is dropped; pulls fail.
    async fn detach_subscription(&self, r: Request<DetachSubscriptionRequest>) -> Result<Resp<DetachSubscriptionResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let mut s = need_sub(&ctx, &r.into_inner().subscription).await?;
        s.pb.detached = true;
        store::save_sub(&ctx, &s).await.map_err(int)?;
        store::drop_deliveries(&ctx, &s.name).await.map_err(int)?;
        Ok(Resp::new(DetachSubscriptionResponse {}))
    }
}

// ---------------------------------------------------------------- Subscriber

fn copy_sub(cur: &mut Subscription, new: &Subscription, path: &str) -> Result<(), Status> {
    match path.split('.').next().unwrap_or("") {
        "ack_deadline_seconds" => cur.ack_deadline_seconds = new.ack_deadline_seconds,
        "push_config" => {
            cur.push_config = new.push_config.clone();
            if cur.push_config.as_ref().is_some_and(|p| !p.push_endpoint.is_empty()) { cur.bigquery_config = None; cur.cloud_storage_config = None; }
        }
        "bigquery_config" => {
            cur.bigquery_config = new.bigquery_config.clone();
            if cur.bigquery_config.is_some() { cur.push_config = None; cur.cloud_storage_config = None; }
        }
        "cloud_storage_config" => {
            cur.cloud_storage_config = new.cloud_storage_config.clone();
            if cur.cloud_storage_config.is_some() { cur.push_config = None; cur.bigquery_config = None; }
        }
        "labels" => cur.labels = new.labels.clone(),
        "retain_acked_messages" => cur.retain_acked_messages = new.retain_acked_messages,
        "message_retention_duration" => cur.message_retention_duration = new.message_retention_duration,
        "expiration_policy" => cur.expiration_policy = new.expiration_policy,
        "dead_letter_policy" => cur.dead_letter_policy = new.dead_letter_policy.clone(),
        "retry_policy" => cur.retry_policy = new.retry_policy,
        "enable_exactly_once_delivery" => cur.enable_exactly_once_delivery = new.enable_exactly_once_delivery,
        "message_transforms" => cur.message_transforms = new.message_transforms.clone(),
        p @ ("filter" | "enable_message_ordering" | "topic" | "name" | "detached") => return Err(Status::invalid_argument(format!("The field '{p}' of a subscription cannot be updated."))),
        p => return Err(Status::invalid_argument(format!("Invalid update_mask provided in the UpdateSubscriptionRequest: the field '{p}' cannot be updated or does not exist."))),
    }
    Ok(())
}

fn snap_pb(s: &Snap) -> Snapshot {
    Snapshot { name: s.name.clone(), topic: s.topic.clone(), expire_time: Some(ts(s.expire)), labels: s.labels.clone().into_iter().collect() }
}

#[tonic::async_trait]
impl Subscriber for Svc {
    async fn create_subscription(&self, r: Request<Subscription>) -> Result<Resp<Subscription>, Status> {
        let ctx = self.ctx(&r)?;
        let sub = sub_settings(&ctx, r.into_inner(), true).await?;
        if !store::create_sub(&ctx, &sub).await.map_err(int)? {
            return Err(Status::already_exists("Subscription already exists"));
        }
        store::seen(&ctx, &sub.name).await.map_err(int)?;
        Ok(Resp::new(sub_pb(&sub)))
    }

    async fn get_subscription(&self, r: Request<GetSubscriptionRequest>) -> Result<Resp<Subscription>, Status> {
        let ctx = self.ctx(&r)?;
        Ok(Resp::new(sub_pb(&need_sub(&ctx, &r.into_inner().subscription).await?)))
    }

    /// The fields the update mask names; filter, ordering and topic cannot change.
    async fn update_subscription(&self, r: Request<UpdateSubscriptionRequest>) -> Result<Resp<Subscription>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let new = q.subscription.ok_or_else(|| Status::invalid_argument("subscription is required"))?;
        let mut cur = sub_pb(&need_sub(&ctx, &new.name).await?);
        for p in mask(q.update_mask)? { copy_sub(&mut cur, &new, &p)?; }
        let s = sub_settings(&ctx, cur, false).await?;
        store::save_sub(&ctx, &s).await.map_err(int)?;
        Ok(Resp::new(sub_pb(&s)))
    }

    async fn list_subscriptions(&self, r: Request<ListSubscriptionsRequest>) -> Result<Resp<ListSubscriptionsResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let all = store::subs(&ctx, &format!("{}/subscriptions/", q.project), None).await.map_err(int)?.iter().map(sub_pb).collect();
        let (subscriptions, next_page_token) = page(all, q.page_size, &q.page_token)?;
        Ok(Resp::new(ListSubscriptionsResponse { subscriptions, next_page_token }))
    }

    async fn delete_subscription(&self, r: Request<DeleteSubscriptionRequest>) -> Result<Resp<()>, Status> {
        let ctx = self.ctx(&r)?;
        let s = need_sub(&ctx, &r.into_inner().subscription).await?;
        store::delete_sub(&ctx, &s.name).await.map_err(int)?;
        Ok(Resp::new(()))
    }

    async fn modify_ack_deadline(&self, r: Request<ModifyAckDeadlineRequest>) -> Result<Resp<()>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let s = need_sub(&ctx, &q.subscription).await?;
        let secs = q.ack_deadline_seconds;
        let total = q.ack_ids.len();
        let r = apply_acks(&ctx, &s, vec![], q.ack_ids.into_iter().map(|a| (a, secs)).collect()).await?;
        ack_error(&s, &r.bad_modacks, total, r.transient)?;
        Ok(Resp::new(()))
    }

    async fn acknowledge(&self, r: Request<AcknowledgeRequest>) -> Result<Resp<()>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let s = need_sub(&ctx, &q.subscription).await?;
        let total = q.ack_ids.len();
        let r = apply_acks(&ctx, &s, q.ack_ids, vec![]).await?;
        ack_error(&s, &r.bad_acks, total, r.transient)?;
        Ok(Resp::new(()))
    }

    async fn modify_push_config(&self, r: Request<ModifyPushConfigRequest>) -> Result<Resp<()>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let mut cur = sub_pb(&need_sub(&ctx, &q.subscription).await?);
        copy_sub(&mut cur, &Subscription { push_config: q.push_config, ..Default::default() }, "push_config")?;
        let s = sub_settings(&ctx, cur, false).await?;
        store::save_sub(&ctx, &s).await.map_err(int)?;
        Ok(Resp::new(()))
    }

    async fn pull(&self, r: Request<PullRequest>) -> Result<Resp<PullResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        stuntman_core::usage::hit(&ctx, "pubsub");
        let s = need_sub(&ctx, &q.subscription).await?;
        pullable(&s)?;
        store::seen(&ctx, &s.name).await.map_err(int)?;
        let max = if q.max_messages <= 0 { 100 } else { q.max_messages as usize };
        #[allow(deprecated)]
        let wait = !q.return_immediately;
        // Pull without messages waits a little for one, as Pub/Sub does.
        let end = util::now_ms() + if wait { 1500 } else { 0 };
        loop {
            let bell = crate::bell().notified();
            let got = crate::deliver::take(&ctx, &s, max, s.ack_deadline, 0).await.map_err(int)?;
            if !got.is_empty() || util::now_ms() >= end {
                return Ok(Resp::new(PullResponse { received_messages: got.into_iter().map(|g| received(&s, g)).collect() }));
            }
            tokio::select! { _ = bell => {} _ = tokio::time::sleep(Duration::from_millis(250)) => {} }
        }
    }

    /// The stream the Python, .NET, Go and Java subscribers live on: the first request names the
    /// subscription and the stream's ack deadline; later ones carry acks, modacks and heartbeats.
    /// With exactly-once delivery, each ack and modack is confirmed on the stream (invalid ids named).
    async fn streaming_pull(&self, r: Request<Streaming<StreamingPullRequest>>) -> Result<Resp<tonic::codegen::BoxStream<StreamingPullResponse>>, Status> {
        let ctx = self.ctx(&r)?;
        let mut inb = r.into_inner();
        stuntman_core::usage::hit(&ctx, "pubsub");
        let first = inb.message().await?.ok_or_else(|| Status::invalid_argument("The first StreamingPullRequest must name a subscription."))?;
        let s = need_sub(&ctx, &first.subscription).await?;
        pullable(&s)?;
        let secs = if first.stream_ack_deadline_seconds > 0 { first.stream_ack_deadline_seconds.clamp(10, 600) } else { s.ack_deadline };
        let max_out = if first.max_outstanding_messages > 0 { first.max_outstanding_messages } else { 1000 };
        let max_bytes = first.max_outstanding_bytes.max(0);
        store::seen(&ctx, &s.name).await.map_err(int)?;
        let props = SubscriptionProperties { exactly_once_delivery_enabled: s.exactly_once(), message_ordering_enabled: s.ordering };
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<StreamingPullResponse, Status>>(8);
        let done = std::sync::Arc::new(tokio::sync::Notify::new());
        {
            let (ctx, s, done, tx) = (ctx.clone(), s.clone(), done.clone(), tx.clone());
            tokio::spawn(async move {
                let mut q = Some(first);
                while let Some(req) = q.take() {
                    let mods: Vec<(String, i32)> = req.modify_deadline_ack_ids.iter().cloned().zip(req.modify_deadline_seconds.iter().copied()).collect();
                    let (acks, mod_ids) = (req.ack_ids.clone(), req.modify_deadline_ack_ids.clone());
                    if let Ok(r) = apply_acks(&ctx, &s, acks.clone(), mods).await {
                        if s.exactly_once() && !(acks.is_empty() && mod_ids.is_empty()) {
                            let ok = |all: &[String], bad: &[String]| all.iter().filter(|a| !bad.contains(a)).cloned().collect::<Vec<_>>();
                            let (inv_a, inv_m, tmp_a, tmp_m) = if r.transient { (vec![], vec![], r.bad_acks.clone(), r.bad_modacks.clone()) } else { (r.bad_acks.clone(), r.bad_modacks.clone(), vec![], vec![]) };
                            let resp = StreamingPullResponse {
                                acknowledge_confirmation: (!acks.is_empty()).then(|| AcknowledgeConfirmation { ack_ids: ok(&acks, &r.bad_acks), invalid_ack_ids: inv_a, temporary_failed_ack_ids: tmp_a, ..Default::default() }),
                                modify_ack_deadline_confirmation: (!mod_ids.is_empty()).then(|| ModifyAckDeadlineConfirmation { ack_ids: ok(&mod_ids, &r.bad_modacks), invalid_ack_ids: inv_m, temporary_failed_ack_ids: tmp_m }),
                                subscription_properties: Some(props),
                                ..Default::default()
                            };
                            if tx.send(Ok(resp)).await.is_err() { break }
                        }
                    }
                    q = match inb.message().await { Ok(Some(x)) => Some(x), _ => None };
                }
                done.notify_one();
            });
        }
        tokio::spawn(async move {
            loop {
                if ctx.state.gone() { break }
                let bell = crate::bell().notified();
                let room = max_out - store::outstanding(&ctx, &s.name).await.unwrap_or(0);
                // max_outstanding_bytes: data out may not pass it (one message always goes when none is out).
                let room_bytes = if max_bytes > 0 { max_bytes - store::outstanding_bytes(&ctx, &s.name).await.unwrap_or(0) } else { 0 };
                let byte_room = max_bytes == 0 || room_bytes > 0;
                if room > 0 && byte_room {
                    // The subscription may be deleted or detached while the stream is open.
                    let cur = match store::sub(&ctx, &s.name).await {
                        Ok(Some(c)) if !c.pb.detached => c,
                        Ok(Some(_)) => { let _ = tx.send(Err(Status::failed_precondition(format!("Subscription {} is detached.", s.name)))).await; break }
                        _ => { let _ = tx.send(Err(not_found(&s.name))).await; break }
                    };
                    let out_now = store::outstanding(&ctx, &s.name).await.unwrap_or(0);
                    let limit = if max_bytes > 0 && out_now > 0 { -room_bytes } else { max_bytes };
                    let got = crate::deliver::take(&ctx, &cur, room.min(500) as usize, secs, limit).await.unwrap_or_default();
                    if !got.is_empty() {
                        let resp = StreamingPullResponse { received_messages: got.into_iter().map(|g| received(&cur, g)).collect(), subscription_properties: Some(props), ..Default::default() };
                        if tx.send(Ok(resp)).await.is_err() { break }
                        continue;
                    }
                }
                tokio::select! {
                    _ = bell => {}
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                    _ = done.notified() => break,
                    _ = tx.closed() => break,
                }
            }
        });
        let out = futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x, rx)) });
        Ok(Resp::new(Box::pin(out)))
    }

    async fn create_snapshot(&self, r: Request<CreateSnapshotRequest>) -> Result<Resp<Snapshot>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let s = need_sub(&ctx, &q.subscription).await?;
        if s.pb.detached { return Err(Status::failed_precondition(format!("Subscription {} is detached.", s.name))) }
        let name = if q.name.is_empty() { format!("projects/{}/snapshots/snapshot-{}", s.name.split('/').nth(1).unwrap_or(""), &util::uuid()[..8]) } else { q.name };
        check(&name, "snapshots")?;
        let snap = store::create_snapshot(&ctx, &name, &s, &q.labels.into_iter().collect()).await.map_err(int)?.ok_or_else(|| Status::already_exists("Snapshot already exists"))?;
        Ok(Resp::new(snap_pb(&snap)))
    }

    async fn get_snapshot(&self, r: Request<GetSnapshotRequest>) -> Result<Resp<Snapshot>, Status> {
        let ctx = self.ctx(&r)?;
        let n = r.into_inner().snapshot;
        check(&n, "snapshots")?;
        Ok(Resp::new(snap_pb(&store::snapshot(&ctx, &n).await.map_err(int)?.ok_or_else(|| not_found(&n))?)))
    }

    async fn list_snapshots(&self, r: Request<ListSnapshotsRequest>) -> Result<Resp<ListSnapshotsResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let all = store::snapshots(&ctx, &format!("{}/snapshots/", q.project), None).await.map_err(int)?.iter().map(snap_pb).collect();
        let (snapshots, next_page_token) = page(all, q.page_size, &q.page_token)?;
        Ok(Resp::new(ListSnapshotsResponse { snapshots, next_page_token }))
    }

    /// Labels and expire_time.
    async fn update_snapshot(&self, r: Request<UpdateSnapshotRequest>) -> Result<Resp<Snapshot>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let new = q.snapshot.ok_or_else(|| Status::invalid_argument("snapshot is required"))?;
        check(&new.name, "snapshots")?;
        let mut cur = store::snapshot(&ctx, &new.name).await.map_err(int)?.ok_or_else(|| not_found(&new.name))?;
        for p in mask(q.update_mask)? {
            match p.as_str() {
                "labels" => cur.labels = new.labels.clone().into_iter().collect(),
                "expire_time" => cur.expire = new.expire_time.as_ref().map(ts_ms).ok_or_else(|| Status::invalid_argument("expire_time is required"))?,
                p => return Err(Status::invalid_argument(format!("Invalid update_mask provided in the UpdateSnapshotRequest: the field '{p}' cannot be updated."))),
            }
        }
        store::update_snapshot(&ctx, &cur).await.map_err(int)?;
        Ok(Resp::new(snap_pb(&cur)))
    }

    async fn delete_snapshot(&self, r: Request<DeleteSnapshotRequest>) -> Result<Resp<()>, Status> {
        let ctx = self.ctx(&r)?;
        let n = r.into_inner().snapshot;
        check(&n, "snapshots")?;
        if !store::delete_snapshot(&ctx, &n).await.map_err(int)? { return Err(not_found(&n)) }
        Ok(Resp::new(()))
    }

    /// Seek to a time (within what is retained) or to a snapshot of the same topic.
    async fn seek(&self, r: Request<SeekRequest>) -> Result<Resp<SeekResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let s = need_sub(&ctx, &q.subscription).await?;
        if s.pb.detached { return Err(Status::failed_precondition(format!("Subscription {} is detached.", s.name))) }
        match q.target {
            Some(seek_request::Target::Time(t)) => {
                let ret = if s.topic == DELETED_TOPIC { None } else { store::topic(&ctx, &s.topic).await.map_err(int)?.and_then(|t| dur_ms(&t.message_retention_duration)) };
                store::seek(&ctx, &s, ts_ms(&t), ret).await.map_err(int)?
            }
            Some(seek_request::Target::Snapshot(n)) => {
                check(&n, "snapshots")?;
                let snap = store::snapshot(&ctx, &n).await.map_err(int)?.ok_or_else(|| not_found(&n))?;
                if snap.topic != s.topic { return Err(Status::failed_precondition(format!("The snapshot's topic ({}) is not the subscription's ({}).", snap.topic, s.topic))) }
                store::seek_snapshot(&ctx, &s, &snap).await.map_err(int)?
            }
            None => return Err(Status::invalid_argument("Seek needs a time or a snapshot.")),
        }
        Ok(Resp::new(SeekResponse {}))
    }
}

// ---------------------------------------------------------------- SchemaService

/// "projects/p/schemas/s@rev" into the name and the revision.
fn schema_ref(n: &str) -> Result<(String, Option<String>), Status> {
    let (name, rev) = match n.split_once('@') { Some((a, b)) => (a.to_string(), Some(b.to_string())), None => (n.to_string(), None) };
    check(&name, "schemas")?;
    Ok((name, rev))
}

fn schema_pb(r: &Rev, basic: bool) -> Schema {
    Schema { name: r.name.clone(), r#type: r.kind, definition: if basic { String::new() } else { r.definition.clone() }, revision_id: r.revision.clone(),
        revision_create_time: Some(ts(r.created)), configuration: None }
}

async fn need_rev(ctx: &Ctx, n: &str) -> Result<Rev, Status> {
    let (name, rev) = schema_ref(n)?;
    let revs = store::revs(ctx, &name).await.map_err(int)?;
    match rev {
        Some(r) => revs.into_iter().find(|x| x.revision == r),
        None => revs.into_iter().next(),
    }
    .ok_or_else(|| not_found(n))
}

fn valid_def(s: &Schema) -> Result<(), Status> {
    schema::check(s.r#type, &s.definition).map_err(|e| Status::invalid_argument(format!("Invalid schema definition: {e}")))
}

#[tonic::async_trait]
impl SchemaService for Svc {
    async fn create_schema(&self, r: Request<CreateSchemaRequest>) -> Result<Resp<Schema>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let s = q.schema.ok_or_else(|| Status::invalid_argument("schema is required"))?;
        let name = if q.schema_id.is_empty() { s.name.clone() } else { format!("{}/schemas/{}", q.parent, q.schema_id) };
        check(&name, "schemas")?;
        valid_def(&s)?;
        if !store::revs(&ctx, &name).await.map_err(int)?.is_empty() { return Err(Status::already_exists("Schema already exists")) }
        Ok(Resp::new(schema_pb(&store::add_rev(&ctx, &name, s.r#type, &s.definition).await.map_err(int)?, false)))
    }

    async fn get_schema(&self, r: Request<GetSchemaRequest>) -> Result<Resp<Schema>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        Ok(Resp::new(schema_pb(&need_rev(&ctx, &q.name).await?, q.view == SchemaView::Basic as i32)))
    }

    async fn list_schemas(&self, r: Request<ListSchemasRequest>) -> Result<Resp<ListSchemasResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let basic = q.view != SchemaView::Full as i32;
        let all = store::schemas(&ctx, &format!("{}/schemas/", q.parent)).await.map_err(int)?.iter().map(|x| schema_pb(x, basic)).collect();
        let (schemas, next_page_token) = page(all, q.page_size, &q.page_token)?;
        Ok(Resp::new(ListSchemasResponse { schemas, next_page_token }))
    }

    async fn list_schema_revisions(&self, r: Request<ListSchemaRevisionsRequest>) -> Result<Resp<ListSchemaRevisionsResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let (name, _) = schema_ref(&q.name)?;
        let revs = store::revs(&ctx, &name).await.map_err(int)?;
        if revs.is_empty() { return Err(not_found(&name)) }
        let basic = q.view == SchemaView::Basic as i32;
        let (schemas, next_page_token) = page(revs.iter().map(|x| schema_pb(x, basic)).collect(), q.page_size, &q.page_token)?;
        Ok(Resp::new(ListSchemaRevisionsResponse { schemas, next_page_token }))
    }

    async fn commit_schema(&self, r: Request<CommitSchemaRequest>) -> Result<Resp<Schema>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let s = q.schema.ok_or_else(|| Status::invalid_argument("schema is required"))?;
        let cur = need_rev(&ctx, &q.name).await?;
        if s.r#type != cur.kind { return Err(Status::invalid_argument("A new revision must keep the schema's type.")) }
        valid_def(&s)?;
        Ok(Resp::new(schema_pb(&store::add_rev(&ctx, &cur.name, s.r#type, &s.definition).await.map_err(int)?, false)))
    }

    /// A new revision with the definition of an older one.
    async fn rollback_schema(&self, r: Request<RollbackSchemaRequest>) -> Result<Resp<Schema>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let old = need_rev(&ctx, &format!("{}@{}", schema_ref(&q.name)?.0, q.revision_id)).await?;
        Ok(Resp::new(schema_pb(&store::add_rev(&ctx, &old.name, old.kind, &old.definition).await.map_err(int)?, false)))
    }

    async fn delete_schema_revision(&self, r: Request<DeleteSchemaRevisionRequest>) -> Result<Resp<Schema>, Status> {
        let ctx = self.ctx(&r)?;
        #[allow(deprecated)]
        let q = r.into_inner();
        let (name, rev) = schema_ref(&q.name)?;
        #[allow(deprecated)]
        let rev = rev.or(Some(q.revision_id).filter(|x| !x.is_empty())).ok_or_else(|| Status::invalid_argument("Name the revision: projects/p/schemas/s@revision"))?;
        let revs = store::revs(&ctx, &name).await.map_err(int)?;
        let gone = revs.iter().find(|x| x.revision == rev).cloned().ok_or_else(|| not_found(&format!("{name}@{rev}")))?;
        if revs.len() == 1 { return Err(Status::failed_precondition("The only revision of a schema cannot be deleted; delete the schema.")) }
        store::delete_rev(&ctx, &name, &rev).await.map_err(int)?;
        Ok(Resp::new(schema_pb(&gone, false)))
    }

    /// Topics that used it keep publishing nothing: their schema reads _deleted-schema_.
    async fn delete_schema(&self, r: Request<DeleteSchemaRequest>) -> Result<Resp<()>, Status> {
        let ctx = self.ctx(&r)?;
        let (name, _) = schema_ref(&r.into_inner().name)?;
        if !store::delete_schema(&ctx, &name).await.map_err(int)? { return Err(not_found(&name)) }
        for mut t in store::topics(&ctx, "").await.map_err(int)? {
            if let Some(ss) = t.schema_settings.as_mut().filter(|s| s.schema == name) {
                ss.schema = DELETED_SCHEMA.into();
                store::save_topic(&ctx, &t).await.map_err(int)?;
            }
        }
        Ok(Resp::new(()))
    }

    async fn validate_schema(&self, r: Request<ValidateSchemaRequest>) -> Result<Resp<ValidateSchemaResponse>, Status> {
        let _ = self.ctx(&r)?;
        valid_def(&r.into_inner().schema.ok_or_else(|| Status::invalid_argument("schema is required"))?)?;
        Ok(Resp::new(ValidateSchemaResponse {}))
    }

    async fn validate_message(&self, r: Request<ValidateMessageRequest>) -> Result<Resp<ValidateMessageResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let (kind, def) = match q.schema_spec {
            Some(validate_message_request::SchemaSpec::Name(n)) => { let r = need_rev(&ctx, &n).await?; (r.kind, r.definition) }
            Some(validate_message_request::SchemaSpec::Schema(s)) => { valid_def(&s)?; (s.r#type, s.definition) }
            None => return Err(Status::invalid_argument("Name a schema or give one.")),
        };
        if q.encoding == 0 { return Err(Status::invalid_argument("encoding must be JSON or BINARY")) }
        schema::validate(kind, &def, q.encoding, &q.message).map_err(|e| Status::invalid_argument(format!("Invalid data in message: Message failed schema validation. {e}")))?;
        Ok(Resp::new(ValidateMessageResponse {}))
    }
}

// ---------------------------------------------------------------- REST publish

fn http(s: Status) -> Response {
    let (code, name) = match s.code() {
        tonic::Code::NotFound => (404, "NOT_FOUND"),
        tonic::Code::InvalidArgument => (400, "INVALID_ARGUMENT"),
        tonic::Code::Unavailable => (503, "UNAVAILABLE"),
        tonic::Code::DeadlineExceeded => (504, "DEADLINE_EXCEEDED"),
        tonic::Code::ResourceExhausted => (429, "RESOURCE_EXHAUSTED"),
        _ => (500, "INTERNAL"),
    };
    (StatusCode::from_u16(code).unwrap(), Json(json!({"error": {"code": code, "message": s.message(), "status": name}}))).into_response()
}

/// POST /v1/projects/{p}/topics/{t}:publish {"messages": [{"data": base64, "attributes", "orderingKey"}]}
async fn rest_publish(State(envs): State<Envs>, Path((p, t)): Path<(String, String)>, h: HeaderMap, body: Bytes) -> Response {
    use base64::Engine;
    let Some(t) = t.strip_suffix(":publish") else { return http(Status::not_found("Only :publish is offered over REST; use gRPC for the rest.")) };
    let ctx = match env_of(&envs, h.get("x-stuntman-env").and_then(|v| v.to_str().ok())) { Ok(c) => c, Err(e) => return http(e) };
    let v: Value = match serde_json::from_slice(&body) { Ok(v) => v, Err(e) => return http(Status::invalid_argument(format!("Invalid JSON payload: {e}"))) };
    let mut msgs = vec![];
    for m in v["messages"].as_array().cloned().unwrap_or_default() {
        let Ok(data) = base64::engine::general_purpose::STANDARD.decode(m["data"].as_str().unwrap_or("")) else { return http(Status::invalid_argument("data: not base64")) };
        let attributes = m["attributes"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect()).unwrap_or_default();
        msgs.push(PubsubMessage { data, attributes, ordering_key: m["orderingKey"].as_str().unwrap_or("").into(), ..Default::default() });
    }
    let by = format!("app:{}", h.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("rest").split('/').next().unwrap_or("rest"));
    match publish(&ctx, &format!("projects/{p}/topics/{t}"), msgs, &by).await {
        Ok(ids) => Json(json!({"messageIds": ids})).into_response(),
        Err(e) => http(e),
    }
}

pub fn serve(envs: Envs, port: u16) {
    tokio::spawn(async move {
        let l = match std::net::TcpListener::bind(("0.0.0.0", port)) {
            Ok(l) => l,
            Err(_) => {
                eprintln!("Pub/Sub: port {port} is taken");
                return;
            }
        };
        l.set_nonblocking(true).unwrap();
        let l = tokio::net::TcpListener::from_std(l).unwrap();
        let rest = Router::new().route("/v1/projects/{p}/topics/{t}", post(rest_publish)).with_state(envs.clone());
        let routes = tonic::service::Routes::from(rest).add_service(PublisherServer::new(Svc(envs.clone()))).add_service(SubscriberServer::new(Svc(envs.clone()))).add_service(SchemaServiceServer::new(Svc(envs)));
        let incoming = futures::stream::unfold(l, |l| async move { Some((l.accept().await.map(|(s, _)| s), l)) });
        let _ = tonic::transport::Server::builder().accept_http1(true).add_routes(routes).serve_with_incoming(incoming).await;
    });
}

#[cfg(test)]
mod kms_t {
    #[test]
    fn names() {
        assert!(super::kms_ok("projects/acme-stg/locations/us-central1/keyRings/app/cryptoKeys/topics"));
        assert!(!super::kms_ok("projects/acme-stg/keyRings/app"));
        assert!(super::region_ok("southamerica-east1") && super::region_ok("us-central1") && !super::region_ok("mars") && !super::region_ok("us-central"));
    }
}
