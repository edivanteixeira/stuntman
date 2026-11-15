//! The emulator port: google.pubsub.v1 Publisher and Subscriber over plaintext gRPC, and on the same
//! port the REST publish (POST /v1/projects/{p}/topics/{t}:publish). gRPC has no path to carry an
//! environment: the x-stuntman-env metadata names it, else the default one answers.

use std::{collections::BTreeMap, time::Duration};

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use stuntman_core::{env::Envs, util, ApiError, Ctx};
use tonic::{metadata::MetadataMap, Request, Response as Resp, Status, Streaming};

use crate::{
    config,
    store::{self, Got, Labels, Sub},
    v1::{
        publisher_server::{Publisher, PublisherServer},
        streaming_pull_response::SubscriptionProperties,
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

fn topic_pb(name: &str, labels: Labels) -> Topic {
    Topic { name: name.into(), labels: labels.into_iter().collect(), state: 1, ..Default::default() }
}

fn sub_pb(s: &Sub) -> Subscription {
    Subscription {
        name: s.name.clone(),
        topic: s.topic.clone(),
        ack_deadline_seconds: s.ack_deadline,
        enable_message_ordering: s.ordering,
        labels: s.labels.clone().into_iter().collect(),
        dead_letter_policy: (!s.dead_letter.is_empty()).then(|| DeadLetterPolicy { dead_letter_topic: s.dead_letter.clone(), max_delivery_attempts: s.max_attempts }),
        message_retention_duration: Some(prost_types::Duration { seconds: 7 * 86400, nanos: 0 }),
        expiration_policy: Some(ExpirationPolicy { ttl: Some(prost_types::Duration { seconds: 31 * 86400, nanos: 0 }) }),
        state: 1,
        ..Default::default()
    }
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
            publish_time: Some(prost_types::Timestamp { seconds: g.msg.published / 1000, nanos: (g.msg.published % 1000) as i32 * 1_000_000 }),
            ordering_key: g.msg.ordering_key,
        }),
    }
}

async fn need_sub(ctx: &Ctx, name: &str) -> Result<Sub, Status> {
    check(name, "subscriptions")?;
    store::sub(ctx, name).await.map_err(int)?.ok_or_else(|| Status::not_found(format!("Resource not found (resource={}).", short(name))))
}

/// Publish as the app does (gRPC or REST): provocations first, then the topic, then the messages.
pub async fn publish(ctx: &Ctx, topic: &str, msgs: Vec<PubsubMessage>, by: &str) -> Result<Vec<String>, Status> {
    check(topic, "topics")?;
    if let Some((kind, delay)) = config::take(ctx, topic) {
        match kind.as_str() {
            "unavailable" => return Err(Status::unavailable("The service is currently unavailable. (provoked by Stuntman)")),
            "deadline_exceeded" => return Err(Status::deadline_exceeded("Deadline exceeded (provoked by Stuntman)")),
            "resource_exhausted" => return Err(Status::resource_exhausted("Quota exceeded for quota metric 'Publish requests' (provoked by Stuntman)")),
            "not_found" => return Err(Status::not_found(format!("Resource not found (resource={}). (provoked by Stuntman)", short(topic)))),
            "slow_publish" => tokio::time::sleep(Duration::from_millis(delay)).await,
            _ => {}
        }
    }
    if store::topic(ctx, topic).await.map_err(int)?.is_none() {
        return Err(Status::not_found(format!("Resource not found (resource={}).", short(topic))));
    }
    if msgs.is_empty() {
        return Err(Status::invalid_argument("The request contains no messages."));
    }
    if let Some(m) = msgs.iter().find(|m| m.data.is_empty() && m.attributes.is_empty()) {
        return Err(Status::invalid_argument(format!("Some messages contain neither data nor attributes (message_id={}).", m.message_id)));
    }
    ctx.live.seen(by.trim_start_matches("app:"), "Pub/Sub");
    let ids = store::publish(ctx, topic, msgs.into_iter().map(|m| (m.data, m.attributes.into_iter().collect::<BTreeMap<_, _>>(), m.ordering_key)).collect(), by).await.map_err(int)?;
    Ok(ids.into_iter().map(|i| i.to_string()).collect())
}

async fn apply_acks(ctx: &Ctx, sub: &str, acks: Vec<String>, modacks: Vec<(String, i32)>) -> Result<(), Status> {
    let dup = config::load(ctx).provoke == "duplicate";
    store::ack(ctx, sub, acks, dup).await.map_err(int)?;
    store::modack(ctx, sub, modacks).await.map_err(int)
}

#[tonic::async_trait]
impl Publisher for Svc {
    async fn create_topic(&self, r: Request<Topic>) -> Result<Resp<Topic>, Status> {
        let ctx = self.ctx(&r)?;
        let t = r.into_inner();
        check(&t.name, "topics")?;
        let labels: Labels = t.labels.into_iter().collect();
        if !store::create_topic(&ctx, &t.name, &labels).await.map_err(int)? {
            return Err(Status::already_exists("Topic already exists"));
        }
        Ok(Resp::new(topic_pb(&t.name, labels)))
    }

    async fn get_topic(&self, r: Request<GetTopicRequest>) -> Result<Resp<Topic>, Status> {
        let ctx = self.ctx(&r)?;
        let n = r.into_inner().topic;
        check(&n, "topics")?;
        let l = store::topic(&ctx, &n).await.map_err(int)?.ok_or_else(|| Status::not_found("Topic not found"))?;
        Ok(Resp::new(topic_pb(&n, l)))
    }

    /// Labels only: the other topic settings (schemas, retention, ingestion, KMS) are not kept.
    async fn update_topic(&self, r: Request<UpdateTopicRequest>) -> Result<Resp<Topic>, Status> {
        let ctx = self.ctx(&r)?;
        let t = r.into_inner().topic.ok_or_else(|| Status::invalid_argument("topic is required"))?;
        check(&t.name, "topics")?;
        store::topic(&ctx, &t.name).await.map_err(int)?.ok_or_else(|| Status::not_found("Topic not found"))?;
        let labels: Labels = t.labels.into_iter().collect();
        store::set_topic_labels(&ctx, &t.name, &labels).await.map_err(int)?;
        Ok(Resp::new(topic_pb(&t.name, labels)))
    }

    async fn publish(&self, r: Request<PublishRequest>) -> Result<Resp<PublishResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let by = client(r.metadata());
        let req = r.into_inner();
        Ok(Resp::new(PublishResponse { message_ids: publish(&ctx, &req.topic, req.messages, &by).await? }))
    }

    async fn list_topics(&self, r: Request<ListTopicsRequest>) -> Result<Resp<ListTopicsResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let p = r.into_inner().project;
        let topics = store::topics(&ctx, &format!("{p}/topics/")).await.map_err(int)?.into_iter().map(|(n, l)| topic_pb(&n, l)).collect();
        Ok(Resp::new(ListTopicsResponse { topics, next_page_token: String::new() }))
    }

    async fn list_topic_subscriptions(&self, r: Request<ListTopicSubscriptionsRequest>) -> Result<Resp<ListTopicSubscriptionsResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let t = r.into_inner().topic;
        check(&t, "topics")?;
        store::topic(&ctx, &t).await.map_err(int)?.ok_or_else(|| Status::not_found("Topic not found"))?;
        let subscriptions = store::subs(&ctx, "", Some(&t)).await.map_err(int)?.into_iter().map(|s| s.name).collect();
        Ok(Resp::new(ListTopicSubscriptionsResponse { subscriptions, next_page_token: String::new() }))
    }

    async fn delete_topic(&self, r: Request<DeleteTopicRequest>) -> Result<Resp<()>, Status> {
        let ctx = self.ctx(&r)?;
        let t = r.into_inner().topic;
        check(&t, "topics")?;
        if !store::delete_topic(&ctx, &t).await.map_err(int)? {
            return Err(Status::not_found("Topic not found"));
        }
        Ok(Resp::new(()))
    }
}

#[tonic::async_trait]
impl Subscriber for Svc {
    async fn create_subscription(&self, r: Request<Subscription>) -> Result<Resp<Subscription>, Status> {
        let ctx = self.ctx(&r)?;
        let s = r.into_inner();
        check(&s.name, "subscriptions")?;
        check(&s.topic, "topics")?;
        if !s.filter.is_empty() {
            return Err(Status::invalid_argument("Stuntman does not apply subscription filters yet; create the subscription without one."));
        }
        if s.push_config.as_ref().is_some_and(|p| !p.push_endpoint.is_empty()) || s.bigquery_config.is_some() || s.cloud_storage_config.is_some() || s.bigtable_config.is_some() {
            return Err(Status::invalid_argument("Stuntman offers pull subscriptions only (no push, BigQuery, Cloud Storage or Bigtable delivery)."));
        }
        store::topic(&ctx, &s.topic).await.map_err(int)?.ok_or_else(|| Status::not_found(format!("Resource not found (resource={}).", short(&s.topic))))?;
        let (dl, max) = s.dead_letter_policy.map(|d| (d.dead_letter_topic, if d.max_delivery_attempts == 0 { 5 } else { d.max_delivery_attempts.clamp(5, 100) })).unwrap_or_default();
        let sub = Sub { name: s.name, topic: s.topic, ack_deadline: if s.ack_deadline_seconds == 0 { 10 } else { s.ack_deadline_seconds.clamp(10, 600) }, ordering: s.enable_message_ordering,
            dead_letter: dl, max_attempts: max, labels: s.labels.into_iter().collect() };
        if !store::create_sub(&ctx, &sub).await.map_err(int)? {
            return Err(Status::already_exists("Subscription already exists"));
        }
        Ok(Resp::new(sub_pb(&sub)))
    }

    async fn get_subscription(&self, r: Request<GetSubscriptionRequest>) -> Result<Resp<Subscription>, Status> {
        let ctx = self.ctx(&r)?;
        Ok(Resp::new(sub_pb(&need_sub(&ctx, &r.into_inner().subscription).await?)))
    }

    async fn list_subscriptions(&self, r: Request<ListSubscriptionsRequest>) -> Result<Resp<ListSubscriptionsResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let p = r.into_inner().project;
        let subscriptions = store::subs(&ctx, &format!("{p}/subscriptions/"), None).await.map_err(int)?.iter().map(sub_pb).collect();
        Ok(Resp::new(ListSubscriptionsResponse { subscriptions, next_page_token: String::new() }))
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
        apply_acks(&ctx, &s.name, vec![], q.ack_ids.into_iter().map(|a| (a, secs)).collect()).await?;
        Ok(Resp::new(()))
    }

    async fn acknowledge(&self, r: Request<AcknowledgeRequest>) -> Result<Resp<()>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let s = need_sub(&ctx, &q.subscription).await?;
        apply_acks(&ctx, &s.name, q.ack_ids, vec![]).await?;
        Ok(Resp::new(()))
    }

    async fn pull(&self, r: Request<PullRequest>) -> Result<Resp<PullResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let s = need_sub(&ctx, &q.subscription).await?;
        let max = if q.max_messages <= 0 { 100 } else { q.max_messages as usize };
        #[allow(deprecated)]
        let wait = !q.return_immediately;
        // Pull without messages waits a little for one, as Pub/Sub does.
        let end = util::now_ms() + if wait { 1500 } else { 0 };
        loop {
            let bell = crate::bell().notified();
            let got = store::pull(&ctx, &s, max, s.ack_deadline).await.map_err(int)?;
            if !got.is_empty() || util::now_ms() >= end {
                return Ok(Resp::new(PullResponse { received_messages: got.into_iter().map(|g| received(&s, g)).collect() }));
            }
            tokio::select! { _ = bell => {} _ = tokio::time::sleep(Duration::from_millis(250)) => {} }
        }
    }

    /// The stream the Python, .NET, Go and Java subscribers live on: the first request names the
    /// subscription and the stream's ack deadline; later ones carry acks, modacks and heartbeats.
    async fn streaming_pull(&self, r: Request<Streaming<StreamingPullRequest>>) -> Result<Resp<tonic::codegen::BoxStream<StreamingPullResponse>>, Status> {
        let ctx = self.ctx(&r)?;
        let mut inb = r.into_inner();
        let first = inb.message().await?.ok_or_else(|| Status::invalid_argument("The first StreamingPullRequest must name a subscription."))?;
        let s = need_sub(&ctx, &first.subscription).await?;
        let secs = if first.stream_ack_deadline_seconds > 0 { first.stream_ack_deadline_seconds.clamp(10, 600) } else { s.ack_deadline };
        let max_out = if first.max_outstanding_messages > 0 { first.max_outstanding_messages } else { 1000 };
        let modacks = |q: &StreamingPullRequest| q.modify_deadline_ack_ids.iter().cloned().zip(q.modify_deadline_seconds.iter().copied()).collect::<Vec<_>>();
        apply_acks(&ctx, &s.name, first.ack_ids.clone(), modacks(&first)).await?;
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<StreamingPullResponse, Status>>(8);
        let done = std::sync::Arc::new(tokio::sync::Notify::new());
        {
            let (ctx, name, done) = (ctx.clone(), s.name.clone(), done.clone());
            tokio::spawn(async move {
                while let Ok(Some(q)) = inb.message().await {
                    let _ = apply_acks(&ctx, &name, q.ack_ids.clone(), modacks(&q)).await;
                }
                done.notify_one();
            });
        }
        tokio::spawn(async move {
            let props = SubscriptionProperties { exactly_once_delivery_enabled: false, message_ordering_enabled: s.ordering };
            loop {
                if ctx.state.gone() { break }
                let bell = crate::bell().notified();
                let room = max_out - store::outstanding(&ctx, &s.name).await.unwrap_or(0);
                if room > 0 {
                    // The subscription may be deleted while the stream is open.
                    let Ok(Some(cur)) = store::sub(&ctx, &s.name).await else {
                        let _ = tx.send(Err(Status::not_found(format!("Resource not found (resource={}).", short(&s.name))))).await;
                        break;
                    };
                    let got = store::pull(&ctx, &cur, room.min(500) as usize, secs).await.unwrap_or_default();
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

    /// Seek to a time (snapshots are not offered).
    async fn seek(&self, r: Request<SeekRequest>) -> Result<Resp<SeekResponse>, Status> {
        let ctx = self.ctx(&r)?;
        let q = r.into_inner();
        let s = need_sub(&ctx, &q.subscription).await?;
        match q.target {
            Some(seek_request::Target::Time(t)) => store::seek(&ctx, &s.name, t.seconds * 1000 + t.nanos as i64 / 1_000_000).await.map_err(int)?,
            _ => return Err(Status::unimplemented("Stuntman seeks to a time only (no snapshots).")),
        }
        Ok(Resp::new(SeekResponse {}))
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
        let routes = tonic::service::Routes::from(rest).add_service(PublisherServer::new(Svc(envs.clone()))).add_service(SubscriberServer::new(Svc(envs)));
        let incoming = futures::stream::unfold(l, |l| async move { Some((l.accept().await.map(|(s, _)| s), l)) });
        let _ = tonic::transport::Server::builder().accept_http1(true).add_routes(routes).serve_with_incoming(incoming).await;
    });
}
