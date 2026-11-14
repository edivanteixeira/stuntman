//! Azure Service Bus management (what ServiceBusAdministrationClient calls): queues, topics and
//! subscriptions as Atom XML over plain HTTP, on the same host and port as AMQP in emulator mode.
//! GET answers 404 for a missing entity; PUT creates (201) or says 409; DELETE removes.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, Request, Response, StatusCode};
use stuntman_core::{env::Envs, util, Ctx};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::config;

/// The socket with the 8 bytes already read put back in front.
pub struct Prefixed {
    pre: Vec<u8>,
    at: usize,
    inner: crate::amqp::Io,
}

impl AsyncRead for Prefixed {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.at < self.pre.len() {
            let n = (self.pre.len() - self.at).min(buf.remaining());
            let at = self.at;
            buf.put_slice(&self.pre[at..at + n]);
            self.at += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Prefixed {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, b: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, b)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

pub async fn serve(envs: Envs, sock: crate::amqp::Io, first: [u8; 8]) {
    let io = hyper_util::rt::TokioIo::new(Prefixed { pre: first.to_vec(), at: 0, inner: sock });
    let svc = hyper::service::service_fn(move |req: Request<Incoming>| {
        let envs = envs.clone();
        async move { Ok::<_, std::convert::Infallible>(handle(envs, req).await) }
    });
    let _ = hyper::server::conn::http1::Builder::new().keep_alive(true).serve_connection(io, svc).await;
}

fn reply(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    Response::builder().status(status).header("content-type", "application/atom+xml;type=entry;charset=utf-8").body(Full::new(Bytes::from(body))).unwrap()
}

fn error(status: StatusCode, detail: &str) -> Response<Full<Bytes>> {
    reply(status, format!("<Error><Code>{}</Code><Detail>{}</Detail></Error>", status.as_u16(), esc(detail)))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// ISO 8601 duration for a number of milliseconds: PT30S, PT2M, P14D.
pub fn iso(ms: i64) -> String {
    let s = ms / 1000;
    if s % 86_400 == 0 && s > 0 {
        format!("P{}D", s / 86_400)
    } else if s % 3600 == 0 && s > 0 {
        format!("PT{}H", s / 3600)
    } else if s % 60 == 0 && s > 0 {
        format!("PT{}M", s / 60)
    } else {
        format!("PT{}S", s.max(1))
    }
}

/// "PT2M" → 120000; "P7D" → 604800000; "PT1M30S" → 90000. Huge values (AutoDeleteOnIdle) → None.
pub fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.trim_start_matches('P').split_once('T').unwrap_or((s.trim_start_matches('P'), ""));
    let mut ms: f64 = 0.0;
    let mut num = String::new();
    for c in date.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
        } else {
            let n: f64 = num.parse().ok()?;
            num.clear();
            ms += n * match c { 'D' => 86_400_000.0, 'W' => 604_800_000.0, _ => return None };
        }
    }
    for c in time.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
        } else {
            let n: f64 = num.parse().ok()?;
            num.clear();
            ms += n * match c { 'H' => 3_600_000.0, 'M' => 60_000.0, 'S' => 1000.0, _ => return None };
        }
    }
    if ms > 365.0 * 86_400_000.0 * 100.0 { None } else { Some(ms as i64) }
}

fn tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let i = xml.find(&open)? + open.len();
    let j = xml[i..].find(&format!("</{name}>"))? + i;
    Some(xml[i..j].to_string())
}

const NS: &str = "xmlns=\"http://schemas.microsoft.com/netservices/2010/10/servicebus/connect\" xmlns:i=\"http://www.w3.org/2001/XMLSchema-instance\"";
const FOREVER: &str = "P10675199DT2H48M5.4775807S";

fn entry(base: &str, path: &str, title: &str, content: &str) -> String {
    let now = chrono_now();
    format!("<entry xmlns=\"http://www.w3.org/2005/Atom\"><id>{base}/{path}</id><title type=\"text\">{}</title><published>{now}</published><updated>{now}</updated><author><name>stuntman</name></author><link rel=\"self\" href=\"{base}/{path}\"/><content type=\"application/xml\">{content}</content></entry>", esc(title))
}

/// RFC 3339 UTC for the Atom dates.
fn chrono_now() -> String {
    let s = util::now_ms() / 1000;
    let (d, t) = (s / 86_400, s % 86_400);
    // civil from days (Howard Hinnant)
    let z = d + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", if m <= 2 { y + 1 } else { y }, m, day, t / 3600, t % 3600 / 60, t % 60)
}

fn queue_xml(q: &config::Queue) -> String {
    let lock = util::parse_ms(&q.lock).unwrap_or(30_000) as i64;
    let ttl = if q.ttl.is_empty() || q.ttl == "never" { "P10675199DT2H48M5.4775807S".to_string() } else { iso(util::parse_ms(&q.ttl).unwrap_or(1_209_600_000) as i64) };
    format!("<QueueDescription {NS}><LockDuration>{}</LockDuration><MaxSizeInMegabytes>1024</MaxSizeInMegabytes><RequiresDuplicateDetection>false</RequiresDuplicateDetection><RequiresSession>{}</RequiresSession><DefaultMessageTimeToLive>{ttl}</DefaultMessageTimeToLive><DeadLetteringOnMessageExpiration>{}</DeadLetteringOnMessageExpiration><DuplicateDetectionHistoryTimeWindow>PT10M</DuplicateDetectionHistoryTimeWindow><MaxDeliveryCount>{}</MaxDeliveryCount><EnableBatchedOperations>true</EnableBatchedOperations><Status>Active</Status><AutoDeleteOnIdle>{FOREVER}</AutoDeleteOnIdle><EnablePartitioning>false</EnablePartitioning><EnableExpress>false</EnableExpress></QueueDescription>",
        iso(lock), q.sessions, q.dead_letter_on_expiration, q.max_delivery)
}

fn topic_xml() -> String {
    format!("<TopicDescription {NS}><DefaultMessageTimeToLive>P14D</DefaultMessageTimeToLive><MaxSizeInMegabytes>1024</MaxSizeInMegabytes><RequiresDuplicateDetection>false</RequiresDuplicateDetection><DuplicateDetectionHistoryTimeWindow>PT10M</DuplicateDetectionHistoryTimeWindow><EnableBatchedOperations>true</EnableBatchedOperations><Status>Active</Status><SupportOrdering>true</SupportOrdering><AutoDeleteOnIdle>{FOREVER}</AutoDeleteOnIdle><EnablePartitioning>false</EnablePartitioning><EnableExpress>false</EnableExpress></TopicDescription>")
}

fn sub_xml(s: &config::Subscription) -> String {
    let lock = util::parse_ms(&s.lock).unwrap_or(30_000) as i64;
    let ttl = if s.ttl.is_empty() || s.ttl == "never" { "P14D".to_string() } else { iso(util::parse_ms(&s.ttl).unwrap_or(1_209_600_000) as i64) };
    format!("<SubscriptionDescription {NS}><LockDuration>{}</LockDuration><RequiresSession>false</RequiresSession><DefaultMessageTimeToLive>{ttl}</DefaultMessageTimeToLive><DeadLetteringOnMessageExpiration>{}</DeadLetteringOnMessageExpiration><DeadLetteringOnFilterEvaluationExceptions>true</DeadLetteringOnFilterEvaluationExceptions><MaxDeliveryCount>{}</MaxDeliveryCount><EnableBatchedOperations>true</EnableBatchedOperations><Status>Active</Status><AutoDeleteOnIdle>{FOREVER}</AutoDeleteOnIdle></SubscriptionDescription>",
        iso(lock), s.dead_letter_on_expiration, s.max_delivery)
}

fn rule_xml() -> String {
    format!("<RuleDescription {NS}><Filter i:type=\"TrueFilter\"><SqlExpression>1=1</SqlExpression><CompatibilityLevel>20</CompatibilityLevel></Filter><Action i:type=\"EmptyRuleAction\"/><Name>$Default</Name></RuleDescription>")
}

fn dur_field(xml: &str, name: &str) -> Option<String> {
    tag(xml, name).and_then(|v| parse_iso(&v)).map(|ms| util::fmt_ms(ms as u64))
}

async fn handle(envs: Envs, req: Request<Incoming>) -> Response<Full<Bytes>> {
    let host = req.headers().get("host").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let auth = req.headers().get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let method = req.method().clone();
    let path = req.uri().path().trim_matches('/').to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let body = req.into_body().collect().await.map(|b| String::from_utf8_lossy(&b.to_bytes()).to_string()).unwrap_or_default();
    let Some((ctx, n)) = crate::amqp1::resolve_pub(&envs, &host) else {
        return error(StatusCode::NOT_FOUND, &format!("No Service Bus namespace answers for {host}"));
    };
    match n.behaviour.reachability.as_str() {
        "401" => return error(StatusCode::UNAUTHORIZED, "InvalidSignature: The token has an invalid signature (Stuntman: the namespace is set to 401)."),
        "busy" => return error(StatusCode::SERVICE_UNAVAILABLE, "ServerBusy (Stuntman: the namespace is set to busy)."),
        "timeout" => {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            return error(StatusCode::GATEWAY_TIMEOUT, "timeout");
        }
        _ => {}
    }
    if !n.access.any_key && !crate::amqp1::sas_ok(&n, &ctx, &auth) {
        return error(StatusCode::UNAUTHORIZED, "InvalidSignature: The token has an invalid signature.");
    }
    let base = format!("https://{}", host.split(':').next().unwrap_or(&host));
    let parts: Vec<&str> = path.split('/').collect();
    let lower: Vec<String> = parts.iter().map(|p| p.to_lowercase()).collect();
    let _ = query;
    match (method.as_str(), lower.iter().map(|s| s.as_str()).collect::<Vec<_>>().as_slice()) {
        ("GET", ["$resources", kind]) => list(&ctx, &n, &base, kind),
        ("GET", [_, "subscriptions"]) => list_subs(&n, &base, parts[0]),
        ("GET", [_, "subscriptions", _, "rules"]) => feed("Rules", vec![entry(&base, &format!("{}/Subscriptions/{}/Rules/$Default", parts[0], parts[2]), "$Default", &rule_xml())]),
        ("GET", [_]) => get_entity(&n, &base, parts[0]),
        ("GET", [_, "subscriptions", _]) => get_sub(&n, &base, parts[0], parts[2]),
        ("PUT", [_]) => put_entity(&ctx, &n, &base, parts[0], &body),
        ("PUT", [_, "subscriptions", _]) => put_sub(&ctx, &n, &base, parts[0], parts[2], &body),
        ("PUT", [_, "subscriptions", _, "rules", _]) => reply(StatusCode::CREATED, entry(&base, &path, parts[4], &rule_xml())),
        ("DELETE", [_]) => delete(&ctx, &n, parts[0], None).await,
        ("DELETE", [_, "subscriptions", _]) => delete(&ctx, &n, parts[0], Some(parts[2])).await,
        ("GET", []) => reply(StatusCode::OK, entry(&base, "", &n.namespace, &format!("<NamespaceInfo {NS}><Name>{}</Name><NamespaceType>Messaging</NamespaceType><MessagingSKU>Standard</MessagingSKU></NamespaceInfo>", n.namespace))),
        _ => error(StatusCode::BAD_REQUEST, &format!("Stuntman does not answer {method} /{path} yet")),
    }
}

fn feed(title: &str, entries: Vec<String>) -> Response<Full<Bytes>> {
    reply(StatusCode::OK, format!("<feed xmlns=\"http://www.w3.org/2005/Atom\"><title type=\"text\">{title}</title>{}</feed>", entries.join("")))
}

fn list(_ctx: &Ctx, n: &config::Namespace, base: &str, kind: &str) -> Response<Full<Bytes>> {
    let mut v = vec![];
    for (name, e) in &n.entities {
        match (kind, &e.queue, &e.topic) {
            ("queues", Some(q), _) => v.push(entry(base, name, name, &queue_xml(q))),
            ("topics", _, Some(_)) => v.push(entry(base, name, name, &topic_xml())),
            _ => {}
        }
    }
    feed(if kind == "queues" { "Queues" } else { "Topics" }, v)
}

fn list_subs(n: &config::Namespace, base: &str, topic: &str) -> Response<Full<Bytes>> {
    let Some(t) = n.entities.get(topic).and_then(|e| e.topic.as_ref()) else { return error(StatusCode::NOT_FOUND, &format!("Entity '{}:Topic:{topic}' was not found.", n.namespace)) };
    feed("Subscriptions", t.subscriptions.iter().map(|(s, sub)| entry(base, &format!("{topic}/Subscriptions/{s}"), s, &sub_xml(sub))).collect())
}

fn get_entity(n: &config::Namespace, base: &str, name: &str) -> Response<Full<Bytes>> {
    match n.entities.get(name) {
        Some(config::Entity { queue: Some(q), .. }) => reply(StatusCode::OK, entry(base, name, name, &queue_xml(q))),
        Some(config::Entity { topic: Some(_), .. }) => reply(StatusCode::OK, entry(base, name, name, &topic_xml())),
        _ => error(StatusCode::NOT_FOUND, &format!("Entity '{}:{name}' was not found.", n.namespace)),
    }
}

fn get_sub(n: &config::Namespace, base: &str, topic: &str, sub: &str) -> Response<Full<Bytes>> {
    match n.entities.get(topic).and_then(|e| e.topic.as_ref()).and_then(|t| t.subscriptions.get(sub)) {
        Some(s) => reply(StatusCode::OK, entry(base, &format!("{topic}/Subscriptions/{sub}"), sub, &sub_xml(s))),
        None => error(StatusCode::NOT_FOUND, &format!("Entity '{}:Topic:{topic}|{sub}' was not found.", n.namespace)),
    }
}

fn put_entity(ctx: &Ctx, n: &config::Namespace, base: &str, name: &str, body: &str) -> Response<Full<Bytes>> {
    if config::valid_entity(name).is_err() {
        return error(StatusCode::BAD_REQUEST, &format!("The entity name '{name}' is not valid."));
    }
    let update = body.contains("<IsUpdate") || n.entities.contains_key(name) && false;
    if n.entities.contains_key(name) && !update {
        return error(StatusCode::CONFLICT, &format!("Entity '{}:{name}' already exists.", n.namespace));
    }
    let topic = body.contains("TopicDescription");
    let e = if topic {
        config::Entity { queue: None, topic: Some(config::Topic { created_by: "admin client".into(), ..Default::default() }) }
    } else {
        config::Entity { queue: Some(config::Queue {
            lock: dur_field(body, "LockDuration").unwrap_or("30s".into()),
            max_delivery: tag(body, "MaxDeliveryCount").and_then(|v| v.parse().ok()).unwrap_or(10),
            sessions: tag(body, "RequiresSession").map(|v| v == "true").unwrap_or(false),
            ttl: dur_field(body, "DefaultMessageTimeToLive").unwrap_or_default(),
            dead_letter_on_expiration: tag(body, "DeadLetteringOnMessageExpiration").map(|v| v == "true").unwrap_or(false),
            created_by: "admin client".into(),
            ..Default::default()
        }), topic: None }
    };
    let x = if topic { topic_xml() } else { queue_xml(e.queue.as_ref().unwrap()) };
    match config::change(ctx, &n.namespace, |ns| {
        ns.entities.insert(name.to_string(), e.clone());
        Ok(())
    }) {
        Ok(_) => reply(StatusCode::CREATED, entry(base, name, name, &x)),
        Err(e) => error(StatusCode::BAD_REQUEST, &e.message),
    }
}

fn put_sub(ctx: &Ctx, n: &config::Namespace, base: &str, topic: &str, sub: &str, body: &str) -> Response<Full<Bytes>> {
    if n.entities.get(topic).and_then(|e| e.topic.as_ref()).is_none() {
        return error(StatusCode::NOT_FOUND, &format!("Entity '{}:Topic:{topic}' was not found.", n.namespace));
    }
    if n.entities.get(topic).and_then(|e| e.topic.as_ref()).map(|t| t.subscriptions.contains_key(sub)).unwrap_or(false) {
        return error(StatusCode::CONFLICT, &format!("Entity '{}:Topic:{topic}|{sub}' already exists.", n.namespace));
    }
    let s = config::Subscription {
        lock: dur_field(body, "LockDuration").unwrap_or("30s".into()),
        max_delivery: tag(body, "MaxDeliveryCount").and_then(|v| v.parse().ok()).unwrap_or(10),
        ttl: dur_field(body, "DefaultMessageTimeToLive").unwrap_or_default(),
        dead_letter_on_expiration: tag(body, "DeadLetteringOnMessageExpiration").map(|v| v == "true").unwrap_or(false),
        created_by: "admin client".into(),
        ..Default::default()
    };
    let x = sub_xml(&s);
    match config::change(ctx, &n.namespace, |ns| {
        if let Some(t) = ns.entities.get_mut(topic).and_then(|e| e.topic.as_mut()) {
            t.subscriptions.insert(sub.to_string(), s.clone());
        }
        Ok(())
    }) {
        Ok(_) => reply(StatusCode::CREATED, entry(base, &format!("{topic}/Subscriptions/{sub}"), sub, &x)),
        Err(e) => error(StatusCode::BAD_REQUEST, &e.message),
    }
}

async fn delete(ctx: &Ctx, n: &config::Namespace, name: &str, sub: Option<&str>) -> Response<Full<Bytes>> {
    let r = config::change(ctx, &n.namespace, |ns| {
        match sub {
            Some(s) => {
                let t = ns.entities.get_mut(name).and_then(|e| e.topic.as_mut()).ok_or_else(|| stuntman_core::ApiError::not_found("no topic"))?;
                t.subscriptions.remove(s).ok_or_else(|| stuntman_core::ApiError::not_found("no subscription"))?;
            }
            None => {
                ns.entities.remove(name).ok_or_else(|| stuntman_core::ApiError::not_found("no entity"))?;
            }
        }
        Ok(())
    });
    if r.is_err() {
        return error(StatusCode::NOT_FOUND, &format!("Entity '{}:{name}' was not found.", n.namespace));
    }
    let holder = match sub { Some(s) => format!("{name}/subscriptions/{s}"), None => name.to_string() };
    let _ = crate::broker::purge(ctx, &n.namespace, &holder, None).await;
    reply(StatusCode::OK, String::new())
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn durations_and_tags() {
        assert_eq!(parse_iso("PT2M"), Some(120_000));
        assert_eq!(parse_iso("P7D"), Some(604_800_000));
        assert_eq!(parse_iso("PT1M30S"), Some(90_000));
        assert_eq!(parse_iso(FOREVER), None);
        assert_eq!(iso(30_000), "PT30S");
        assert_eq!(iso(120_000), "PT2M");
        assert_eq!(iso(1_209_600_000), "P14D");
        assert_eq!(tag("<a><MaxDeliveryCount>3</MaxDeliveryCount></a>", "MaxDeliveryCount").as_deref(), Some("3"));
        assert_eq!(chrono_now().len(), 20);
    }
}
