//! Azure Service Bus over AMQP 1.0: what Azure.Messaging.ServiceBus (.NET), azure-servicebus
//! (Python) and the other SDKs speak in emulator mode (UseDevelopmentEmulator=true, no TLS).
//! Frames, sessions and links are handled here; claims, locks and settlement go to the broker.
//! The delivery tag is the lock token's GUID (.NET byte order): that is how the SDKs settle.
//! Request/response nodes: $cbs (put-token) and <entity>/$management (renew-lock, dispositions,
//! schedule, cancel, peek).

use std::{
    collections::HashMap,
    io::Cursor,
    sync::{Arc, Mutex},
};

use fe2o3_amqp_types::{
    definitions::{AmqpError, Error as AErr, ErrorCondition, Handle, LinkError, ReceiverSettleMode, Role, SenderSettleMode},
    messaging::{
        message::__private::{Deserializable, Serializable},
        AmqpValue, ApplicationProperties, Batch, Body, Data, DeliveryState, Header, Message, MessageAnnotations, MessageId, Modified, Properties, Rejected, Accepted,
        TargetArchetype,
        annotations::OwnedKey,
    },
    performatives::{Attach, Begin, Close, Detach, Disposition, End, Flow, Open, Performative, Transfer},
    primitives::{Array, Binary, OrderedMap, SimpleValue, Symbol, Timestamp},
    sasl::{SaslCode, SaslInit, SaslMechanisms, SaslOutcome},
};
use serde_amqp::Value;
use serde_json::{json, Map};
use stuntman_core::{env::Envs, util, Ctx};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, Notify},
};

use crate::{broker, config};

type Tx = mpsc::UnboundedSender<Vec<u8>>;

// ---------------------------------------------------------------- frames

fn frame(kind: u8, ch: u16, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + body.len());
    v.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
    v.push(2);
    v.push(kind);
    v.extend_from_slice(&ch.to_be_bytes());
    v.extend_from_slice(body);
    v
}

fn perf(ch: u16, p: &impl serde::Serialize) -> Vec<u8> {
    frame(0, ch, &full(serde_amqp::to_vec(p).unwrap_or_default()))
}

/// Writes every field of a performative, trailing nulls included. Leaving them out is allowed,
/// but some clients (Python's pyamqp) read fields by position.
fn full(b: Vec<u8>) -> Vec<u8> {
    let Ok(Value::Described(mut d)) = serde_amqp::from_slice::<Value>(&b) else { return b };
    let n = match d.descriptor {
        serde_amqp::descriptor::Descriptor::Code(c) => match c {
            0x10 => 10, 0x11 => 8, 0x12 => 14, 0x13 => 11, 0x14 => 11, 0x15 => 6, 0x16 => 3, 0x17 => 1, 0x18 => 1,
            0x40 => 1, 0x44 => 2,
            _ => return b,
        },
        _ => return b,
    };
    let code = match d.descriptor { serde_amqp::descriptor::Descriptor::Code(c) => c, _ => 0 };
    if let Value::List(items) = &mut d.value {
        if items.len() < n {
            items.resize(n, Value::Null);
        }
        // Fields left out because they hold the default value: written out, as some clients read them.
        let defaults: &[(usize, Value)] = match code {
            0x10 => &[(2, Value::Uint(u32::MAX)), (3, Value::Ushort(u16::MAX))],
            0x11 => &[(4, Value::Uint(u32::MAX))],
            0x12 => &[(3, Value::Ubyte(2)), (4, Value::Ubyte(0)), (8, Value::Bool(false))],
            0x13 => &[(8, Value::Bool(false)), (9, Value::Bool(false))],
            0x14 => &[(5, Value::Bool(false)), (8, Value::Bool(false)), (9, Value::Bool(false)), (10, Value::Bool(false))],
            0x15 => &[(3, Value::Bool(false)), (5, Value::Bool(false))],
            0x16 => &[(1, Value::Bool(false))],
            _ => &[],
        };
        for (i, v) in defaults {
            if matches!(items.get(*i), Some(Value::Null)) {
                items[*i] = v.clone();
            }
        }
        return serde_amqp::to_vec(&Value::Described(d)).unwrap_or(b);
    }
    b
}

async fn read_frame<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> Option<(u8, u16, Vec<u8>)> {
    let mut sz = [0u8; 4];
    r.read_exact(&mut sz).await.ok()?;
    let size = u32::from_be_bytes(sz) as usize;
    if !(8..=64 * 1024 * 1024).contains(&size) {
        return None;
    }
    let mut rest = vec![0u8; size - 4];
    r.read_exact(&mut rest).await.ok()?;
    let doff = rest[0] as usize * 4;
    let kind = rest[1];
    let ch = u16::from_be_bytes([rest[2], rest[3]]);
    Some((kind, ch, rest[doff.saturating_sub(4).min(rest.len())..].to_vec()))
}

/// The performative, and what follows it (a transfer's payload).
fn split(body: &[u8]) -> Option<(Performative, Vec<u8>)> {
    let mut c = Cursor::new(body);
    let p: Performative = serde_amqp::from_reader(&mut c).ok()?;
    let at = c.position() as usize;
    Some((p, body[at..].to_vec()))
}

fn err(cond: &str, text: &str) -> AErr {
    let condition = match cond {
        "amqp:not-found" => ErrorCondition::AmqpError(AmqpError::NotFound),
        "amqp:unauthorized-access" => ErrorCondition::AmqpError(AmqpError::UnauthorizedAccess),
        "amqp:resource-limit-exceeded" => ErrorCondition::AmqpError(AmqpError::ResourceLimitExceeded),
        "amqp:internal-error" => ErrorCondition::AmqpError(AmqpError::InternalError),
        "amqp:not-implemented" => ErrorCondition::AmqpError(AmqpError::NotImplemented),
        "amqp:link:detach-forced" => ErrorCondition::LinkError(LinkError::DetachForced),
        c => ErrorCondition::Custom(Symbol::from(c)),
    };
    AErr { condition, description: Some(text.to_string()), info: None }
}

// ---------------------------------------------------------------- lock tokens

fn parse_uuid(s: &str) -> Option<[u8; 16]> {
    let h: String = s.chars().filter(|c| *c != '-').collect();
    if h.len() != 32 {
        return None;
    }
    let mut b = [0u8; 16];
    for i in 0..16 {
        b[i] = u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(b)
}

fn uuid_string(b: &[u8; 16]) -> String {
    format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15])
}

/// .NET's Guid.ToByteArray(): the first three groups little-endian. The SDKs read the lock token
/// from the delivery tag this way.
fn guid_bytes_le(token: &str) -> Vec<u8> {
    let Some(b) = parse_uuid(token) else { return token.as_bytes().to_vec() };
    vec![b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]
}

// ---------------------------------------------------------------- values

fn sv_json(v: &SimpleValue) -> serde_json::Value {
    match v {
        SimpleValue::Null => serde_json::Value::Null,
        SimpleValue::Bool(b) => json!(b),
        SimpleValue::Ubyte(n) => json!(n),
        SimpleValue::Ushort(n) => json!(n),
        SimpleValue::Uint(n) => json!(n),
        SimpleValue::Ulong(n) => json!(n),
        SimpleValue::Byte(n) => json!(n),
        SimpleValue::Short(n) => json!(n),
        SimpleValue::Int(n) => json!(n),
        SimpleValue::Long(n) => json!(n),
        SimpleValue::Float(f) => json!(f.into_inner()),
        SimpleValue::Double(f) => json!(f.into_inner()),
        SimpleValue::Char(c) => json!(c.to_string()),
        SimpleValue::Timestamp(t) => json!(t.milliseconds()),
        SimpleValue::Uuid(u) => json!(uuid_string(u.as_inner())),
        SimpleValue::Binary(b) => json!(String::from_utf8_lossy(b)),
        SimpleValue::String(s) => json!(s),
        SimpleValue::Symbol(s) => json!(s.as_str()),
        other => json!(format!("{other:?}")),
    }
}

fn json_sv(v: &serde_json::Value) -> SimpleValue {
    match v {
        serde_json::Value::Bool(b) => SimpleValue::Bool(*b),
        serde_json::Value::Number(n) if n.is_i64() => SimpleValue::Long(n.as_i64().unwrap()),
        serde_json::Value::Number(n) => SimpleValue::Double(n.as_f64().unwrap_or(0.0).into()),
        serde_json::Value::String(s) => SimpleValue::String(s.clone()),
        serde_json::Value::Null => SimpleValue::Null,
        other => SimpleValue::String(other.to_string()),
    }
}

fn mid(m: &MessageId) -> String {
    match m {
        MessageId::String(s) => s.clone(),
        MessageId::Ulong(n) => n.to_string(),
        MessageId::Uuid(u) => uuid_string(u.as_inner()),
        MessageId::Binary(b) => String::from_utf8_lossy(b).into(),
    }
}

fn body_bytes(b: &Body<Value>) -> Vec<u8> {
    match b {
        Body::Data(batch) => batch.iter().flat_map(|d| d.0.to_vec()).collect(),
        Body::Value(AmqpValue(Value::String(s))) => s.clone().into_bytes(),
        Body::Value(AmqpValue(Value::Binary(b))) => b.to_vec(),
        Body::Value(AmqpValue(v)) => format!("{v:?}").into_bytes(),
        Body::Sequence(_) => b"(amqp-sequence body)".to_vec(),
        _ => vec![],
    }
}

fn ann(m: &Message<Body<Value>>, k: &str) -> Option<Value> {
    m.message_annotations.as_ref()?.0.iter().find(|(key, _)| matches!(key, OwnedKey::Symbol(s) if s.as_str() == k)).map(|(_, v)| v.clone())
}

/// An AMQP message from the app → the broker's words.
fn outgoing(m: &Message<Body<Value>>, sender: &str) -> broker::Outgoing {
    let mut sys = Map::new();
    let p = m.properties.clone().unwrap_or_default();
    if let Some(c) = &p.correlation_id { sys.insert("correlationId".into(), json!(mid(c))); }
    if let Some(r) = &p.reply_to { sys.insert("replyTo".into(), json!(r)); }
    if let Some(t) = &p.to { sys.insert("to".into(), json!(t)); }
    if let Some(g) = &p.group_id { sys.insert("sessionId".into(), json!(g)); }
    if let Some(g) = &p.reply_to_group_id { sys.insert("replyToSessionId".into(), json!(g)); }
    if let Some(h) = &m.header {
        if let Some(t) = h.ttl { sys.insert("timeToLive".into(), json!(t as i64)); }
    }
    if let Some(Value::String(pk)) = ann(m, "x-opt-partition-key") { sys.insert("partitionKey".into(), json!(pk)); }
    let scheduled = match ann(m, "x-opt-scheduled-enqueue-time") {
        Some(Value::Timestamp(t)) => Some(t.milliseconds()),
        _ => None,
    };
    let props: Map<String, serde_json::Value> = m.application_properties.as_ref().map(|a| a.0.iter().map(|(k, v)| (k.clone(), sv_json(v))).collect()).unwrap_or_default();
    broker::Outgoing {
        message_id: p.message_id.as_ref().map(mid).unwrap_or_default(),
        subject: p.subject.clone().unwrap_or_default(),
        content_type: p.content_type.as_ref().map(|s| s.as_str().to_string()).unwrap_or_default(),
        body: body_bytes(&m.body),
        props,
        sys,
        scheduled_at: scheduled,
        sender: sender.into(),
        via: "amqp".into(),
    }
}

/// A claimed message → what the SDK expects: header, x-opt annotations, properties, body.
fn incoming(c: &broker::Claimed) -> Message<Body<Value>> {
    let mut a: OrderedMap<OwnedKey, Value> = OrderedMap::new();
    a.insert(OwnedKey::Symbol(Symbol::from("x-opt-sequence-number")), Value::Long(c.seq));
    a.insert(OwnedKey::Symbol(Symbol::from("x-opt-enqueue-sequence-number")), Value::Long(c.seq));
    a.insert(OwnedKey::Symbol(Symbol::from("x-opt-enqueued-time")), Value::Timestamp(Timestamp::from_milliseconds(c.enqueued_at)));
    if c.locked_until < i64::MAX / 2 {
        a.insert(OwnedKey::Symbol(Symbol::from("x-opt-locked-until")), Value::Timestamp(Timestamp::from_milliseconds(c.locked_until)));
    }
    if let Some(pk) = c.sys.get("partitionKey").and_then(|v| v.as_str()) {
        a.insert(OwnedKey::Symbol(Symbol::from("x-opt-partition-key")), Value::String(pk.into()));
    }
    let g = |k: &str| c.sys.get(k).and_then(|v| v.as_str()).map(String::from);
    let props = Properties {
        message_id: if c.message_id.is_empty() { None } else { Some(MessageId::String(c.message_id.clone())) },
        subject: if c.subject.is_empty() { None } else { Some(c.subject.clone()) },
        correlation_id: g("correlationId").map(MessageId::String),
        content_type: if c.content_type.is_empty() { None } else { Some(Symbol::from(c.content_type.as_str())) },
        reply_to: g("replyTo"),
        to: g("to"),
        group_id: g("sessionId"),
        reply_to_group_id: g("replyToSessionId"),
        ..Default::default()
    };
    let app: OrderedMap<String, SimpleValue> = c.props.iter().map(|(k, v)| (k.clone(), json_sv(v))).collect();
    let header = Header { durable: true, delivery_count: (c.delivery_count - 1).max(0) as u32, ttl: c.sys.get("timeToLive").and_then(|v| v.as_u64()).map(|t| t as u32), ..Default::default() };
    Message {
        header: Some(header),
        delivery_annotations: None,
        message_annotations: Some(MessageAnnotations(a)),
        properties: Some(props),
        application_properties: if app.iter().next().is_some() { Some(ApplicationProperties(app)) } else { None },
        body: Body::Data(Batch::new(vec![Data(Binary::from(c.body.clone()))])),
        footer: None,
    }
}

fn encode(m: Message<Body<Value>>) -> Vec<u8> {
    serde_amqp::to_vec(&Serializable(m)).unwrap_or_default()
}

fn decode(b: &[u8]) -> Option<Message<Body<Value>>> {
    serde_amqp::from_slice::<Deserializable<Message<Body<Value>>>>(b).ok().map(|d| d.0)
}

// ---------------------------------------------------------------- connection state

#[derive(Clone)]
enum Node {
    /// Messages from the app to an entity.
    Send(String),
    /// Messages to the app from a holder (queue or subscription).
    Receive(String),
    Cbs,
    /// Requests to <entity>/$management.
    Mgmt(String),
    /// Where answers to requests go (the app's reply address).
    Reply(String),
}

struct Link {
    name: String,
    node: Node,
    /// Our role: true = we send (the app receives).
    we_send: bool,
    settled_mode: bool,
    credit: u32,
    delivery_count: u32,
    drain: bool,
    received: u32,
    consumer: String,
    wake: Arc<Notify>,
    /// A session receiver's session id.
    session: Option<String>,
}

#[derive(Default)]
struct Session {
    next_outgoing: u32,
    remote_next_outgoing: u32,
    next_delivery: u32,
}

struct Partial {
    handle: u32,
    delivery_id: u32,
    settled: bool,
    payload: Vec<u8>,
}

struct State {
    ctx: Option<Ctx>,
    ns: Option<config::Namespace>,
    sessions: HashMap<u16, Session>,
    links: HashMap<(u16, u32), Link>,
    /// (channel, delivery id) → (handle, lock token, settled on send)
    out: HashMap<(u16, u32), (u32, String)>,
    partial: HashMap<(u16, u32), Partial>,
    max_frame: usize,
}

struct Conn {
    envs: Envs,
    client: String,
    remote: String,
    tx: Tx,
    st: Mutex<State>,
    authed: Mutex<bool>,
}

impl Conn {
    fn send(&self, b: Vec<u8>) {
        let _ = self.tx.send(b);
    }

    /// The namespace as the file says now: queues and subscriptions made after this connection
    /// opened (by the admin client, the API or another connection) count.
    fn ctx_ns(&self) -> Option<(Ctx, config::Namespace)> {
        let (ctx, ns) = {
            let g = self.st.lock().unwrap();
            (g.ctx.clone()?, g.ns.clone()?)
        };
        let fresh = config::get(&ctx, &ns.namespace).unwrap_or(ns);
        Some((ctx, fresh))
    }

    /// A transfer on a link we send on, split to the peer's frame size.
    fn transfer(&self, ch: u16, handle: u32, tag: Vec<u8>, settled: bool, payload: Vec<u8>) -> u32 {
        let (delivery_id, max) = {
            let mut g = self.st.lock().unwrap();
            let max = g.max_frame;
            let s = g.sessions.entry(ch).or_default();
            let d = s.next_delivery;
            s.next_delivery = s.next_delivery.wrapping_add(1);
            (d, max)
        };
        let room = max.saturating_sub(512).max(4096);
        let chunks: Vec<&[u8]> = if payload.is_empty() { vec![&payload[..]] } else { payload.chunks(room).collect() };
        let n = chunks.len();
        for (i, part) in chunks.into_iter().enumerate() {
            let t = Transfer {
                handle: Handle(handle),
                delivery_id: if i == 0 { Some(delivery_id) } else { None },
                delivery_tag: if i == 0 { Some(Binary::from(tag.clone())) } else { None },
                message_format: if i == 0 { Some(0) } else { None },
                settled: if i == 0 { Some(settled) } else { None },
                more: i + 1 < n,
                rcv_settle_mode: None,
                state: None,
                resume: false,
                aborted: false,
                batchable: false,
            };
            let mut b = full(serde_amqp::to_vec(&t).unwrap_or_default());
            b.extend_from_slice(part);
            self.send(frame(0, ch, &b));
            let mut g = self.st.lock().unwrap();
            let s = g.sessions.entry(ch).or_default();
            s.next_outgoing = s.next_outgoing.wrapping_add(1);
        }
        delivery_id
    }

    fn flow(&self, ch: u16, handle: u32, credit: u32, delivery_count: u32, drain: bool) {
        let (no, rno) = {
            let mut g = self.st.lock().unwrap();
            let s = g.sessions.entry(ch).or_default();
            (s.next_outgoing, s.remote_next_outgoing)
        };
        let f = Flow { next_incoming_id: Some(rno), incoming_window: 5000, next_outgoing_id: no, outgoing_window: 5000, handle: Some(Handle(handle)), delivery_count: Some(delivery_count), link_credit: Some(credit), available: None, drain, echo: false, properties: None };
        self.send(perf(ch, &Performative::Flow(f)));
    }
}

/// "sb://acme-bus-stg.localhost/orders-created" → "orders-created"; case of "Subscriptions" kept as "subscriptions".
fn entity_path(address: &str) -> String {
    let a = address.split_once("://").map(|(_, r)| r.split_once('/').map(|(_, p)| p).unwrap_or("")).unwrap_or(address);
    let a = a.trim_matches('/');
    let mut parts: Vec<String> = a.split('/').map(String::from).collect();
    for p in parts.iter_mut() {
        if p.eq_ignore_ascii_case("subscriptions") {
            *p = "subscriptions".into();
        }
    }
    parts.join("/")
}

fn host_of(s: &str) -> String {
    let rest = s.split_once("://").map(|(_, r)| r).unwrap_or(s);
    rest.split(['/', ':']).next().unwrap_or("").to_lowercase()
}

/// The namespace a host names: "<ns>.<env base>" or "<ns>.localhost". Created on first use, so an
/// app works on day zero.
pub fn resolve_pub(envs: &Envs, host: &str) -> Option<(Ctx, config::Namespace)> {
    resolve(envs, &host_of(host))
}

fn resolve(envs: &Envs, host: &str) -> Option<(Ctx, config::Namespace)> {
    let h = host.trim_end_matches('.').to_lowercase();
    let (first, rest) = h.split_once('.').unwrap_or((h.as_str(), ""));
    let ctx = envs.by_host(Some(rest)).or_else(|| envs.default_env())?;
    if let Ok(n) = config::get(&ctx, first) {
        if n.kind == config::SB {
            return Some((ctx, n));
        }
    }
    if first == "localhost" || first.parse::<u8>().is_ok() || first.is_empty() {
        let n = config::list(&ctx).into_iter().find(|n| n.kind == config::SB)?;
        return Some((ctx, n));
    }
    config::valid_name(first).ok()?;
    config::create(&ctx, config::SB, first).ok().map(|n| (ctx, n))
}

// ---------------------------------------------------------------- serve

pub async fn serve(envs: Envs, sock: crate::amqp::Io, remote: String, header: [u8; 8]) {
    let (mut r, mut w) = tokio::io::split(sock);
    // SASL first (the SDKs ask for it), then the AMQP header again.
    let mut hdr = header;
    if hdr[4] == 3 {
        if w.write_all(b"AMQP\x03\x01\x00\x00").await.is_err() {
            return;
        }
        let mechs = SaslMechanisms { sasl_server_mechanisms: Array::from(vec![Symbol::from("MSSBCBS"), Symbol::from("ANONYMOUS"), Symbol::from("PLAIN"), Symbol::from("EXTERNAL")]) };
        let _ = w.write_all(&frame(1, 0, &full(serde_amqp::to_vec(&mechs).unwrap_or_default()))).await;
        let Some((_, _, b)) = read_frame(&mut r).await else { return };
        let _init: Option<SaslInit> = serde_amqp::from_slice(&b).ok();
        let out = SaslOutcome { code: SaslCode::Ok, additional_data: None };
        let _ = w.write_all(&frame(1, 0, &full(serde_amqp::to_vec(&out).unwrap_or_default()))).await;
        if r.read_exact(&mut hdr).await.is_err() {
            return;
        }
    }
    if w.write_all(b"AMQP\x00\x01\x00\x00").await.is_err() {
        return;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        while let Some(b) = rx.recv().await {
            if w.write_all(&b).await.is_err() {
                break;
            }
        }
        let _ = w.shutdown().await;
    });
    // Open
    let Some((_, _, b)) = read_frame(&mut r).await else { return };
    let Some((Performative::Open(open), _)) = split(&b) else { return };
    let ua = open.properties.as_ref().and_then(|p| p.iter().find(|(k, _)| k.as_str() == "user-agent").map(|(_, v)| v.clone()));
    let product = match ua {
        Some(Value::String(s)) => s.split_whitespace().next().unwrap_or("amqp").split('/').next().unwrap_or("amqp").to_string(),
        _ => "amqp".into(),
    };
    let client = format!("{product} :{}", remote.rsplit(':').next().unwrap_or(""));
    let found = open.hostname.as_deref().and_then(|h| resolve(&envs, h));
    if let Some((_, n)) = &found {
        match n.behaviour.reachability.as_str() {
            "refuse" => return,
            "timeout" => {
                tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                return;
            }
            _ => {}
        }
    }
    let max_frame = (open.max_frame_size.0 as usize).clamp(4096, 1024 * 1024);
    let idle = open.idle_time_out.unwrap_or(0);
    let mine = Open { container_id: "stuntman".into(), hostname: None, max_frame_size: (262_144u32).into(), channel_max: open.channel_max.clone(), idle_time_out: Some(120_000), ..open.clone() };
    let mine = Open { outgoing_locales: None, incoming_locales: None, offered_capabilities: None, desired_capabilities: None, properties: None, ..mine };
    let _ = tx.send(perf(0, &Performative::Open(mine)));
    let conn = Arc::new(Conn {
        envs: envs.clone(),
        client: client.clone(),
        remote: remote.clone(),
        tx: tx.clone(),
        st: Mutex::new(State { ctx: found.as_ref().map(|f| f.0.clone()), ns: found.as_ref().map(|f| f.1.clone()), sessions: HashMap::new(), links: HashMap::new(), out: HashMap::new(), partial: HashMap::new(), max_frame }),
        authed: Mutex::new(false),
    });
    if let Some((ctx, _)) = &found {
        ctx.live.seen(client.split(" :").next().unwrap_or(&client), "Service Bus");
    }
    let hb_tx = tx.clone();
    let hb = tokio::spawn(async move {
        if idle == 0 {
            return;
        }
        loop {
            tokio::time::sleep(std::time::Duration::from_millis((idle as u64 / 2).max(1000))).await;
            if hb_tx.send(frame(0, 0, &[])).is_err() {
                break;
            }
        }
    });
    let reason = loop {
        let Some((kind, ch, body)) = read_frame(&mut r).await else { break "connection closed by the client" };
        if kind != 0 || body.is_empty() {
            continue;
        }
        let Some((p, payload)) = split(&body) else { continue };
        if !handle(&conn, ch, p, payload).await {
            break "connection closed by the client";
        }
    };
    hb.abort();
    // Links die with the connection: consumers close; Service Bus keeps their locks until they expire.
    let links: Vec<(String, Arc<Notify>)> = conn.st.lock().unwrap().links.drain().filter(|(_, l)| !l.consumer.is_empty()).map(|(_, l)| (l.consumer, l.wake)).collect();
    if let Some((ctx, _)) = conn.ctx_ns() {
        for (id, wake) in links {
            wake.notify_waiters();
            broker::kills().lock().unwrap().remove(&id);
            broker::consumer_close(&ctx, &id, reason).await;
        }
    }
    drop(tx);
    let _ = writer.await;
}

async fn handle(conn: &Arc<Conn>, ch: u16, p: Performative, payload: Vec<u8>) -> bool {
    match p {
        Performative::Begin(b) => {
            {
                let mut g = conn.st.lock().unwrap();
                let s = g.sessions.entry(ch).or_default();
                s.remote_next_outgoing = b.next_outgoing_id;
                s.next_outgoing = 1;
            }
            let mine = Begin { remote_channel: Some(ch), next_outgoing_id: 1, incoming_window: 5000, outgoing_window: 5000, handle_max: Handle(u32::MAX), offered_capabilities: None, desired_capabilities: None, properties: None };
            conn.send(perf(ch, &Performative::Begin(mine)));
            true
        }
        Performative::Attach(a) => {
            attach(conn, ch, a).await;
            true
        }
        Performative::Flow(f) => {
            {
                let mut g = conn.st.lock().unwrap();
                if let Some(s) = g.sessions.get_mut(&ch) {
                    s.remote_next_outgoing = f.next_outgoing_id;
                }
            }
            if let Some(h) = f.handle {
                let wake = {
                    let mut g = conn.st.lock().unwrap();
                    g.links.get_mut(&(ch, h.0)).filter(|l| l.we_send).map(|l| {
                        let theirs = f.delivery_count.unwrap_or(l.delivery_count);
                        l.credit = theirs.wrapping_add(f.link_credit.unwrap_or(0)).wrapping_sub(l.delivery_count).min(100_000);
                        l.drain = f.drain;
                        l.wake.clone()
                    })
                };
                if let Some(w) = wake {
                    w.notify_waiters();
                    w.notify_one();
                }
            }
            true
        }
        Performative::Transfer(t) => {
            transfer_in(conn, ch, t, payload).await;
            true
        }
        Performative::Disposition(d) => {
            disposition(conn, ch, d).await;
            true
        }
        Performative::Detach(d) => {
            let l = conn.st.lock().unwrap().links.remove(&(ch, d.handle.0));
            conn.send(perf(ch, &Performative::Detach(Detach { handle: d.handle, closed: true, error: None })));
            if let (Some(l), Some((ctx, n))) = (l, conn.ctx_ns()) {
                l.wake.notify_waiters();
                if let (Some(sid), Node::Receive(e)) = (&l.session, &l.node) {
                    broker::release_session(&ctx, &n.namespace, e, sid).await;
                }
                if !l.consumer.is_empty() {
                    broker::kills().lock().unwrap().remove(&l.consumer);
                    broker::consumer_close(&ctx, &l.consumer, "receiver closed by the client").await;
                }
            }
            true
        }
        Performative::End(_) => {
            let gone: Vec<Link> = {
                let mut g = conn.st.lock().unwrap();
                g.sessions.remove(&ch);
                let keys: Vec<(u16, u32)> = g.links.keys().filter(|k| k.0 == ch).copied().collect();
                keys.into_iter().filter_map(|k| g.links.remove(&k)).collect()
            };
            if let Some((ctx, _)) = conn.ctx_ns() {
                for l in gone {
                    l.wake.notify_waiters();
                    if !l.consumer.is_empty() {
                        broker::consumer_close(&ctx, &l.consumer, "session ended by the client").await;
                    }
                }
            }
            conn.send(perf(ch, &Performative::End(End { error: None })));
            true
        }
        Performative::Close(_) => {
            conn.send(perf(0, &Performative::Close(Close { error: None })));
            false
        }
        Performative::Open(_) => true,
    }
}

// ---------------------------------------------------------------- links

async fn attach(conn: &Arc<Conn>, ch: u16, a: Attach) {
    let they_send = a.role == Role::Sender;
    let target_addr = a.target.as_ref().and_then(|t| match t.as_ref() {
        TargetArchetype::Target(t) => t.address.clone(),
        #[allow(unreachable_patterns)]
        _ => None,
    }).unwrap_or_default();
    let source_addr = a.source.as_ref().and_then(|s| s.address.clone()).unwrap_or_default();
    // A namespace not named by the host comes from the first address that carries one.
    if conn.ctx_ns().is_none() {
        for addr in [&target_addr, &source_addr] {
            if addr.contains("://") {
                if let Some((c, n)) = resolve(&conn.envs, &host_of(addr)) {
                    let mut g = conn.st.lock().unwrap();
                    g.ctx = Some(c);
                    g.ns = Some(n);
                    break;
                }
            }
        }
    }
    let path = entity_path(if they_send { &target_addr } else { &source_addr });
    let node = if path == "$cbs" {
        if they_send { Node::Cbs } else { Node::Reply(target_addr.clone()) }
    } else if let Some(e) = path.strip_suffix("/$management") {
        if they_send { Node::Mgmt(e.to_string()) } else { Node::Reply(target_addr.clone()) }
    } else if !they_send && (source_addr == "$cbs" || source_addr.ends_with("$management")) {
        Node::Reply(target_addr.clone())
    } else if they_send {
        Node::Send(path.clone())
    } else {
        Node::Receive(path.clone())
    };
    // An entity link must name one that exists (or may be created).
    let mut refuse: Option<AErr> = None;
    if let (Node::Send(e) | Node::Receive(e), Some((ctx, n))) = (&node, conn.ctx_ns()) {
        let holder = e.trim_end_matches("/$DeadLetterQueue").trim_end_matches("/$deadletterqueue").to_string();
        let known = n.holders().contains(&holder) || n.entities.contains_key(&holder);
        if !known {
            let is_sub = holder.contains("/subscriptions/");
            if n.behaviour.auto_create && !is_sub {
                let c = conn.client.clone();
                let _ = config::change(&ctx, &n.namespace, |ns| {
                    ns.entities.entry(holder.clone()).or_insert(config::Entity { queue: Some(config::Queue { lock: "30s".into(), max_delivery: 10, created_by: c.clone(), ..Default::default() }), topic: None });
                    Ok(())
                });
                let fresh = config::get(&ctx, &n.namespace).ok();
                conn.st.lock().unwrap().ns = fresh;
            } else {
                refuse = Some(err("amqp:not-found", &format!("The messaging entity '{}:{}' could not be found. To know more visit https://aka.ms/sbResourceMgrExceptions.", n.namespace, holder)));
            }
        }
    }
    // Sessions: the receiver asks for one session (or the next free one) in its source filter.
    const SESSION_FILTER: &str = "com.microsoft:session-filter";
    let wants_session = !they_send && a.source.as_ref().and_then(|s| s.filter.as_ref()).map(|f| f.iter().any(|(k, _)| k.as_str() == SESSION_FILTER)).unwrap_or(false);
    let mut session: Option<(String, i64)> = None;
    let mut source = a.source.clone();
    if wants_session && refuse.is_none() {
        let asked = a.source.as_ref().and_then(|s| s.filter.as_ref()).and_then(|f| f.iter().find(|(k, _)| k.as_str() == SESSION_FILTER).map(|(_, v)| v.clone()));
        let want = match asked {
            Some(Value::Described(d)) => match &d.value { Value::String(s) => Some(s.clone()), _ => None },
            Some(Value::String(s)) => Some(s),
            _ => None,
        };
        if let (Node::Receive(e), Some((ctx, n))) = (&node, conn.ctx_ns()) {
            let lock = n.limits(e).lock_ms;
            let end = util::now_ms() + 10_000;
            while session.is_none() && util::now_ms() < end {
                session = broker::accept_session(&ctx, &n.namespace, e, want.clone(), &format!("{}:{}", conn.remote, a.name), lock).await;
                if session.is_none() {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                }
            }
            match &session {
                None => refuse = Some(err("com.microsoft:timeout", &if want.is_some() { format!("The session '{}' is locked by another receiver.", want.unwrap_or_default()) } else { "No session is available: no message with a session id waits in this entity.".to_string() })),
                Some((sid, _)) => {
                    if let Some(src) = source.as_mut() {
                        let mut f = src.filter.clone().unwrap_or_default();
                        let desc = match f.iter().find(|(k, _)| k.as_str() == SESSION_FILTER).map(|(_, v)| v.clone()) {
                            Some(Value::Described(mut d)) => { d.value = Value::String(sid.clone()); Value::Described(d) }
                            _ => Value::String(sid.clone()),
                        };
                        f.insert(Symbol::from(SESSION_FILTER), desc);
                        src.filter = Some(f);
                    }
                }
            }
        }
    }
    let props = session.as_ref().map(|(_, until)| {
        // .NET ticks: 100 ns since 0001-01-01.
        let ticks = (*until + 62_135_596_800_000) * 10_000;
        let mut m: OrderedMap<Symbol, Value> = OrderedMap::new();
        m.insert(Symbol::from("com.microsoft:locked-until-utc"), Value::Long(ticks));
        m
    });
    let reply = Attach {
        name: a.name.clone(),
        handle: a.handle.clone(),
        role: if they_send { Role::Receiver } else { Role::Sender },
        snd_settle_mode: a.snd_settle_mode.clone(),
        rcv_settle_mode: a.rcv_settle_mode.clone(),
        source: if refuse.is_some() && !they_send { None } else { source.clone() },
        target: if refuse.is_some() && they_send { None } else { a.target.clone() },
        unsettled: None,
        incomplete_unsettled: false,
        initial_delivery_count: if they_send { None } else { Some(0) },
        max_message_size: Some(1024 * 1024),
        offered_capabilities: None,
        desired_capabilities: None,
        properties: props,
    };
    conn.send(perf(ch, &Performative::Attach(reply)));
    if let Some(e) = refuse {
        conn.send(perf(ch, &Performative::Detach(Detach { handle: a.handle, closed: true, error: Some(e) })));
        return;
    }
    let wake = Arc::new(Notify::new());
    let consumer = if let Node::Receive(_) = &node { format!("sb-{}", broker::uuid4()) } else { String::new() };
    let initial = a.initial_delivery_count.unwrap_or(0);
    conn.st.lock().unwrap().links.insert((ch, a.handle.0), Link {
        name: a.name.clone(),
        node: node.clone(),
        we_send: !they_send,
        settled_mode: a.snd_settle_mode == SenderSettleMode::Settled,
        credit: 0,
        delivery_count: if they_send { initial } else { 0 },
        drain: false,
        received: 0,
        consumer: consumer.clone(),
        wake: wake.clone(),
        session: session.as_ref().map(|s| s.0.clone()),
    });
    if they_send {
        conn.flow(ch, a.handle.0, 1000, initial, false);
    }
    if let (Node::Receive(e), Some((ctx, n))) = (&node, conn.ctx_ns()) {
        let holder = e.trim_end_matches("/$DeadLetterQueue").to_string();
        let mode = if a.snd_settle_mode == SenderSettleMode::Settled { "ReceiveAndDelete" } else { "PeekLock" };
        let lock = n.limits(&holder).lock_ms;
        broker::consumer_open(&ctx, &consumer, &n.namespace, &holder, &conn.client, &a.name, mode, 0, &conn.remote, "amqp1").await;
        broker::kills().lock().unwrap().insert(consumer.clone(), wake.clone());
        let scope = broker::Scope { session: session.as_ref().map(|s| s.0.clone()), dead_letter: e.ends_with("$DeadLetterQueue") };
        tokio::spawn(deliver(conn.clone(), ch, a.handle.0, holder, consumer, lock, a.snd_settle_mode == SenderSettleMode::Settled, scope));
    }
    let _ = ReceiverSettleMode::First;
}

/// Sends messages on one receive link while the app gives credit.
async fn deliver(conn: Arc<Conn>, ch: u16, handle: u32, holder: String, consumer: String, lock_ms: i64, settled: bool, scope: broker::Scope) {
    let Some((ctx, n)) = conn.ctx_ns() else { return };
    let bell = broker::bell(&ctx, &n.namespace, &holder);
    loop {
        let (credit, drain, wake, alive) = {
            let g = conn.st.lock().unwrap();
            match g.links.get(&(ch, handle)) {
                Some(l) => (l.credit, l.drain, l.wake.clone(), true),
                None => (0, false, Arc::new(Notify::new()), false),
            }
        };
        if !alive || conn.tx.is_closed() || ctx.state.gone() {
            break;
        }
        // "Disconnect it": the link was told to go.
        if broker::kills().lock().unwrap().get(&consumer).is_none() {
            conn.send(perf(ch, &Performative::Detach(Detach { handle: Handle(handle), closed: true, error: Some(err("amqp:link:detach-forced", "The link was closed by Stuntman (Disconnect it).")) })));
            conn.st.lock().unwrap().links.remove(&(ch, handle));
            broker::consumer_close(&ctx, &consumer, "disconnected from Stuntman (Disconnect it)").await;
            break;
        }
        if credit == 0 {
            tokio::select! { _ = wake.notified() => {} _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {} }
            continue;
        }
        let claimed = broker::claim_in(&ctx, &n.namespace, &holder, &consumer, &conn.client, if settled { None } else { Some(lock_ms) }, scope.clone()).await;
        match claimed {
            Some(c) => {
                let tag = guid_bytes_le(&c.lock_token);
                let payload = encode(incoming(&c));
                let id = conn.transfer(ch, handle, tag, settled, payload);
                {
                    let mut g = conn.st.lock().unwrap();
                    if let Some(l) = g.links.get_mut(&(ch, handle)) {
                        l.credit = l.credit.saturating_sub(1);
                        l.delivery_count = l.delivery_count.wrapping_add(1);
                    }
                    g.out.insert((ch, id), (handle, c.lock_token.clone()));
                }
                if settled {
                    let _ = broker::settle(&ctx, &n.namespace, &c.lock_token, "complete", "", "", &conn.client).await;
                }
            }
            None if drain => {
                let dc = {
                    let mut g = conn.st.lock().unwrap();
                    let Some(l) = g.links.get_mut(&(ch, handle)) else { break };
                    l.delivery_count = l.delivery_count.wrapping_add(l.credit);
                    l.credit = 0;
                    l.drain = false;
                    l.delivery_count
                };
                conn.flow(ch, handle, 0, dc, true);
            }
            None => {
                tokio::select! { _ = bell.notified() => {} _ = wake.notified() => {} _ = tokio::time::sleep(std::time::Duration::from_millis(1000)) => {} }
            }
        }
    }
}

// ---------------------------------------------------------------- incoming transfers

async fn transfer_in(conn: &Arc<Conn>, ch: u16, t: Transfer, payload: Vec<u8>) {
    let key = (ch, t.handle.0);
    {
        let mut g = conn.st.lock().unwrap();
        if let Some(s) = g.sessions.get_mut(&ch) {
            s.remote_next_outgoing = s.remote_next_outgoing.wrapping_add(1);
        }
    }
    let whole = {
        let mut g = conn.st.lock().unwrap();
        let p = g.partial.entry(key).or_insert(Partial { handle: t.handle.0, delivery_id: t.delivery_id.unwrap_or(0), settled: t.settled.unwrap_or(false), payload: Vec::new() });
        if let Some(d) = t.delivery_id {
            p.delivery_id = d;
        }
        if let Some(s) = t.settled {
            p.settled = s;
        }
        p.payload.extend_from_slice(&payload);
        if t.more { None } else { g.partial.remove(&key) }
    };
    let Some(p) = whole else { return };
    let (node, credit_low, dc) = {
        let mut g = conn.st.lock().unwrap();
        let Some(l) = g.links.get_mut(&key) else { return };
        l.received += 1;
        l.delivery_count = l.delivery_count.wrapping_add(1);
        (l.node.clone(), l.received % 500 == 0, l.delivery_count)
    };
    if credit_low {
        conn.flow(ch, p.handle, 1000, dc, false);
    }
    let Some(msg) = decode(&p.payload) else {
        reply_state(conn, ch, p.delivery_id, p.settled, DeliveryState::Rejected(Rejected { error: Some(err("amqp:decode-error", "Stuntman could not read this message")) }));
        return;
    };
    match node {
        Node::Send(entity) => {
            let state = match conn.ctx_ns() {
                None => DeliveryState::Rejected(Rejected { error: Some(err("amqp:unauthorized-access", "Put a token on $cbs first")) }),
                Some((ctx, n)) => {
                    let mut o = outgoing(&msg, &conn.client);
                    // A batch (format 0x80013700) carries several messages as Data sections.
                    let batch: Vec<broker::Outgoing> = if msg.message_annotations.is_none() && matches!(&msg.body, Body::Data(b) if b.len() > 1) {
                        match &msg.body {
                            Body::Data(b) => b.iter().filter_map(|d| decode(&d.0)).map(|m| outgoing(&m, &conn.client)).collect(),
                            _ => vec![],
                        }
                    } else {
                        vec![]
                    };
                    let all = if batch.is_empty() { vec![std::mem::take(&mut o)] } else { batch };
                    let mut st = DeliveryState::Accepted(Accepted {});
                    for o in all {
                        if let Err(e) = broker::publish(&ctx, &n.namespace, &entity, o).await {
                            let (cond, text) = match &e {
                                broker::Refusal::NotFound(t) => ("amqp:not-found", t.clone()),
                                broker::Refusal::Busy(t) => ("com.microsoft:server-busy", t.clone()),
                                broker::Refusal::Quota(t) => ("amqp:resource-limit-exceeded", t.clone()),
                                broker::Refusal::Unauthorized(t) => ("amqp:unauthorized-access", t.clone()),
                                broker::Refusal::LockLost(t) => ("com.microsoft:message-lock-lost", t.clone()),
                            };
                            st = DeliveryState::Rejected(Rejected { error: Some(err(cond, &text)) });
                            break;
                        }
                    }
                    st
                }
            };
            reply_state(conn, ch, p.delivery_id, p.settled, state);
        }
        Node::Cbs => {
            reply_state(conn, ch, p.delivery_id, p.settled, DeliveryState::Accepted(Accepted {}));
            cbs(conn, ch, msg).await;
        }
        Node::Mgmt(entity) => {
            reply_state(conn, ch, p.delivery_id, p.settled, DeliveryState::Accepted(Accepted {}));
            management(conn, ch, &entity, msg).await;
        }
        _ => {}
    }
}

fn reply_state(conn: &Conn, ch: u16, id: u32, settled: bool, state: DeliveryState) {
    if settled {
        return;
    }
    conn.send(perf(ch, &Performative::Disposition(Disposition { role: Role::Receiver, first: id, last: None, settled: true, state: Some(state), batchable: false })));
}

fn app_prop(m: &Message<Body<Value>>, k: &str) -> Option<String> {
    m.application_properties.as_ref()?.0.iter().find(|(key, _)| key.as_str() == k).map(|(_, v)| match v {
        SimpleValue::String(s) => s.clone(),
        SimpleValue::Symbol(s) => s.as_str().to_string(),
        other => format!("{other:?}"),
    })
}

/// Sends an answer on the link whose target is the request's reply-to.
fn respond(conn: &Conn, ch: u16, req: &Message<Body<Value>>, status: i32, description: &str, body: Value) {
    let reply_to = req.properties.as_ref().and_then(|p| p.reply_to.clone()).unwrap_or_default();
    let handle = {
        let g = conn.st.lock().unwrap();
        g.links.iter().find(|((c, _), l)| *c == ch && matches!(&l.node, Node::Reply(t) if *t == reply_to)).map(|((_, h), _)| *h)
            .or_else(|| g.links.iter().find(|((c, _), l)| *c == ch && matches!(&l.node, Node::Reply(_))).map(|((_, h), _)| *h))
    };
    let Some(handle) = handle else { return };
    let mut app: OrderedMap<String, SimpleValue> = OrderedMap::new();
    app.insert("status-code".into(), SimpleValue::Int(status));
    app.insert("statusCode".into(), SimpleValue::Int(status));
    app.insert("status-description".into(), SimpleValue::String(description.into()));
    app.insert("statusDescription".into(), SimpleValue::String(description.into()));
    let corr = req.properties.as_ref().and_then(|p| p.message_id.clone());
    let m: Message<Body<Value>> = Message {
        header: None,
        delivery_annotations: None,
        message_annotations: None,
        properties: Some(Properties { correlation_id: corr, ..Default::default() }),
        application_properties: Some(ApplicationProperties(app)),
        body: Body::Value(AmqpValue(body)),
        footer: None,
    };
    let tag = broker::uuid4().into_bytes();
    conn.transfer(ch, handle, tag, true, encode(m));
    let mut g = conn.st.lock().unwrap();
    if let Some(l) = g.links.get_mut(&(ch, handle)) {
        l.credit = l.credit.saturating_sub(1);
        l.delivery_count = l.delivery_count.wrapping_add(1);
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 + 1 {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..(i + 3).min(b.len())]).unwrap_or(""), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into()
}

/// A SAS token "SharedAccessSignature sr=…&sig=…&se=…&skn=…" checked against a policy key.
pub fn sas_ok(n: &config::Namespace, ctx: &Ctx, token: &str) -> bool {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use hmac::{Hmac, Mac};
    let t = token.trim_start_matches("SharedAccessSignature").trim();
    let kv: HashMap<&str, &str> = t.split('&').filter_map(|p| p.split_once('=')).collect();
    let (Some(sr), Some(sig), Some(se), Some(skn)) = (kv.get("sr"), kv.get("sig"), kv.get("se"), kv.get("skn")) else { return false };
    if !n.access.policies.contains_key(*skn) {
        return false;
    }
    if se.parse::<i64>().map(|e| e * 1000 < util::now_ms()).unwrap_or(true) {
        return false;
    }
    let key = config::key_value(ctx, &n.namespace, skn);
    let mut m = Hmac::<sha2::Sha256>::new_from_slice(key.as_bytes()).unwrap();
    m.update(format!("{sr}\n{se}").as_bytes());
    B64.encode(m.finalize().into_bytes()) == pct_decode(sig)
}

async fn cbs(conn: &Arc<Conn>, ch: u16, m: Message<Body<Value>>) {
    let audience = app_prop(&m, "name").unwrap_or_default();
    if conn.ctx_ns().is_none() {
        if let Some((c, n)) = resolve(&conn.envs, &host_of(&audience)) {
            c.live.seen(conn.client.split(" :").next().unwrap_or(&conn.client), "Service Bus");
            let mut g = conn.st.lock().unwrap();
            g.ctx = Some(c);
            g.ns = Some(n);
        }
    }
    let token = match &m.body {
        Body::Value(AmqpValue(Value::String(s))) => s.clone(),
        b => String::from_utf8_lossy(&body_bytes(b)).into(),
    };
    let ok = match conn.ctx_ns() {
        None => false,
        Some((ctx, n)) => n.behaviour.reachability != "401" && (n.access.any_key || token.starts_with("Bearer") && n.access.managed_identity || sas_ok(&n, &ctx, &token)),
    };
    *conn.authed.lock().unwrap() = ok;
    if ok {
        respond(conn, ch, &m, 202, "Accepted", Value::Null);
    } else {
        respond(conn, ch, &m, 401, "InvalidSignature: The token has an invalid signature.", Value::Null);
    }
}

fn map_get<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    match v {
        Value::Map(m) => m.iter().find(|(key, _)| matches!(key, Value::String(s) if s == k) || matches!(key, Value::Symbol(s) if s.as_str() == k)).map(|(_, v)| v),
        _ => None,
    }
}

fn tokens(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(|x| if let Value::Uuid(u) = x { Some(uuid_string(u.as_inner())) } else { None }).collect(),
        Some(Value::List(a)) => a.iter().filter_map(|x| if let Value::Uuid(u) = x { Some(uuid_string(u.as_inner())) } else { None }).collect(),
        _ => vec![],
    }
}

fn longs(v: Option<&Value>) -> Vec<i64> {
    let f = |x: &Value| match x {
        Value::Long(n) => Some(*n),
        Value::Int(n) => Some(*n as i64),
        Value::Ulong(n) => Some(*n as i64),
        _ => None,
    };
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(f).collect(),
        Some(Value::List(a)) => a.iter().filter_map(f).collect(),
        _ => vec![],
    }
}

fn vmap(pairs: Vec<(&str, Value)>) -> Value {
    let mut m: OrderedMap<Value, Value> = OrderedMap::new();
    for (k, v) in pairs {
        m.insert(Value::String(k.into()), v);
    }
    Value::Map(m)
}

async fn management(conn: &Arc<Conn>, ch: u16, entity: &str, m: Message<Body<Value>>) {
    let Some((ctx, n)) = conn.ctx_ns() else {
        respond(conn, ch, &m, 401, "Unauthorized", Value::Null);
        return;
    };
    let op = app_prop(&m, "operation").unwrap_or_default();
    let body = match &m.body {
        Body::Value(AmqpValue(v)) => v.clone(),
        _ => Value::Null,
    };
    let holder = entity.trim_end_matches("/$DeadLetterQueue").to_string();
    match op.as_str() {
        "com.microsoft:renew-lock" => {
            let mut exp = vec![];
            for t in tokens(map_get(&body, "lock-tokens")) {
                match broker::renew(&ctx, &n.namespace, &t).await {
                    Ok(u) => exp.push(Value::Timestamp(Timestamp::from_milliseconds(u))),
                    Err(e) => {
                        respond(conn, ch, &m, 410, &format!("MessageLockLost: {}", e.text()), Value::Null);
                        return;
                    }
                }
            }
            respond(conn, ch, &m, 200, "OK", vmap(vec![("expirations", Value::Array(Array::from(exp)))]));
        }
        "com.microsoft:update-disposition" => {
            let status = match map_get(&body, "disposition-status") {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            let outcome = match status.as_str() {
                "completed" => "complete",
                "abandoned" => "abandon",
                "suspended" => "dead_letter",
                "defered" | "deferred" => "defer",
                _ => "complete",
            };
            let reason = match map_get(&body, "deadletter-reason") { Some(Value::String(s)) => s.clone(), _ => String::new() };
            let desc = match map_get(&body, "deadletter-description") { Some(Value::String(s)) => s.clone(), _ => String::new() };
            for t in tokens(map_get(&body, "lock-tokens")) {
                if let Err(e) = broker::settle(&ctx, &n.namespace, &t, outcome, &reason, &desc, &conn.client).await {
                    respond(conn, ch, &m, 410, &format!("MessageLockLost: {}", e.text()), Value::Null);
                    return;
                }
            }
            respond(conn, ch, &m, 200, "OK", vmap(vec![]));
        }
        "com.microsoft:schedule-message" => {
            let mut seqs = vec![];
            if let Some(Value::List(msgs)) = map_get(&body, "messages") {
                for item in msgs {
                    if let Some(Value::Binary(b)) = map_get(item, "message") {
                        if let Some(inner) = decode(b) {
                            match broker::publish(&ctx, &n.namespace, &holder, outgoing(&inner, &conn.client)).await {
                                Ok(s) => seqs.extend(s.into_iter().map(Value::Long)),
                                Err(e) => {
                                    respond(conn, ch, &m, 503, e.text(), Value::Null);
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            respond(conn, ch, &m, 200, "OK", vmap(vec![("sequence-numbers", Value::Array(Array::from(seqs)))]));
        }
        "com.microsoft:cancel-scheduled-message" => {
            for s in longs(map_get(&body, "sequence-numbers")) {
                let _ = broker::delete(&ctx, &n.namespace, s).await;
            }
            respond(conn, ch, &m, 200, "OK", vmap(vec![]));
        }
        "com.microsoft:peek-message" => {
            let from = longs(map_get(&body, "from-sequence-number").map(|v| Value::List(vec![v.clone()])).as_ref()).first().copied().unwrap_or(0);
            let count = match map_get(&body, "message-count") { Some(Value::Int(n)) => *n as u32, _ => 1 };
            let state = if entity.ends_with("$DeadLetterQueue") { Some("dead_letter".to_string()) } else { None };
            let mut rows = broker::list(&ctx, &n.namespace, &holder, state, None, 1000).await.unwrap_or_default();
            rows.retain(|r| r["seq"].as_i64().unwrap_or(0) >= from && r["state"] != "completed");
            rows.sort_by_key(|r| r["seq"].as_i64().unwrap_or(0));
            rows.truncate(count as usize);
            if rows.is_empty() {
                respond(conn, ch, &m, 204, "No Content", vmap(vec![]));
                return;
            }
            let mut list = vec![];
            for r in rows {
                let seq = r["seq"].as_i64().unwrap_or(0);
                let body = broker::raw_body(&ctx, seq).await.unwrap_or_default();
                let c = broker::Claimed { seq, holder: holder.clone(), lock_token: String::new(), locked_until: i64::MAX, delivery_count: r["deliveryCount"].as_i64().unwrap_or(0) + 1,
                    enqueued_at: r["enqueuedAt"].as_i64().unwrap_or(0), message_id: r["messageId"].as_str().unwrap_or("").into(), subject: r["subject"].as_str().unwrap_or("").into(),
                    content_type: r["contentType"].as_str().unwrap_or("").into(), body, props: r["properties"].as_object().cloned().unwrap_or_default(), sys: r["system"].as_object().cloned().unwrap_or_default(), lose_lock: false };
                list.push(vmap(vec![("message", Value::Binary(Binary::from(encode(incoming(&c)))))]));
            }
            respond(conn, ch, &m, 200, "OK", vmap(vec![("messages", Value::List(list))]));
        }
        "com.microsoft:renew-session-lock" => {
            let sid = match map_get(&body, "session-id") { Some(Value::String(s)) => s.clone(), _ => String::new() };
            let until = broker::renew_session(&ctx, &n.namespace, &holder, &sid, n.limits(&holder).lock_ms).await;
            respond(conn, ch, &m, 200, "OK", vmap(vec![("expiration", Value::Timestamp(Timestamp::from_milliseconds(until)))]));
        }
        "com.microsoft:get-session-state" => {
            let sid = match map_get(&body, "session-id") { Some(Value::String(s)) => s.clone(), _ => String::new() };
            let st = broker::session_state(&ctx, &n.namespace, &holder, &sid).await;
            respond(conn, ch, &m, 200, "OK", vmap(vec![("session-state", st.map(|b| Value::Binary(Binary::from(b))).unwrap_or(Value::Null))]));
        }
        "com.microsoft:set-session-state" => {
            let sid = match map_get(&body, "session-id") { Some(Value::String(s)) => s.clone(), _ => String::new() };
            let st = match map_get(&body, "session-state") { Some(Value::Binary(b)) => Some(b.to_vec()), _ => None };
            broker::set_session_state(&ctx, &n.namespace, &holder, &sid, st).await;
            respond(conn, ch, &m, 200, "OK", vmap(vec![]));
        }
        "com.microsoft:receive-by-sequence-number" => {
            let seqs = longs(map_get(&body, "sequence-numbers"));
            let got = broker::claim_deferred(&ctx, &n.namespace, &holder, seqs, &format!("sb-mgmt-{}", conn.remote), &conn.client, n.limits(&holder).lock_ms).await;
            let list: Vec<Value> = got.iter().map(|c| {
                let u = parse_uuid(&c.lock_token).unwrap_or([0; 16]);
                vmap(vec![("lock-token", Value::Uuid(u.into())), ("message", Value::Binary(Binary::from(encode(incoming(c)))))])
            }).collect();
            respond(conn, ch, &m, 200, "OK", vmap(vec![("messages", Value::List(list))]));
        }
        other => respond(conn, ch, &m, 501, &format!("Stuntman does not do {other} yet"), Value::Null),
    }
}

// ---------------------------------------------------------------- settlement from the app

async fn disposition(conn: &Arc<Conn>, ch: u16, d: Disposition) {
    if d.role != Role::Receiver {
        return;
    }
    let Some((ctx, n)) = conn.ctx_ns() else { return };
    let last = d.last.unwrap_or(d.first);
    let (outcome, reason, desc) = match &d.state {
        Some(DeliveryState::Accepted(_)) => ("complete", String::new(), String::new()),
        Some(DeliveryState::Released(_)) => ("abandon", String::new(), String::new()),
        // Defer is "undeliverable here" (both SDKs); abandon only says the delivery failed.
        Some(DeliveryState::Modified(Modified { undeliverable_here: Some(true), .. })) => ("defer", String::new(), String::new()),
        Some(DeliveryState::Modified(_)) => ("abandon", String::new(), String::new()),
        Some(DeliveryState::Rejected(r)) => {
            let info = r.error.as_ref().and_then(|e| e.info.as_ref());
            let get = |k: &str| info.and_then(|i| i.iter().find(|(key, _)| key.as_str() == k).map(|(_, v)| match v { Value::String(s) => s.clone(), o => format!("{o:?}") })).unwrap_or_default();
            ("dead_letter", get("DeadLetterReason"), get("DeadLetterErrorDescription"))
        }
        _ => return,
    };
    let mut id = d.first;
    loop {
        let entry = conn.st.lock().unwrap().out.remove(&(ch, id));
        if let Some((_, token)) = entry {
            let r = broker::settle(&ctx, &n.namespace, &token, outcome, &reason, &desc, &conn.client).await;
            if !d.settled {
                let st = match r {
                    // The service echoes the outcome it applied; a dead-letter comes back `rejected`
                    // with no error (an error there means it failed: azure-servicebus 7.15+ checks).
                    Ok(_) => match d.state.clone() {
                        Some(DeliveryState::Rejected(_)) => DeliveryState::Rejected(Rejected { error: None }),
                        o => o.unwrap_or(DeliveryState::Accepted(Accepted {})),
                    },
                    Err(e) => DeliveryState::Rejected(Rejected { error: Some(err("com.microsoft:message-lock-lost", e.text())) }),
                };
                conn.send(perf(ch, &Performative::Disposition(Disposition { role: Role::Sender, first: id, last: None, settled: true, state: Some(st), batchable: false })));
            }
        }
        if id == last {
            break;
        }
        id = id.wrapping_add(1);
    }
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn tokens_paths_and_messages() {
        let t = "9f1c27e0-1234-4abc-8def-0123456789ab";
        let le = guid_bytes_le(t);
        assert_eq!(&le[..4], &[0xe0, 0x27, 0x1c, 0x9f]);
        assert_eq!(uuid_string(&parse_uuid(t).unwrap()), t);
        assert_eq!(entity_path("sb://acme-bus-stg.localhost/orders-created"), "orders-created");
        assert_eq!(entity_path("events/Subscriptions/audit"), "events/subscriptions/audit");
        assert_eq!(host_of("sb://acme-bus-stg.localhost:5673/x"), "acme-bus-stg.localhost");
        let c = broker::Claimed { seq: 1042, holder: "q".into(), lock_token: t.into(), locked_until: 1, delivery_count: 2, enqueued_at: 5, message_id: "m1".into(), subject: "OrderCreated".into(),
            content_type: "application/json".into(), body: b"{\"a\":1}".to_vec(), props: [("tenant".to_string(), json!("globex"))].into_iter().collect(), sys: Map::new(), lose_lock: false };
        let back = decode(&encode(incoming(&c))).unwrap();
        let o = outgoing(&back, "x");
        assert_eq!((o.subject.as_str(), o.message_id.as_str(), o.body.as_slice()), ("OrderCreated", "m1", &b"{\"a\":1}"[..]));
        assert_eq!(o.props["tenant"], json!("globex"));
        assert_eq!(back.header.unwrap().delivery_count, 1);
        assert_eq!(pct_decode("a%2Fb%3D"), "a/b=");
    }
}
