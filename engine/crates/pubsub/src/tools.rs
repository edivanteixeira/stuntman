//! The Pub/Sub tools: topics with their subscriptions and ack states, messages, publishing as
//! another service would, subscriptions, waiting for what the app published, provocations.

use serde_json::{json, Map, Value};
use stuntman_core::{tools::Args, util, ApiError, Ctx, Tool};

use crate::{config, store::{self, Labels, Sub}};

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
    let all: Vec<String> = if kind == "topics" { store::topics(ctx, "").await?.into_iter().map(|x| x.0).collect() } else { store::subs(ctx, "", None).await?.into_iter().map(|s| s.name).collect() };
    let hits: Vec<&String> = all.iter().filter(|x| x.ends_with(&format!("/{kind}/{n}"))).collect();
    Ok(if hits.len() == 1 { hits[0].clone() } else { format!("projects/{}/{kind}/{n}", config::load(ctx).project) })
}

fn labels(a: &Args, k: &str) -> Labels {
    a.get(k).and_then(|v| v.as_object()).map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().map(String::from).unwrap_or(v.to_string()))).collect()).unwrap_or_default()
}

async fn sub_view(ctx: &Ctx, s: &Sub) -> Result<Value, ApiError> {
    let [pending, outstanding, acked, dead] = store::counts(ctx, &s.name).await?;
    Ok(json!({"name": s.name, "topic": s.topic, "ackDeadlineSeconds": s.ack_deadline, "messageOrdering": s.ordering,
        "deadLetterTopic": (!s.dead_letter.is_empty()).then_some(&s.dead_letter), "maxDeliveryAttempts": (!s.dead_letter.is_empty()).then_some(s.max_attempts),
        "unacked": pending + outstanding, "pending": pending, "outstanding": outstanding, "acked": acked, "deadLettered": dead}))
}

fn host_hint(ctx: &Ctx) -> Value {
    json!({"PUBSUB_EMULATOR_HOST": format!("localhost:{}", crate::port()), "environment": if ctx.prefix().is_empty() { Value::Null } else { json!(format!("send gRPC metadata x-stuntman-env: {}", ctx.env)) }})
}

pub fn all() -> Vec<Tool> {
    vec![
        Tool::new("list_pubsub_topics", "The Pub/Sub topics of this environment with their subscriptions (ack deadline, ordering, dead-letter) and how many messages each has pending, outstanding, acked and dead-lettered; how apps connect.", |ctx, _a| async move {
            let mut out = vec![];
            for (t, l) in store::topics(&ctx, "").await? {
                let mut subs = vec![];
                for s in store::subs(&ctx, "", Some(&t)).await? {
                    subs.push(sub_view(&ctx, &s).await?);
                }
                out.push(json!({"name": t, "labels": l, "messages": store::message_count(&ctx, &t).await?, "subscriptions": subs}));
            }
            let mut detached = vec![];
            for s in store::subs(&ctx, "", Some(store::DELETED_TOPIC)).await? {
                detached.push(sub_view(&ctx, &s).await?);
            }
            let c = config::load(&ctx);
            Ok(json!({"topics": out, "detachedSubscriptions": detached, "connect": host_hint(&ctx), "project": c.project,
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
        Tool::new("pubsub_publish", "Publishes a message to a topic as another service would (the app's subscribers receive it): data (JSON or text), attributes, ordering_key. Makes the topic when it does not exist.", |ctx, a| async move {
            let t = resolve(&ctx, &a, "topic", "topics").await?;
            if store::topic(&ctx, &t).await?.is_none() {
                if !t.starts_with("projects/") || t.split('/').count() != 4 { return Err(ApiError::bad("topic: projects/<project>/topics/<name>")) }
                store::create_topic(&ctx, &t, &Labels::new()).await?;
            }
            let data = match a.get("data") { Some(Value::String(s)) => s.clone().into_bytes(), Some(v) => v.to_string().into_bytes(), None => vec![] };
            let attrs = labels(&a, "attributes");
            if data.is_empty() && attrs.is_empty() { return Err(ApiError::bad("data or attributes")) }
            let by = a.str("_by").unwrap_or("you".into());
            let ids = store::publish(&ctx, &t, vec![(data, attrs, a.str("ordering_key").unwrap_or_default())], &by).await?;
            Ok(json!({"topic": t, "messageId": ids[0].to_string()}))
        })
        .req("topic", "shipments").param("data", "JSON or text").param("attributes", "{name: value}").param("ordering_key", "the ordering key").param("project", "acme-stg")
        .route("POST", "/pubsub/topics/{topic}/publish"),
        Tool::new("create_pubsub_subscription", "Creates a pull subscription on a topic (the topic too when missing): ack deadline, message ordering, a dead-letter topic with max delivery attempts.", |ctx, a| async move {
            let s = resolve(&ctx, &a, "subscription", "subscriptions").await?;
            let t = resolve(&ctx, &a, "topic", "topics").await?;
            for (n, k) in [(&s, "subscriptions"), (&t, "topics")] {
                let p: Vec<&str> = n.split('/').collect();
                if p.len() != 4 || p[0] != "projects" || p[2] != k { return Err(ApiError::bad(format!("{n}: projects/<project>/{k}/<name>"))) }
            }
            store::create_topic(&ctx, &t, &Labels::new()).await?;
            let dl = match a.str("dead_letter_topic") { Some(_) => resolve(&ctx, &a, "dead_letter_topic", "topics").await?, None => String::new() };
            if !dl.is_empty() { store::create_topic(&ctx, &dl, &Labels::new()).await?; }
            let sub = Sub { name: s.clone(), topic: t, ack_deadline: a.u64("ack_deadline_seconds").unwrap_or(10).clamp(10, 600) as i32, ordering: a.bool("ordering").unwrap_or(false),
                max_attempts: if dl.is_empty() { 0 } else { a.u64("max_delivery_attempts").unwrap_or(5).clamp(5, 100) as i32 }, dead_letter: dl, labels: labels(&a, "labels") };
            let made = store::create_sub(&ctx, &sub).await?;
            Ok(json!({"subscription": sub_view(&ctx, &store::sub(&ctx, &s).await?.unwrap_or(sub)).await?, "created": made}))
        })
        .req("subscription", "shipments-billing").req("topic", "shipments").param("ack_deadline_seconds", "10").param("ordering", "bool").param("dead_letter_topic", "shipments-dlq")
        .param("max_delivery_attempts", "5").param("labels", "{name: value}").param("project", "acme-stg")
        .route("PUT", "/pubsub/subscriptions/{subscription}"),
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
        store::create_topic(&c, t, &Labels::new()).await.unwrap();
        store::create_topic(&c, "projects/acme-stg/topics/shipments-dlq", &Labels::new()).await.unwrap();
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
        store::ack(&c, &s.name, vec![got[2].ack_id.clone()], false).await.unwrap();
        store::modack(&c, &s.name, vec![(got[0].ack_id.clone(), 0)]).await.unwrap();
        assert!(store::pull(&c, &s, 10, 10).await.unwrap().is_empty(), "a2 is still out: key A waits");
        store::modack(&c, &s.name, vec![(got[1].ack_id.clone(), 0)]).await.unwrap();
        let again = store::pull(&c, &s, 10, 10).await.unwrap();
        assert_eq!(again.iter().map(|g| (g.msg.id, g.attempt)).collect::<Vec<_>>(), [(got[0].msg.id, 2), (got[1].msg.id, 2)]);
        // A third delivery would pass max_attempts=2: both go to the dead-letter topic.
        store::modack(&c, &s.name, again.iter().map(|g| (g.ack_id.clone(), 0)).collect()).await.unwrap();
        assert!(store::pull(&c, &s, 10, 10).await.unwrap().is_empty());
        assert_eq!(store::counts(&c, &s.name).await.unwrap(), [0, 0, 1, 2]);
        assert_eq!(store::message_count(&c, "projects/acme-stg/topics/shipments-dlq").await.unwrap(), 2);

        // duplicate: the first ack does not stick.
        let plain = Sub { name: "projects/acme-stg/subscriptions/audit".into(), topic: t.into(), ack_deadline: 10, ..Default::default() };
        store::create_sub(&c, &plain).await.unwrap();
        store::publish(&c, t, vec![m("c1", "")], "app:test").await.unwrap();
        let g = store::pull(&c, &plain, 10, 10).await.unwrap();
        store::ack(&c, &plain.name, vec![g[0].ack_id.clone()], true).await.unwrap();
        let g2 = store::pull(&c, &plain, 10, 10).await.unwrap();
        assert_eq!((g2.len(), g2[0].msg.id), (1, g[0].msg.id));
        store::ack(&c, &plain.name, vec![g2[0].ack_id.clone()], true).await.unwrap();
        assert_eq!(store::counts(&c, &plain.name).await.unwrap(), [0, 0, 1, 0]);

        // Deleting the topic detaches its subscriptions.
        store::delete_topic(&c, t).await.unwrap();
        assert_eq!(store::sub(&c, &plain.name).await.unwrap().unwrap().topic, store::DELETED_TOPIC);
    }
}
