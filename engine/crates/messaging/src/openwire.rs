//! ActiveMQ "Classic" over OpenWire, on its own port (61616): what Apache.NMS.ActiveMQ and the
//! Java activemq-client speak. The broker offers OpenWire version 10 with loose encoding, no
//! marshal cache, size-prefixed frames and no stack traces; both clients take it (their versions
//! are 10 and 12, the lower one wins). Field orders follow the official v10 marshallers.
//!
//! Queues and plain (non-durable) topics live in the environment's SQLite. A message is pending,
//! delivered (dispatched to a consumer, not acked yet), acked, dead_letter (poison ack) or
//! discarded (a topic copy whose subscriber left). Not implemented: transactions (XA or local),
//! durable subscriptions, selectors, message groups, scheduled delivery, compression (compressed
//! bodies pass through as they came), blob messages, the tight encoding and the marshal cache.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicU16, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};

use base64::Engine;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use stuntman_core::{env::Envs, util, ApiError, Ctx, Tool};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{tcp::OwnedWriteHalf, TcpListener, TcpStream},
    sync::{mpsc, Notify},
};

pub const VERSION: i32 = 10;
const MAX_FRAME: usize = 64 << 20;

// ================================================================ codec

/// One code path for both directions: a field reads itself from a Reader or writes itself to a
/// Writer, so decode(encode(x)) == x by construction.
pub trait Wire {
    fn reading(&self) -> bool;
    fn room(&self) -> usize;
    fn fail(&mut self);
    fn u8(&mut self, v: &mut u8);
    fn i16(&mut self, v: &mut i16);
    fn i32(&mut self, v: &mut i32);
    fn i64(&mut self, v: &mut i64);
    fn raw(&mut self, v: &mut [u8]);
    fn str(&mut self, v: &mut Option<String>);
    fn nested(&mut self, v: &mut Obj);
}

pub trait Field {
    fn io<W: Wire>(&mut self, w: &mut W);
}

pub type Obj = Option<Box<Ds>>;
pub type Objs = Option<Vec<Obj>>;
type S = Option<String>;
type B = Option<Vec<u8>>;

impl Field for u8 {
    fn io<W: Wire>(&mut self, w: &mut W) {
        w.u8(self)
    }
}
impl Field for bool {
    fn io<W: Wire>(&mut self, w: &mut W) {
        let mut b = *self as u8;
        w.u8(&mut b);
        *self = b != 0;
    }
}
impl Field for i32 {
    fn io<W: Wire>(&mut self, w: &mut W) {
        w.i32(self)
    }
}
impl Field for i64 {
    fn io<W: Wire>(&mut self, w: &mut W) {
        w.i64(self)
    }
}
impl Field for [u8; 8] {
    fn io<W: Wire>(&mut self, w: &mut W) {
        w.raw(self)
    }
}
impl Field for S {
    fn io<W: Wire>(&mut self, w: &mut W) {
        w.str(self)
    }
}
impl Field for Obj {
    fn io<W: Wire>(&mut self, w: &mut W) {
        w.nested(self)
    }
}
/// Byte sequences: a "present" flag, an int length, the bytes.
impl Field for B {
    fn io<W: Wire>(&mut self, w: &mut W) {
        let mut some = self.is_some();
        some.io(w);
        if !some {
            *self = None;
            return;
        }
        let v = self.get_or_insert_with(Vec::new);
        let mut n = v.len() as i32;
        w.i32(&mut n);
        if w.reading() {
            if n < 0 || n as usize > w.room() {
                return w.fail();
            }
            v.resize(n as usize, 0);
        }
        w.raw(v);
    }
}
/// Arrays of objects (broker paths): a "present" flag, a short count, nested objects.
impl Field for Objs {
    fn io<W: Wire>(&mut self, w: &mut W) {
        let mut some = self.is_some();
        some.io(w);
        if !some {
            *self = None;
            return;
        }
        let v = self.get_or_insert_with(Vec::new);
        let mut n = v.len() as i16;
        w.i16(&mut n);
        if w.reading() {
            if n < 0 || n as usize > w.room() {
                return w.fail();
            }
            v.resize(n as usize, None);
        }
        for x in v.iter_mut() {
            x.io(w);
        }
    }
}

/// A Java exception (BrokerError), without its stack trace: StackTraceEnabled is negotiated off.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BErr {
    pub class: S,
    pub message: S,
}
impl Field for Option<BErr> {
    fn io<W: Wire>(&mut self, w: &mut W) {
        let mut some = self.is_some();
        some.io(w);
        if !some {
            *self = None;
            return;
        }
        let e = self.get_or_insert_with(Default::default);
        e.class.io(w);
        e.message.io(w);
    }
}

pub struct Reader<'a> {
    b: &'a [u8],
    p: usize,
    pub bad: bool,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Reader { b, p: 0, bad: false }
    }
    fn take(&mut self, n: usize) -> &'a [u8] {
        if self.bad || self.b.len() - self.p < n {
            self.bad = true;
            return &[];
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        s
    }
    fn utf(&mut self) -> String {
        let mut n = 0i16;
        self.i16(&mut n);
        unmutf8(self.take(n as u16 as usize))
    }
}

impl Wire for Reader<'_> {
    fn reading(&self) -> bool {
        true
    }
    fn room(&self) -> usize {
        self.b.len() - self.p
    }
    fn fail(&mut self) {
        self.bad = true
    }
    fn u8(&mut self, v: &mut u8) {
        *v = self.take(1).first().copied().unwrap_or(0)
    }
    fn i16(&mut self, v: &mut i16) {
        *v = self.take(2).try_into().map(i16::from_be_bytes).unwrap_or(0)
    }
    fn i32(&mut self, v: &mut i32) {
        *v = self.take(4).try_into().map(i32::from_be_bytes).unwrap_or(0)
    }
    fn i64(&mut self, v: &mut i64) {
        *v = self.take(8).try_into().map(i64::from_be_bytes).unwrap_or(0)
    }
    fn raw(&mut self, v: &mut [u8]) {
        let s = self.take(v.len());
        if s.len() == v.len() {
            v.copy_from_slice(s)
        }
    }
    fn str(&mut self, v: &mut Option<String>) {
        let mut some = false;
        some.io(self);
        *v = if some { Some(self.utf()) } else { None };
    }
    fn nested(&mut self, v: &mut Obj) {
        let mut some = false;
        some.io(self);
        *v = None;
        if !some {
            return;
        }
        let mut t = 0u8;
        self.u8(&mut t);
        match Ds::new(t) {
            Some(mut d) => {
                d.io(self);
                *v = Some(Box::new(d));
            }
            None => self.bad = true,
        }
    }
}

#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    fn utf(&mut self, s: &str) {
        let b = mutf8(s);
        let n = b.len().min(u16::MAX as usize);
        self.0.extend((n as u16).to_be_bytes());
        self.0.extend(&b[..n]);
    }
}

impl Wire for Writer {
    fn reading(&self) -> bool {
        false
    }
    fn room(&self) -> usize {
        usize::MAX
    }
    fn fail(&mut self) {}
    fn u8(&mut self, v: &mut u8) {
        self.0.push(*v)
    }
    fn i16(&mut self, v: &mut i16) {
        self.0.extend(v.to_be_bytes())
    }
    fn i32(&mut self, v: &mut i32) {
        self.0.extend(v.to_be_bytes())
    }
    fn i64(&mut self, v: &mut i64) {
        self.0.extend(v.to_be_bytes())
    }
    fn raw(&mut self, v: &mut [u8]) {
        self.0.extend_from_slice(v)
    }
    fn str(&mut self, v: &mut Option<String>) {
        self.0.push(v.is_some() as u8);
        if let Some(s) = v {
            self.utf(s);
        }
    }
    fn nested(&mut self, v: &mut Obj) {
        self.0.push(v.is_some() as u8);
        if let Some(d) = v {
            self.0.push(d.typ());
            d.io(self);
        }
    }
}

/// Java's "modified UTF-8": UTF-16 units, NUL as two bytes, surrogates as three bytes each.
pub fn mutf8(s: &str) -> Vec<u8> {
    let mut o = Vec::with_capacity(s.len());
    for c in s.encode_utf16() {
        match c {
            1..=0x7f => o.push(c as u8),
            0 | 0x80..=0x7ff => o.extend([0xc0 | (c >> 6) as u8, 0x80 | (c & 0x3f) as u8]),
            _ => o.extend([0xe0 | (c >> 12) as u8, 0x80 | ((c >> 6) & 0x3f) as u8, 0x80 | (c & 0x3f) as u8]),
        }
    }
    o
}

/// Reads modified UTF-8, and plain 4-byte UTF-8 too (some clients write that).
pub fn unmutf8(b: &[u8]) -> String {
    let mut u: Vec<u16> = Vec::with_capacity(b.len());
    let at = |i: usize| b.get(i).map(|x| (*x & 0x3f) as u32).unwrap_or(0);
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as u32;
        if c < 0x80 {
            u.push(c as u16);
            i += 1;
        } else if c & 0xe0 == 0xc0 {
            u.push((((c & 0x1f) << 6) | at(i + 1)) as u16);
            i += 2;
        } else if c & 0xf0 == 0xe0 {
            u.push((((c & 0x0f) << 12) | (at(i + 1) << 6) | at(i + 2)) as u16);
            i += 3;
        } else if c & 0xf8 == 0xf0 {
            let ch = char::from_u32(((c & 7) << 18) | (at(i + 1) << 12) | (at(i + 2) << 6) | at(i + 3)).unwrap_or('\u{fffd}');
            u.extend_from_slice(ch.encode_utf16(&mut [0u16; 2]));
            i += 4;
        } else {
            u.push(0xfffd);
            i += 1;
        }
    }
    String::from_utf16_lossy(&u)
}

macro_rules! fields {
    ($($name:ident { $($f:ident : $t:ty),* $(,)? })*) => {$(
        #[derive(Clone, Debug, Default, PartialEq)]
        pub struct $name { $(pub $f: $t),* }
        impl Field for $name {
            #[allow(unused_variables)]
            fn io<W: Wire>(&mut self, w: &mut W) { $(self.$f.io(w);)* }
        }
    )*};
}

macro_rules! types {
    ($($var:ident = $typ:literal : $ty:ident),* $(,)?) => {
        /// Every data structure this broker reads or writes, by its OpenWire type byte.
        #[derive(Clone, Debug, PartialEq)]
        pub enum Ds { $($var($ty)),* }
        impl Ds {
            pub fn new(t: u8) -> Option<Ds> { match t { $($typ => Some(Ds::$var($ty::default())),)* _ => None } }
            pub fn typ(&self) -> u8 { match self { $(Ds::$var(_) => $typ),* } }
            pub fn io<W: Wire>(&mut self, w: &mut W) { match self { $(Ds::$var(x) => x.io(w)),* } }
        }
    };
}

// The v10 loose field orders (activemq-client openwire/v10/*Marshaller.java, the same in
// Apache.NMS.ActiveMQ OpenWire/V10). Commands start with BaseCommand: commandId, responseRequired.
fields! {
    Hdr { id: i32, resp: bool }
    Plain { h: Hdr }
    WireFormatInfo { magic: [u8; 8], version: i32, props: B }
    BrokerInfo { h: Hdr, broker_id: Obj, url: S, peers: Objs, name: S, slave: bool, master: bool, fault_tolerant: bool, duplex: bool, network: bool, connection_id: i64, upload_url: S, network_props: S }
    ConnectionInfo { h: Hdr, connection_id: Obj, client_id: S, password: S, user: S, broker_path: Objs, broker_master: bool, manageable: bool, client_master: bool, fault_tolerant: bool, failover_reconnect: bool, client_ip: S }
    SessionInfo { h: Hdr, session_id: Obj }
    ConsumerInfo { h: Hdr, consumer_id: Obj, browser: bool, destination: Obj, prefetch: i32, max_pending: i32, dispatch_async: bool, selector: S, client_id: S, subscription: S,
        no_local: bool, exclusive: bool, retroactive: bool, priority: u8, broker_path: Objs, predicate: Obj, network_subscription: bool, optimized_ack: bool, no_range_acks: bool, network_consumer_path: Objs }
    ProducerInfo { h: Hdr, producer_id: Obj, destination: Obj, broker_path: Objs, dispatch_async: bool, window: i32 }
    TransactionInfo { h: Hdr, connection_id: Obj, tx: Obj, kind: u8 }
    DestinationInfo { h: Hdr, connection_id: Obj, destination: Obj, op: u8, timeout: i64, broker_path: Objs }
    RemoveSubscriptionInfo { h: Hdr, connection_id: Obj, subscription: S, client_id: S }
    RemoveInfo { h: Hdr, object_id: Obj, last_delivered: i64 }
    ControlCommand { h: Hdr, command: S }
    ConnectionError { h: Hdr, error: Option<BErr>, connection_id: Obj }
    ConsumerControl { h: Hdr, destination: Obj, close: bool, consumer_id: Obj, prefetch: i32, flush: bool, start: bool, stop: bool }
    ConnectionControl { h: Hdr, close: bool, exit: bool, fault_tolerant: bool, resume: bool, suspend: bool, connected_brokers: S, reconnect_to: S, rebalance: bool, token: B }
    ProducerAck { h: Hdr, producer_id: Obj, size: i32 }
    MessagePull { h: Hdr, consumer_id: Obj, destination: Obj, timeout: i64, correlation_id: S, message_id: Obj }
    MessageDispatch { h: Hdr, consumer_id: Obj, destination: Obj, message: Obj, redelivery: i32 }
    MessageAck { h: Hdr, destination: Obj, tx: Obj, consumer_id: Obj, ack_type: u8, first: Obj, last: Obj, count: i32, poison: Option<BErr> }
    Msg { h: Hdr, producer_id: Obj, destination: Obj, tx: Obj, original_destination: Obj, message_id: Obj, original_tx: Obj, group_id: S, group_seq: i32, correlation_id: S,
        persistent: bool, expiration: i64, priority: u8, reply_to: Obj, timestamp: i64, kind: S, content: B, props: B, data_structure: Obj, target_consumer: Obj, compressed: bool,
        redelivery: i32, broker_path: Objs, arrival: i64, user_id: S, df_bridge: bool, droppable: bool, cluster: Objs, broker_in: i64, broker_out: i64, group_first: bool }
    Response { h: Hdr, correlation_id: i32 }
    ExceptionResponse { h: Hdr, correlation_id: i32, error: Option<BErr> }
    Dest { name: S }
    MessageId { text: S, producer_id: Obj, producer_seq: i64, broker_seq: i64 }
    LocalTxId { value: i64, connection_id: Obj }
    StrId { value: S }
    SessionId { connection: S, value: i64 }
    ConsumerId { connection: S, session: i64, value: i64 }
    ProducerId { connection: S, value: i64, session: i64 }
}

types! {
    WireFormatInfo = 1: WireFormatInfo, BrokerInfo = 2: BrokerInfo, ConnectionInfo = 3: ConnectionInfo, SessionInfo = 4: SessionInfo, ConsumerInfo = 5: ConsumerInfo,
    ProducerInfo = 6: ProducerInfo, TransactionInfo = 7: TransactionInfo, DestinationInfo = 8: DestinationInfo, RemoveSubscriptionInfo = 9: RemoveSubscriptionInfo,
    KeepAliveInfo = 10: Plain, ShutdownInfo = 11: Plain, RemoveInfo = 12: RemoveInfo, ControlCommand = 14: ControlCommand, FlushCommand = 15: Plain,
    ConnectionError = 16: ConnectionError, ConsumerControl = 17: ConsumerControl, ConnectionControl = 18: ConnectionControl, ProducerAck = 19: ProducerAck,
    MessagePull = 20: MessagePull, MessageDispatch = 21: MessageDispatch, MessageAck = 22: MessageAck,
    Message = 23: Msg, BytesMessage = 24: Msg, MapMessage = 25: Msg, ObjectMessage = 26: Msg, StreamMessage = 27: Msg, TextMessage = 28: Msg,
    Response = 30: Response, ExceptionResponse = 31: ExceptionResponse,
    Queue = 100: Dest, Topic = 101: Dest, TempQueue = 102: Dest, TempTopic = 103: Dest,
    MessageId = 110: MessageId, LocalTxId = 111: LocalTxId, ConnectionId = 120: StrId, SessionId = 121: SessionId, ConsumerId = 122: ConsumerId, ProducerId = 123: ProducerId, BrokerId = 124: StrId,
}

/// A frame: int size (of what follows), the type byte, the fields.
pub fn encode(d: &mut Ds) -> Vec<u8> {
    let mut w = Writer(vec![0; 4]);
    w.0.push(d.typ());
    d.io(&mut w);
    let n = (w.0.len() - 4) as u32;
    w.0[..4].copy_from_slice(&n.to_be_bytes());
    w.0
}

/// A frame without its size prefix. Trailing bytes (fields of a later version) are ignored.
pub fn decode(b: &[u8]) -> Option<Ds> {
    let (&t, rest) = b.split_first()?;
    let mut d = Ds::new(t)?;
    let mut r = Reader::new(rest);
    d.io(&mut r);
    (!r.bad).then_some(d)
}

/// TextMessage content: int length, modified UTF-8 (-1: null).
pub fn text_content(s: &str) -> Vec<u8> {
    let b = mutf8(s);
    let mut o = (b.len() as i32).to_be_bytes().to_vec();
    o.extend(b);
    o
}

pub fn content_text(c: &[u8]) -> String {
    let mut r = Reader::new(c);
    let mut n = 0i32;
    r.i32(&mut n);
    if n <= 0 {
        return String::new();
    }
    unmutf8(r.take((n as usize).min(r.room())))
}

/// Message properties and WireFormatInfo options: the "primitive map" of MarshallingSupport.
pub fn map_encode(m: &Map<String, Value>) -> Vec<u8> {
    let mut w = Writer::default();
    w.0.extend((m.len() as i32).to_be_bytes());
    for (k, v) in m {
        w.utf(k);
        prim_put(&mut w, v);
    }
    w.0
}

fn prim_put(w: &mut Writer, v: &Value) {
    match v {
        Value::Null => w.0.push(0),
        Value::Bool(b) => w.0.extend([1, *b as u8]),
        Value::Number(n) => match n.as_i64() {
            Some(i) if i32::try_from(i).is_ok() => {
                w.0.push(5);
                w.0.extend((i as i32).to_be_bytes())
            }
            Some(i) => {
                w.0.push(6);
                w.0.extend(i.to_be_bytes())
            }
            None => {
                w.0.push(7);
                w.0.extend(n.as_f64().unwrap_or(0.0).to_be_bytes())
            }
        },
        Value::String(s) if s.encode_utf16().count() < 8191 => {
            w.0.push(9);
            w.utf(s)
        }
        Value::String(s) => {
            w.0.push(13);
            w.0.extend(text_content(s))
        }
        Value::Array(a) => {
            w.0.push(12);
            w.0.extend((a.len() as i32).to_be_bytes());
            a.iter().for_each(|x| prim_put(w, x));
        }
        Value::Object(o) => {
            w.0.push(11);
            w.0.extend(map_encode(o))
        }
    }
}

pub fn map_decode(b: &[u8]) -> Map<String, Value> {
    map_read(&mut Reader::new(b))
}

fn map_read(r: &mut Reader) -> Map<String, Value> {
    let mut m = Map::new();
    let mut n = 0i32;
    r.i32(&mut n);
    for _ in 0..n.max(0) {
        if r.bad {
            break;
        }
        let k = r.utf();
        let v = prim_read(r);
        m.insert(k, v);
    }
    m
}

fn prim_read(r: &mut Reader) -> Value {
    let mut t = 0u8;
    r.u8(&mut t);
    let (mut b, mut s, mut i, mut l) = (0u8, 0i16, 0i32, 0i64);
    match t {
        0 => Value::Null,
        1 => {
            r.u8(&mut b);
            json!(b != 0)
        }
        2 => {
            r.u8(&mut b);
            json!(b as i8)
        }
        3 => {
            r.i16(&mut s);
            json!(String::from_utf16_lossy(&[s as u16]))
        }
        4 => {
            r.i16(&mut s);
            json!(s)
        }
        5 => {
            r.i32(&mut i);
            json!(i)
        }
        6 => {
            r.i64(&mut l);
            json!(l)
        }
        7 => {
            r.i64(&mut l);
            json!(f64::from_bits(l as u64))
        }
        8 => {
            r.i32(&mut i);
            json!(f32::from_bits(i as u32))
        }
        9 => json!(r.utf()),
        10 => {
            r.i32(&mut i);
            let bytes = r.take(i.max(0) as usize);
            json!({ "bytes": base64::engine::general_purpose::STANDARD.encode(bytes) })
        }
        11 => Value::Object(map_read(r)),
        12 => {
            r.i32(&mut i);
            Value::Array((0..i.max(0)).map_while(|_| (!r.bad).then(|| prim_read(r))).collect())
        }
        13 => {
            r.i32(&mut i);
            json!(unmutf8(r.take(i.max(0) as usize)))
        }
        _ => {
            r.bad = true;
            Value::Null
        }
    }
}

// ================================================================ store

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS amq_messages(seq INTEGER PRIMARY KEY AUTOINCREMENT, dest TEXT NOT NULL, target TEXT NOT NULL DEFAULT '', kind TEXT NOT NULL,
  message_id TEXT NOT NULL DEFAULT '', body BLOB NOT NULL, props TEXT NOT NULL DEFAULT '{}', headers TEXT NOT NULL DEFAULT '{}', state TEXT NOT NULL,
  consumer TEXT NOT NULL DEFAULT '', redelivered INTEGER NOT NULL DEFAULT 0, enqueued_at INTEGER NOT NULL, delivered_at INTEGER, acked_at INTEGER,
  sender TEXT NOT NULL DEFAULT '', via TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS amq_pending ON amq_messages(dest, state, seq);
CREATE TABLE IF NOT EXISTS amq_destinations(name TEXT PRIMARY KEY, created_at INTEGER NOT NULL, created_by TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS amq_settings(id INTEGER PRIMARY KEY, v TEXT NOT NULL);
";

pub const STATES: [&str; 5] = ["pending", "delivered", "acked", "dead_letter", "discarded"];

/// What set_activemq changes. Empty users: any user name and password log in.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub users: BTreeMap<String, String>,
    pub refuse_login: bool,
    pub drop_after: u64,
    pub slow_dispatch_ms: u64,
    pub redeliver: u64,
}

pub async fn settings(ctx: &Ctx) -> Settings {
    ctx.db
        .call(|c| c.query_row("SELECT v FROM amq_settings WHERE id=1", [], |r| r.get::<_, String>(0)).optional())
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

async fn save(ctx: &Ctx, s: &Settings) -> Result<(), ApiError> {
    let v = serde_json::to_string(s).unwrap_or_default();
    ctx.db.call(move |c| c.execute("INSERT OR REPLACE INTO amq_settings(id, v) VALUES(1, ?1)", [v])).await.map(|_| ())
}

fn bell(env: &str) -> Arc<Notify> {
    static B: OnceLock<Mutex<HashMap<String, Arc<Notify>>>> = OnceLock::new();
    B.get_or_init(Default::default).lock().unwrap().entry(env.to_string()).or_default().clone()
}

fn ring(ctx: &Ctx, dest: &str) {
    bell(&ctx.env).notify_waiters();
    ctx.live.send("activemq.message", dest, "changed");
}

/// Consumers connected now, for the tools and topic fan-out.
#[derive(Clone, Debug)]
struct Live {
    env: String,
    key: String,
    dest: String,
    prefetch: i32,
    remote: String,
    user: String,
    since: i64,
}

fn live() -> &'static Mutex<Vec<Live>> {
    static L: OnceLock<Mutex<Vec<Live>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// "queue://positions", "topic://prices", "temp-queue://ID:…"; a bare name is a queue.
pub fn dest_ds(s: &str) -> Ds {
    let (scheme, name) = s.split_once("://").unwrap_or(("queue", s));
    let d = Dest { name: Some(name.to_string()) };
    match scheme {
        "topic" => Ds::Topic(d),
        "temp-queue" => Ds::TempQueue(d),
        "temp-topic" => Ds::TempTopic(d),
        _ => Ds::Queue(d),
    }
}

pub fn dest_name(d: &Ds) -> Option<String> {
    let (scheme, n) = match d {
        Ds::Queue(x) => ("queue", x),
        Ds::Topic(x) => ("topic", x),
        Ds::TempQueue(x) => ("temp-queue", x),
        Ds::TempTopic(x) => ("temp-topic", x),
        _ => return None,
    };
    Some(format!("{scheme}://{}", n.name.as_deref()?))
}

pub fn normalize(s: &str) -> String {
    dest_name(&dest_ds(s.trim())).unwrap_or_default()
}

fn obj_dest(o: &Obj) -> Option<String> {
    o.as_deref().and_then(dest_name)
}

fn id_key(o: &Obj) -> String {
    match o.as_deref() {
        Some(Ds::ConsumerId(c)) => format!("{}:{}:{}", c.connection.as_deref().unwrap_or(""), c.session, c.value),
        Some(Ds::SessionId(s)) => format!("{}:{}", s.connection.as_deref().unwrap_or(""), s.value),
        Some(Ds::ProducerId(p)) => format!("{}:{}:{}", p.connection.as_deref().unwrap_or(""), p.session, p.value),
        Some(Ds::ConnectionId(c)) => c.value.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

/// ActiveMQ's message id text: <connection id>:<session>:<producer>:<sequence>.
fn mid_text(o: &Obj) -> String {
    let Some(Ds::MessageId(m)) = o.as_deref() else { return String::new() };
    match m.producer_id.as_deref() {
        Some(Ds::ProducerId(p)) => format!("{}:{}:{}:{}", p.connection.as_deref().unwrap_or(""), p.session, p.value, m.producer_seq),
        _ => m.text.clone().unwrap_or_default(),
    }
}

fn mid(text: &str, seq: i64) -> MessageId {
    let mut p = text.rsplitn(4, ':');
    let mut num = || p.next().and_then(|x| x.parse::<i64>().ok());
    let (pseq, value, session) = (num().unwrap_or(seq), num().unwrap_or(0), num().unwrap_or(0));
    let conn = p.next().filter(|c| !c.is_empty()).unwrap_or("ID:stuntman-1-0").to_string();
    MessageId { text: None, producer_id: Some(Box::new(Ds::ProducerId(ProducerId { connection: Some(conn), value, session }))), producer_seq: pseq, broker_seq: seq }
}

async fn register(ctx: &Ctx, dest: &str, by: &str) {
    if dest.is_empty() || dest.contains("ActiveMQ.Advisory") || dest.starts_with("temp-") {
        return;
    }
    let (d, by, now) = (dest.to_string(), by.to_string(), util::now_ms());
    let _ = ctx.db.call(move |c| c.execute("INSERT OR IGNORE INTO amq_destinations(name, created_at, created_by) VALUES(?1, ?2, ?3)", params![d, now, by])).await;
}

#[derive(Clone, Debug, Default)]
pub struct NewMsg {
    pub dest: String,
    pub kind: String,
    pub body: Vec<u8>,
    pub props: Map<String, Value>,
    pub headers: Map<String, Value>,
    pub message_id: String,
    pub sender: String,
    pub via: String,
}

/// Puts a message on a queue, or a copy for each subscriber of a topic (none: discarded).
pub async fn publish(ctx: &Ctx, m: NewMsg) -> Result<Vec<i64>, ApiError> {
    register(ctx, &m.dest, &m.sender).await;
    let targets: Vec<String> = if m.dest.starts_with("topic://") {
        let env = ctx.env.to_string();
        let subs: Vec<String> = live().lock().unwrap().iter().filter(|l| l.env == env && l.dest == m.dest).map(|l| l.key.clone()).collect();
        if subs.is_empty() { vec![String::new()] } else { subs }
    } else {
        vec![String::new()]
    };
    let dest = m.dest.clone();
    let seqs = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let mut seqs = vec![];
            let now = util::now_ms();
            for t in targets {
                let state = if m.dest.starts_with("topic://") && t.is_empty() { "discarded" } else { "pending" };
                tx.execute(
                    "INSERT INTO amq_messages(dest, target, kind, message_id, body, props, headers, state, enqueued_at, sender, via) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![m.dest, t, m.kind, m.message_id, m.body, Value::Object(m.props.clone()).to_string(), Value::Object(m.headers.clone()).to_string(), state, now, m.sender, m.via],
                )?;
                let seq = tx.last_insert_rowid();
                if m.message_id.is_empty() {
                    tx.execute("UPDATE amq_messages SET message_id=?1 WHERE seq=?2", params![format!("ID:stuntman-{}-0:0:0:{seq}", port()), seq])?;
                }
                seqs.push(seq);
            }
            tx.commit()?;
            Ok(seqs)
        })
        .await?;
    ring(ctx, &dest);
    Ok(seqs)
}

#[derive(Clone, Debug)]
struct Row {
    seq: i64,
    dest: String,
    kind: String,
    message_id: String,
    body: Vec<u8>,
    props: String,
    headers: String,
    redelivered: i64,
    enqueued_at: i64,
}

const ROW: &str = "seq, dest, kind, message_id, body, props, headers, redelivered, enqueued_at";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row { seq: r.get(0)?, dest: r.get(1)?, kind: r.get(2)?, message_id: r.get(3)?, body: r.get(4)?, props: r.get(5)?, headers: r.get(6)?, redelivered: r.get(7)?, enqueued_at: r.get(8)? })
}

/// Marks the oldest pending message of a destination as delivered to this consumer.
async fn claim(ctx: &Ctx, dest: &str, consumer: &str) -> Option<Row> {
    let (d, k, now) = (dest.to_string(), consumer.to_string(), util::now_ms());
    ctx.db
        .call(move |c| {
            c.query_row(
                &format!("UPDATE amq_messages SET state='delivered', consumer=?2, delivered_at=?3 WHERE seq=(SELECT seq FROM amq_messages WHERE dest=?1 AND state='pending' AND target IN ('', ?2) ORDER BY seq LIMIT 1) RETURNING {ROW}"),
                params![d, k, now],
                row,
            )
            .optional()
        })
        .await
        .ok()
        .flatten()
}

async fn pending_rows(ctx: &Ctx, dest: &str) -> Vec<Row> {
    let d = dest.to_string();
    ctx.db
        .call(move |c| c.prepare(&format!("SELECT {ROW} FROM amq_messages WHERE dest=?1 AND state='pending' AND target='' ORDER BY seq LIMIT 1000"))?.query_map([d], row)?.collect())
        .await
        .unwrap_or_default()
}

/// Settles messages: acked (or, while the redeliver provocation lasts, back to pending), or dead_letter.
async fn settle(ctx: &Ctx, seqs: Vec<i64>, state: &'static str) {
    if seqs.is_empty() {
        return;
    }
    let mut s = settings(ctx).await;
    let again = if state == "acked" { (s.redeliver as usize).min(seqs.len()) } else { 0 };
    if again > 0 {
        s.redeliver -= again as u64;
        let _ = save(ctx, &s).await;
    }
    let dest = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let now = util::now_ms();
            for (i, seq) in seqs.iter().enumerate() {
                if i < again {
                    tx.execute("UPDATE amq_messages SET state='pending', consumer='', delivered_at=NULL, redelivered=redelivered+1 WHERE seq=?1 AND state='delivered'", [seq])?;
                } else {
                    tx.execute("UPDATE amq_messages SET state=?2, acked_at=?3 WHERE seq=?1 AND state='delivered'", params![seq, state, now])?;
                }
            }
            let d: String = tx.query_row("SELECT dest FROM amq_messages WHERE seq=?1", [seqs[0]], |r| r.get(0)).unwrap_or_default();
            tx.commit()?;
            Ok(d)
        })
        .await
        .unwrap_or_default();
    ring(ctx, &dest);
}

/// A consumer went away: what it held goes back to pending (redelivered once more when the app
/// had seen it); a topic subscriber's copies are discarded.
async fn release(ctx: &Ctx, key: String, held: Vec<i64>, last_delivered: Option<i64>, topic: bool) {
    let _ = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if topic {
                tx.execute("UPDATE amq_messages SET state='discarded' WHERE target=?1 AND state IN ('pending','delivered')", [&key])?;
            }
            for seq in held {
                let bump = last_delivered.map(|l| seq <= l).unwrap_or(true) as i64;
                tx.execute("UPDATE amq_messages SET state='pending', consumer='', delivered_at=NULL, redelivered=redelivered+?2 WHERE seq=?1 AND state='delivered'", params![seq, bump])?;
            }
            tx.commit()
        })
        .await;
    ring(ctx, "");
}

// ================================================================ broker

static PORT: AtomicU16 = AtomicU16::new(61616);
pub static BIND_ERROR: Mutex<Option<String>> = Mutex::new(None);

pub fn port() -> u16 {
    PORT.load(Ordering::Relaxed)
}

pub async fn serve(envs: Envs, port: u16) {
    PORT.store(port, Ordering::Relaxed);
    let l = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ActiveMQ (OpenWire): port {port} is taken");
            *BIND_ERROR.lock().unwrap() = Some(format!("port {port}: {e}"));
            return;
        }
    };
    loop {
        let Ok((s, a)) = l.accept().await else { continue };
        let _ = s.set_nodelay(true);
        tokio::spawn(connection(envs.clone(), s, a.to_string()));
    }
}

fn wire_format() -> Ds {
    let mut w = Writer(11i32.to_be_bytes().to_vec());
    for (k, v) in [("TightEncodingEnabled", false), ("CacheEnabled", false), ("SizePrefixDisabled", false), ("StackTraceEnabled", false), ("TcpNoDelayEnabled", true)] {
        w.utf(k);
        w.0.extend([1, v as u8]);
    }
    // Java reads these two as Longs.
    for (k, v) in [("MaxInactivityDuration", 30_000i64), ("MaxInactivityDurationInitalDelay", 10_000)] {
        w.utf(k);
        w.0.push(6);
        w.0.extend(v.to_be_bytes());
    }
    w.utf("CacheSize");
    w.0.extend([5, 0, 0, 0, 0]);
    for (k, v) in [("ProviderName", "ActiveMQ"), ("ProviderVersion", "5.18.6"), ("PlatformDetails", "Stuntman")] {
        w.utf(k);
        prim_put(&mut w, &json!(v));
    }
    Ds::WireFormatInfo(WireFormatInfo { magic: *b"ActiveMQ", version: VERSION, props: Some(w.0) })
}

#[derive(PartialEq)]
enum Flow {
    Go,
    Stop,
}

struct Cons {
    key: String,
    id: Obj,
    dest: String,
    prefetch: i32,
    /// Dispatched, not acked: seq and whether a delivered-ack freed its prefetch slot.
    held: Vec<(i64, bool)>,
    /// MessagePull waiting: -1 answer now, 0 wait for ever, else the deadline (ms).
    pull: Option<i64>,
    /// Advisory topics, composite destinations and browsers get nothing from the pump.
    idle: bool,
}

struct Conn {
    envs: Envs,
    ctx: Option<Ctx>,
    user: String,
    remote: String,
    cons: Vec<Cons>,
    sent: u64,
    next: i32,
}

impl Conn {
    fn hdr(&mut self) -> Hdr {
        self.next += 1;
        Hdr { id: self.next, resp: false }
    }

    async fn send(&mut self, wr: &mut OwnedWriteHalf, mut d: Ds) -> Flow {
        if wr.write_all(&encode(&mut d)).await.is_err() {
            return Flow::Stop;
        }
        Flow::Go
    }

    async fn reply(&mut self, wr: &mut OwnedWriteHalf, corr: i32, resp: bool) -> Flow {
        if !resp {
            return Flow::Go;
        }
        let h = self.hdr();
        self.send(wr, Ds::Response(Response { h, correlation_id: corr })).await
    }

    async fn fail(&mut self, wr: &mut OwnedWriteHalf, corr: i32, class: &str, message: String) -> Flow {
        let h = self.hdr();
        self.send(wr, Ds::ExceptionResponse(ExceptionResponse { h, correlation_id: corr, error: Some(BErr { class: Some(class.into()), message: Some(message) }) })).await
    }

    fn dispatch(&mut self, i: usize, r: Option<&Row>) -> Ds {
        let c = &self.cons[i];
        let (consumer_id, destination) = (c.id.clone(), Some(Box::new(dest_ds(&c.dest))));
        let message = r.map(|r| {
            let hd: Map<String, Value> = serde_json::from_str(&r.headers).unwrap_or_default();
            let props: Map<String, Value> = serde_json::from_str(&r.props).unwrap_or_default();
            let compressed = hd.get("compressed").and_then(|v| v.as_bool()).unwrap_or(false);
            let s = |k: &str| hd.get(k).and_then(|v| v.as_str()).map(String::from);
            let id = mid(&r.message_id, r.seq);
            let m = Msg {
                producer_id: id.producer_id.clone(),
                destination: Some(Box::new(dest_ds(&r.dest))),
                message_id: Some(Box::new(Ds::MessageId(id))),
                group_id: s("groupId"),
                group_seq: hd.get("groupSeq").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                correlation_id: s("correlationId"),
                persistent: hd.get("persistent").and_then(|v| v.as_bool()).unwrap_or(true),
                expiration: hd.get("expiration").and_then(|v| v.as_i64()).unwrap_or(0),
                priority: hd.get("priority").and_then(|v| v.as_u64()).unwrap_or(4) as u8,
                reply_to: s("replyTo").map(|d| Box::new(dest_ds(&d))),
                timestamp: hd.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(r.enqueued_at),
                kind: s("type"),
                content: Some(if r.kind == "text" && !compressed { text_content(&String::from_utf8_lossy(&r.body)) } else { r.body.clone() }),
                props: (!props.is_empty()).then(|| map_encode(&props)),
                compressed,
                redelivery: r.redelivered as i32,
                user_id: s("userId"),
                broker_in: r.enqueued_at,
                broker_out: util::now_ms(),
                ..Default::default()
            };
            Box::new(match r.kind.as_str() {
                "text" => Ds::TextMessage(m),
                "bytes" => Ds::BytesMessage(m),
                "map" => Ds::MapMessage(m),
                "object" => Ds::ObjectMessage(m),
                "stream" => Ds::StreamMessage(m),
                _ => Ds::Message(m),
            })
        });
        let redelivery = r.map(|r| r.redelivered as i32).unwrap_or(0);
        Ds::MessageDispatch(MessageDispatch { h: self.hdr(), consumer_id, destination, message, redelivery })
    }

    async fn handle(&mut self, wr: &mut OwnedWriteHalf, b: &[u8]) -> Flow {
        let typ = b[0];
        let (cid, resp) = if b.len() >= 6 { (i32::from_be_bytes([b[1], b[2], b[3], b[4]]), b[5] != 0) } else { (0, false) };
        let Some(d) = decode(b) else {
            eprintln!("ActiveMQ: {}: cannot read command type {typ}", self.remote);
            return if resp { self.fail(wr, cid, "java.io.IOException", format!("Stuntman's ActiveMQ does not implement OpenWire command type {typ}")).await } else { Flow::Go };
        };
        let connected = self.ctx.is_some();
        match d {
            Ds::ConnectionInfo(i) => {
                let Some(ctx) = self.envs.default_env() else { return self.fail(wr, cid, "javax.jms.JMSException", "No Stuntman environment".into()).await };
                let s = settings(&ctx).await;
                let (user, pass) = (i.user.unwrap_or_default(), i.password.unwrap_or_default());
                if s.refuse_login || (!s.users.is_empty() && s.users.get(&user) != Some(&pass)) {
                    return self.fail(wr, cid, "java.lang.SecurityException", format!("User name [{user}] or password is invalid.")).await;
                }
                self.ctx = Some(ctx);
                self.user = user;
                self.reply(wr, cid, resp).await
            }
            Ds::ShutdownInfo(_) => Flow::Stop,
            Ds::KeepAliveInfo(_) => {
                if resp {
                    let h = self.hdr();
                    return self.send(wr, Ds::KeepAliveInfo(Plain { h })).await;
                }
                Flow::Go
            }
            _ if !connected => {
                if resp {
                    self.fail(wr, cid, "javax.jms.IllegalStateException", "Send a ConnectionInfo first".into()).await
                } else {
                    Flow::Go
                }
            }
            Ds::ProducerInfo(p) => {
                if let (Some(ctx), Some(d)) = (&self.ctx, obj_dest(&p.destination)) {
                    register(ctx, &d, &self.user).await;
                }
                self.reply(wr, cid, resp).await
            }
            Ds::ConsumerInfo(i) => {
                let ctx = self.ctx.clone().unwrap();
                let key = id_key(&i.consumer_id);
                let dest = obj_dest(&i.destination).unwrap_or_default();
                let idle = i.browser || dest.is_empty() || dest.contains("ActiveMQ.Advisory") || dest.contains(',');
                register(&ctx, &dest, &self.user).await;
                self.cons.push(Cons { key: key.clone(), id: i.consumer_id.clone(), dest: dest.clone(), prefetch: i.prefetch, held: vec![], pull: None, idle });
                if !idle {
                    live().lock().unwrap().push(Live { env: ctx.env.to_string(), key, dest: dest.clone(), prefetch: i.prefetch, remote: self.remote.clone(), user: self.user.clone(), since: util::now_ms() });
                }
                if self.reply(wr, cid, resp).await == Flow::Stop {
                    return Flow::Stop;
                }
                if i.browser {
                    // A QueueBrowser: copies of what is pending, then the null dispatch that ends it.
                    let n = self.cons.len() - 1;
                    for r in pending_rows(&ctx, &dest).await {
                        let d = self.dispatch(n, Some(&r));
                        if self.send(wr, d).await == Flow::Stop {
                            return Flow::Stop;
                        }
                    }
                    let d = self.dispatch(n, None);
                    return self.send(wr, d).await;
                }
                Flow::Go
            }
            Ds::MessagePull(p) => {
                let key = id_key(&p.consumer_id);
                let now = util::now_ms();
                if let Some(c) = self.cons.iter_mut().find(|c| c.key == key) {
                    c.pull = Some(match p.timeout { t if t < 0 => -1, 0 => 0, t => now + t });
                }
                self.reply(wr, cid, resp).await
            }
            Ds::MessageAck(a) => {
                let ctx = self.ctx.clone().unwrap();
                let key = id_key(&a.consumer_id);
                let last = match a.last.as_deref() {
                    Some(Ds::MessageId(m)) => m.broker_seq,
                    _ => -1,
                };
                if let Some(c) = self.cons.iter_mut().find(|c| c.key == key) {
                    let at = c.held.iter().position(|e| e.0 == last).or_else(|| (a.count > 0 && !c.held.is_empty()).then(|| (a.count as usize).min(c.held.len()) - 1));
                    let done: Vec<i64> = match (a.ack_type, at) {
                        (_, None) | (3, _) => vec![],
                        (0, Some(i)) => {
                            c.held[..=i].iter_mut().for_each(|e| e.1 = true);
                            vec![]
                        }
                        (4, Some(i)) => vec![c.held.remove(i).0],
                        (_, Some(i)) => c.held.drain(..=i).map(|e| e.0).collect(),
                    };
                    settle(&ctx, done, if a.ack_type == 1 { "dead_letter" } else { "acked" }).await;
                }
                self.reply(wr, cid, resp).await
            }
            Ds::Message(m) | Ds::BytesMessage(m) | Ds::MapMessage(m) | Ds::ObjectMessage(m) | Ds::StreamMessage(m) | Ds::TextMessage(m) => {
                let kind = match typ {
                    24 => "bytes",
                    25 => "map",
                    26 => "object",
                    27 => "stream",
                    28 => "text",
                    _ => "message",
                };
                let Some(dest) = obj_dest(&m.destination) else { return self.fail(wr, cid, "javax.jms.InvalidDestinationException", "The message has no destination".into()).await };
                let content = m.content.unwrap_or_default();
                let body = if kind == "text" && !m.compressed { content_text(&content).into_bytes() } else { content };
                let mut hd = Map::new();
                let mut put = |k: &str, v: Value| {
                    if !v.is_null() && v != json!(false) {
                        hd.insert(k.into(), v);
                    }
                };
                put("correlationId", json!(m.correlation_id));
                put("type", json!(m.kind));
                put("replyTo", json!(obj_dest(&m.reply_to)));
                put("groupId", json!(m.group_id));
                put("userId", json!(m.user_id));
                put("compressed", json!(m.compressed));
                hd.insert("persistent".into(), json!(m.persistent));
                hd.insert("priority".into(), json!(m.priority));
                hd.insert("timestamp".into(), json!(m.timestamp));
                if m.expiration > 0 {
                    hd.insert("expiration".into(), json!(m.expiration));
                }
                if m.group_seq != 0 {
                    hd.insert("groupSeq".into(), json!(m.group_seq));
                }
                let n = NewMsg { dest, kind: kind.into(), body, props: m.props.as_deref().map(map_decode).unwrap_or_default(), headers: hd, message_id: mid_text(&m.message_id), sender: self.user.clone(), via: "openwire".into() };
                if let Err(e) = publish(self.ctx.as_ref().unwrap(), n).await {
                    return self.fail(wr, cid, "javax.jms.JMSException", e.message).await;
                }
                self.reply(wr, cid, resp).await
            }
            Ds::RemoveInfo(r) => {
                let key = id_key(&r.object_id);
                match r.object_id.as_deref() {
                    Some(Ds::ConsumerId(_)) => {
                        if let Some(i) = self.cons.iter().position(|c| c.key == key) {
                            self.drop_consumer(i, Some(r.last_delivered)).await;
                        }
                    }
                    Some(Ds::SessionId(_)) => {
                        while let Some(i) = self.cons.iter().position(|c| c.key.starts_with(&format!("{key}:"))) {
                            self.drop_consumer(i, Some(r.last_delivered)).await;
                        }
                    }
                    Some(Ds::ConnectionId(_)) => self.close().await,
                    _ => {}
                }
                self.reply(wr, cid, resp).await
            }
            Ds::TransactionInfo(_) => self.fail(wr, cid, "javax.jms.JMSException", "Stuntman's ActiveMQ does not implement transactions".into()).await,
            _ => self.reply(wr, cid, resp).await,
        }
    }

    async fn drop_consumer(&mut self, i: usize, last_delivered: Option<i64>) {
        let c = self.cons.remove(i);
        let Some(ctx) = self.ctx.clone() else { return };
        live().lock().unwrap().retain(|l| !(l.env == *ctx.env && l.key == c.key));
        release(&ctx, c.key, c.held.into_iter().map(|e| e.0).collect(), last_delivered, c.dest.starts_with("topic://")).await;
    }

    async fn close(&mut self) {
        while !self.cons.is_empty() {
            self.drop_consumer(0, None).await;
        }
    }

    /// Dispatches what each consumer has room for (its prefetch, or a pull), then answers pulls
    /// that timed out with an empty dispatch.
    async fn pump(&mut self, wr: &mut OwnedWriteHalf) -> Flow {
        let Some(ctx) = self.ctx.clone() else { return Flow::Go };
        let mut st: Option<Settings> = None;
        for i in 0..self.cons.len() {
            loop {
                let c = &self.cons[i];
                let room = if c.prefetch > 0 { c.held.iter().filter(|e| !e.1).count() < c.prefetch as usize } else { c.pull.is_some() };
                if c.idle || !room {
                    break;
                }
                let Some(r) = claim(&ctx, &c.dest, &c.key).await else { break };
                if st.is_none() {
                    st = Some(settings(&ctx).await);
                }
                let s = st.clone().unwrap();
                let c = &mut self.cons[i];
                c.held.push((r.seq, false));
                if c.prefetch <= 0 {
                    c.pull = None;
                }
                if s.slow_dispatch_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(s.slow_dispatch_ms)).await;
                }
                let d = self.dispatch(i, Some(&r));
                if self.send(wr, d).await == Flow::Stop {
                    return Flow::Stop;
                }
                ring(&ctx, &r.dest);
                self.sent += 1;
                if s.drop_after > 0 && self.sent >= s.drop_after {
                    eprintln!("ActiveMQ: {}: dropped after {} message(s) (set_activemq drop_after)", self.remote, self.sent);
                    return Flow::Stop;
                }
            }
        }
        let now = util::now_ms();
        for i in 0..self.cons.len() {
            if let Some(dl) = self.cons[i].pull {
                if dl < 0 || (dl > 0 && now >= dl) {
                    self.cons[i].pull = None;
                    let d = self.dispatch(i, None);
                    if self.send(wr, d).await == Flow::Stop {
                        return Flow::Stop;
                    }
                }
            }
        }
        Flow::Go
    }
}

async fn connection(envs: Envs, sock: TcpStream, remote: String) {
    let (mut rd, mut wr) = sock.into_split();
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
    let reader = tokio::spawn(async move {
        loop {
            let mut h = [0u8; 4];
            if rd.read_exact(&mut h).await.is_err() {
                break;
            }
            let n = u32::from_be_bytes(h) as usize;
            if n == 0 || n > MAX_FRAME {
                break;
            }
            let mut b = vec![0; n];
            if rd.read_exact(&mut b).await.is_err() || tx.send(b).await.is_err() {
                break;
            }
        }
    });
    let mut c = Conn { envs, ctx: None, user: String::new(), remote, cons: vec![], sent: 0, next: 0 };
    if c.send(&mut wr, wire_format()).await == Flow::Go {
        let first = tokio::time::timeout(Duration::from_secs(30), rx.recv()).await.ok().flatten();
        match first.as_deref().and_then(decode) {
            Some(Ds::WireFormatInfo(w)) if &w.magic == b"ActiveMQ" && w.version >= VERSION => {
                let h = c.hdr();
                let info = BrokerInfo { h, broker_id: Some(Box::new(Ds::BrokerId(StrId { value: Some("ID:stuntman-1".into()) }))), url: Some(format!("tcp://localhost:{}", port())), name: Some("stuntman".into()), ..Default::default() };
                if c.send(&mut wr, Ds::BrokerInfo(info)).await == Flow::Go {
                    run(&mut c, &mut wr, &mut rx).await;
                }
            }
            Some(Ds::WireFormatInfo(w)) => eprintln!("ActiveMQ: {}: OpenWire version {} (this broker speaks {VERSION} and up)", c.remote, w.version),
            _ => eprintln!("ActiveMQ: {}: not an OpenWire client", c.remote),
        }
    }
    c.close().await;
    reader.abort();
}

async fn run(c: &mut Conn, wr: &mut OwnedWriteHalf, rx: &mut mpsc::Receiver<Vec<u8>>) {
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    let mut keep = util::now_ms();
    loop {
        let b = bell(c.ctx.as_ref().map(|x| x.env.to_string()).unwrap_or_default().as_str());
        tokio::select! {
            f = rx.recv() => match f {
                Some(f) => if c.handle(wr, &f).await == Flow::Stop { return },
                None => return,
            },
            _ = b.notified() => {}
            _ = tick.tick() => {
                if util::now_ms() - keep > 10_000 {
                    keep = util::now_ms();
                    let h = c.hdr();
                    if c.send(wr, Ds::KeepAliveInfo(Plain { h })).await == Flow::Stop { return }
                }
            }
        }
        if c.pump(wr).await == Flow::Stop {
            return;
        }
    }
}

// ================================================================ tools

fn by(a: &stuntman_core::tools::Args) -> (String, String) {
    match a.str("_by").as_deref() {
        Some(b) if b.starts_with("mcp:") => (b[4..].to_string(), "mcp".into()),
        Some(b) if b.starts_with("api:") => (b[4..].to_string(), "api".into()),
        Some(b) if b != "you" => (b.to_string(), "you".into()),
        _ => ("you".into(), "you".into()),
    }
}

fn view(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let kind: String = r.get(3)?;
    let body: Vec<u8> = r.get(5)?;
    let mut v = json!({"seq": r.get::<_, i64>(0)?, "destination": r.get::<_, String>(1)?, "messageId": r.get::<_, String>(4)?, "kind": kind,
        "properties": serde_json::from_str::<Value>(&r.get::<_, String>(6)?).unwrap_or(json!({})), "headers": serde_json::from_str::<Value>(&r.get::<_, String>(7)?).unwrap_or(json!({})),
        "state": r.get::<_, String>(8)?, "consumer": r.get::<_, String>(9)?, "redeliveryCount": r.get::<_, i64>(10)?, "enqueuedAt": r.get::<_, i64>(11)?,
        "deliveredAt": r.get::<_, Option<i64>>(12)?, "ackedAt": r.get::<_, Option<i64>>(13)?, "sender": r.get::<_, String>(14)?, "via": r.get::<_, String>(15)?});
    match (kind.as_str(), String::from_utf8(body.clone())) {
        ("text", Ok(t)) => v["text"] = json!(t),
        _ => v["bodyBase64"] = json!(base64::engine::general_purpose::STANDARD.encode(body)),
    }
    if !r.get::<_, String>(2)?.is_empty() {
        v["subscriber"] = json!(r.get::<_, String>(2)?);
    }
    Ok(v)
}

async fn messages(ctx: &Ctx, dest: Option<String>, state: Option<String>, last: u32) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |c| {
            c.prepare("SELECT seq, dest, target, kind, message_id, body, props, headers, state, consumer, redelivered, enqueued_at, delivered_at, acked_at, sender, via FROM amq_messages WHERE (?1 IS NULL OR dest=?1) AND (?2 IS NULL OR state=?2) ORDER BY seq DESC LIMIT ?3")?
                .query_map(params![dest, state, last], view)?
                .collect()
        })
        .await
}

async fn counts(ctx: &Ctx, dest: &str) -> Map<String, Value> {
    let d = dest.to_string();
    let rows: Vec<(String, i64)> = ctx.db.call(move |c| c.prepare("SELECT state, count(*) FROM amq_messages WHERE dest=?1 GROUP BY state")?.query_map([d], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()).await.unwrap_or_default();
    let mut m: Map<String, Value> = STATES.iter().map(|s| (s.to_string(), json!(0))).collect();
    for (s, n) in rows {
        m.insert(s, json!(n));
    }
    m
}

fn dest_arg(a: &stuntman_core::tools::Args) -> Result<String, ApiError> {
    a.str("destination").or(a.str("queue")).map(|d| normalize(&d)).ok_or_else(|| ApiError::bad("“destination” is required: queue://<name>, topic://<name> or a queue name"))
}

fn settings_view(s: &Settings) -> Value {
    json!({"users": s.users.keys().collect::<Vec<_>>(), "anyUser": s.users.is_empty(), "refuseLogin": s.refuse_login, "dropAfter": s.drop_after, "slowDispatchMs": s.slow_dispatch_ms, "redeliver": s.redeliver})
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("list_activemq_destinations", "ActiveMQ (OpenWire, port 61616) queues and topics with their messages per state (pending, delivered, acked, dead_letter, discarded), the consumers connected now, the broker URL and what set_activemq changed.", |ctx, _a| async move {
            let names: Vec<String> = ctx.db.call(|c| c.prepare("SELECT name FROM amq_destinations UNION SELECT DISTINCT dest FROM amq_messages ORDER BY 1")?.query_map([], |r| r.get(0))?.collect()).await?;
            let env = ctx.env.to_string();
            let conns: Vec<Live> = live().lock().unwrap().iter().filter(|l| l.env == env).cloned().collect();
            let mut out = vec![];
            for n in names {
                let consumers: Vec<Value> = conns.iter().filter(|l| l.dest == n).map(|l| json!({"id": l.key, "prefetch": l.prefetch, "remote": l.remote, "user": l.user, "since": l.since})).collect();
                out.push(json!({"name": n, "kind": n.split("://").next().unwrap_or("queue"), "counts": counts(&ctx, &n).await, "consumers": consumers}));
            }
            Ok(json!({"destinations": out, "url": format!("activemq:tcp://localhost:{}", port()), "port": port(), "openwireVersion": VERSION,
                "bindError": BIND_ERROR.lock().unwrap().clone(), "settings": settings_view(&settings(&ctx).await)}))
        })
        .route("GET", "/activemq/destinations"),
        Tool::new("list_activemq_messages", "ActiveMQ messages, newest first, with their state: pending (waiting), delivered (dispatched, not acked yet), acked, dead_letter (poison ack), discarded (topic copy whose subscriber left).", |ctx, a| async move {
            let dest = a.str("destination").or(a.str("queue")).map(|d| normalize(&d));
            let state = a.str("state").filter(|s| s != "all");
            let rows = messages(&ctx, dest.clone(), state, a.u64("last").unwrap_or(50) as u32).await?;
            Ok(json!({"destination": dest, "counts": match &dest { Some(d) => Value::Object(counts(&ctx, d).await), None => Value::Null }, "messages": rows}))
        })
        .param("destination", "queue://<name>, topic://<name> or a queue name (empty: all)").param("state", "pending | delivered | acked | dead_letter | discarded").param("last", "default 50")
        .route("GET", "/activemq/messages"),
        Tool::new("activemq_send", "Puts a message on an ActiveMQ queue (or topic) as the partner system would; it is dispatched to the app's consumers. Recorded as sent by you or the agent.", |ctx, a| async move {
            let dest = dest_arg(&a)?;
            let kind = a.str("kind").unwrap_or("text".into());
            let body = match kind.as_str() {
                "text" => match a.get("text").or(a.get("body")) {
                    Some(Value::String(s)) => s.clone().into_bytes(),
                    Some(v) => v.to_string().into_bytes(),
                    None => vec![],
                },
                "bytes" => base64::engine::general_purpose::STANDARD.decode(a.str("body_base64").or(a.str("body")).unwrap_or_default()).map_err(|_| ApiError::bad("body_base64: not base64"))?,
                k => return Err(ApiError::bad(format!("kind “{k}”: text or bytes"))),
            };
            let mut headers = Map::new();
            for (k, h) in [("correlation_id", "correlationId"), ("type", "type"), ("group_id", "groupId")] {
                if let Some(v) = a.str(k) {
                    headers.insert(h.into(), json!(v));
                }
            }
            if let Some(r) = a.str("reply_to") {
                headers.insert("replyTo".into(), json!(normalize(&r)));
            }
            headers.insert("persistent".into(), json!(a.bool("persistent").unwrap_or(true)));
            headers.insert("priority".into(), json!(a.u64("priority").unwrap_or(4)));
            headers.insert("timestamp".into(), json!(util::now_ms()));
            let (sender, via) = by(&a);
            let props = match a.get("properties") {
                Some(Value::Object(o)) => o.clone(),
                Some(Value::String(s)) => serde_json::from_str(s).map_err(|_| ApiError::bad("properties: a JSON object"))?,
                _ => Map::new(),
            };
            let mut seqs = vec![];
            for _ in 0..a.u64("count").unwrap_or(1).clamp(1, 1000) {
                seqs.extend(publish(&ctx, NewMsg { dest: dest.clone(), kind: kind.clone(), body: body.clone(), props: props.clone(), headers: headers.clone(), message_id: String::new(), sender: sender.clone(), via: via.clone() }).await?);
            }
            Ok(json!({"destination": dest, "seqs": seqs, "counts": counts(&ctx, &dest).await}))
        })
        .req("destination", "queue://<name> (a bare name is a queue) or topic://<name>").param("text", "the TextMessage body (JSON is sent as its text)").param("kind", "text (default) | bytes")
        .param("body_base64", "kind=bytes: the BytesMessage body").param("properties", "{key: value} message properties").param("correlation_id", "JMSCorrelationID").param("type", "JMSType")
        .param("reply_to", "JMSReplyTo destination").param("group_id", "JMSXGroupID").param("persistent", "default true").param("priority", "0-9, default 4").param("count", "send this many copies (default 1)")
        .route("POST", "/activemq/messages"),
        Tool::new("set_activemq", "ActiveMQ broker behaviour: users (name → password; empty: anyone logs in), refuse_login (every login fails with a SecurityException), drop_after (close the connection after N dispatched messages), slow_dispatch (wait before each dispatch), redeliver (the next N acks are lost: those messages come again with a higher redelivery count). Only what you pass changes; reset=true goes back to defaults.", |ctx, a| async move {
            let mut s = if a.bool("reset").unwrap_or(false) { Settings::default() } else { settings(&ctx).await };
            match a.0.get("users") {
                Some(Value::Object(o)) => s.users = o.iter().map(|(k, v)| (k.clone(), v.as_str().map(String::from).unwrap_or(v.to_string()))).collect(),
                Some(Value::String(t)) => {
                    s.users = t.split(',').filter_map(|p| p.trim().split_once(':')).map(|(u, p)| (u.to_string(), p.to_string())).collect();
                    if !t.trim().is_empty() && s.users.is_empty() {
                        return Err(ApiError::bad("users: {\"name\": \"password\"} or \"name:password,other:password\""));
                    }
                }
                Some(Value::Null) | None => {}
                Some(_) => return Err(ApiError::bad("users: {\"name\": \"password\"} or \"name:password,other:password\"")),
            }
            if let Some(b) = a.bool("refuse_login") {
                s.refuse_login = b;
            }
            if let Some(Value::Number(_) | Value::String(_)) = a.0.get("drop_after") {
                s.drop_after = a.u64("drop_after").unwrap_or(0);
            }
            if a.0.contains_key("slow_dispatch") {
                s.slow_dispatch_ms = a.ms("slow_dispatch")?.unwrap_or(0);
            }
            if let Some(Value::Number(_) | Value::String(_)) = a.0.get("redeliver") {
                s.redeliver = a.u64("redeliver").unwrap_or(0);
            }
            save(&ctx, &s).await?;
            ctx.live.send("activemq.settings", "", "changed");
            Ok(json!({"settings": settings_view(&s)}))
        })
        .param("users", "{\"acme\": \"s3cret\"} or \"acme:s3cret,globex:pw\"; {} or \"\": anyone").param("refuse_login", "true: every login fails (java.lang.SecurityException)")
        .param("drop_after", "close the connection after this many dispatched messages; 0: never").param("slow_dispatch", "a delay like 2s before each dispatch; 0: none")
        .param("redeliver", "the next N acks are lost and those messages are delivered again").param("reset", "true: back to defaults")
        .route("PUT", "/activemq/settings"),
        Tool::new("wait_for_activemq_ack", "Waits until the app acked: one message (seq), at least `count` messages of the destination, or (neither) everything sent to it: nothing pending or delivered and at least one acked. 408 after the timeout.", |ctx, a| async move {
            let dest = dest_arg(&a)?;
            let seq = a.str("seq").map(|s| s.trim_start_matches('#').parse::<i64>().map_err(|_| ApiError::bad("seq: a number"))).transpose()?;
            let want = a.u64("count").map(|c| c as i64);
            let end = util::now_ms() + a.ms("timeout")?.unwrap_or(30_000) as i64;
            let b = bell(&ctx.env);
            loop {
                let n = b.notified();
                let c = counts(&ctx, &dest).await;
                let num = |k: &str| c[k].as_i64().unwrap_or(0);
                let done = match (seq, want) {
                    (Some(s), _) => ctx.db.call(move |db| db.query_row("SELECT state FROM amq_messages WHERE seq=?1", [s], |r| r.get::<_, String>(0)).optional()).await?.is_some_and(|st| st == "acked" || st == "dead_letter"),
                    (None, Some(w)) => num("acked") >= w,
                    (None, None) => num("pending") + num("delivered") == 0 && num("acked") > 0,
                };
                if done {
                    return Ok(json!({"destination": dest, "counts": c, "messages": messages(&ctx, Some(dest.clone()), None, 50).await?}));
                }
                if util::now_ms() > end {
                    return Err(ApiError::new(408, format!("{dest}: not acked yet")).with(json!({"counts": c})));
                }
                tokio::select! { _ = n => {} _ = tokio::time::sleep(Duration::from_millis(300)) => {} }
            }
        })
        .req("destination", "queue://<name> or a queue name").param("seq", "one message").param("count", "at least this many acked").param("timeout", "default 30s")
        .route("GET", "/activemq/wait-ack"),
    ]
}

// ================================================================ tests

#[cfg(test)]
mod t {
    use super::*;

    fn roundtrip(mut d: Ds) {
        let f = encode(&mut d);
        assert_eq!(u32::from_be_bytes(f[..4].try_into().unwrap()) as usize, f.len() - 4, "size prefix of {d:?}");
        assert_eq!(decode(&f[4..]).as_ref(), Some(&d), "round trip of type {}", d.typ());
    }

    fn cid() -> Obj {
        Some(Box::new(Ds::ConsumerId(ConsumerId { connection: Some("ID:acme-host-1234-1:1".into()), session: 1, value: 2 })))
    }

    fn pid() -> Obj {
        Some(Box::new(Ds::ProducerId(ProducerId { connection: Some("ID:acme-host-1234-1:1".into()), value: 3, session: 1 })))
    }

    fn q() -> Obj {
        Some(Box::new(dest_ds("queue://positions")))
    }

    fn h(resp: bool) -> Hdr {
        Hdr { id: 7, resp }
    }

    #[test]
    fn every_command_round_trips() {
        let mid = Some(Box::new(Ds::MessageId(MessageId { text: None, producer_id: pid(), producer_seq: 9, broker_seq: 42 })));
        let props = {
            let mut m = Map::new();
            m.insert("region".into(), json!("north"));
            m.insert("n".into(), json!(5));
            m
        };
        let msg = Msg { h: h(true), producer_id: pid(), destination: q(), message_id: mid.clone(), correlation_id: Some("c-1".into()), persistent: true, priority: 4, timestamp: 1_700_000_000_000,
            content: Some(text_content("olá, acme ✓ 🚚")), props: Some(map_encode(&props)), redelivery: 1, broker_path: Some(vec![Some(Box::new(Ds::BrokerId(StrId { value: Some("b1".into()) })))]),
            reply_to: Some(Box::new(dest_ds("topic://replies"))), ..Default::default() };
        for d in [
            wire_format(),
            Ds::BrokerInfo(BrokerInfo { h: h(false), broker_id: Some(Box::new(Ds::BrokerId(StrId { value: Some("ID:x".into()) }))), url: Some("tcp://localhost:61616".into()), name: Some("stuntman".into()), connection_id: 3, ..Default::default() }),
            Ds::ConnectionInfo(ConnectionInfo { h: h(true), connection_id: Some(Box::new(Ds::ConnectionId(StrId { value: Some("ID:c".into()) }))), client_id: Some("cli".into()), password: Some("s3cret".into()), user: Some("acme".into()), manageable: true, ..Default::default() }),
            Ds::SessionInfo(SessionInfo { h: h(false), session_id: Some(Box::new(Ds::SessionId(SessionId { connection: Some("ID:c".into()), value: 1 }))) }),
            Ds::ConsumerInfo(ConsumerInfo { h: h(true), consumer_id: cid(), destination: q(), prefetch: 1000, max_pending: -1, priority: 0, ..Default::default() }),
            Ds::ProducerInfo(ProducerInfo { h: h(true), producer_id: pid(), destination: q(), window: 0, ..Default::default() }),
            Ds::TransactionInfo(TransactionInfo { h: h(true), tx: Some(Box::new(Ds::LocalTxId(LocalTxId { value: 1, connection_id: None }))), kind: 0, ..Default::default() }),
            Ds::DestinationInfo(DestinationInfo { h: h(true), destination: Some(Box::new(dest_ds("temp-queue://ID:t"))), op: 0, timeout: 0, ..Default::default() }),
            Ds::KeepAliveInfo(Plain { h: h(true) }),
            Ds::ShutdownInfo(Plain { h: h(false) }),
            Ds::RemoveInfo(RemoveInfo { h: h(true), object_id: cid(), last_delivered: 42 }),
            Ds::ConnectionError(ConnectionError { h: h(false), error: Some(BErr { class: Some("java.lang.SecurityException".into()), message: Some("no".into()) }), connection_id: None }),
            Ds::MessagePull(MessagePull { h: h(false), consumer_id: cid(), destination: q(), timeout: 5000, ..Default::default() }),
            Ds::MessageDispatch(MessageDispatch { h: h(false), consumer_id: cid(), destination: q(), message: Some(Box::new(Ds::TextMessage(msg.clone()))), redelivery: 1 }),
            Ds::MessageDispatch(MessageDispatch { h: h(false), consumer_id: cid(), destination: q(), message: None, redelivery: 0 }),
            Ds::MessageAck(MessageAck { h: h(false), destination: q(), consumer_id: cid(), ack_type: 2, first: mid.clone(), last: mid.clone(), count: 1, ..Default::default() }),
            Ds::TextMessage(msg.clone()),
            Ds::BytesMessage(Msg { content: Some(vec![0, 1, 2, 255]), props: None, ..msg }),
            Ds::Response(Response { h: h(false), correlation_id: 7 }),
            Ds::ExceptionResponse(ExceptionResponse { h: h(false), correlation_id: 7, error: Some(BErr { class: Some("java.lang.SecurityException".into()), message: Some("User name [acme] or password is invalid.".into()) }) }),
        ] {
            roundtrip(d);
        }
    }

    #[test]
    fn known_bytes() {
        // A Response is: size 10, type 30, commandId 1, responseRequired false, correlationId 5.
        let f = encode(&mut Ds::Response(Response { h: Hdr { id: 1, resp: false }, correlation_id: 5 }));
        assert_eq!(f, vec![0, 0, 0, 10, 30, 0, 0, 0, 1, 0, 0, 0, 0, 5]);
        // A queue is a present flag, the type, a present flag and a short-prefixed name.
        let mut w = Writer::default();
        w.nested(&mut Some(Box::new(dest_ds("queue://q"))));
        assert_eq!(w.0, vec![1, 100, 1, 0, 1, b'q']);
        // WireFormatInfo has no BaseCommand header: the magic comes right after the type.
        let f = encode(&mut wire_format());
        assert_eq!(&f[4..13], b"\x01ActiveMQ");
        assert_eq!(&f[13..17], &VERSION.to_be_bytes());
        let Some(Ds::WireFormatInfo(w)) = decode(&f[4..]) else { panic!() };
        let p = map_decode(w.props.as_deref().unwrap());
        assert_eq!(p["TightEncodingEnabled"], json!(false));
        assert_eq!(p["MaxInactivityDuration"], json!(30_000));
        assert!(f.windows(25).any(|x| x == b"\x00\x15MaxInactivityDuration\x06\x00"), "MaxInactivityDuration goes as a long");
    }

    #[test]
    fn modified_utf8_and_maps() {
        for s in ["", "plain", "nul\0inside", "acentuação", "🚚 truck"] {
            assert_eq!(unmutf8(&mutf8(s)), s);
            assert_eq!(content_text(&text_content(s)), s);
        }
        assert_eq!(mutf8("\0"), vec![0xc0, 0x80]);
        assert_eq!(mutf8("🚚").len(), 6, "a surrogate pair is two 3-byte units");
        assert_eq!(unmutf8("🚚".as_bytes()), "🚚", "4-byte UTF-8 is read too");
        let v = json!({"s": "x", "b": true, "i": 7, "l": 1_700_000_000_000i64, "d": 1.5, "n": null, "list": [1, "a"], "map": {"k": "v"}, "big": "y".repeat(9000)});
        assert_eq!(Value::Object(map_decode(&map_encode(v.as_object().unwrap()))), v);
    }

    #[test]
    fn destinations_and_ids() {
        assert_eq!(normalize("positions"), "queue://positions");
        assert_eq!(normalize("topic://prices"), "topic://prices");
        let m = mid("ID:acme-host-1234-1:1:1:3:9", 42);
        assert_eq!((m.producer_seq, m.broker_seq), (9, 42));
        assert_eq!(mid_text(&Some(Box::new(Ds::MessageId(m)))), "ID:acme-host-1234-1:1:1:3:9");
        assert_eq!(id_key(&cid()), "ID:acme-host-1234-1:1:1:2");
    }

    #[test]
    fn short_or_unknown_frames_are_refused() {
        assert!(decode(&[30, 0, 0]).is_none());
        assert!(decode(&[99, 0, 0, 0, 0, 0]).is_none());
        // A byte sequence claiming more bytes than the frame has does not allocate them.
        let mut f = encode(&mut Ds::TextMessage(Msg { content: Some(vec![1, 2, 3]), ..Default::default() }));
        let at = f.windows(4).position(|x| x == [0, 0, 0, 3]).unwrap();
        f[at..at + 4].copy_from_slice(&i32::MAX.to_be_bytes());
        assert!(decode(&f[4..]).is_none());
    }
}
