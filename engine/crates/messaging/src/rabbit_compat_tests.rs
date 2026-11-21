//! RabbitMQ 4 behaviour a MassTransit app and a provisioning script depend on, checked over
//! the management API (HTTP) and AMQP 0-9-1 against a real RabbitMQ 4.4 first: policies,
//! quorum queues (delivery limit, max-length drop-head, dead-lettering), exchange-to-exchange
//! bindings, a queue and an exchange of the same name, users restricted to one vhost.

use std::sync::Arc;

use amq_protocol::{
    frame::{AMQPContentHeader, AMQPFrame},
    protocol::{basic, channel, connection, exchange, queue, AMQPClass},
    types::{FieldTable, LongString},
};
use serde_json::{json, Map, Value};
use stuntman_core::{
    callback::Signer,
    env::{EnvFile, EnvState, Envs},
    live::Live,
    store::Db,
    util, Ctx,
};
use tokio::io::{AsyncWriteExt, DuplexStream};

use crate::{broker, config, rabbit, rabbit_mgmt};

fn envs() -> (Envs, Ctx) {
    let dir = std::env::temp_dir().join(util::id("rabbit-compat"));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::memory();
    db.migrate(broker::SQL).unwrap();
    let envs = Envs::default();
    let ctx = Ctx { env: "t".into(), state: Arc::new(EnvState::new(EnvFile::new("t"))), envs: envs.clone(), db, live: Live::default(), data_dir: Arc::new(dir.clone()),
        http: reqwest::Client::new(), signer: Arc::new(Signer::load_or_create(&dir.join("k.pem")).unwrap()), host: "localhost".into() };
    envs.insert(ctx.clone());
    (envs, ctx)
}

/// The management API on a free port; returns its base URL.
async fn mgmt(envs: &Envs) -> String {
    let l = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let base = format!("http://{}/api/", l.local_addr().unwrap());
    let r = rabbit_mgmt::router(envs.clone());
    tokio::spawn(async move { axum::serve(l, r).await });
    base
}

struct Http {
    c: reqwest::Client,
    base: String,
    auth: (String, String),
}

impl Http {
    fn as_user(&self, u: &str, p: &str) -> Http {
        Http { c: self.c.clone(), base: self.base.clone(), auth: (u.into(), p.into()) }
    }
    async fn call(&self, m: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut r = self.c.request(m.parse().unwrap(), format!("{}{path}", self.base)).basic_auth(&self.auth.0, Some(&self.auth.1));
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let s = resp.status().as_u16();
        (s, resp.json().await.unwrap_or(Value::Null))
    }
    async fn st(&self, m: &str, path: &str, body: Option<Value>) -> u16 {
        self.call(m, path, body).await.0
    }
}

/// A tiny AMQP 0-9-1 client over an in-memory pipe to `rabbit::serve`.
struct Amqp {
    io: DuplexStream,
    buf: Vec<u8>,
}

impl Amqp {
    /// Ok after connection.open-ok; Err(code, text) when the broker closes the connection.
    async fn open(envs: &Envs, user: &str, pass: &str, vhost: &str) -> Result<Amqp, (u16, String)> {
        let (a, b) = tokio::io::duplex(1 << 20);
        // Each connection its own client port: /api/connections names them by it.
        static PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(50000);
        let port = PORT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::spawn(rabbit::serve(envs.clone(), Box::new(b), format!("127.0.0.1:{port}")));
        let mut c = Amqp { io: a, buf: vec![] };
        c.next().await; // connection.start
        c.send(0, AMQPClass::Connection(connection::AMQPMethod::StartOk(connection::StartOk { client_properties: FieldTable::default(), mechanism: "PLAIN".into(), response: LongString::from(format!("\0{user}\0{pass}")), locale: "en_US".into() }))).await;
        if let Some(AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Close(x)))) = c.next().await {
            return Err((x.reply_code, x.reply_text.to_string()));
        }
        c.send(0, AMQPClass::Connection(connection::AMQPMethod::TuneOk(connection::TuneOk { channel_max: 2047, frame_max: 131072, heartbeat: 0 }))).await;
        c.send(0, AMQPClass::Connection(connection::AMQPMethod::Open(connection::Open { virtual_host: vhost.into() }))).await;
        match c.next().await {
            Some(AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::OpenOk(_)))) => {
                c.send(1, AMQPClass::Channel(channel::AMQPMethod::Open(channel::Open {}))).await;
                c.next().await;
                Ok(c)
            }
            Some(AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Close(x)))) => Err((x.reply_code, x.reply_text.to_string())),
            None => Err((0, "connection closed".into())),
            f => panic!("unexpected {f:?}"),
        }
    }
    async fn send(&mut self, ch: u16, m: AMQPClass) {
        self.io.write_all(&rabbit::enc(&AMQPFrame::Method(ch, m))).await.unwrap();
    }
    async fn next(&mut self) -> Option<AMQPFrame> {
        loop {
            match rabbit::next_frame(&mut self.io, &mut self.buf).await? {
                AMQPFrame::Heartbeat => continue,
                f => return Some(f),
            }
        }
    }
    /// One method and its answer (a channel.close comes back as Err).
    async fn call(&mut self, m: AMQPClass) -> Result<AMQPClass, (u16, String)> {
        self.send(1, m).await;
        match self.next().await {
            Some(AMQPFrame::Method(_, AMQPClass::Channel(channel::AMQPMethod::Close(x)))) => {
                self.send(1, AMQPClass::Channel(channel::AMQPMethod::CloseOk(channel::CloseOk {}))).await;
                self.send(1, AMQPClass::Channel(channel::AMQPMethod::Open(channel::Open {}))).await;
                self.next().await;
                Err((x.reply_code, x.reply_text.to_string()))
            }
            Some(AMQPFrame::Method(_, m)) => Ok(m),
            f => panic!("unexpected {f:?}"),
        }
    }
    async fn queue(&mut self, name: &str, args: Value) -> Result<AMQPClass, (u16, String)> {
        self.call(AMQPClass::Queue(queue::AMQPMethod::Declare(queue::Declare { queue: name.into(), durable: true, arguments: rabbit::table_of(&args), ..Default::default() }))).await
    }
    async fn exchange(&mut self, name: &str, kind: &str) {
        self.call(AMQPClass::Exchange(exchange::AMQPMethod::Declare(exchange::Declare { exchange: name.into(), kind: kind.into(), durable: true, ..Default::default() }))).await.unwrap();
    }
    async fn bind(&mut self, q: &str, x: &str, key: &str) {
        self.call(AMQPClass::Queue(queue::AMQPMethod::Bind(queue::Bind { queue: q.into(), exchange: x.into(), routing_key: key.into(), ..Default::default() }))).await.unwrap();
    }
    async fn bind_exchange(&mut self, dest: &str, source: &str, key: &str) {
        self.call(AMQPClass::Exchange(exchange::AMQPMethod::Bind(exchange::Bind { destination: dest.into(), source: source.into(), routing_key: key.into(), ..Default::default() }))).await.unwrap();
    }
    async fn publish(&mut self, x: &str, key: &str, body: &str) {
        let f = |f: AMQPFrame| rabbit::enc(&f);
        let mut b = f(AMQPFrame::Method(1, AMQPClass::Basic(basic::AMQPMethod::Publish(basic::Publish { exchange: x.into(), routing_key: key.into(), ..Default::default() }))));
        b.extend(f(AMQPFrame::Header(1, AMQPContentHeader { class_id: 60, body_size: body.len() as u64, properties: basic::AMQPProperties::default() })));
        b.extend(f(AMQPFrame::Body(1, body.as_bytes().to_vec())));
        self.io.write_all(&b).await.unwrap();
    }
    /// basic.get: (body, redelivered, x-delivery-count, delivery tag, headers).
    async fn get(&mut self, q: &str) -> Option<(String, bool, Option<i64>, u64, Value)> {
        self.send(1, AMQPClass::Basic(basic::AMQPMethod::Get(basic::Get { queue: q.into(), no_ack: false, ..Default::default() }))).await;
        let Some(AMQPFrame::Method(_, AMQPClass::Basic(basic::AMQPMethod::GetOk(ok)))) = self.next().await else { return None };
        let Some(AMQPFrame::Header(_, h)) = self.next().await else { panic!("no header") };
        let mut body = vec![];
        while (body.len() as u64) < h.body_size {
            let Some(AMQPFrame::Body(_, b)) = self.next().await else { panic!("no body") };
            body.extend(b);
        }
        let headers = h.properties.headers().as_ref().map(|t| Value::Object(t.inner().iter().map(|(k, v)| (k.to_string(), rabbit::to_json(v))).collect())).unwrap_or(json!({}));
        let dc = headers.get("x-delivery-count").and_then(|v| v.as_i64());
        Some((String::from_utf8(body).unwrap(), ok.redelivered, dc, ok.delivery_tag, headers))
    }
    async fn nack(&mut self, tag: u64, requeue: bool) {
        self.send(1, AMQPClass::Basic(basic::AMQPMethod::Nack(basic::Nack { delivery_tag: tag, multiple: false, requeue }))).await;
    }
    /// Closes channel 1 (its unacked messages go back) and opens it again.
    async fn reopen(&mut self) {
        self.call(AMQPClass::Channel(channel::AMQPMethod::Close(channel::Close { reply_code: 200, reply_text: "bye".into(), class_id: 0, method_id: 0 }))).await.unwrap();
        self.call(AMQPClass::Channel(channel::AMQPMethod::Open(channel::Open {}))).await.unwrap();
    }
    async fn count(&mut self, q: &str) -> u32 {
        match self.call(AMQPClass::Queue(queue::AMQPMethod::Declare(queue::Declare { queue: q.into(), passive: true, ..Default::default() }))).await {
            Ok(AMQPClass::Queue(queue::AMQPMethod::DeclareOk(ok))) => ok.message_count,
            x => panic!("{x:?}"),
        }
    }
    async fn drain(&mut self, q: &str) -> Vec<(String, Value)> {
        let mut out = vec![];
        while let Some((b, _, _, tag, h)) = self.get(q).await {
            self.send(1, AMQPClass::Basic(basic::AMQPMethod::Ack(basic::Ack { delivery_tag: tag, multiple: false }))).await;
            out.push((b, h));
        }
        out
    }
}

/// A provisioning script before MassTransit declares anything: a vhost with quorum queues by
/// default and policies it reads back to compare; then what a health check reads.
#[tokio::test]
async fn deploy_tool_management_calls() {
    let (envs, _) = envs();
    let h = Http { c: reqwest::Client::new(), base: mgmt(&envs).await, auth: ("guest".into(), "guest".into()) };
    assert_eq!(h.st("GET", "vhosts/acme-bus", None).await, 404);
    assert_eq!(h.st("PUT", "vhosts/acme-bus", None).await, 201);
    assert_eq!(h.st("PUT", "vhosts/acme-bus", Some(json!({"default_queue_type": "quorum"}))).await, 204);
    assert_eq!(h.call("GET", "vhosts/acme-bus", None).await.1["default_queue_type"], "quorum");
    // RabbitMQ: a PUT without it sets it back to classic.
    h.st("PUT", "vhosts/acme-bus", None).await;
    assert_eq!(h.call("GET", "vhosts/acme-bus", None).await.1["default_queue_type"], "classic");
    h.st("PUT", "vhosts/acme-bus", Some(json!({"default_queue_type": "quorum"}))).await;

    let policy = json!({"pattern": ".*", "apply-to": "quorum_queues", "priority": 10, "definition": {"max-length": 5000, "overflow": "drop-head", "delivery-limit": 10}});
    assert_eq!(h.st("GET", "policies/acme-bus/acme-defaults", None).await, 404);
    assert_eq!(h.st("PUT", "policies/acme-bus/acme-defaults", Some(policy.clone())).await, 201);
    assert_eq!(h.st("PUT", "policies/acme-bus/acme-defaults", Some(policy)).await, 204);
    let (s, p) = h.call("GET", "policies/acme-bus/acme-defaults", None).await;
    assert_eq!(s, 200);
    assert_eq!(p, json!({"vhost": "acme-bus", "name": "acme-defaults", "pattern": ".*", "apply-to": "quorum_queues", "priority": 10, "definition": {"max-length": 5000, "overflow": "drop-head", "delivery-limit": 10}}));
    let reports = json!({"pattern": "^globex-reports$", "apply-to": "quorum_queues", "priority": 20, "definition": {"max-length": 5000, "overflow": "drop-head", "delivery-limit": 10, "consumer-timeout": 3_600_000i64}});
    assert_eq!(h.st("PUT", "policies/acme-bus/globex-reports", Some(reports)).await, 201);
    assert_eq!(h.call("GET", "policies/acme-bus", None).await.1.as_array().unwrap().len(), 2);
    assert_eq!(h.call("GET", "policies/acme-bus/globex-reports", None).await.1["definition"]["consumer-timeout"], 3_600_000);
    assert_eq!(h.st("PUT", "policies/acme-bus/bad", Some(json!({"pattern": ".*", "apply-to": "nope", "definition": {"max-length": 1}}))).await, 400);
    assert_eq!(h.st("PUT", "policies/no-such-vhost/x", Some(json!({"pattern": ".*", "definition": {}}))).await, 404);
    assert_eq!(h.st("DELETE", "policies/acme-bus/globex-reports", None).await, 204);
    assert_eq!(h.st("GET", "policies/acme-bus/globex-reports", None).await, 404);

    assert_eq!(h.call("GET", "health/checks/alarms", None).await, (200, json!({"status": "ok"})));
    // An unknown vhost is 404 for the topology health check, and asking does not create it.
    assert_eq!(h.st("GET", "queues/no-such-vhost?columns=name", None).await, 404);
    assert_eq!(h.st("GET", "vhosts/no-such-vhost", None).await, 404);
}

/// rabbitmqadmin `users declare` (a password hash) and `permissions declare`, then the app user:
/// whoami, its vhost only, and on AMQP refused elsewhere (NOT_ALLOWED) or with a wrong password.
#[tokio::test]
async fn declared_user_is_restricted_to_its_vhost() {
    let (envs, _) = envs();
    let admin = Http { c: reqwest::Client::new(), base: mgmt(&envs).await, auth: ("guest".into(), "guest".into()) };
    for v in ["acme-bus", "acme-other"] {
        admin.st("PUT", &format!("vhosts/{v}"), None).await;
    }
    assert_eq!(admin.st("PUT", "users/acme-app", Some(json!({"name": "acme-app", "password_hash": rabbit_mgmt::hash("s3cret-pass"), "tags": "management"}))).await, 201);
    assert_eq!(admin.st("PUT", "permissions/acme%2Dbus/acme%2Dapp", Some(json!({"user": "acme-app", "vhost": "acme-bus", "configure": ".*", "read": ".*", "write": ".*"}))).await, 201);
    assert_eq!(admin.call("GET", "users/acme-app/permissions", None).await.1, json!([{"user": "acme-app", "vhost": "acme-bus", "configure": ".*", "write": ".*", "read": ".*"}]));

    let app = admin.as_user("acme-app", "s3cret-pass");
    assert_eq!(app.call("GET", "whoami", None).await.1, json!({"name": "acme-app", "tags": ["management"]}));
    assert_eq!(app.st("GET", "queues/acme-bus", None).await, 200);
    assert_eq!(app.st("GET", "queues/acme-other", None).await, 404);
    assert_eq!(app.st("PUT", "vhosts/acme-sneaky", None).await, 401);
    assert_eq!(admin.as_user("acme-app", "wrong").st("GET", "whoami", None).await, 401);

    assert!(Amqp::open(&envs, "acme-app", "s3cret-pass", "acme-bus").await.is_ok());
    assert_eq!(Amqp::open(&envs, "acme-app", "s3cret-pass", "acme-other").await.err().unwrap(), (530, "NOT_ALLOWED - access to vhost 'acme-other' refused for user 'acme-app'".into()));
    assert_eq!(Amqp::open(&envs, "acme-app", "wrong", "acme-bus").await.err().unwrap().0, 403);
    // Users nobody declared still get in (the namespace accepts any key by default).
    assert!(Amqp::open(&envs, "someone", "anything", "acme-other").await.is_ok());
}

/// MassTransit's topology: a fanout exchange per message type bound to the endpoint's exchange,
/// which has the same name as the endpoint's queue; direct exchanges with several keys.
#[tokio::test]
async fn masstransit_topology_routes_through_exchange_bindings() {
    let (envs, ctx) = envs();
    let h = Http { c: reqwest::Client::new(), base: mgmt(&envs).await, auth: ("guest".into(), "guest".into()) };
    h.st("PUT", "vhosts/acme-bus", Some(json!({"default_queue_type": "quorum"}))).await;
    let mut c = Amqp::open(&envs, "guest", "guest", "acme-bus").await.unwrap();
    c.exchange("acme-ping", "fanout").await;
    c.queue("acme-ping", json!({})).await.unwrap();
    c.bind("acme-ping", "acme-ping", "").await;
    c.exchange("Acme.Contracts:Ping", "fanout").await;
    c.bind_exchange("acme-ping", "Acme.Contracts:Ping", "").await;
    c.exchange("Acme.Contracts:Routed", "direct").await;
    c.exchange("acme-routed", "fanout").await;
    c.queue("acme-routed", json!({})).await.unwrap();
    c.bind("acme-routed", "acme-routed", "").await;
    c.bind_exchange("acme-routed", "Acme.Contracts:Routed", "tenant-a").await;
    c.bind_exchange("acme-routed", "Acme.Contracts:Routed", "tenant-b").await;

    c.publish("Acme.Contracts:Ping", "", "ping").await;
    for k in ["tenant-a", "tenant-b", "tenant-c"] {
        c.publish("Acme.Contracts:Routed", k, k).await;
    }
    assert_eq!(c.drain("acme-ping").await.into_iter().map(|m| m.0).collect::<Vec<_>>(), ["ping"]);
    assert_eq!(c.drain("acme-routed").await.into_iter().map(|m| m.0).collect::<Vec<_>>(), ["tenant-a", "tenant-b"]);

    // tenant-c reached no queue: a warning with the exchange and the key; the trace shows why.
    let n = config::get(&ctx, "acme-bus").unwrap();
    let w = crate::tools::warnings(&ctx, &n).await;
    let un: Vec<(String, String, u64)> = w.iter().filter(|x| x["kind"] == "unrouted").map(|x| (x["exchange"].as_str().unwrap().into(), x["routingKey"].as_str().unwrap().into(), x["count"].as_u64().unwrap())).collect();
    assert_eq!(un, [("Acme.Contracts:Routed".to_string(), "tenant-c".to_string(), 1)]);
    let mut hops = vec![];
    assert_eq!(broker::rabbit_trace(&n, "Acme.Contracts:Routed", "tenant-a", &Map::new(), Some(&mut hops)), ["acme-routed"]);
    let seen: Vec<String> = hops.iter().map(|h| format!("{}->{}:{}={}", h["source"].as_str().unwrap(), h["destinationType"].as_str().unwrap(), h["destination"].as_str().unwrap(), h["matches"])).collect();
    assert_eq!(seen, ["Acme.Contracts:Routed->exchange:acme-routed=true", "acme-routed->queue:acme-routed=true"]);
    // A binding for tenant-c made later: the warning goes.
    c.bind_exchange("acme-routed", "Acme.Contracts:Routed", "tenant-c").await;
    let n = config::get(&ctx, "acme-bus").unwrap();
    assert!(!crate::tools::warnings(&ctx, &n).await.iter().any(|x| x["kind"] == "unrouted"));

    // The vhost's default type makes them quorum queues; an explicit other type is refused.
    let (_, q) = h.call("GET", "queues/acme-bus/acme-ping", None).await;
    assert_eq!((q["type"].clone(), q["arguments"]["x-queue-type"].clone()), (json!("quorum"), json!("quorum")));
    assert_eq!(c.queue("acme-ping", json!({"x-queue-type": "classic"})).await.err().unwrap().0, 406);
    let (_, b) = h.call("GET", "bindings/acme-bus", None).await;
    let rows: Vec<String> = b.as_array().unwrap().iter().filter(|b| b["source"] != "").map(|b| format!("{}->{}:{}[{}]", b["source"].as_str().unwrap(), b["destination_type"].as_str().unwrap(), b["destination"].as_str().unwrap(), b["routing_key"].as_str().unwrap())).collect();
    assert_eq!(rows, ["Acme.Contracts:Ping->exchange:acme-ping[]", "Acme.Contracts:Routed->exchange:acme-routed[tenant-a]", "Acme.Contracts:Routed->exchange:acme-routed[tenant-b]", "Acme.Contracts:Routed->exchange:acme-routed[tenant-c]", "acme-ping->queue:acme-ping[]", "acme-routed->queue:acme-routed[]"]);

    // Deleting the queue keeps the exchange of the same name (and the other way round).
    assert_eq!(h.st("DELETE", "queues/acme-bus/acme-ping?if-unused=true&if-empty=true", None).await, 400, "quorum queues refuse if-unused");
    assert_eq!(h.st("DELETE", "exchanges/acme-bus/acme-ping?if-unused=true", None).await, 400, "bound: in use");
    assert_eq!(h.st("DELETE", "queues/acme-bus/acme-ping", None).await, 204);
    assert_eq!(h.st("GET", "exchanges/acme-bus/acme-ping", None).await, 200);
    assert_eq!(h.st("DELETE", "queues/acme-bus/acme-ping", None).await, 404);
}

/// A policy's max-length with drop-head drops the oldest; its delivery-limit drops a message
/// returned (channel closed with it unacked) more than N times; nacks with requeue do not count
/// (RabbitMQ 4). With a dead-letter exchange both go there with x-death.
#[tokio::test]
async fn quorum_policy_limits_drop_and_dead_letter() {
    let (envs, _) = envs();
    let h = Http { c: reqwest::Client::new(), base: mgmt(&envs).await, auth: ("guest".into(), "guest".into()) };
    h.st("PUT", "vhosts/acme-lim", None).await;
    h.st("PUT", "policies/acme-lim/lim", Some(json!({"pattern": "^lim", "apply-to": "quorum_queues", "priority": 10, "definition": {"max-length": 3, "overflow": "drop-head", "delivery-limit": 2}}))).await;
    let mut c = Amqp::open(&envs, "guest", "guest", "acme-lim").await.unwrap();
    c.exchange("dlx", "fanout").await;
    c.queue("dlq", json!({})).await.unwrap();
    c.bind("dlq", "dlx", "").await;
    c.queue("lim-q", json!({"x-queue-type": "quorum", "x-dead-letter-exchange": "dlx"})).await.unwrap();
    c.queue("classic-q", json!({})).await.unwrap();
    for i in 1..=5 {
        c.publish("", "lim-q", &format!("m{i}")).await;
        c.publish("", "classic-q", &format!("m{i}")).await;
    }
    assert_eq!((c.count("lim-q").await, c.count("classic-q").await), (3, 5), "the policy is for quorum queues");
    let dead = c.drain("dlq").await;
    assert_eq!(dead.iter().map(|m| m.0.as_str()).collect::<Vec<_>>(), ["m1", "m2"]);
    assert_eq!((dead[0].1["x-first-death-reason"].clone(), dead[0].1["x-death"][0]["queue"].clone()), (json!("maxlen"), json!("lim-q")));
    assert_eq!(h.call("GET", "queues/acme-lim/lim-q", None).await.1["policy"], "lim");

    // nack requeue=true: redelivered, never counted
    for _ in 0..4 {
        let (b, _, dc, tag, _) = c.get("lim-q").await.unwrap();
        assert_eq!((b.as_str(), dc), ("m3", None));
        c.nack(tag, true).await;
    }
    // returned by a closed channel: counted in x-delivery-count; the third return passes the limit
    let mut seen = vec![];
    for _ in 0..3 {
        let (b, red, dc, _, _) = c.get("lim-q").await.unwrap();
        seen.push((b, red, dc));
        c.reopen().await;
    }
    assert_eq!(seen, [("m3".to_string(), true, None), ("m3".to_string(), true, Some(1)), ("m3".to_string(), true, Some(2))]);
    assert_eq!(c.get("lim-q").await.unwrap().0, "m4");
    let dead = c.drain("dlq").await;
    assert_eq!((dead.len(), dead[0].0.as_str(), dead[0].1["x-first-death-reason"].clone()), (1, "m3", json!("delivery_limit")));

    // reject requeue=false: dead-lettered with reason rejected; without a dead-letter exchange, gone
    c.reopen().await;
    let (_, _, _, tag, _) = c.get("lim-q").await.unwrap();
    c.nack(tag, false).await;
    let (_, _, _, tag, _) = c.get("classic-q").await.unwrap();
    c.nack(tag, false).await;
    assert_eq!(c.drain("dlq").await[0].1["x-last-death-reason"], "rejected");
    assert_eq!(c.count("classic-q").await, 4);
    // classic queues have no delivery limit
    for _ in 0..4 {
        c.get("classic-q").await.unwrap();
        c.reopen().await;
    }
    assert_eq!(c.count("classic-q").await, 4);
    assert_eq!(h.st("DELETE", "queues/acme-lim/classic-q?if-unused=true&if-empty=true", None).await, 400, "not empty");
}

/// `down` is a broker that died (docker stop): open connections drop at once (TCP closed, no
/// connection.close), new ones are refused, the management API answers 503 for the vhost; unacked
/// deliveries go back to the queue. `close` says CONNECTION_FORCED first. `ok` brings it back.
#[tokio::test]
async fn reachability_down_drops_open_connections() {
    let (envs, ctx) = envs();
    let h = Http { c: reqwest::Client::new(), base: mgmt(&envs).await, auth: ("guest".into(), "guest".into()) };
    h.st("PUT", "vhosts/acme-down", Some(json!({"default_queue_type": "quorum"}))).await;
    let set = |mode: &str| {
        let m = mode.to_string();
        crate::config::change(&ctx, "acme-down", |n| {
            n.behaviour.reachability = m;
            Ok(())
        })
        .unwrap();
    };
    let mut c = Amqp::open(&envs, "guest", "guest", "acme-down").await.unwrap();
    c.queue("acme-orders", json!({})).await.unwrap();
    c.publish("", "acme-orders", "o1").await;
    c.publish("", "acme-orders", "o2").await;
    assert_eq!(c.get("acme-orders").await.unwrap().0, "o1"); // unacked when the broker dies

    set("down");
    let gone = tokio::time::timeout(std::time::Duration::from_secs(3), c.next()).await.expect("the open connection must drop");
    assert!(gone.is_none(), "a dead broker sends nothing: {gone:?}");
    assert_eq!(Amqp::open(&envs, "guest", "guest", "acme-down").await.err().unwrap().0, 0, "new connections are refused");
    assert_eq!(h.st("GET", "queues/acme-down", None).await, 503);

    set("ok");
    let mut c = Amqp::open(&envs, "guest", "guest", "acme-down").await.unwrap();
    assert_eq!(c.count("acme-orders").await, 2, "durable messages stay; the unacked one came back");
    let (b, redelivered, ..) = c.get("acme-orders").await.unwrap();
    assert_eq!((b.as_str(), redelivered), ("o1", true));

    set("close");
    match tokio::time::timeout(std::time::Duration::from_secs(3), c.next()).await.unwrap() {
        Some(AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Close(x)))) => assert_eq!(x.reply_code, 320),
        f => panic!("expected connection.close 320, got {f:?}"),
    }
    set("ok");
}

/// vhost "/" is the namespace named default (made on first use), the tools reach it by "/",
/// /api/connections lists and closes connections, deleting a vhost closes its connections, and
/// every user, permission and vhost change is a Timeline row with who and from where.
#[tokio::test]
async fn slash_vhost_connections_and_audit() {
    let (envs, ctx) = envs();
    ctx.db.migrate(stuntman_core::trips::SQL).unwrap();
    stuntman_core::trips::upgrade(&ctx.db);
    let h = Http { c: reqwest::Client::new(), base: mgmt(&envs).await, auth: ("guest".into(), "guest".into()) };
    // "/" always exists in RabbitMQ: 204, and the namespace behind it is made now.
    assert_eq!(h.st("PUT", "vhosts/%2F", None).await, 204);
    assert_eq!(config::get(&ctx, "default").unwrap().kind, config::RABBIT);
    h.st("PUT", "vhosts/acme-late", None).await;
    assert_eq!(rabbit::slash(&ctx).unwrap().namespace, "default", "a later namespace does not take over /");

    let mut c = Amqp::open(&envs, "guest", "guest", "/").await.unwrap();
    // Other tests run in parallel in an environment of the same name: look at this vhost's.
    let (_, list) = h.call("GET", "vhosts/%2F/connections", None).await;
    assert_eq!((list[0]["vhost"].as_str(), list[0]["namespace"].as_str(), list[0]["channels"].as_u64()), (Some("/"), Some("default"), Some(1)), "{list}");
    let name = list[0]["name"].as_str().unwrap().to_string();
    assert!(h.call("GET", "connections", None).await.1.as_array().unwrap().iter().any(|c| c["name"] == json!(name)));
    assert_eq!(h.call("GET", &format!("connections/{}", name.replace(' ', "%20").replace('>', "%3E")), None).await.1["user"], "guest");
    assert_eq!(h.call("GET", "vhosts/acme-late/connections", None).await.1, json!([]));

    assert_eq!(h.st("DELETE", "vhosts/%2F", None).await, 204);
    match tokio::time::timeout(std::time::Duration::from_secs(3), c.next()).await.unwrap() {
        Some(AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Close(x)))) => {
            assert_eq!((x.reply_code, x.reply_text.to_string()), (320, "CONNECTION_FORCED - vhost '/' is deleted".to_string()))
        }
        f => panic!("expected connection.close 320, got {f:?}"),
    }

    let mut c = Amqp::open(&envs, "guest", "guest", "acme-late").await.unwrap();
    let name = h.call("GET", "vhosts/acme-late/connections", None).await.1[0]["name"].as_str().unwrap().to_string();
    let r = h.c.delete(format!("{}connections/{}", h.base, name.replace(' ', "%20").replace('>', "%3E"))).header("X-Reason", "rotating credentials").basic_auth("guest", Some("guest")).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 204);
    match tokio::time::timeout(std::time::Duration::from_secs(3), c.next()).await.unwrap() {
        Some(AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Close(x)))) => assert_eq!(x.reply_text.to_string(), "CONNECTION_FORCED - rotating credentials"),
        f => panic!("expected connection.close, got {f:?}"),
    }
    assert_eq!(h.st("DELETE", "connections/nope", None).await, 404);

    assert_eq!(h.st("PUT", "users/acme-app", Some(json!({"password": "s3", "tags": ""}))).await, 201);
    h.st("PUT", "permissions/acme-late/acme-app", Some(json!({"configure": ".*", "write": ".*", "read": ".*"}))).await;
    let rows: Vec<(String, String, String)> = ctx.db.call(|c| c.prepare("SELECT label, actor, detail FROM trips WHERE host = 'rabbitmq management' ORDER BY at, rowid")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect()).await.unwrap();
    let labels: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert!(labels.contains(&"user acme-app: password set"), "{labels:?}");
    assert!(labels.contains(&"permission of acme-app on vhost acme-late set (configure .*, write .*, read .*)"), "{labels:?}");
    assert!(labels.contains(&"vhost / deleted"), "{labels:?}");
    assert!(rows.iter().all(|r| r.1 == "guest" && r.2.contains("management API")), "{rows:?}");
}

/// Every vhost has amq.direct, amq.fanout, amq.topic, amq.headers and amq.match: they route by
/// their type, a declare of the same type (passive or not) answers ok, the management API lists
/// them, and nobody deletes them (ACCESS_REFUSED), nor the default exchange.
#[tokio::test]
async fn builtin_exchanges_exist_route_and_cannot_be_deleted() {
    let (envs, _) = envs();
    let h = Http { c: reqwest::Client::new(), base: mgmt(&envs).await, auth: ("guest".into(), "guest".into()) };
    h.st("PUT", "vhosts/globex-bus", None).await;
    let (_, x) = h.call("GET", "exchanges/globex-bus?columns=name,type", None).await;
    let rows: Vec<String> = x.as_array().unwrap().iter().map(|r| format!("{}:{}", r["name"].as_str().unwrap(), r["type"].as_str().unwrap())).collect();
    assert_eq!(rows, [":direct", "amq.direct:direct", "amq.fanout:fanout", "amq.headers:headers", "amq.match:headers", "amq.topic:topic"]);

    let mut c = Amqp::open(&envs, "guest", "guest", "globex-bus").await.unwrap();
    let declare = |name: &str, kind: &str, passive: bool| AMQPClass::Exchange(exchange::AMQPMethod::Declare(exchange::Declare { exchange: name.into(), kind: kind.into(), passive, durable: true, ..Default::default() }));
    assert!(c.call(declare("amq.direct", "direct", true)).await.is_ok());
    assert!(c.call(declare("amq.topic", "topic", false)).await.is_ok());
    assert_eq!(c.call(declare("amq.fanout", "direct", false)).await.err().unwrap().0, 406);
    assert_eq!(c.call(declare("amq.acme", "direct", false)).await.err().unwrap().0, 403, "amq.* is reserved");

    for q in ["acme-orders", "acme-audit"] {
        c.queue(q, json!({})).await.unwrap();
    }
    c.bind("acme-orders", "amq.direct", "orders").await;
    c.bind("acme-audit", "amq.topic", "orders.#").await;
    c.bind("acme-audit", "amq.fanout", "").await;
    c.publish("amq.direct", "orders", "direct").await;
    c.publish("amq.direct", "other", "lost").await;
    c.publish("amq.topic", "orders.created", "topic").await;
    c.publish("amq.fanout", "whatever", "fanout").await;
    assert_eq!(c.drain("acme-orders").await.into_iter().map(|m| m.0).collect::<Vec<_>>(), ["direct"]);
    assert_eq!(c.drain("acme-audit").await.into_iter().map(|m| m.0).collect::<Vec<_>>(), ["topic", "fanout"]);

    let delete = |name: &str| AMQPClass::Exchange(exchange::AMQPMethod::Delete(exchange::Delete { exchange: name.into(), ..Default::default() }));
    assert_eq!(c.call(delete("amq.direct")).await.err().unwrap(), (403, "ACCESS_REFUSED - deletion of system exchange 'amq.direct' in vhost 'globex-bus' not allowed".into()));
    assert_eq!(c.call(delete("")).await.err().unwrap().0, 403);
    assert_eq!(h.st("DELETE", "exchanges/globex-bus/amq.topic", None).await, 403);
    assert_eq!(h.st("GET", "exchanges/globex-bus/amq.topic", None).await, 200);
    assert_eq!(h.st("PUT", "exchanges/globex-bus/amq.headers", Some(json!({"type": "headers"}))).await, 204);
}
