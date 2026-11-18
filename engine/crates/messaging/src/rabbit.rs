//! RabbitMQ (AMQP 0-9-1): what RabbitMQ.Client, pika and lapin speak. A vhost is a namespace;
//! exchanges are topics whose bindings are subscriptions pointing at queues. Unacked messages
//! stay with their channel and go back to the queue (redelivered) when it closes.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use amq_protocol::{
    frame::{gen_frame, parse_frame, AMQPContentHeader, AMQPFrame},
    protocol::{basic, channel, confirm, connection, exchange, queue, AMQPClass},
    types::{AMQPValue, FieldArray, FieldTable, LongString, ShortString},
};
use serde_json::{json, Map, Value};
use stuntman_core::{env::Envs, Ctx};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, Notify},
};

use crate::{broker, config};

type Tx = mpsc::UnboundedSender<Vec<u8>>;

pub(crate) fn enc(f: &AMQPFrame) -> Vec<u8> {
    cookie_factory::gen_simple(gen_frame(f), Vec::new()).unwrap_or_default()
}

fn send(tx: &Tx, f: AMQPFrame) {
    let _ = tx.send(enc(&f));
}

fn ss(s: &str) -> ShortString {
    ShortString::from(s.to_string())
}

// ---------------------------------------------------------------- value conversion

pub fn to_json(v: &AMQPValue) -> Value {
    match v {
        AMQPValue::Boolean(b) => json!(b),
        AMQPValue::ShortShortInt(n) => json!(n),
        AMQPValue::ShortShortUInt(n) => json!(n),
        AMQPValue::ShortInt(n) => json!(n),
        AMQPValue::ShortUInt(n) => json!(n),
        AMQPValue::LongInt(n) => json!(n),
        AMQPValue::LongUInt(n) => json!(n),
        AMQPValue::LongLongInt(n) => json!(n),
        AMQPValue::Float(n) => json!(n),
        AMQPValue::Double(n) => json!(n),
        AMQPValue::ShortString(s) => json!(s.as_str()),
        AMQPValue::LongString(s) => json!(String::from_utf8_lossy(s.as_bytes())),
        AMQPValue::FieldArray(a) => Value::Array(a.as_slice().iter().map(to_json).collect()),
        AMQPValue::Timestamp(t) => json!(t),
        AMQPValue::FieldTable(t) => Value::Object(t.inner().iter().map(|(k, v)| (k.as_str().to_string(), to_json(v))).collect()),
        AMQPValue::ByteArray(b) => json!(String::from_utf8_lossy(b.as_slice())),
        AMQPValue::Void => Value::Null,
        AMQPValue::DecimalValue(d) => json!(format!("{d:?}")),
    }
}

pub fn from_json(v: &Value) -> AMQPValue {
    match v {
        Value::Bool(b) => AMQPValue::Boolean(*b),
        Value::Number(n) if n.is_i64() => AMQPValue::LongLongInt(n.as_i64().unwrap()),
        Value::Number(n) => AMQPValue::Double(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => AMQPValue::LongString(LongString::from(s.clone())),
        Value::Array(a) => AMQPValue::FieldArray(FieldArray::from(a.iter().map(from_json).collect::<Vec<_>>())),
        Value::Object(o) => AMQPValue::FieldTable(table(o)),
        Value::Null => AMQPValue::Void,
    }
}

#[cfg(test)]
pub(crate) fn table_of(v: &Value) -> FieldTable {
    v.as_object().map(table).unwrap_or_default()
}

fn table(o: &Map<String, Value>) -> FieldTable {
    let mut t = FieldTable::default();
    for (k, v) in o {
        t.insert(ss(k), from_json(v));
    }
    t
}

fn table_json(t: &FieldTable) -> Map<String, Value> {
    t.inner().iter().map(|(k, v)| (k.as_str().to_string(), to_json(v))).collect()
}

/// AMQP 0-9-1 properties → the broker's words (props = headers; the rest in sys).
fn outgoing(p: &basic::AMQPProperties, exchange: &str, key: &str, body: Vec<u8>, sender: &str) -> broker::Outgoing {
    let mut sys = Map::new();
    sys.insert("exchange".into(), json!(exchange));
    sys.insert("routingKey".into(), json!(key));
    let s = |o: &Option<ShortString>| o.as_ref().map(|x| x.as_str().to_string());
    if let Some(v) = s(p.correlation_id()) { sys.insert("correlationId".into(), json!(v)); }
    if let Some(v) = s(p.reply_to()) { sys.insert("replyTo".into(), json!(v)); }
    if let Some(v) = s(p.expiration()) {
        if let Ok(ms) = v.parse::<i64>() { sys.insert("timeToLive".into(), json!(ms)); }
        sys.insert("expiration".into(), json!(v));
    }
    if let Some(v) = s(p.kind()) { sys.insert("type".into(), json!(v)); }
    if let Some(v) = s(p.user_id()) { sys.insert("userId".into(), json!(v)); }
    if let Some(v) = s(p.app_id()) { sys.insert("appId".into(), json!(v)); }
    if let Some(v) = s(p.content_encoding()) { sys.insert("contentEncoding".into(), json!(v)); }
    if let Some(v) = p.delivery_mode() { sys.insert("deliveryMode".into(), json!(v)); }
    if let Some(v) = p.priority() { sys.insert("priority".into(), json!(v)); }
    if let Some(v) = p.timestamp() { sys.insert("timestamp".into(), json!(v)); }
    let subject = s(p.kind()).filter(|x| !x.is_empty()).unwrap_or_else(|| if exchange.is_empty() { key.to_string() } else { exchange.to_string() });
    broker::Outgoing {
        message_id: s(p.message_id()).unwrap_or_default(),
        subject,
        content_type: s(p.content_type()).unwrap_or_default(),
        body,
        props: p.headers().as_ref().map(table_json).unwrap_or_default(),
        sys,
        scheduled_at: None,
        sender: sender.into(),
        via: "amqp".into(),
    }
}

fn properties(c: &broker::Claimed) -> basic::AMQPProperties {
    let mut p = basic::AMQPProperties::default();
    let g = |k: &str| c.sys.get(k).and_then(|v| v.as_str()).map(ss);
    if !c.content_type.is_empty() { p = p.with_content_type(ss(&c.content_type)); }
    if !c.message_id.is_empty() { p = p.with_message_id(ss(&c.message_id)); }
    if !c.props.is_empty() { p = p.with_headers(table(&c.props)); }
    if let Some(v) = g("correlationId") { p = p.with_correlation_id(v); }
    if let Some(v) = g("replyTo") { p = p.with_reply_to(v); }
    if let Some(v) = g("expiration") { p = p.with_expiration(v); }
    if let Some(v) = g("type") { p = p.with_type(v); }
    if let Some(v) = g("userId") { p = p.with_user_id(v); }
    if let Some(v) = g("appId") { p = p.with_app_id(v); }
    if let Some(v) = g("contentEncoding") { p = p.with_content_encoding(v); }
    if let Some(v) = c.sys.get("deliveryMode").and_then(|v| v.as_u64()) { p = p.with_delivery_mode(v as u8); }
    if let Some(v) = c.sys.get("priority").and_then(|v| v.as_u64()) { p = p.with_priority(v as u8); }
    if let Some(v) = c.sys.get("timestamp").and_then(|v| v.as_u64()) { p = p.with_timestamp(v); }
    p
}

// ---------------------------------------------------------------- namespace lookup

/// A vhost names a namespace: "<ns>", "<env>/<ns>" or "/" (the default environment's first
/// RabbitMQ namespace). An unknown name is created in the default environment, so an app works
/// on day zero; it shows up on the screen like any other.
pub fn find(envs: &Envs, vhost: &str) -> Option<(Ctx, config::Namespace)> {
    if let Some(found) = find_existing(envs, vhost) {
        return Some(found);
    }
    let v = vhost.trim_start_matches('/');
    if v.contains('/') {
        return None;
    }
    let ctx = envs.default_env()?;
    config::valid_name(v).ok()?;
    config::create(&ctx, config::RABBIT, v).ok().map(|n| (ctx, n))
}

/// The namespace a vhost names, without creating one (the management API answers 404).
pub fn find_existing(envs: &Envs, vhost: &str) -> Option<(Ctx, config::Namespace)> {
    let v = vhost.trim_start_matches('/');
    if let Some((e, n)) = v.split_once('/') {
        let ctx = envs.get(e)?;
        return config::get(&ctx, n).ok().filter(|x| x.kind == config::RABBIT).map(|x| (ctx, x));
    }
    let mut all = envs.all();
    let d = envs.default_name();
    all.sort_by_key(|c| *c.env != d);
    if v.is_empty() {
        let ctx = envs.default_env()?;
        let n = config::list(&ctx).into_iter().find(|n| n.kind == config::RABBIT).or_else(|| config::create(&ctx, config::RABBIT, "default-rabbit").ok())?;
        return Some((ctx, n));
    }
    all.into_iter().find_map(|c| config::get(&c, v).ok().filter(|n| n.kind == config::RABBIT).map(|n| (c, n)))
}

// ---------------------------------------------------------------- connection

struct Unacked {
    token: String,
    queue: String,
    lose_lock: bool,
}

#[derive(Default)]
struct Chan {
    confirm: bool,
    pub_seq: u64,
    prefetch: u16,
    next_tag: u64,
    unacked: BTreeMap<u64, Unacked>,
    /// consumer tag → (consumer id, stop)
    consumers: HashMap<String, (String, Arc<Notify>)>,
    /// A publish waiting for its header and body.
    pending: Option<(basic::Publish, Option<AMQPContentHeader>, Vec<u8>)>,
}

struct Conn {
    ctx: Ctx,
    ns: config::Namespace,
    client: String,
    remote: String,
    tx: Tx,
    chans: Mutex<HashMap<u16, Arc<Mutex<Chan>>>>,
    /// Rung when a channel's unacked count drops (prefetch waits on it).
    acked: Arc<Notify>,
    /// "Disconnect it" on any of its consumers closes the whole connection (CONNECTION_FORCED).
    kill: Arc<Notify>,
}

fn close_conn(tx: &Tx, code: u16, text: &str, class: u16, method: u16) {
    send(tx, AMQPFrame::Method(0, AMQPClass::Connection(connection::AMQPMethod::Close(connection::Close { reply_code: code, reply_text: ss(text), class_id: class, method_id: method }))));
}

fn close_chan(tx: &Tx, ch: u16, code: u16, text: &str, class: u16, method: u16) {
    send(tx, AMQPFrame::Method(ch, AMQPClass::Channel(channel::AMQPMethod::Close(channel::Close { reply_code: code, reply_text: ss(text), class_id: class, method_id: method }))));
}

/// Reads whole frames from the socket into `buf`.
pub(crate) async fn next_frame<R: tokio::io::AsyncRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> Option<AMQPFrame> {
    loop {
        if !buf.is_empty() {
            match parse_frame(&buf[..]) {
                Ok((rest, f)) => {
                    let used = buf.len() - rest.len();
                    buf.drain(..used);
                    return Some(f);
                }
                Err(e) if e.is_incomplete() => {}
                Err(_) => return None,
            }
        }
        // Straight into buf: a 64 KiB array here would sit in every future that awaits a frame.
        buf.reserve(65536);
        if r.read_buf(buf).await.ok()? == 0 {
            return None;
        }
    }
}

pub async fn serve(envs: Envs, sock: crate::amqp::Io, remote: String) {
    let (mut r, mut w) = tokio::io::split(sock);
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        while let Some(b) = rx.recv().await {
            if w.write_all(&b).await.is_err() {
                break;
            }
        }
        let _ = w.shutdown().await;
    });
    let mut buf = Vec::new();
    // connection.start
    let mut caps = FieldTable::default();
    for c in ["publisher_confirms", "exchange_exchange_bindings", "basic.nack", "consumer_cancel_notify", "connection.blocked", "authentication_failure_close", "per_consumer_qos"] {
        caps.insert(ss(c), AMQPValue::Boolean(true));
    }
    let mut props = FieldTable::default();
    props.insert(ss("product"), AMQPValue::LongString("RabbitMQ".into()));
    props.insert(ss("version"), AMQPValue::LongString("3.13.7 (Stuntman)".into()));
    props.insert(ss("platform"), AMQPValue::LongString("Stuntman".into()));
    props.insert(ss("capabilities"), AMQPValue::FieldTable(caps));
    send(&tx, AMQPFrame::Method(0, AMQPClass::Connection(connection::AMQPMethod::Start(connection::Start { version_major: 0, version_minor: 9, server_properties: props, mechanisms: "PLAIN AMQPLAIN".into(), locales: "en_US".into() }))));
    let Some(AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::StartOk(ok)))) = next_frame(&mut r, &mut buf).await else { return };
    let cp = table_json(&ok.client_properties);
    let (user, pass) = credentials(ok.mechanism.as_str(), ok.response.as_bytes());
    let client = cp.get("connection_name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(String::from)
        .unwrap_or_else(|| {
            // No client-provided name: product, platform and the client's port tell connections apart.
            let product = cp.get("product").and_then(|v| v.as_str()).unwrap_or("client");
            let platform = cp.get("platform").and_then(|v| v.as_str()).unwrap_or("");
            let platform = if platform.contains(".NET") { ".NET" } else { platform.split_whitespace().next().unwrap_or("") };
            format!("{product}{}{} :{}", if platform.is_empty() { "" } else { " " }, platform, remote.rsplit(':').next().unwrap_or(""))
        });
    // A declared user with a wrong password is refused right after start-ok, as RabbitMQ does.
    if envs.default_env().map(|d| matches!(crate::rabbit_mgmt::login(&d, &user, &pass), crate::rabbit_mgmt::Login::Refused)).unwrap_or(false) {
        close_conn(&tx, 403, "ACCESS_REFUSED - Login was refused using authentication mechanism PLAIN. For details see the broker logfile.", 10, 40);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        return;
    }
    send(&tx, AMQPFrame::Method(0, AMQPClass::Connection(connection::AMQPMethod::Tune(connection::Tune { channel_max: 2047, frame_max: 131072, heartbeat: 60 }))));
    let mut heartbeat = 60u16;
    let vhost = loop {
        match next_frame(&mut r, &mut buf).await {
            Some(AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::TuneOk(t)))) => heartbeat = t.heartbeat,
            Some(AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Open(o)))) => break o.virtual_host.as_str().to_string(),
            Some(AMQPFrame::Heartbeat) => {}
            _ => return,
        }
    };
    let Some((ctx, ns)) = find(&envs, &vhost) else {
        close_conn(&tx, 530, &format!("NOT_ALLOWED - vhost {vhost} not found"), 10, 40);
        return;
    };
    match ns.behaviour.reachability.as_str() {
        "refuse" | "down" | "close" => return,
        "timeout" => {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            return;
        }
        "401" => {
            close_conn(&tx, 403, "ACCESS_REFUSED - Login was refused using authentication mechanism PLAIN. For details see the broker logfile.", 10, 40);
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            return;
        }
        _ => {}
    }
    // A user declared through the management API (rabbitmqadmin) is checked as RabbitMQ does:
    // its password, then its permission on this vhost. Anyone else: the namespace's own rules.
    let allowed = match crate::rabbit_mgmt::login(&ctx, &user, &pass) {
        crate::rabbit_mgmt::Login::Ok(_) if !ns.access.permissions.contains_key(&user) => {
            close_conn(&tx, 530, &format!("NOT_ALLOWED - access to vhost '{}' refused for user '{user}'", if vhost.is_empty() { "/" } else { &vhost }), 10, 40);
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            return;
        }
        crate::rabbit_mgmt::Login::Ok(_) => true,
        crate::rabbit_mgmt::Login::Refused => false,
        crate::rabbit_mgmt::Login::Unknown => ns.access.any_key || (ns.access.policies.contains_key(&user) && config::key_value(&ctx, &ns.namespace, &user) == pass),
    };
    if !allowed {
        close_conn(&tx, 403, "ACCESS_REFUSED - Login was refused using authentication mechanism PLAIN. For details see the broker logfile.", 10, 40);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        return;
    }
    send(&tx, AMQPFrame::Method(0, AMQPClass::Connection(connection::AMQPMethod::OpenOk(connection::OpenOk {}))));
    if ns.behaviour.reachability == "busy" {
        send(&tx, AMQPFrame::Method(0, AMQPClass::Connection(connection::AMQPMethod::Blocked(connection::Blocked { reason: ss("low on memory (Stuntman: the namespace is set to busy)") }))));
    }
    ctx.live.seen(client.split(" :").next().unwrap_or(&client), "RabbitMQ");
    let kill = Arc::new(Notify::new());
    let conn = Arc::new(Conn { ctx: ctx.clone(), ns: ns.clone(), client: client.clone(), remote: remote.clone(), tx: tx.clone(), chans: Mutex::new(HashMap::new()), acked: Arc::new(Notify::new()), kill: kill.clone() });
    // Heartbeats both ways: ours every half interval.
    let hb_tx = tx.clone();
    let hb = tokio::spawn(async move {
        if heartbeat == 0 {
            return;
        }
        loop {
            tokio::time::sleep(std::time::Duration::from_secs((heartbeat as u64 / 2).max(1))).await;
            if hb_tx.send(enc(&AMQPFrame::Heartbeat)).is_err() {
                break;
            }
        }
    });

    let mut reach = config::reach_watch(&ctx, &ns);
    let mut dead = false;
    let reason = loop {
        let f = tokio::select! {
            f = next_frame(&mut r, &mut buf) => f,
            _ = kill.notified() => {
                close_conn(&tx, 320, "CONNECTION_FORCED - Closed via management plugin (Stuntman: Disconnect it)", 0, 0);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                break "disconnected from Stuntman (Disconnect it)";
            }
            Ok(()) = reach.changed() => {
                let mode = reach.borrow_and_update().clone();
                if mode == "close" {
                    close_conn(&tx, 320, "CONNECTION_FORCED - broker forced connection closure with reason 'shutdown'", 0, 0);
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    break "the broker shut down (Stuntman: reachability close)";
                }
                if mode == "down" {
                    // A killed broker: the socket just goes, nothing is said.
                    dead = true;
                    writer.abort();
                    break "the broker went down (Stuntman: reachability down)";
                }
                continue;
            }
        };
        let Some(f) = f else { break "connection closed by the client" };
        if !handle(&conn, f).await {
            break "connection closed by the client";
        }
    };
    hb.abort();
    // The channels die with the connection: unacked messages go back, consumers stop.
    let chans: Vec<Arc<Mutex<Chan>>> = conn.chans.lock().unwrap().drain().map(|(_, c)| c).collect();
    for c in chans {
        close_channel_state(&conn, &c, reason).await;
    }
    drop(tx);
    if !dead {
        let _ = writer.await;
    }
}

fn credentials(mech: &str, resp: &[u8]) -> (String, String) {
    if mech == "PLAIN" {
        let parts: Vec<&[u8]> = resp.split(|b| *b == 0).collect();
        if parts.len() >= 3 {
            return (String::from_utf8_lossy(parts[1]).into(), String::from_utf8_lossy(parts[2]).into());
        }
    }
    // AMQPLAIN: a field table with LOGIN and PASSWORD; good enough to read them loosely.
    let s = String::from_utf8_lossy(resp);
    let grab = |k: &str| s.split(k).nth(1).map(|x| x.chars().skip(5).take_while(|c| !c.is_control()).collect::<String>()).unwrap_or_default();
    (grab("LOGIN"), grab("PASSWORD"))
}

async fn close_channel_state(conn: &Conn, c: &Arc<Mutex<Chan>>, why: &str) {
    let (consumers, unacked) = {
        let mut g = c.lock().unwrap();
        (g.consumers.drain().collect::<Vec<_>>(), std::mem::take(&mut g.unacked))
    };
    for (_, (id, stop)) in &consumers {
        stop.notify_waiters();
        stop.notify_one();
        broker::consumer_close(&conn.ctx, id, why).await;
    }
    // Unacked messages go back to their queue; a quorum queue counts the return and drops (or
    // dead-letters) the message past its delivery limit. Nacks with requeue do not count (RabbitMQ 4).
    let ns = config::get(&conn.ctx, &conn.ns.namespace).unwrap_or_else(|_| conn.ns.clone());
    for u in unacked.values() {
        let limit = ns.rabbit_queue(&u.queue).and_then(|q| q.delivery_limit);
        broker::rabbit_return(&conn.ctx, &conn.ns.namespace, &u.token, &format!("{why}: unacked, redelivered"), limit).await;
    }
    // Messages fetched with basic.get carry the channel's own id.
    conn.acked.notify_waiters();
}

fn chan(conn: &Conn, id: u16) -> Arc<Mutex<Chan>> {
    conn.chans.lock().unwrap().entry(id).or_insert_with(|| Arc::new(Mutex::new(Chan::default()))).clone()
}

/// One frame; false ends the connection.
async fn handle(conn: &Arc<Conn>, f: AMQPFrame) -> bool {
    let tx = &conn.tx;
    let ctx = &conn.ctx;
    let nsn = conn.ns.namespace.clone();
    match f {
        AMQPFrame::Heartbeat | AMQPFrame::InvalidHeartbeat(_) | AMQPFrame::ProtocolHeader(_) => true,
        AMQPFrame::Header(ch, h) => {
            let c = chan(conn, ch);
            let ready = {
                let mut g = c.lock().unwrap();
                if let Some(p) = g.pending.as_mut() {
                    p.1 = Some(h.clone());
                    h.body_size == 0
                } else {
                    false
                }
            };
            if ready {
                finish_publish(conn, ch).await;
            }
            true
        }
        AMQPFrame::Body(ch, data) => {
            let c = chan(conn, ch);
            let ready = {
                let mut g = c.lock().unwrap();
                match g.pending.as_mut() {
                    Some(p) => {
                        p.2.extend_from_slice(&data);
                        p.1.as_ref().map(|h| p.2.len() as u64 >= h.body_size).unwrap_or(false)
                    }
                    None => false,
                }
            };
            if ready {
                finish_publish(conn, ch).await;
            }
            true
        }
        AMQPFrame::Method(ch, m) => match m {
            AMQPClass::Connection(connection::AMQPMethod::Close(_)) => {
                send(tx, AMQPFrame::Method(0, AMQPClass::Connection(connection::AMQPMethod::CloseOk(connection::CloseOk {}))));
                false
            }
            AMQPClass::Connection(connection::AMQPMethod::CloseOk(_)) => false,
            AMQPClass::Connection(_) => true,
            AMQPClass::Channel(channel::AMQPMethod::Open(_)) => {
                conn.chans.lock().unwrap().insert(ch, Arc::new(Mutex::new(Chan::default())));
                send(tx, AMQPFrame::Method(ch, AMQPClass::Channel(channel::AMQPMethod::OpenOk(channel::OpenOk {}))));
                true
            }
            AMQPClass::Channel(channel::AMQPMethod::Close(_)) => {
                let c = conn.chans.lock().unwrap().remove(&ch);
                if let Some(c) = c {
                    close_channel_state(conn, &c, "channel closed by the client").await;
                }
                send(tx, AMQPFrame::Method(ch, AMQPClass::Channel(channel::AMQPMethod::CloseOk(channel::CloseOk {}))));
                true
            }
            AMQPClass::Channel(channel::AMQPMethod::CloseOk(_)) => {
                conn.chans.lock().unwrap().remove(&ch);
                true
            }
            AMQPClass::Channel(channel::AMQPMethod::Flow(fl)) => {
                send(tx, AMQPFrame::Method(ch, AMQPClass::Channel(channel::AMQPMethod::FlowOk(channel::FlowOk { active: fl.active }))));
                true
            }
            AMQPClass::Channel(_) => true,
            AMQPClass::Confirm(confirm::AMQPMethod::SelectOk(_)) => true,
            AMQPClass::Confirm(confirm::AMQPMethod::Select(s)) => {
                chan(conn, ch).lock().unwrap().confirm = true;
                if !s.nowait {
                    send(tx, AMQPFrame::Method(ch, AMQPClass::Confirm(confirm::AMQPMethod::SelectOk(confirm::SelectOk {}))));
                }
                true
            }
            AMQPClass::Exchange(exchange::AMQPMethod::Declare(d)) => {
                let name = d.exchange.as_str().to_string();
                let kind = d.kind.as_str().to_string();
                match declare_exchange(ctx, &nsn, &conn.client, &name, &kind, d.passive) {
                    Ok(()) => {
                        if !d.nowait {
                            send(tx, AMQPFrame::Method(ch, AMQPClass::Exchange(exchange::AMQPMethod::DeclareOk(exchange::DeclareOk {}))));
                        }
                    }
                    Err((code, text)) => close_and_forget(conn, ch, code, &text, 40, 10).await,
                }
                true
            }
            AMQPClass::Exchange(exchange::AMQPMethod::Delete(d)) => {
                let name = d.exchange.as_str().to_string();
                let r = config::change(ctx, &nsn, |n| {
                    if d.if_unused && exchange_in_use(n, &name) {
                        return Err(stuntman_core::ApiError::new(406, format!("PRECONDITION_FAILED - exchange '{name}' in vhost '{nsn}' in use")));
                    }
                    drop_exchange(n, &name);
                    Ok(())
                });
                match r {
                    Err(e) if e.status == 406 => close_and_forget(conn, ch, 406, &e.message, 40, 20).await,
                    _ if !d.nowait => send(tx, AMQPFrame::Method(ch, AMQPClass::Exchange(exchange::AMQPMethod::DeleteOk(exchange::DeleteOk {})))),
                    _ => {}
                }
                true
            }
            AMQPClass::Exchange(exchange::AMQPMethod::Bind(b)) => {
                // Exchange-to-exchange: what MassTransit binds message types to its endpoints with.
                let (dst, src, key) = (b.destination.as_str().to_string(), b.source.as_str().to_string(), b.routing_key.as_str().to_string());
                match config::change(ctx, &nsn, |n| bind(n, &src, &dst, true, &key, &binding_args(&b.arguments))) {
                    Ok(_) if !b.nowait => send(tx, AMQPFrame::Method(ch, AMQPClass::Exchange(exchange::AMQPMethod::BindOk(exchange::BindOk {})))),
                    Ok(_) => {}
                    Err(e) => close_and_forget(conn, ch, e.status, &e.message, 40, 30).await,
                }
                true
            }
            AMQPClass::Exchange(exchange::AMQPMethod::Unbind(u)) => {
                let (dst, src, key) = (u.destination.as_str().to_string(), u.source.as_str().to_string(), u.routing_key.as_str().to_string());
                let _ = config::change(ctx, &nsn, |n| {
                    unbind(n, &src, &dst, true, &key);
                    Ok(())
                });
                if !u.nowait {
                    send(tx, AMQPFrame::Method(ch, AMQPClass::Exchange(exchange::AMQPMethod::UnbindOk(exchange::UnbindOk {}))));
                }
                true
            }
            AMQPClass::Exchange(_) => true,
            AMQPClass::Queue(queue::AMQPMethod::Declare(d)) => {
                let mut name = d.queue.as_str().to_string();
                if name.is_empty() {
                    name = format!("amq.gen-{}", &broker::uuid4()[..13]);
                }
                let args = table_json(&d.arguments);
                match declare_queue(ctx, &nsn, &conn.client, &name, d.passive, d.durable && !d.exclusive, args) {
                    Ok(()) => {
                        if !d.nowait {
                            let counts = broker::counts(ctx, &nsn, &name).await;
                            let consumers = broker::consumers(ctx, &nsn, &name).await.map(|v| v.iter().filter(|c| c["state"] == "receiving").count()).unwrap_or(0);
                            send(tx, AMQPFrame::Method(ch, AMQPClass::Queue(queue::AMQPMethod::DeclareOk(queue::DeclareOk { queue: ss(&name), message_count: counts["active"].as_u64().unwrap_or(0) as u32, consumer_count: consumers as u32 }))));
                        }
                    }
                    Err((code, text)) => close_and_forget(conn, ch, code, &text, 50, 10).await,
                }
                true
            }
            AMQPClass::Queue(queue::AMQPMethod::Bind(b)) => {
                let (q, ex, key) = (b.queue.as_str().to_string(), b.exchange.as_str().to_string(), b.routing_key.as_str().to_string());
                let r = config::change(ctx, &nsn, |n| bind(n, &ex, &q, false, &key, &binding_args(&b.arguments)));
                match r {
                    Ok(_) => {
                        if !b.nowait {
                            send(tx, AMQPFrame::Method(ch, AMQPClass::Queue(queue::AMQPMethod::BindOk(queue::BindOk {}))));
                        }
                    }
                    Err(e) => close_and_forget(conn, ch, e.status, &e.message, 50, 20).await,
                }
                true
            }
            AMQPClass::Queue(queue::AMQPMethod::Unbind(u)) => {
                let (q, ex, key) = (u.queue.as_str().to_string(), u.exchange.as_str().to_string(), u.routing_key.as_str().to_string());
                let _ = config::change(ctx, &nsn, |n| {
                    unbind(n, &ex, &q, false, &key);
                    Ok(())
                });
                send(tx, AMQPFrame::Method(ch, AMQPClass::Queue(queue::AMQPMethod::UnbindOk(queue::UnbindOk {}))));
                true
            }
            AMQPClass::Queue(queue::AMQPMethod::Purge(p)) => {
                let n = broker::purge(ctx, &nsn, p.queue.as_str(), Some("active".into())).await.unwrap_or(0);
                if !p.nowait {
                    send(tx, AMQPFrame::Method(ch, AMQPClass::Queue(queue::AMQPMethod::PurgeOk(queue::PurgeOk { message_count: n as u32 }))));
                }
                true
            }
            AMQPClass::Queue(queue::AMQPMethod::Delete(d)) => {
                let q = d.queue.as_str().to_string();
                if let Err(text) = can_delete_queue(ctx, &conn.ns.namespace, &q, d.if_unused, d.if_empty).await {
                    close_and_forget(conn, ch, 406, &format!("PRECONDITION_FAILED - {text}"), 50, 40).await;
                    return true;
                }
                let n = broker::purge(ctx, &nsn, &q, None).await.unwrap_or(0);
                let _ = config::change(ctx, &nsn, |ns| {
                    drop_queue(ns, &q);
                    Ok(())
                });
                if !d.nowait {
                    send(tx, AMQPFrame::Method(ch, AMQPClass::Queue(queue::AMQPMethod::DeleteOk(queue::DeleteOk { message_count: n as u32 }))));
                }
                true
            }
            AMQPClass::Queue(_) => true,
            AMQPClass::Basic(b) => basic(conn, ch, b).await,
            AMQPClass::Tx(_) => {
                close_and_forget(conn, ch, 540, "NOT_IMPLEMENTED - Stuntman does not do transactions; use publisher confirms", 90, 10).await;
                true
            }
            AMQPClass::Access(_) => true,
        },
    }
}

/// A channel error: RabbitMQ closes the channel with the code; the client must open another.
async fn close_and_forget(conn: &Arc<Conn>, ch: u16, code: u16, text: &str, class: u16, method: u16) {
    let c = conn.chans.lock().unwrap().remove(&ch);
    if let Some(c) = c {
        close_channel_state(conn, &c, text).await;
    }
    close_chan(&conn.tx, ch, code, text, class, method);
}

pub fn declare_exchange(ctx: &Ctx, nsn: &str, creator: &str, name: &str, kind: &str, passive: bool) -> Result<(), (u16, String)> {
    if name.is_empty() || name.starts_with("amq.") {
        return Ok(());
    }
    config::change(ctx, nsn, |n| {
        match n.entities.get(name).and_then(|e| e.topic.as_ref()) {
            Some(t) if !t.kind.is_empty() && !kind.is_empty() && t.kind != kind => Err(stuntman_core::ApiError::new(406, format!("PRECONDITION_FAILED - inequivalent arg 'type' for exchange '{name}' in vhost '{nsn}': received '{kind}' but current is '{}'", t.kind))),
            Some(_) => Ok(()),
            None if passive => Err(stuntman_core::ApiError::not_found(format!("NOT_FOUND - no exchange '{name}' in vhost '{nsn}'"))),
            None if !n.behaviour.auto_create => Err(stuntman_core::ApiError::not_found(format!("NOT_FOUND - no exchange '{name}' in vhost '{nsn}' (Stuntman: “Create queues the app asks for” is off)"))),
            None => {
                // A queue of the same name keeps its own entry: RabbitMQ keeps them apart.
                n.entities.entry(name.into()).or_default().topic = Some(config::Topic { kind: if kind.is_empty() { "direct".into() } else { kind.into() }, created_by: creator.into(), ..Default::default() });
                Ok(())
            }
        }
    })
    .map(|_| ())
    .map_err(|e| (e.status, e.message))
}

/// The queue type a declare gets: its x-queue-type, else the vhost's default for durable,
/// non-exclusive queues (RabbitMQ 4), else classic.
fn queue_type(n: &config::Namespace, args: &Map<String, Value>, durable_shared: bool) -> String {
    match args.get("x-queue-type").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ if durable_shared && !n.default_queue_type.is_empty() => n.default_queue_type.clone(),
        _ => "classic".into(),
    }
}

pub fn declare_queue(ctx: &Ctx, nsn: &str, creator: &str, name: &str, passive: bool, durable_shared: bool, mut args: Map<String, Value>) -> Result<(), (u16, String)> {
    config::change(ctx, nsn, |n| {
        if let Some(q) = n.entities.get(name).and_then(|e| e.queue.as_ref()) {
            let have = q.arguments.get("x-queue-type").and_then(|v| v.as_str()).unwrap_or("classic");
            return match args.get("x-queue-type").and_then(|v| v.as_str()) {
                Some(want) if !passive && !want.is_empty() && want != have => Err(stuntman_core::ApiError::new(406, format!("PRECONDITION_FAILED - inequivalent arg 'x-queue-type' for queue '{name}' in vhost '{nsn}': received '{want}' but current is '{have}'"))),
                _ => Ok(()),
            };
        }
        if passive {
            return Err(stuntman_core::ApiError::not_found(format!("NOT_FOUND - no queue '{name}' in vhost '{nsn}'")));
        }
        if !n.behaviour.auto_create {
            return Err(stuntman_core::ApiError::not_found(format!("NOT_FOUND - no queue '{name}' in vhost '{nsn}' (Stuntman: “Create queues the app asks for” is off)")));
        }
        let kind = queue_type(n, &args, durable_shared);
        args.insert("x-queue-type".into(), json!(kind));
        let dlx = args.remove("x-dead-letter-exchange").and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
        n.entities.entry(name.into()).or_default().queue = Some(config::Queue { lock: "30s".into(), max_delivery: 10, dead_letter_exchange: dlx, created_by: creator.into(), arguments: args.into_iter().collect(), ..Default::default() });
        Ok(())
    })
    .map(|_| ())
    .map_err(|e| (e.status, e.message))
}

// ---------------------------------------------------------------- topology, shared with the management API

/// Headers-exchange binding arguments as "k=v, k2=v2"; other bindings carry none Stuntman reads.
fn binding_args(t: &FieldTable) -> String {
    t.inner().iter().map(|(k, v)| format!("{}={}", k.as_str(), to_json(v).as_str().map(String::from).unwrap_or_else(|| to_json(v).to_string()))).collect::<Vec<_>>().join(", ")
}

/// Binds a queue (or an exchange) to an exchange with one more key. Both must exist.
pub fn bind(n: &mut config::Namespace, source: &str, dest: &str, to_exchange: bool, key: &str, headers: &str) -> Result<(), stuntman_core::ApiError> {
    let vh = n.namespace.clone();
    if source.is_empty() {
        return Err(stuntman_core::ApiError::new(403, "ACCESS_REFUSED - operation not permitted on the default exchange"));
    }
    let dest_ok = n.entities.get(dest).map(|e| if to_exchange { e.topic.is_some() } else { e.queue.is_some() }).unwrap_or(false);
    if !dest_ok {
        return Err(stuntman_core::ApiError::not_found(format!("NOT_FOUND - no {} '{dest}' in vhost '{vh}'", if to_exchange { "exchange" } else { "queue" })));
    }
    let Some(t) = n.entities.get_mut(source).and_then(|e| e.topic.as_mut()) else {
        return Err(stuntman_core::ApiError::not_found(format!("NOT_FOUND - no exchange '{source}' in vhost '{vh}'")));
    };
    let key = if t.kind == "headers" { headers } else { key };
    let map = if to_exchange { &mut t.exchanges } else { &mut t.subscriptions };
    match map.get_mut(dest) {
        Some(s) => {
            s.add_key(key);
        }
        None => {
            map.insert(dest.into(), config::Subscription { lock: "30s".into(), max_delivery: 10, filter: key.into(), ..Default::default() });
        }
    }
    Ok(())
}

/// Removes one binding (the key between source and destination); unknown ones are ignored.
pub fn unbind(n: &mut config::Namespace, source: &str, dest: &str, to_exchange: bool, key: &str) {
    if let Some(t) = n.entities.get_mut(source).and_then(|e| e.topic.as_mut()) {
        let key = if t.kind == "headers" { t.subscriptions.get(dest).map(|s| s.filter.clone()).unwrap_or_default() } else { key.to_string() };
        let map = if to_exchange { &mut t.exchanges } else { &mut t.subscriptions };
        if map.get_mut(dest).map(|s| s.remove_key(&key)).unwrap_or(false) {
            map.remove(dest);
        }
    }
}

/// An exchange is in use while it is the source of a binding.
pub fn exchange_in_use(n: &config::Namespace, x: &str) -> bool {
    n.entities.get(x).and_then(|e| e.topic.as_ref()).map(|t| !t.subscriptions.is_empty() || !t.exchanges.is_empty()).unwrap_or(false)
}

/// Removes a queue and the bindings to it; an exchange of the same name stays.
pub fn drop_queue(n: &mut config::Namespace, q: &str) {
    if let Some(e) = n.entities.get_mut(q) {
        e.queue = None;
        if e.topic.is_none() {
            n.entities.remove(q);
        }
    }
    for e in n.entities.values_mut() {
        if let Some(t) = e.topic.as_mut() {
            t.subscriptions.remove(q);
        }
    }
}

/// Removes an exchange and the exchange bindings to it; a queue of the same name stays.
pub fn drop_exchange(n: &mut config::Namespace, x: &str) {
    if let Some(e) = n.entities.get_mut(x) {
        e.topic = None;
        if e.queue.is_none() {
            n.entities.remove(x);
        }
    }
    for e in n.entities.values_mut() {
        if let Some(t) = e.topic.as_mut() {
            t.exchanges.remove(x);
        }
    }
}

/// queue.delete's if-unused and if-empty (and the management API's): why not, or Ok.
pub async fn can_delete_queue(ctx: &Ctx, ns: &str, q: &str, if_unused: bool, if_empty: bool) -> Result<(), String> {
    if if_empty && broker::counts(ctx, ns, q).await["active"].as_i64().unwrap_or(0) > 0 {
        return Err(format!("queue '{q}' in vhost '{ns}' not empty"));
    }
    if if_unused && active_consumers(ctx, ns, q).await > 0 {
        return Err(format!("queue '{q}' in vhost '{ns}' in use"));
    }
    Ok(())
}

/// Consumers attached now (not the ones that left today).
pub async fn active_consumers(ctx: &Ctx, ns: &str, q: &str) -> usize {
    broker::consumers(ctx, ns, q).await.map(|v| v.iter().filter(|c| c["state"] == "receiving" && c["entity"] == json!(q)).count()).unwrap_or(0)
}

async fn finish_publish(conn: &Arc<Conn>, ch: u16) {
    let c = chan(conn, ch);
    let (p, h, body, confirm, tag) = {
        let mut g = c.lock().unwrap();
        let Some((p, h, body)) = g.pending.take() else { return };
        let confirm = g.confirm;
        if confirm {
            g.pub_seq += 1;
        }
        (p, h, body, confirm, g.pub_seq)
    };
    let props = h.map(|h| h.properties).unwrap_or_default();
    let (ex, key) = (p.exchange.as_str().to_string(), p.routing_key.as_str().to_string());
    let o = outgoing(&props, &ex, &key, body, &conn.client);
    let entity = if ex.is_empty() { key.clone() } else { ex.clone() };
    let ns = config::get(&conn.ctx, &conn.ns.namespace).unwrap_or(conn.ns.clone());
    // The default exchange routes by queue name; publishing there to a missing queue drops it.
    if ex.is_empty() && ns.entities.get(&key).and_then(|e| e.queue.as_ref()).is_none() {
        if p.mandatory {
            send_return(conn, ch, 312, "NO_ROUTE", &ex, &key, &props, &o.body);
        }
        if confirm {
            ack_pub(conn, ch, tag);
        }
        return;
    }
    if !ex.is_empty() && !ex.starts_with("amq.") && ns.entities.get(&ex).and_then(|e| e.topic.as_ref()).is_none() {
        close_and_forget(conn, ch, 404, &format!("NOT_FOUND - no exchange '{ex}' in vhost '{}'", ns.namespace), 60, 40).await;
        return;
    }
    let body = o.body.clone();
    match broker::publish(&conn.ctx, &ns.namespace, &entity, o).await {
        Ok(seqs) => {
            if seqs.is_empty() && p.mandatory {
                send_return(conn, ch, 312, "NO_ROUTE", &ex, &key, &props, &body);
            }
            if confirm {
                ack_pub(conn, ch, tag);
            }
        }
        Err(broker::Refusal::NotFound(t)) => close_and_forget(conn, ch, 404, &format!("NOT_FOUND - {t}"), 60, 40).await,
        Err(broker::Refusal::Unauthorized(t)) => close_and_forget(conn, ch, 403, &format!("ACCESS_REFUSED - {t}"), 60, 40).await,
        Err(broker::Refusal::Quota(_)) if !confirm => {}
        Err(_) if confirm => {
            send(&conn.tx, AMQPFrame::Method(ch, AMQPClass::Basic(basic::AMQPMethod::Nack(basic::Nack { delivery_tag: tag, multiple: false, requeue: false }))));
        }
        Err(e) => close_and_forget(conn, ch, 506, &format!("RESOURCE_ERROR - {}", e.text()), 60, 40).await,
    }
}

fn ack_pub(conn: &Conn, ch: u16, tag: u64) {
    send(&conn.tx, AMQPFrame::Method(ch, AMQPClass::Basic(basic::AMQPMethod::Ack(basic::Ack { delivery_tag: tag, multiple: false }))));
}

fn send_return(conn: &Conn, ch: u16, code: u16, text: &str, ex: &str, key: &str, props: &basic::AMQPProperties, body: &[u8]) {
    send(&conn.tx, AMQPFrame::Method(ch, AMQPClass::Basic(basic::AMQPMethod::Return(basic::Return { reply_code: code, reply_text: ss(text), exchange: ss(ex), routing_key: ss(key) }))));
    send(&conn.tx, AMQPFrame::Header(ch, AMQPContentHeader { class_id: 60, body_size: body.len() as u64, properties: props.clone() }));
    for part in body.chunks(131_000) {
        send(&conn.tx, AMQPFrame::Body(ch, part.to_vec()));
    }
}

fn deliver_frames(conn: &Conn, ch: u16, m: AMQPClass, c: &broker::Claimed) {
    send(&conn.tx, AMQPFrame::Method(ch, m));
    send(&conn.tx, AMQPFrame::Header(ch, AMQPContentHeader { class_id: 60, body_size: c.body.len() as u64, properties: properties(c) }));
    for part in c.body.chunks(131_000) {
        send(&conn.tx, AMQPFrame::Body(ch, part.to_vec()));
    }
}

async fn basic(conn: &Arc<Conn>, ch: u16, b: basic::AMQPMethod) -> bool {
    let tx = conn.tx.clone();
    let ctx = conn.ctx.clone();
    let nsn = conn.ns.namespace.clone();
    match b {
        basic::AMQPMethod::Qos(q) => {
            chan(conn, ch).lock().unwrap().prefetch = q.prefetch_count;
            send(&tx, AMQPFrame::Method(ch, AMQPClass::Basic(basic::AMQPMethod::QosOk(basic::QosOk {}))));
        }
        basic::AMQPMethod::Publish(p) => {
            chan(conn, ch).lock().unwrap().pending = Some((p, None, Vec::new()));
        }
        basic::AMQPMethod::Consume(c) => {
            let q = c.queue.as_str().to_string();
            let exists = config::get(&ctx, &nsn).map(|n| n.entities.get(&q).and_then(|e| e.queue.as_ref()).is_some()).unwrap_or(false);
            if !exists {
                close_and_forget(conn, ch, 404, &format!("NOT_FOUND - no queue '{q}' in vhost '{nsn}'"), 60, 20).await;
                return true;
            }
            let tag = if c.consumer_tag.as_str().is_empty() { format!("amq.ctag-{}", &broker::uuid4()[..18]) } else { c.consumer_tag.as_str().to_string() };
            let id = format!("rmq-{}", broker::uuid4());
            let stop = Arc::new(Notify::new());
            let channel = chan(conn, ch);
            let prefetch = channel.lock().unwrap().prefetch;
            channel.lock().unwrap().consumers.insert(tag.clone(), (id.clone(), stop.clone()));
            broker::consumer_open(&ctx, &id, &nsn, &q, &conn.client, &format!("{q}/{tag}"), if c.no_ack { "auto-ack" } else { "manual ack" }, prefetch as i64, &conn.remote, "amqp091").await;
            broker::kills().lock().unwrap().insert(id.clone(), conn.kill.clone());
            if !c.nowait {
                send(&tx, AMQPFrame::Method(ch, AMQPClass::Basic(basic::AMQPMethod::ConsumeOk(basic::ConsumeOk { consumer_tag: ss(&tag) }))));
            }
            tokio::spawn(consume(conn.clone(), ch, tag, id, q, c.no_ack, stop));
        }
        basic::AMQPMethod::Cancel(c) => {
            let tag = c.consumer_tag.as_str().to_string();
            let e = chan(conn, ch).lock().unwrap().consumers.remove(&tag);
            if let Some((id, stop)) = e {
                stop.notify_one();
                stop.notify_waiters();
                broker::consumer_close(&ctx, &id, "cancelled by the client").await;
            }
            if !c.nowait {
                send(&tx, AMQPFrame::Method(ch, AMQPClass::Basic(basic::AMQPMethod::CancelOk(basic::CancelOk { consumer_tag: ss(&tag) }))));
            }
        }
        basic::AMQPMethod::Get(g) => {
            let q = g.queue.as_str().to_string();
            let id = format!("rmq-get-{}-{ch}", conn.client);
            match broker::claim(&ctx, &nsn, &q, &id, &conn.client, None).await {
                Some(c) => {
                    let tag = {
                        let ch2 = chan(conn, ch);
                        let mut gd = ch2.lock().unwrap();
                        gd.next_tag += 1;
                        if !g.no_ack {
                            let nt = gd.next_tag;
                            gd.unacked.insert(nt, Unacked { token: c.lock_token.clone(), queue: c.holder.clone(), lose_lock: c.lose_lock });
                        }
                        gd.next_tag
                    };
                    let left = broker::counts(&ctx, &nsn, &q).await["active"].as_u64().unwrap_or(0) as u32;
                    deliver_frames(conn, ch, AMQPClass::Basic(basic::AMQPMethod::GetOk(basic::GetOk { delivery_tag: tag, redelivered: c.delivery_count > 1, exchange: ss(c.sys.get("exchange").and_then(|v| v.as_str()).unwrap_or("")), routing_key: ss(c.sys.get("routingKey").and_then(|v| v.as_str()).unwrap_or(&q)), message_count: left })), &c);
                    if g.no_ack {
                        let _ = broker::settle(&ctx, &nsn, &c.lock_token, "complete", "", "", &conn.client).await;
                    }
                }
                None => send(&tx, AMQPFrame::Method(ch, AMQPClass::Basic(basic::AMQPMethod::GetEmpty(basic::GetEmpty {})))),
            }
        }
        basic::AMQPMethod::Ack(a) => settle_tags(conn, ch, a.delivery_tag, a.multiple, Settle::Ack).await,
        basic::AMQPMethod::Nack(n) => settle_tags(conn, ch, n.delivery_tag, n.multiple, if n.requeue { Settle::Requeue } else { Settle::Reject }).await,
        basic::AMQPMethod::Reject(r) => settle_tags(conn, ch, r.delivery_tag, false, if r.requeue { Settle::Requeue } else { Settle::Reject }).await,
        basic::AMQPMethod::Recover(_) | basic::AMQPMethod::RecoverAsync(_) => {
            settle_tags(conn, ch, 0, true, Settle::Requeue).await;
            send(&tx, AMQPFrame::Method(ch, AMQPClass::Basic(basic::AMQPMethod::RecoverOk(basic::RecoverOk {}))));
        }
        _ => {}
    }
    true
}

#[derive(Clone, Copy, PartialEq)]
enum Settle {
    Ack,
    Requeue,
    Reject,
}

async fn settle_tags(conn: &Arc<Conn>, ch: u16, tag: u64, multiple: bool, how: Settle) {
    let c = chan(conn, ch);
    let picked: Vec<(u64, Unacked)> = {
        let mut g = c.lock().unwrap();
        let tags: Vec<u64> = if multiple { g.unacked.keys().copied().filter(|t| tag == 0 || *t <= tag).collect() } else { vec![tag] };
        tags.into_iter().filter_map(|t| g.unacked.remove(&t).map(|u| (t, u))).collect()
    };
    if picked.is_empty() && !multiple {
        close_and_forget(conn, ch, 406, &format!("PRECONDITION_FAILED - unknown delivery tag {tag}"), 60, 80).await;
        return;
    }
    let nsn = conn.ns.namespace.clone();
    for (_, u) in picked {
        let r = match how {
            // "Lose the lock": the ack arrives too late; RabbitMQ would have redelivered it.
            Settle::Ack if u.lose_lock => broker::settle(&conn.ctx, &nsn, &u.token, "release", "the ack came after the lock was lost (provoked): redelivered", "", &conn.client).await,
            Settle::Ack => broker::settle(&conn.ctx, &nsn, &u.token, "complete", "", "", &conn.client).await,
            Settle::Requeue => broker::settle(&conn.ctx, &nsn, &u.token, "release", "nack, requeue=true", "", &conn.client).await,
            Settle::Reject => {
                let dlx = config::get(&conn.ctx, &nsn).ok().and_then(|n| n.rabbit_queue(&u.queue)).and_then(|q| q.dead_letter_exchange);
                let desc = match &dlx { Some(x) => format!("sent to its dead-letter exchange '{x}'"), None => "a real RabbitMQ discards it: this queue has no dead-letter exchange".to_string() };
                let r = broker::settle(&conn.ctx, &nsn, &u.token, "dead_letter", "Rejected (nack, requeue=false)", &desc, &conn.client).await;
                if let Ok(seq) = r {
                    broker::rabbit_forward(&conn.ctx, &nsn, seq, "rejected").await;
                }
                r
            }
        };
        let _ = r;
    }
    conn.acked.notify_waiters();
}

/// One basic.consume: claims messages while the channel's prefetch allows and sends them.
async fn consume(conn: Arc<Conn>, ch: u16, tag: String, id: String, q: String, no_ack: bool, stop: Arc<Notify>) {
    let nsn = conn.ns.namespace.clone();
    let bell = broker::bell(&conn.ctx, &nsn, &q);
    loop {
        if conn.ctx.state.gone() || conn.tx.is_closed() {
            break;
        }
        let c = chan(&conn, ch);
        if !c.lock().unwrap().consumers.contains_key(&tag) {
            break;
        }
        let (prefetch, inflight) = {
            let g = c.lock().unwrap();
            (g.prefetch, g.unacked.len())
        };
        if !no_ack && prefetch > 0 && inflight >= prefetch as usize {
            tokio::select! {
                _ = conn.acked.notified() => {}
                _ = stop.notified() => break,
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            }
            continue;
        }
        match broker::claim(&conn.ctx, &nsn, &q, &id, &conn.client, None).await {
            Some(m) => {
                let dtag = {
                    let mut g = c.lock().unwrap();
                    g.next_tag += 1;
                    if !no_ack {
                        let nt = g.next_tag;
                        g.unacked.insert(nt, Unacked { token: m.lock_token.clone(), queue: m.holder.clone(), lose_lock: m.lose_lock });
                    }
                    g.next_tag
                };
                deliver_frames(&conn, ch, AMQPClass::Basic(basic::AMQPMethod::Deliver(basic::Deliver { consumer_tag: ss(&tag), delivery_tag: dtag, redelivered: m.delivery_count > 1,
                    exchange: ss(m.sys.get("exchange").and_then(|v| v.as_str()).unwrap_or("")), routing_key: ss(m.sys.get("routingKey").and_then(|v| v.as_str()).unwrap_or(&q)) })), &m);
                if no_ack {
                    let _ = broker::settle(&conn.ctx, &nsn, &m.lock_token, "complete", "", "", &conn.client).await;
                }
            }
            None => {
                tokio::select! {
                    _ = bell.notified() => {}
                    _ = stop.notified() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(1000)) => {}
                }
            }
        }
    }
    broker::kills().lock().unwrap().remove(&id);
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn frames_round_trip_and_values() {
        let f = AMQPFrame::Method(1, AMQPClass::Basic(basic::AMQPMethod::Ack(basic::Ack { delivery_tag: 7, multiple: false })));
        let b = enc(&f);
        let (_, g) = parse_frame(&b[..]).unwrap();
        assert_eq!(g, f);
        let v = json!({"tenant": "globex", "n": 3, "ok": true, "list": ["a"]});
        let t = table(v.as_object().unwrap());
        assert_eq!(Value::Object(table_json(&t)), v);
        assert_eq!(credentials("PLAIN", b"\0guest\0secret"), ("guest".to_string(), "secret".to_string()));
    }
}
