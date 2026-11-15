//! The Pub/Sub tools: topics with their subscriptions and ack states, messages, publishing as
//! another service would, subscriptions, waiting for what the app published, provocations.

use serde_json::{json, Map, Value};
use stuntman_core::{tools::Args, util, ApiError, Ctx, Tool};

use crate::{config, grpc, schema, store::{self, Labels, Sub}, v1};

/// A full name from "shipments": an existing topic (or subscription) of that name, else one in
/// `project` (or the configured project).
async fn resolve(ctx: &Ctx, a: &Args, key: &str, kind: &str) -> Result<String, ApiError> {
    let n = a.need(key)?;
    if n.starts_with("projects/") {
        return Ok(n);
    }
    if let Some(p) = a.str("project") {
        return Ok(format!("projects/{p}/{kind}/{n}"));
    }
    let all: Vec<String> = if kind == "topics" { store::topics(ctx, "").await?.into_iter().map(|t| t.name).collect() } else { store::subs(ctx, "", None).await?.into_iter().map(|s| s.name).collect() };
    let hits: Vec<&String> = all.iter().filter(|x| x.ends_with(&format!("/{kind}/{n}"))).collect();
    Ok(if hits.len() == 1 { hits[0].clone() } else { format!("projects/{}/{kind}/{n}", config::load(ctx).project) })
}

fn labels(a: &Args, k: &str) -> Labels {
    a.get(k).and_then(|v| v.as_object()).map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().map(String::from).unwrap_or(v.to_string()))).collect()).unwrap_or_default()
}

async fn sub_view(ctx: &Ctx, s: &Sub) -> Result<Value, ApiError> {
    let [pending, outstanding, acked, dead] = store::counts(ctx, &s.name).await?;
    let pb = &s.pb;
    let target = match s.kind() {
        "push" => pb.push_config.as_ref().map(|p| json!({"endpoint": p.push_endpoint, "wrapped": !matches!(p.wrapper, Some(v1::push_config::Wrapper::NoWrapper(_))),
            "oidcServiceAccount": match &p.authentication_method { Some(v1::push_config::AuthenticationMethod::OidcToken(o)) => json!(o.service_account_email), None => Value::Null }})),
        "bigquery" => pb.bigquery_config.as_ref().map(|b| json!({"table": b.table, "useTopicSchema": b.use_topic_schema, "writeMetadata": b.write_metadata})),
        "cloud_storage" => pb.cloud_storage_config.as_ref().map(|c| json!({"bucket": c.bucket, "filenamePrefix": c.filename_prefix, "filenameSuffix": c.filename_suffix, "maxMessages": c.max_messages,
            "maxDurationMs": store::dur_ms(&c.max_duration), "format": if matches!(c.output_format, Some(v1::cloud_storage_config::OutputFormat::AvroConfig(_))) { "avro" } else { "text" }})),
        "bigtable" => pb.bigtable_config.as_ref().map(|b| json!({"table": b.table, "writeMetadata": b.write_metadata})),
        _ => None,
    };
    let last = store::last_seen(ctx, &s.name).await?;
    let err = store::error_of(ctx, &s.name).await?;
    Ok(json!({"name": s.name, "topic": s.topic, "type": s.kind(), "delivery": target, "filter": (!pb.filter.is_empty()).then_some(&pb.filter), "ackDeadlineSeconds": s.ack_deadline,
        "messageOrdering": s.ordering, "exactlyOnce": s.exactly_once(), "detached": pb.detached, "retainAcked": pb.retain_acked_messages, "retentionMs": s.retention_ms(),
        "deadLetterTopic": (!s.dead_letter.is_empty()).then_some(&s.dead_letter), "maxDeliveryAttempts": (!s.dead_letter.is_empty()).then_some(s.max_attempts),
        "unacked": pending + outstanding, "pending": pending, "outstanding": outstanding, "acked": acked, "deadLettered": dead,
        "transforms": udf_names(&pb.message_transforms), "retryBackoffMs": pb.retry_policy.as_ref().map(|r| [store::dur_ms(&r.minimum_backoff), store::dur_ms(&r.maximum_backoff)]),
        "lastActiveAt": last, "expiresAt": s.ttl_ms().map(|t| last + t), "lastError": err.map(|(e, at)| json!({"error": e, "at": at}))}))
}

fn udf_names(t: &[v1::MessageTransform]) -> Vec<String> {
    crate::udf::steps(t).iter().map(|u| u.name()).collect()
}

fn topic_view(t: &v1::Topic) -> Value {
    let ss = t.schema_settings.as_ref();
    json!({"name": t.name, "labels": t.labels.iter().collect::<std::collections::BTreeMap<_, _>>(),
        "schema": ss.map(|s| json!({"name": s.schema, "encoding": if s.encoding == schema::BINARY { "BINARY" } else { "JSON" }, "firstRevision": s.first_revision_id, "lastRevision": s.last_revision_id})),
        "retentionMs": store::dur_ms(&t.message_retention_duration), "kmsKeyName": (!t.kms_key_name.is_empty()).then_some(&t.kms_key_name),
        "allowedRegions": t.message_storage_policy.as_ref().map(|p| &p.allowed_persistence_regions), "ingestion": crate::ingest::describe(t), "transforms": udf_names(&t.message_transforms)})
}

fn udf_of(a: &Args) -> Vec<v1::MessageTransform> {
    match (a.str("udf_code"), a.str("udf_function")) {
        (Some(code), Some(function_name)) => vec![v1::MessageTransform { transform: Some(v1::message_transform::Transform::JavascriptUdf(v1::JavaScriptUdf { function_name, code })), ..Default::default() }],
        _ => vec![],
    }
}

/// {kind, …} as the topic's ingestion settings.
fn ingestion_of(v: &Value) -> Result<v1::IngestionDataSourceSettings, ApiError> {
    use v1::ingestion_data_source_settings::{self as i, Source};
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let src = match v["kind"].as_str().unwrap_or("") {
        "aws_kinesis" => Source::AwsKinesis(i::AwsKinesis { stream_arn: s("stream_arn"), consumer_arn: s("consumer_arn"), aws_role_arn: s("aws_role_arn"), gcp_service_account: s("gcp_service_account"), ..Default::default() }),
        "cloud_storage" => Source::CloudStorage(i::CloudStorage { bucket: s("bucket"), match_glob: s("match_glob"),
            input_format: Some(match v["format"].as_str().unwrap_or("text") {
                "avro" => i::cloud_storage::InputFormat::AvroFormat(Default::default()),
                "pubsub_avro" => i::cloud_storage::InputFormat::PubsubAvroFormat(Default::default()),
                _ => i::cloud_storage::InputFormat::TextFormat(i::cloud_storage::TextFormat { delimiter: v["delimiter"].as_str().map(String::from) }),
            }), ..Default::default() }),
        "azure_event_hubs" => Source::AzureEventHubs(i::AzureEventHubs { namespace: s("namespace"), event_hub: s("event_hub"), resource_group: s("resource_group"), client_id: s("client_id"),
            tenant_id: s("tenant_id"), subscription_id: s("subscription_id"), gcp_service_account: s("gcp_service_account"), ..Default::default() }),
        "aws_msk" => Source::AwsMsk(i::AwsMsk { cluster_arn: s("cluster_arn"), topic: s("topic"), aws_role_arn: s("aws_role_arn"), gcp_service_account: s("gcp_service_account"), ..Default::default() }),
        "confluent_cloud" => Source::ConfluentCloud(i::ConfluentCloud { bootstrap_server: s("bootstrap_server"), cluster_id: s("cluster_id"), topic: s("topic"), identity_pool_id: s("identity_pool_id"),
            gcp_service_account: s("gcp_service_account"), ..Default::default() }),
        _ => return Err(ApiError::bad("ingestion.kind: aws_kinesis | cloud_storage | azure_event_hubs | aws_msk | confluent_cloud")),
    };
    Ok(v1::IngestionDataSourceSettings { source: Some(src), ..Default::default() })
}

fn host_hint(ctx: &Ctx) -> Value {
    json!({"PUBSUB_EMULATOR_HOST": format!("localhost:{}", crate::port()), "environment": if ctx.prefix().is_empty() { Value::Null } else { json!(format!("send gRPC metadata x-stuntman-env: {}", ctx.env)) }})
}

fn kind_of(t: &str) -> Result<i32, ApiError> {
    match t.to_lowercase().as_str() { "avro" => Ok(schema::AVRO), "protobuf" | "protocol_buffer" | "proto" => Ok(schema::PROTOCOL_BUFFER), _ => Err(ApiError::bad("type: avro | protobuf")) }
}

pub fn all() -> Vec<Tool> {
    vec![
        Tool::new("list_pubsub_topics", "The Pub/Sub topics of this environment (schema, retention, labels) with their subscriptions (pull, push, BigQuery or Cloud Storage; filter, ack deadline, ordering, exactly-once, dead-letter) and how many messages each has pending, outstanding, acked and dead-lettered; snapshots, schemas and how apps connect.", |ctx, _a| async move {
            let mut out = vec![];
            for t in store::topics(&ctx, "").await? {
                let mut subs = vec![];
                for s in store::subs(&ctx, "", Some(&t.name)).await? {
                    subs.push(sub_view(&ctx, &s).await?);
                }
                let mut v = topic_view(&t);
                v["messages"] = json!(store::message_count(&ctx, &t.name).await?);
                v["subscriptions"] = json!(subs);
                out.push(v);
            }
            let mut detached = vec![];
            for s in store::subs(&ctx, "", Some(store::DELETED_TOPIC)).await? {
                detached.push(sub_view(&ctx, &s).await?);
            }
            let snapshots: Vec<Value> = store::snapshots(&ctx, "", None).await?.into_iter().map(|s| json!({"name": s.name, "topic": s.topic, "subscription": s.sub, "createdAt": s.created,
                "expiresAt": s.expire, "unackedAtSnapshot": s.msgs.len(), "labels": s.labels})).collect();
            let schemas: Vec<Value> = store::schemas(&ctx, "").await?.into_iter().map(|r| json!({"name": r.name, "type": if r.kind == schema::AVRO { "AVRO" } else { "PROTOCOL_BUFFER" },
                "revision": r.revision, "definition": r.definition, "createdAt": r.created})).collect();
            let c = config::load(&ctx);
            let (bt, bq): (Vec<_>, Vec<_>) = store::bq_tables(&ctx).await?.into_iter().partition(|(t, _)| t.starts_with("bigtable:"));
            let tables: Vec<Value> = bq.into_iter().map(|(t, s)| json!({"table": t, "schema": s})).collect();
            let bt_tables: Vec<Value> = bt.into_iter().map(|(t, s)| json!({"table": t.trim_start_matches("bigtable:"), "rowKeySchema": s})).collect();
            Ok(json!({"topics": out, "detachedSubscriptions": detached, "snapshots": snapshots, "schemas": schemas, "bigqueryTables": tables, "bigtableTables": bt_tables, "connect": host_hint(&ctx), "project": c.project,
                "provoke": (!c.provoke.is_empty()).then(|| json!({"kind": c.provoke, "remaining": if c.times == 0 { Value::Null } else { json!(c.times) }, "topic": c.topic, "delayMs": c.delay_ms})),
                "provocations": config::PROVOKE.iter().map(|p| json!({"kind": p.0, "label": p.1})).collect::<Vec<_>>()}))
        })
        .route("GET", "/pubsub/topics"),
        Tool::new("list_pubsub_messages", "The newest messages of a topic with their state in each subscription (pending, outstanding, expired, acked, dead), or of one subscription: data (JSON or text), attributes, ordering key, delivery attempts, who published.", |ctx, a| async move {
            let limit = a.u64("limit").unwrap_or(50) as i64;
            if a.get("subscription").is_some() {
                let s = resolve(&ctx, &a, "subscription", "subscriptions").await?;
                let sub = store::sub(&ctx, &s).await?.ok_or_else(|| ApiError::not_found(format!("No subscription {s}")))?;
                return Ok(json!({"subscription": sub_view(&ctx, &sub).await?, "messages": store::deliveries(&ctx, &s, limit).await?}));
            }
            let t = resolve(&ctx, &a, "topic", "topics").await?;
            store::topic(&ctx, &t).await?.ok_or_else(|| ApiError::not_found(format!("No topic {t}")))?;
            Ok(json!({"topic": t, "messages": store::messages(&ctx, &t, 0, limit).await?}))
        })
        .param("topic", "shipments (or projects/acme-stg/topics/shipments)").param("subscription", "or one subscription").param("project", "when the short name is ambiguous").param("limit", "50")
        .route("GET", "/pubsub/messages"),
        Tool::new("pubsub_publish", "Publishes a message to a topic as another service would (the app's subscribers receive it): data (JSON or text), attributes, ordering_key. Makes the topic when it does not exist. A topic with a schema validates the data, as Pub/Sub does.", |ctx, a| async move {
            let t = resolve(&ctx, &a, "topic", "topics").await?;
            if store::topic(&ctx, &t).await?.is_none() {
                if !t.starts_with("projects/") || t.split('/').count() != 4 { return Err(ApiError::bad("topic: projects/<project>/topics/<name>")) }
                store::create_topic(&ctx, &store::bare(&t)).await?;
            }
            let data = match a.get("data") { Some(Value::String(s)) => s.clone().into_bytes(), Some(v) => v.to_string().into_bytes(), None => vec![] };
            let mut attrs = labels(&a, "attributes");
            if data.is_empty() && attrs.is_empty() { return Err(ApiError::bad("data or attributes")) }
            let topic = store::topic(&ctx, &t).await?.unwrap_or_else(|| store::bare(&t));
            let by = a.str("_by").unwrap_or("you".into());
            let m = v1::PubsubMessage { data, attributes: std::mem::take(&mut attrs).into_iter().collect(), ordering_key: a.str("ordering_key").unwrap_or_default(), ..Default::default() };
            let ids = grpc::accept(&ctx, &topic, vec![m], &by).await.map_err(|e| ApiError::bad(e.message().to_string()))?;
            Ok(json!({"topic": t, "messageId": ids[0]}))
        })
        .req("topic", "shipments").param("data", "JSON or text").param("attributes", "{name: value}").param("ordering_key", "the ordering key").param("project", "acme-stg")
        .route("POST", "/pubsub/topics/{topic}/publish"),
        Tool::new("create_pubsub_subscription", "Creates a subscription on a topic (the topic too when missing): pull by default; push (push_endpoint, unwrapped, write_metadata, oidc_service_account), BigQuery (bigquery_table, use_topic_schema, write_metadata), Cloud Storage (bucket, filename_prefix, max_messages, max_duration, avro) or Bigtable (bigtable_table); filter, JavaScript UDF, ack deadline, message ordering, exactly-once, expiration ttl, retry backoff, a dead-letter topic with max delivery attempts.", |ctx, a| async move {
            let s = resolve(&ctx, &a, "subscription", "subscriptions").await?;
            let t = resolve(&ctx, &a, "topic", "topics").await?;
            for (n, k) in [(&s, "subscriptions"), (&t, "topics")] {
                let p: Vec<&str> = n.split('/').collect();
                if p.len() != 4 || p[0] != "projects" || p[2] != k { return Err(ApiError::bad(format!("{n}: projects/<project>/{k}/<name>"))) }
            }
            store::create_topic(&ctx, &store::bare(&t)).await?;
            let dl = match a.str("dead_letter_topic") { Some(_) => resolve(&ctx, &a, "dead_letter_topic", "topics").await?, None => String::new() };
            if !dl.is_empty() { store::create_topic(&ctx, &store::bare(&dl)).await?; }
            let meta = a.bool("write_metadata").unwrap_or(false);
            let mut pb = v1::Subscription { name: s.clone(), topic: t, ack_deadline_seconds: a.u64("ack_deadline_seconds").unwrap_or(10) as i32, enable_message_ordering: a.bool("ordering").unwrap_or(false),
                filter: a.str("filter").unwrap_or_default(), enable_exactly_once_delivery: a.bool("exactly_once").unwrap_or(false), retain_acked_messages: a.bool("retain_acked").unwrap_or(false),
                dead_letter_policy: (!dl.is_empty()).then(|| v1::DeadLetterPolicy { dead_letter_topic: dl, max_delivery_attempts: a.u64("max_delivery_attempts").unwrap_or(5) as i32 }),
                labels: labels(&a, "labels").into_iter().collect(), ..Default::default() };
            if let Some(e) = a.str("push_endpoint") {
                pb.push_config = Some(v1::PushConfig { push_endpoint: e,
                    wrapper: Some(if a.bool("unwrapped").unwrap_or(false) { v1::push_config::Wrapper::NoWrapper(v1::push_config::NoWrapper { write_metadata: meta }) } else { v1::push_config::Wrapper::PubsubWrapper(v1::push_config::PubsubWrapper {}) }),
                    authentication_method: a.str("oidc_service_account").map(|sa| v1::push_config::AuthenticationMethod::OidcToken(v1::push_config::OidcToken { service_account_email: sa, audience: a.str("oidc_audience").unwrap_or_default() })),
                    ..Default::default() });
            }
            if let Some(tb) = a.str("bigquery_table") {
                pb.bigquery_config = Some(v1::BigQueryConfig { table: tb, use_topic_schema: a.bool("use_topic_schema").unwrap_or(false), write_metadata: meta, ..Default::default() });
            }
            if let Some(tb) = a.str("bigtable_table") {
                pb.bigtable_config = Some(v1::BigtableConfig { table: tb, write_metadata: meta, ..Default::default() });
            }
            if let Some(ttl) = a.str("expiration_ttl") {
                pb.expiration_policy = Some(v1::ExpirationPolicy { ttl: if ttl == "never" { None } else { Some(store::dur(util::parse_ms(&ttl).ok_or_else(|| ApiError::bad("expiration_ttl: like 1d, or never"))? as i64)) } });
            }
            if a.get("min_backoff").is_some() || a.get("max_backoff").is_some() {
                pb.retry_policy = Some(v1::RetryPolicy { minimum_backoff: a.ms("min_backoff")?.map(|m| store::dur(m as i64)), maximum_backoff: a.ms("max_backoff")?.map(|m| store::dur(m as i64)) });
            }
            pb.message_transforms = udf_of(&a);
            if let Some(b) = a.str("bucket") {
                pb.cloud_storage_config = Some(v1::CloudStorageConfig { bucket: b, filename_prefix: a.str("filename_prefix").unwrap_or_default(), filename_suffix: a.str("filename_suffix").unwrap_or_default(),
                    max_messages: a.u64("max_messages").unwrap_or(0) as i64, max_duration: a.ms("max_duration")?.map(|m| store::dur(m as i64)),
                    output_format: Some(if a.bool("avro").unwrap_or(false) { v1::cloud_storage_config::OutputFormat::AvroConfig(v1::cloud_storage_config::AvroConfig { write_metadata: meta, use_topic_schema: false }) } else { v1::cloud_storage_config::OutputFormat::TextConfig(v1::cloud_storage_config::TextConfig {}) }),
                    ..Default::default() });
            }
            let sub = grpc::sub_settings(&ctx, pb, true).await.map_err(|e| ApiError::bad(e.message().to_string()))?;
            let made = store::create_sub(&ctx, &sub).await?;
            if made { store::seen(&ctx, &sub.name).await?; }
            Ok(json!({"subscription": sub_view(&ctx, &store::sub(&ctx, &s).await?.unwrap_or(sub)).await?, "created": made}))
        })
        .req("subscription", "shipments-billing").req("topic", "shipments").param("filter", "attributes.region = \"us-east\"").param("ack_deadline_seconds", "10").param("ordering", "bool")
        .param("exactly_once", "bool").param("retain_acked", "bool").param("dead_letter_topic", "shipments-dlq").param("max_delivery_attempts", "5")
        .param("push_endpoint", "http://localhost:5000/pubsub/push").param("unwrapped", "bool: the raw data as the body").param("write_metadata", "bool").param("oidc_service_account", "push-invoker@acme-stg.iam.gserviceaccount.com").param("oidc_audience", "the token's aud")
        .param("bigquery_table", "acme-stg.logistics.shipments").param("use_topic_schema", "bool").param("bucket", "acme-stg-shipments-archive").param("filename_prefix", "shipments/").param("filename_suffix", ".txt")
        .param("max_messages", "per file").param("max_duration", "5m").param("avro", "bool: Avro files instead of text").param("bigtable_table", "projects/acme-stg/instances/ops/tables/shipments")
        .param("expiration_ttl", "31d | never").param("min_backoff", "10s").param("max_backoff", "600s").param("udf_code", "function redact(message, metadata) { … }").param("udf_function", "redact")
        .param("labels", "{name: value}").param("project", "acme-stg")
        .route("PUT", "/pubsub/subscriptions/{subscription}"),
        Tool::new("set_pubsub_schema", "Creates a Pub/Sub schema (Avro or Protocol Buffer) or commits a new revision of it; with topic (and encoding JSON or BINARY), the topic validates every publish against it.", |ctx, a| async move {
            let n = a.need("schema")?;
            let name = if n.starts_with("projects/") { n } else { format!("projects/{}/schemas/{n}", a.str("project").unwrap_or(config::load(&ctx).project)) };
            let revs = store::revs(&ctx, &name).await?;
            let kind = match a.str("type") { Some(t) => kind_of(&t)?, None => revs.first().map(|r| r.kind).ok_or_else(|| ApiError::bad("type: avro | protobuf"))? };
            if revs.first().is_some_and(|r| r.kind != kind) { return Err(ApiError::bad("A new revision must keep the schema's type")) }
            let def = match a.get("definition") { Some(Value::String(s)) => s.clone(), Some(v) => v.to_string(), None => return Err(ApiError::bad("definition")) };
            schema::check(kind, &def).map_err(|e| ApiError::bad(format!("Invalid schema definition: {e}")))?;
            let rev = store::add_rev(&ctx, &name, kind, &def).await?;
            if a.get("topic").is_some() {
                let t = resolve(&ctx, &a, "topic", "topics").await?;
                let mut topic = store::topic(&ctx, &t).await?.ok_or_else(|| ApiError::not_found(format!("No topic {t}")))?;
                let enc = if a.str("encoding").is_some_and(|e| e.eq_ignore_ascii_case("binary")) { schema::BINARY } else { schema::JSON };
                topic.schema_settings = Some(v1::SchemaSettings { schema: name.clone(), encoding: enc, ..Default::default() });
                store::save_topic(&ctx, &topic).await?;
            }
            Ok(json!({"schema": name, "revision": rev.revision, "revisions": revs.len() + 1}))
        })
        .req("schema", "shipment-event").param("type", "avro | protobuf").req("definition", "the Avro JSON or the .proto text").param("topic", "shipments").param("encoding", "JSON | BINARY").param("project", "acme-stg")
        .route("PUT", "/pubsub/schemas/{schema}"),
        Tool::new("list_pubsub_bigquery_rows", "The rows BigQuery subscriptions wrote (Stuntman keeps the tables): one per message, the topic schema's fields with use_topic_schema, else data; subscription_name, message_id, publish_time and attributes with write_metadata.", |ctx, a| async move {
            let sub = match a.get("subscription") { Some(_) => Some(resolve(&ctx, &a, "subscription", "subscriptions").await?), None => None };
            Ok(json!({"rows": store::bq_rows(&ctx, a.str("table"), sub, a.u64("limit").unwrap_or(50) as i64).await?}))
        })
        .param("table", "acme-stg.logistics.shipments").param("subscription", "shipments-bq").param("limit", "50")
        .route("GET", "/pubsub/bigquery"),
        Tool::new("list_pubsub_storage_files", "The files Cloud Storage subscriptions wrote: bucket, object name, messages, format (text or Avro) and content; storedIn names the Stuntman storage account when one has that bucket.", |ctx, a| async move {
            let sub = match a.get("subscription") { Some(_) => Some(resolve(&ctx, &a, "subscription", "subscriptions").await?), None => None };
            Ok(json!({"files": store::files(&ctx, a.str("bucket"), sub, a.u64("limit").unwrap_or(20) as i64).await?}))
        })
        .param("bucket", "acme-stg-shipments-archive").param("subscription", "shipments-archive").param("limit", "20")
        .route("GET", "/pubsub/storage"),
        Tool::new("list_pubsub_bigtable_rows", "The rows Bigtable subscriptions wrote (Stuntman keeps the tables): row key subscription#salt#message_id (or the delimited key), the data in family data (qualifier \"\"), and with write_metadata the pubsub_metadata family (subscription_name, message_id, publish_time, attributes, ordering_key).", |ctx, a| async move {
            let sub = match a.get("subscription") { Some(_) => Some(resolve(&ctx, &a, "subscription", "subscriptions").await?), None => None };
            Ok(json!({"rows": store::bt_rows(&ctx, a.str("table"), sub, a.u64("limit").unwrap_or(50) as i64).await?}))
        })
        .param("table", "projects/acme-stg/instances/ops/tables/shipments").param("subscription", "shipments-bt").param("limit", "50")
        .route("GET", "/pubsub/bigtable"),
        Tool::new("set_pubsub_bigtable_table", "Defines a Bigtable table's row key schema for Bigtable subscriptions with row_key_schema: fields [{name, type STRING | INT64 | BYTES}] taken from the message's JSON, encoding singleton, delimited_bytes (delimiter, default #) or ordered_code_bytes.", |ctx, a| async move {
            let t = a.need("table")?;
            let sc = a.get("row_key_schema").cloned().ok_or_else(|| ApiError::bad("row_key_schema: {fields, encoding, delimiter}"))?;
            if sc["fields"].as_array().is_none_or(|f| f.is_empty() || f.iter().any(|x| x["name"].as_str().is_none())) { return Err(ApiError::bad("row_key_schema.fields: [{name, type}]")) }
            if !["singleton", "delimited_bytes", "ordered_code_bytes"].contains(&sc["encoding"].as_str().unwrap_or("delimited_bytes")) { return Err(ApiError::bad("encoding: singleton | delimited_bytes | ordered_code_bytes")) }
            store::set_bq_table(&ctx, &format!("bigtable:{t}"), &sc).await?;
            Ok(json!({"table": t, "rowKeySchema": sc}))
        })
        .req("table", "projects/acme-stg/instances/ops/tables/orders").req("row_key_schema", "{\"fields\": [{\"name\": \"region\", \"type\": \"STRING\"}], \"encoding\": \"delimited_bytes\", \"delimiter\": \"#\"}")
        .route("PUT", "/pubsub/bigtable/tables/{table}"),
        Tool::new("set_pubsub_bigquery_table", "Defines a BigQuery table's schema for BigQuery subscriptions: columns [{name, type, mode}] (STRING, BYTES, INTEGER, FLOAT, NUMERIC, BOOLEAN, TIMESTAMP, DATE, JSON, RECORD; NULLABLE, REQUIRED, REPEATED). With it, use_table_schema maps the message's JSON fields to columns, drop_unknown_fields drops extras, and a row that does not fit is not written (retried, then dead-lettered).", |ctx, a| async move {
            let t = a.need("table")?;
            let cols = a.get("schema").and_then(|v| v.as_array()).cloned().ok_or_else(|| ApiError::bad("schema: [{name, type, mode}]"))?;
            if cols.iter().any(|c| c["name"].as_str().is_none_or(|n| n.is_empty())) { return Err(ApiError::bad("every column needs a name")) }
            store::set_bq_table(&ctx, &t, &json!(cols)).await?;
            Ok(json!({"table": t, "columns": cols.len()}))
        })
        .req("table", "acme-stg.logistics.shipments").req("schema", "[{\"name\": \"shipment\", \"type\": \"STRING\", \"mode\": \"REQUIRED\"}]")
        .route("PUT", "/pubsub/bigquery/tables/{table}"),
        Tool::new("set_pubsub_topic", "Sets a topic's ingestion source (kind aws_kinesis, cloud_storage, azure_event_hubs, aws_msk, confluent_cloud with that source's fields) and/or a JavaScript UDF transform (udf_code, udf_function), making the topic when missing. Cloud Storage reads a Stuntman storage container of the bucket's name; MSK and Confluent read the Streams (Kafka) topic; Kinesis reads the Stuntman Kinesis stream its stream_arn names, from TRIM_HORIZON.", |ctx, a| async move {
            let t = resolve(&ctx, &a, "topic", "topics").await?;
            store::create_topic(&ctx, &store::bare(&t)).await?;
            let mut topic = store::topic(&ctx, &t).await?.ok_or_else(|| ApiError::not_found(format!("No topic {t}")))?;
            if let Some(i) = a.get("ingestion") { topic.ingestion_data_source_settings = if i.is_null() { None } else { Some(ingestion_of(i)?) }; }
            if a.get("udf_code").is_some() { topic.message_transforms = udf_of(&a); }
            crate::udf::check(&topic.message_transforms).map_err(ApiError::bad)?;
            crate::ingest::set_state(&ctx, &mut topic).await?;
            store::save_topic(&ctx, &topic).await?;
            Ok(topic_view(&topic))
        })
        .req("topic", "shipments").param("ingestion", "{\"kind\": \"aws_msk\", \"cluster_arn\": \"…\", \"topic\": \"shipments\"} or null").param("udf_code", "function f(message, metadata) { return message; }")
        .param("udf_function", "f").param("project", "acme-stg")
        .route("PUT", "/pubsub/topics/{topic}"),
        Tool::new("pubsub_ingest", "Feeds records into an import topic as its source would: records [{data, key, attributes}] — Kafka (MSK, Confluent): value → data, key → ordering key, headers → attributes; Kinesis: partition key → ordering key; Event Hubs: partition key → ordering key, properties → attributes; Cloud Storage: each record one text line. They go through the topic's transforms and schema.", |ctx, a| async move {
            let t = resolve(&ctx, &a, "topic", "topics").await?;
            let topic = store::topic(&ctx, &t).await?.ok_or_else(|| ApiError::not_found(format!("No topic {t}")))?;
            if topic.ingestion_data_source_settings.is_none() { return Err(ApiError::bad(format!("{t} has no ingestion source: set_pubsub_topic(ingestion)"))) }
            let recs = a.get("records").and_then(|v| v.as_array()).cloned().ok_or_else(|| ApiError::bad("records: [{data, key, attributes}]"))?;
            let recs = recs.iter().map(|r| crate::ingest::Rec {
                data: match &r["data"] { Value::String(s) => s.clone().into_bytes(), Value::Null => vec![], v => v.to_string().into_bytes() },
                key: r["key"].as_str().or(r["partition_key"].as_str()).unwrap_or_default().to_string(),
                attrs: r["attributes"].as_object().or(r["headers"].as_object()).or(r["properties"].as_object()).map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().map(String::from).unwrap_or(v.to_string()))).collect()).unwrap_or_default(),
            }).collect();
            let ids = crate::ingest::feed(&ctx, &topic, recs).await?;
            Ok(json!({"topic": t, "source": crate::ingest::describe(&topic), "messageIds": ids}))
        })
        .req("topic", "shipments").req("records", "[{\"data\": {\"shipment\": \"SHP-1\"}, \"key\": \"SHP-1\", \"attributes\": {}}]").param("project", "acme-stg")
        .route("POST", "/pubsub/topics/{topic}/ingest"),
        Tool::new("wait_for_pubsub_message", "Waits for the first message the app published (not you or an agent) on a topic that matches: text in the data, attributes, ordering key. Looks back 15 s by default; 408 after the timeout.", |ctx, a| async move {
            let t = resolve(&ctx, &a, "topic", "topics").await?;
            let since = a.u64("since").map(|x| x as i64).unwrap_or_else(|| util::now_ms() - 15_000);
            let want = labels(&a, "attributes");
            let contains = a.str("contains");
            let key = a.str("ordering_key");
            let end = util::now_ms() + a.ms("timeout")?.unwrap_or(30_000) as i64;
            loop {
                let bell = crate::bell().notified();
                let rows = store::messages(&ctx, &t, since, 500).await?;
                if let Some(m) = rows.iter().rev().find(|m| {
                    m["publisher"].as_str().is_some_and(|p| p.starts_with("app:"))
                        && want.iter().all(|(k, v)| m["attributes"][k].as_str() == Some(v))
                        && key.as_ref().is_none_or(|k| m["orderingKey"].as_str() == Some(k))
                        && contains.as_ref().is_none_or(|c| { let d = &m["data"]["value"]; d.as_str().map(String::from).unwrap_or(d.to_string()).contains(c.as_str()) })
                }) {
                    return Ok(m.clone());
                }
                if util::now_ms() > end {
                    return Err(ApiError::new(408, format!("No matching message on {t} yet")).with(json!({"seen": rows.len()})));
                }
                tokio::select! { _ = bell => {} _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {} }
            }
        })
        .req("topic", "shipments").param("contains", "text in the data").param("attributes", "{name: value}").param("ordering_key", "the key").param("since", "ms epoch").param("timeout", "30s").param("project", "acme-stg")
        .route("GET", "/pubsub/topics/{topic}/wait"),
        Tool::new("set_pubsub", "Pub/Sub settings: project (for short names) and a provocation on Publish: unavailable, deadline_exceeded, resource_exhausted, not_found, slow_publish (delay_ms), duplicate (each message delivered again after its first ack); times = for the next N publishes (0 = until cleared), topic = only that topic; none clears.", |ctx, a| async move {
            let mut c = config::load(&ctx);
            if let Some(p) = a.str("project") { c.project = p; }
            if let Some(p) = a.str("provoke") {
                if p != "none" && !config::PROVOKE.iter().any(|x| x.0 == p) { return Err(ApiError::bad(format!("provoke: {} | none", config::PROVOKE.iter().map(|x| x.0).collect::<Vec<_>>().join(" | ")))) }
                c.provoke = if p == "none" { String::new() } else { p };
                c.times = a.u64("times").unwrap_or(0) as u32;
                c.topic = a.str("topic").unwrap_or_default();
            }
            if let Some(d) = a.ms("delay_ms")? { c.delay_ms = d; }
            config::save(&ctx, &c)?;
            ctx.live.send("pubsub.config", "", "updated");
            let mut out = Map::new();
            out.insert("project".into(), json!(c.project));
            out.insert("provoke".into(), json!(c.provoke));
            out.insert("times".into(), json!(c.times));
            out.insert("topic".into(), json!(c.topic));
            out.insert("delayMs".into(), json!(c.delay_ms));
            Ok(json!({"config": out}))
        })
        .param("project", "acme-stg").param("provoke", "unavailable | deadline_exceeded | resource_exhausted | not_found | slow_publish | duplicate | none").param("times", "next N publishes (0 = until cleared)")
        .param("topic", "only this topic").param("delay_ms", "slow_publish: 3000")
        .route("PUT", "/pubsub/config"),
    ]
}

#[cfg(test)]
mod t {
    use std::sync::Arc;

    use stuntman_core::{callback::Signer, env::{EnvFile, EnvState, Envs}, live::Live, store::Db};

    use super::*;

    fn ctx() -> Ctx {
        let dir = std::env::temp_dir().join(util::id("pubsub-test"));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::memory();
        db.migrate(store::SQL).unwrap();
        Ctx { env: "t".into(), state: Arc::new(EnvState::new(EnvFile::new("t"))), envs: Envs::default(), db, live: Live::default(), data_dir: Arc::new(dir.clone()),
            http: reqwest::Client::new(), signer: Arc::new(Signer::load_or_create(&dir.join("k.pem")).unwrap()), host: "localhost".into() }
    }

    #[tokio::test]
    async fn deliver_ack_redeliver_ordering_dead_letter() {
        let c = ctx();
        let t = "projects/acme-stg/topics/shipments";
        store::create_topic(&c, &store::bare(t)).await.unwrap();
        store::create_topic(&c, &store::bare("projects/acme-stg/topics/shipments-dlq")).await.unwrap();
        let s = Sub { name: "projects/acme-stg/subscriptions/billing".into(), topic: t.into(), ack_deadline: 10, ordering: true, dead_letter: "projects/acme-stg/topics/shipments-dlq".into(), max_attempts: 2, ..Default::default() };
        store::create_sub(&c, &s).await.unwrap();
        let m = |d: &str, k: &str| (d.as_bytes().to_vec(), Labels::from([("kind".to_string(), "created".to_string())]), k.to_string());
        store::publish(&c, t, vec![m("a1", "A"), m("a2", "A"), m("b1", "B")], "app:test").await.unwrap();

        // Every message once, in order per key.
        let got = store::pull(&c, &s, 10, 10).await.unwrap();
        assert_eq!(got.iter().map(|g| String::from_utf8(g.msg.data.clone()).unwrap()).collect::<Vec<_>>(), ["a1", "a2", "b1"]);
        // Nothing more while they are out; key A blocked.
        assert!(store::pull(&c, &s, 10, 10).await.unwrap().is_empty());
        // Ack b1, nack a1: only a1 and a2 (A unblocked? a2 still out, so A stays blocked).
        store::ack(&c, &s.name, vec![got[2].ack_id.clone()], false, false).await.unwrap();
        store::modack(&c, &s, vec![(got[0].ack_id.clone(), 0)]).await.unwrap();
        assert!(store::pull(&c, &s, 10, 10).await.unwrap().is_empty(), "a2 is still out: key A waits");
        store::modack(&c, &s, vec![(got[1].ack_id.clone(), 0)]).await.unwrap();
        let again = store::pull(&c, &s, 10, 10).await.unwrap();
        assert_eq!(again.iter().map(|g| (g.msg.id, g.attempt)).collect::<Vec<_>>(), [(got[0].msg.id, 2), (got[1].msg.id, 2)]);
        // A third delivery would pass max_attempts=2: both go to the dead-letter topic.
        store::modack(&c, &s, again.iter().map(|g| (g.ack_id.clone(), 0)).collect()).await.unwrap();
        assert!(store::pull(&c, &s, 10, 10).await.unwrap().is_empty());
        assert_eq!(store::counts(&c, &s.name).await.unwrap(), [0, 0, 1, 2]);
        assert_eq!(store::message_count(&c, "projects/acme-stg/topics/shipments-dlq").await.unwrap(), 2);

        // duplicate: the first ack does not stick.
        let plain = Sub { name: "projects/acme-stg/subscriptions/audit".into(), topic: t.into(), ack_deadline: 10, ..Default::default() };
        store::create_sub(&c, &plain).await.unwrap();
        store::publish(&c, t, vec![m("c1", "")], "app:test").await.unwrap();
        let g = store::pull(&c, &plain, 10, 10).await.unwrap();
        store::ack(&c, &plain.name, vec![g[0].ack_id.clone()], true, false).await.unwrap();
        let g2 = store::pull(&c, &plain, 10, 10).await.unwrap();
        assert_eq!((g2.len(), g2[0].msg.id), (1, g[0].msg.id));
        store::ack(&c, &plain.name, vec![g2[0].ack_id.clone()], true, false).await.unwrap();
        assert_eq!(store::counts(&c, &plain.name).await.unwrap(), [0, 0, 1, 0]);

        // Deleting the topic detaches its subscriptions.
        store::delete_topic(&c, t).await.unwrap();
        assert_eq!(store::sub(&c, &plain.name).await.unwrap().unwrap().topic, store::DELETED_TOPIC);
    }

    fn sub(name: &str, topic: &str, f: impl FnOnce(&mut v1::Subscription)) -> Sub {
        let mut pb = v1::Subscription { name: format!("projects/acme-stg/subscriptions/{name}"), topic: topic.into(), ..Default::default() };
        f(&mut pb);
        Sub::from_pb(pb)
    }

    fn texts(g: &[store::Got]) -> Vec<String> {
        g.iter().map(|g| String::from_utf8(g.msg.data.clone()).unwrap()).collect()
    }

    #[tokio::test]
    async fn filters_snapshots_seek_exactly_once() {
        let c = ctx();
        let t = "projects/acme-stg/topics/shipments";
        store::create_topic(&c, &store::bare(t)).await.unwrap();
        let east = sub("east", t, |p| p.filter = r#"attributes.region = "us-east" AND NOT attributes:test"#.into());
        let all = sub("all", t, |p| p.enable_exactly_once_delivery = true);
        for s in [&east, &all] { store::create_sub(&c, s).await.unwrap(); }
        let m = |d: &str, r: &str| (d.as_bytes().to_vec(), Labels::from([("region".to_string(), r.to_string())]), String::new());
        store::publish(&c, t, vec![m("e1", "us-east"), m("w1", "us-west")], "app:test").await.unwrap();
        // The filter keeps w1 from east entirely.
        let e_first = store::pull(&c, &east, 10, 10).await.unwrap();
        assert_eq!(texts(&e_first), ["e1"]);
        assert_eq!(store::counts(&c, &east.name).await.unwrap()[..2], [0, 1]);

        // Exactly-once: an ack id of a delivery that was nacked is invalid; the current one acks.
        let g = store::pull(&c, &all, 10, 10).await.unwrap();
        assert_eq!(texts(&g), ["e1", "w1"]);
        store::modack(&c, &all, vec![(g[0].ack_id.clone(), 0)]).await.unwrap();
        let again = store::pull(&c, &all, 10, 10).await.unwrap();
        assert_eq!(store::ack(&c, &all.name, vec![g[0].ack_id.clone(), again[0].ack_id.clone(), g[1].ack_id.clone()], false, true).await.unwrap(), [g[0].ack_id.clone()]);
        assert_eq!(store::counts(&c, &all.name).await.unwrap(), [0, 0, 2, 0]);

        // A snapshot now holds nothing unacked; e2 arrives after it; seeking back replays e2 only.
        let snap = store::create_snapshot(&c, "projects/acme-stg/snapshots/before-e2", &all, &Labels::new()).await.unwrap().unwrap();
        assert!(store::create_snapshot(&c, &snap.name, &all, &Labels::new()).await.unwrap().is_none());
        store::publish(&c, t, vec![m("e2", "us-east")], "app:test").await.unwrap();
        let g = store::pull(&c, &all, 10, 10).await.unwrap();
        store::ack(&c, &all.name, vec![g[0].ack_id.clone()], false, true).await.unwrap();
        store::seek_snapshot(&c, &all, &snap).await.unwrap();
        assert_eq!(texts(&store::pull(&c, &all, 10, 10).await.unwrap()), ["e2"]);
        // A subscription made later can seek to it too (same topic): it gets e2.
        let late = sub("late", t, |_| {});
        store::create_sub(&c, &late).await.unwrap();
        store::seek_snapshot(&c, &late, &snap).await.unwrap();
        assert_eq!(texts(&store::pull(&c, &late, 10, 10).await.unwrap()), ["e2"]);

        // Seek to a time: acked messages replay only when retained.
        let east2 = store::pull(&c, &east, 10, 10).await.unwrap();
        store::ack(&c, &east.name, east2.iter().chain(&e_first).map(|g| g.ack_id.clone()).collect(), false, false).await.unwrap();
        store::seek(&c, &east, 0, None).await.unwrap();
        assert!(store::pull(&c, &east, 10, 10).await.unwrap().is_empty(), "acked, not retained");
        let mut kept = east.clone();
        kept.pb.retain_acked_messages = true;
        store::save_sub(&c, &kept).await.unwrap();
        let kept = store::sub(&c, &east.name).await.unwrap().unwrap();
        store::seek(&c, &kept, 0, None).await.unwrap();
        assert_eq!(texts(&store::pull(&c, &kept, 10, 10).await.unwrap()), ["e1", "e2"]);
        // Topic retention reaches messages from before a subscription existed.
        let fresh = sub("fresh", t, |_| {});
        store::create_sub(&c, &fresh).await.unwrap();
        store::seek(&c, &fresh, 0, Some(store::DAY_MS)).await.unwrap();
        assert_eq!(texts(&store::pull(&c, &fresh, 10, 10).await.unwrap()), ["e1", "w1", "e2"]);
    }

    #[tokio::test]
    async fn bigquery_and_cloud_storage_subscriptions() {
        let c = ctx();
        let t = "projects/acme-stg/topics/shipments";
        let def = r#"{"type": "record", "name": "Shipment", "fields": [{"name": "shipment", "type": "string"}, {"name": "kg", "type": "double"}]}"#;
        let rev = store::add_rev(&c, "projects/acme-stg/schemas/shipment", schema::AVRO, def).await.unwrap();
        let mut topic = store::bare(t);
        topic.schema_settings = Some(v1::SchemaSettings { schema: rev.name.clone(), encoding: schema::JSON, ..Default::default() });
        store::create_topic(&c, &topic).await.unwrap();
        let bq = sub("bq", t, |p| p.bigquery_config = Some(v1::BigQueryConfig { table: "acme-stg.logistics.shipments".into(), use_topic_schema: true, write_metadata: true, ..Default::default() }));
        let gcs = sub("gcs", t, |p| p.cloud_storage_config = Some(v1::CloudStorageConfig { bucket: "acme-stg-archive".into(), filename_prefix: "shipments/".into(), max_messages: 2, ..Default::default() }));
        for s in [&bq, &gcs] { store::create_sub(&c, s).await.unwrap(); }
        // Publishing checks the schema and stamps the attributes.
        let mut a = Labels::new();
        assert!(grpc::conform(&c, &topic, br#"{"shipment": "SHP-1"}"#, &mut a).await.is_err());
        grpc::conform(&c, &topic, br#"{"shipment": "SHP-1", "kg": 2.5}"#, &mut a).await.unwrap();
        assert_eq!(a["googclient_schemarevisionid"], rev.revision);
        assert_eq!(a["googclient_schemaencoding"], "JSON");
        store::publish(&c, t, vec![(br#"{"shipment": "SHP-1", "kg": 2.5}"#.to_vec(), a.clone(), String::new())], "app:test").await.unwrap();
        crate::deliver::tick(&c).await.unwrap();
        let rows = store::bq_rows(&c, Some("acme-stg.logistics.shipments".into()), None, 10).await.unwrap();
        assert_eq!((rows[0]["row"]["shipment"].as_str(), rows[0]["row"]["kg"].as_f64(), rows[0]["row"]["subscription_name"].as_str()), (Some("SHP-1"), Some(2.5), Some(bq.name.as_str())));
        assert!(store::files(&c, None, None, 10).await.unwrap().is_empty(), "one message waits for max_messages=2");
        store::publish(&c, t, vec![(br#"{"shipment": "SHP-2", "kg": 1}"#.to_vec(), a, String::new())], "app:test").await.unwrap();
        crate::deliver::tick(&c).await.unwrap();
        let f = store::files(&c, None, None, 10).await.unwrap();
        assert_eq!((f.len(), f[0]["messages"].as_i64(), f[0]["content"].as_str()), (1, Some(2), Some("{\"shipment\": \"SHP-1\", \"kg\": 2.5}\n{\"shipment\": \"SHP-2\", \"kg\": 1}\n")));
        assert!(f[0]["object"].as_str().unwrap().starts_with("shipments/"));
        assert_eq!(store::counts(&c, &gcs.name).await.unwrap(), [0, 0, 2, 0]);
    }

    #[tokio::test]
    async fn backoff_bytes_expiry_transforms_bigtable_tables() {
        let c = ctx();
        let t = "projects/acme-stg/topics/shipments";
        let mut topic = store::bare(t);
        topic.message_transforms = vec![v1::MessageTransform { transform: Some(v1::message_transform::Transform::JavascriptUdf(v1::JavaScriptUdf { function_name: "f".into(),
            code: "function f(m, md) { if (m.data === 'drop') return null; m.attributes.stage = 'topic'; return m; }".into() })), ..Default::default() }];
        store::create_topic(&c, &topic).await.unwrap();
        // Retry policy: a nacked message waits its minimum backoff.
        let r = sub("retry", t, |p| p.retry_policy = Some(v1::RetryPolicy { minimum_backoff: Some(store::dur(300)), maximum_backoff: Some(store::dur(1000)) }));
        let bt = sub("bt", t, |p| p.bigtable_config = Some(v1::BigtableConfig { table: "projects/acme-stg/instances/ops/tables/shipments".into(), write_metadata: true, ..Default::default() }));
        for s in [&r, &bt] { store::create_sub(&c, s).await.unwrap(); }
        let m = |d: &str| v1::PubsubMessage { data: d.as_bytes().to_vec(), ..Default::default() };
        // The topic transform: one dropped (still an id), the others stamped.
        let ids = grpc::accept(&c, &topic, vec![m("aaaa"), m("drop"), m("bbbbbb")], "app:test").await.unwrap();
        assert_eq!(ids.len(), 3);
        assert_eq!(store::message_count(&c, t).await.unwrap(), 2);
        let g = store::pull_bytes(&c, &r, 10, 10, 5).await.unwrap();
        assert_eq!((texts(&g), g[0].msg.attributes["stage"].as_str()), (vec!["aaaa".to_string()], "topic"), "5 bytes: one message");
        assert!(store::pull_bytes(&c, &r, 10, 10, -5).await.unwrap().is_empty(), "strict: bbbbbb does not fit");
        store::modack(&c, &r, vec![(g[0].ack_id.clone(), 0)]).await.unwrap();
        assert_eq!(texts(&store::pull(&c, &r, 10, 10).await.unwrap()), ["bbbbbb"], "aaaa waits its backoff");
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        assert_eq!(texts(&store::pull(&c, &r, 10, 10).await.unwrap()), ["aaaa"]);
        // Bigtable rows.
        crate::deliver::tick(&c).await.unwrap();
        let rows = store::bt_rows(&c, None, Some(bt.name.clone()), 10).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[1]["rowKey"].as_str().unwrap().starts_with(&format!("{}#", bt.name)) && rows[1]["cells"]["data"][""] == "aaaa", "{rows:?}");
        assert_eq!(rows[1]["cells"]["pubsub_metadata"]["message_id"], ids[0].as_str());
        // Expiration: idle past its ttl on the environment clock.
        let mut e = sub("idle", t, |p| p.expiration_policy = Some(v1::ExpirationPolicy { ttl: Some(store::dur(store::DAY_MS)) }));
        e.pb.expiration_policy = Some(v1::ExpirationPolicy { ttl: Some(store::dur(store::DAY_MS)) });
        store::create_sub(&c, &e).await.unwrap();
        store::seen(&c, &e.name).await.unwrap();
        crate::deliver::tick(&c).await.unwrap();
        assert!(store::sub(&c, &e.name).await.unwrap().is_some());
        stuntman_core::clock::advance(&c, 2 * store::DAY_MS);
        crate::deliver::tick(&c).await.unwrap();
        assert!(store::sub(&c, &e.name).await.unwrap().is_none(), "expired after a day idle");
        assert!(store::sub(&c, &r.name).await.unwrap().is_some(), "31 days by default");
        stuntman_core::clock::reset(&c);
        // A BigQuery table schema.
        let cols = vec![json!({"name": "shipment", "type": "STRING", "mode": "REQUIRED"}), json!({"name": "kg", "type": "FLOAT"})];
        let mut row: Map<String, Value> = serde_json::from_value(json!({"shipment": "SHP-1", "kg": 2.5, "color": "red"})).unwrap();
        assert!(crate::deliver::fit_row(&mut row.clone(), &cols, false).unwrap_err().contains("color"));
        crate::deliver::fit_row(&mut row, &cols, true).unwrap();
        assert_eq!(Value::Object(row), json!({"shipment": "SHP-1", "kg": 2.5}));
        assert!(crate::deliver::fit_row(&mut serde_json::from_value(json!({"kg": "heavy"})).unwrap(), &cols, true).is_err());
    }
}
