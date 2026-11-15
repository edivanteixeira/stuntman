//! ActiveMQ "Classic" over OpenWire, on its own port (61616): what Apache.NMS.ActiveMQ and the
//! Java activemq-client speak. The broker offers OpenWire version 10 with loose encoding, no
//! marshal cache, size-prefixed frames and no stack traces; both clients take it (their versions
//! are 10 and 12, the lower one wins). Field orders follow the official v10 marshallers.
//!
//! Queues and topics live in the environment's SQLite. A message is scheduled (AMQ_SCHEDULED_DELAY
//! not over yet), pending, delivered (dispatched to a consumer, not acked yet), acked, dead_letter
//! (poison ack), expired (its JMSExpiration passed), rolled_back (sent in a transaction that rolled
//! back) or discarded (a topic copy whose subscriber left). Local and XA transactions hold sends
//! and acks until the commit; durable topic subscriptions keep copies while offline; consumers
//! take JMS selectors; message groups stick to one consumer; dispatch follows JMSPriority.
//! Compressed bodies are inflated on arrival. Not implemented: blob messages, the tight encoding,
//! the marshal cache, noLocal, exclusive consumers and the scheduler's repeat/cron properties.

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

use crate::selector::{self, E as Sel, V};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{tcp::OwnedWriteHalf, TcpListener, TcpStream},
    sync::{mpsc, Notify},
};

pub const VERSION: i32 = 10;
/// The marshal cache size this broker offers (the client's own may be smaller: the lower wins).
const CACHE_SIZE: i32 = 1024;
const MAX_FRAME: usize = 64 << 20;

// ================================================================ codec

/// One code path for both directions: a field reads itself from a Reader or writes itself to a
/// Writer, so decode(encode(x)) == x by construction.
pub trait Wire {
    fn reading(&self) -> bool;
    fn room(&self) -> usize;
    fn fail(&mut self);
    /// A boolean field: a byte in the loose encoding, a bit of the BooleanStream in the tight one.
    fn bit(&mut self, v: &mut bool);
    fn u8(&mut self, v: &mut u8);
    fn i16(&mut self, v: &mut i16);
    fn i32(&mut self, v: &mut i32);
    /// A long field: 8 bytes loose; tight, two bits say 0, 2, 4 or 8 bytes (unsigned).
    fn i64(&mut self, v: &mut i64);
    fn raw(&mut self, v: &mut [u8]);
    fn str(&mut self, v: &mut Option<String>);
    fn nested(&mut self, v: &mut Obj);
    /// A "cached" field (ids, destinations): with the marshal cache on, a new/known flag and a
    /// cache index come first. This broker always writes the object (a valid choice: the
    /// receiver stores it under the index), and reads both forms.
    fn cached(&mut self, v: &mut Obj);
}

pub trait Field {
    fn io<W: Wire>(&mut self, w: &mut W);
}

pub type Obj = Option<Box<Ds>>;
pub type Objs = Option<Vec<Obj>>;
/// Fields the v10 marshallers write with looseMarshalCachedObject / tightMarshalCachedObject.
pub type C = Obj;
type S = Option<String>;
type B = Option<Vec<u8>>;
type Throwable = Option<BErr>;

/// The negotiated wire format of a connection, and its marshal cache.
#[derive(Debug, Default)]
pub struct Fmt {
    pub tight: bool,
    pub cache: bool,
    pub cache_size: u16,
    next: u16,
    seen: HashMap<i16, Obj>,
}

impl Fmt {
    pub fn name(&self) -> String {
        format!("{}{}", if self.tight { "tight" } else { "loose" }, if self.cache { "+cache" } else { "" })
    }
}

/// Types whose nested form carries one more flag in the tight encoding (MarshallAware: messages
/// and WireFormatInfo).
fn marshal_aware(t: u8) -> bool {
    t == 1 || (23..=29).contains(&t)
}

impl Field for u8 {
    fn io<W: Wire>(&mut self, w: &mut W) {
        w.u8(self)
    }
}
impl Field for bool {
    fn io<W: Wire>(&mut self, w: &mut W) {
        w.bit(self)
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
impl Field for Throwable {
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
    bits: Vec<bool>,
    bp: usize,
    f: Option<&'a mut Fmt>,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Reader { b, p: 0, bad: false, bits: vec![], bp: 0, f: None }
    }
    fn tight(&self) -> bool {
        self.f.as_ref().is_some_and(|f| f.tight)
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
    /// The BooleanStream that starts a tight command: its size (one byte, or 0xC0 and a byte, or
    /// 0x80 and a short), then the bytes, least significant bit first.
    fn bool_stream(&mut self) {
        let mut n = 0u8;
        self.u8(&mut n);
        let len = match n {
            0xc0 => {
                let mut m = 0u8;
                self.u8(&mut m);
                m as usize
            }
            0x80 => {
                let mut m = 0i16;
                self.i16(&mut m);
                m as u16 as usize
            }
            n => n as usize,
        };
        self.bits = self.take(len).iter().flat_map(|b| (0..8).map(move |i| b >> i & 1 == 1)).collect();
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
    fn bit(&mut self, v: &mut bool) {
        if self.tight() {
            match self.bits.get(self.bp) {
                Some(b) => *v = *b,
                None => self.bad = true,
            }
            self.bp += 1;
        } else {
            let mut b = 0u8;
            self.u8(&mut b);
            *v = b != 0;
        }
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
        if !self.tight() {
            *v = self.take(8).try_into().map(i64::from_be_bytes).unwrap_or(0);
            return;
        }
        let (mut a, mut b) = (false, false);
        self.bit(&mut a);
        self.bit(&mut b);
        *v = match (a, b) {
            (false, false) => 0,
            (false, true) => self.take(2).try_into().map(u16::from_be_bytes).unwrap_or(0) as i64,
            (true, false) => self.take(4).try_into().map(u32::from_be_bytes).unwrap_or(0) as i64,
            (true, true) => self.take(8).try_into().map(i64::from_be_bytes).unwrap_or(0),
        };
    }
    fn raw(&mut self, v: &mut [u8]) {
        let s = self.take(v.len());
        if s.len() == v.len() {
            v.copy_from_slice(s)
        }
    }
    fn str(&mut self, v: &mut Option<String>) {
        let mut some = false;
        self.bit(&mut some);
        if some && self.tight() {
            // The "only ASCII" flag: both forms are a short length and the bytes.
            let mut ascii = false;
            self.bit(&mut ascii);
        }
        *v = if some { Some(self.utf()) } else { None };
    }
    fn nested(&mut self, v: &mut Obj) {
        let mut some = false;
        self.bit(&mut some);
        *v = None;
        if !some {
            return;
        }
        let mut t = 0u8;
        self.u8(&mut t);
        if self.tight() && marshal_aware(t) {
            // A cached marshalled form: Java never sends one.
            let mut cached = false;
            self.bit(&mut cached);
            if cached {
                return self.fail();
            }
        }
        match Ds::new(t) {
            Some(mut d) => {
                d.io(self);
                *v = Some(Box::new(d));
            }
            None => self.bad = true,
        }
    }
    fn cached(&mut self, v: &mut Obj) {
        if !self.f.as_ref().is_some_and(|f| f.cache) {
            return self.nested(v);
        }
        let mut new = false;
        self.bit(&mut new);
        let mut i = 0i16;
        self.i16(&mut i);
        if new {
            self.nested(v);
            let o = v.clone();
            if let Some(f) = self.f.as_mut() {
                f.seen.insert(i, o);
            }
        } else {
            match self.f.as_ref().and_then(|f| f.seen.get(&i)) {
                Some(o) => *v = o.clone(),
                None => self.bad = true,
            }
        }
    }
}

#[derive(Default)]
pub struct Writer<'a>(pub Vec<u8>, Vec<bool>, Option<&'a mut Fmt>);

impl Writer<'_> {
    fn tight(&self) -> bool {
        self.2.as_ref().is_some_and(|f| f.tight)
    }
    fn utf(&mut self, s: &str) {
        let b = mutf8(s);
        let n = b.len().min(u16::MAX as usize);
        self.0.extend((n as u16).to_be_bytes());
        self.0.extend(&b[..n]);
    }
}

/// A BooleanStream: its byte count (see Reader::bool_stream), then the bits, LSB first.
fn bool_stream(bits: &[bool]) -> Vec<u8> {
    let n = bits.len().div_ceil(8);
    let mut o = match n {
        0..64 => vec![n as u8],
        64..256 => vec![0xc0, n as u8],
        _ => vec![0x80, (n >> 8) as u8, n as u8],
    };
    o.extend(bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |a, (i, b)| a | ((*b as u8) << i))));
    o
}

impl Wire for Writer<'_> {
    fn reading(&self) -> bool {
        false
    }
    fn room(&self) -> usize {
        usize::MAX
    }
    fn fail(&mut self) {}
    fn bit(&mut self, v: &mut bool) {
        if self.tight() {
            self.1.push(*v)
        } else {
            self.0.push(*v as u8)
        }
    }
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
        if !self.tight() {
            return self.0.extend(v.to_be_bytes());
        }
        let x = *v as u64;
        let (a, b) = if x == 0 { (false, false) } else if x >> 16 == 0 { (false, true) } else if x >> 32 == 0 { (true, false) } else { (true, true) };
        self.1.extend([a, b]);
        match (a, b) {
            (false, false) => {}
            (false, true) => self.0.extend((x as u16).to_be_bytes()),
            (true, false) => self.0.extend((x as u32).to_be_bytes()),
            (true, true) => self.0.extend(x.to_be_bytes()),
        }
    }
    fn raw(&mut self, v: &mut [u8]) {
        self.0.extend_from_slice(v)
    }
    fn str(&mut self, v: &mut Option<String>) {
        let mut some = v.is_some();
        self.bit(&mut some);
        if let Some(s) = v {
            if self.tight() {
                self.1.push(s.encode_utf16().all(|c| (1..=0x7f).contains(&c)));
            }
            self.utf(s);
        }
    }
    fn nested(&mut self, v: &mut Obj) {
        let mut some = v.is_some();
        self.bit(&mut some);
        if let Some(d) = v {
            if self.tight() && marshal_aware(d.typ()) {
                self.1.push(false);
            }
            self.0.push(d.typ());
            d.io(self);
        }
    }
    fn cached(&mut self, v: &mut Obj) {
        let Some(f) = self.2.as_mut().filter(|f| f.cache) else { return self.nested(v) };
        let mut i = (f.next % f.cache_size.max(1)) as i16;
        f.next = f.next.wrapping_add(1) % f.cache_size.max(1);
        let mut new = true;
        self.bit(&mut new);
        self.i16(&mut i);
        self.nested(v);
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

macro_rules! fio {
    ($w:ident, $e:expr, C) => {
        $w.cached(&mut $e)
    };
    ($w:ident, $e:expr, $t:tt) => {
        $e.io($w)
    };
}

macro_rules! fields {
    ($($name:ident { $($f:ident : $t:tt),* $(,)? })*) => {$(
        #[derive(Clone, Debug, Default, PartialEq)]
        pub struct $name { $(pub $f: $t),* }
        impl Field for $name {
            #[allow(unused_variables)]
            fn io<W: Wire>(&mut self, w: &mut W) { $(fio!(w, self.$f, $t);)* }
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
            pub fn name(&self) -> &'static str { match self { $(Ds::$var(_) => stringify!($var)),* } }
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
    BrokerInfo { h: Hdr, broker_id: C, url: S, peers: Objs, name: S, slave: bool, master: bool, fault_tolerant: bool, duplex: bool, network: bool, connection_id: i64, upload_url: S, network_props: S }
    ConnectionInfo { h: Hdr, connection_id: C, client_id: S, password: S, user: S, broker_path: Objs, broker_master: bool, manageable: bool, client_master: bool, fault_tolerant: bool, failover_reconnect: bool, client_ip: S }
    SessionInfo { h: Hdr, session_id: C }
    ConsumerInfo { h: Hdr, consumer_id: C, browser: bool, destination: C, prefetch: i32, max_pending: i32, dispatch_async: bool, selector: S, client_id: S, subscription: S,
        no_local: bool, exclusive: bool, retroactive: bool, priority: u8, broker_path: Objs, predicate: Obj, network_subscription: bool, optimized_ack: bool, no_range_acks: bool, network_consumer_path: Objs }
    ProducerInfo { h: Hdr, producer_id: C, destination: C, broker_path: Objs, dispatch_async: bool, window: i32 }
    TransactionInfo { h: Hdr, connection_id: C, tx: C, kind: u8 }
    DestinationInfo { h: Hdr, connection_id: C, destination: C, op: u8, timeout: i64, broker_path: Objs }
    RemoveSubscriptionInfo { h: Hdr, connection_id: C, subscription: S, client_id: S }
    RemoveInfo { h: Hdr, object_id: C, last_delivered: i64 }
    ControlCommand { h: Hdr, command: S }
    ConnectionError { h: Hdr, error: Throwable, connection_id: Obj }
    ConsumerControl { h: Hdr, destination: Obj, close: bool, consumer_id: Obj, prefetch: i32, flush: bool, start: bool, stop: bool }
    ConnectionControl { h: Hdr, close: bool, exit: bool, fault_tolerant: bool, resume: bool, suspend: bool, connected_brokers: S, reconnect_to: S, rebalance: bool, token: B }
    ProducerAck { h: Hdr, producer_id: Obj, size: i32 }
    MessagePull { h: Hdr, consumer_id: C, destination: C, timeout: i64, correlation_id: S, message_id: Obj }
    MessageDispatch { h: Hdr, consumer_id: C, destination: C, message: Obj, redelivery: i32 }
    MessageAck { h: Hdr, destination: C, tx: C, consumer_id: C, ack_type: u8, first: Obj, last: Obj, count: i32, poison: Throwable }
    Msg { h: Hdr, producer_id: C, destination: C, tx: C, original_destination: C, message_id: Obj, original_tx: C, group_id: S, group_seq: i32, correlation_id: S,
        persistent: bool, expiration: i64, priority: u8, reply_to: Obj, timestamp: i64, kind: S, content: B, props: B, data_structure: Obj, target_consumer: C, compressed: bool,
        redelivery: i32, broker_path: Objs, arrival: i64, user_id: S, df_bridge: bool, droppable: bool, cluster: Objs, broker_in: i64, broker_out: i64, group_first: bool }
    BlobMsg { m: Msg, url: S, mime: S, deleted: bool }
    Response { h: Hdr, correlation_id: i32 }
    ExceptionResponse { h: Hdr, correlation_id: i32, error: Throwable }
    IntegerResponse { h: Hdr, correlation_id: i32, result: i32 }
    DataArrayResponse { h: Hdr, correlation_id: i32, data: Objs }
    Dest { name: S }
    MessageId { text: S, producer_id: C, producer_seq: i64, broker_seq: i64 }
    LocalTxId { value: i64, connection_id: C }
    XaTxId { format_id: i32, gtid: B, bq: B }
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
    Message = 23: Msg, BytesMessage = 24: Msg, MapMessage = 25: Msg, ObjectMessage = 26: Msg, StreamMessage = 27: Msg, TextMessage = 28: Msg, BlobMessage = 29: BlobMsg,
    Response = 30: Response, ExceptionResponse = 31: ExceptionResponse, DataArrayResponse = 33: DataArrayResponse, IntegerResponse = 34: IntegerResponse,
    Queue = 100: Dest, Topic = 101: Dest, TempQueue = 102: Dest, TempTopic = 103: Dest,
    MessageId = 110: MessageId, LocalTxId = 111: LocalTxId, XaTxId = 112: XaTxId, ConnectionId = 120: StrId, SessionId = 121: SessionId, ConsumerId = 122: ConsumerId, ProducerId = 123: ProducerId, BrokerId = 124: StrId,
}

/// A frame: int size (of what follows), the type byte, the fields (loose encoding).
pub fn encode(d: &mut Ds) -> Vec<u8> {
    encode_with(d, None)
}

/// A frame in the connection's format: tight puts the BooleanStream between the type and the data.
pub fn encode_with(d: &mut Ds, f: Option<&mut Fmt>) -> Vec<u8> {
    let mut w = Writer(vec![], vec![], f);
    d.io(&mut w);
    let mut o = vec![0, 0, 0, 0, d.typ()];
    if w.tight() {
        o.extend(bool_stream(&w.1));
    }
    o.extend(w.0);
    let n = (o.len() - 4) as u32;
    o[..4].copy_from_slice(&n.to_be_bytes());
    o
}

/// A frame without its size prefix. Trailing bytes (fields of a later version) are ignored.
pub fn decode(b: &[u8]) -> Option<Ds> {
    decode_with(b, None)
}

pub fn decode_with(b: &[u8], f: Option<&mut Fmt>) -> Option<Ds> {
    let (&t, rest) = b.split_first()?;
    let mut d = Ds::new(t)?;
    let mut r = Reader { f, ..Reader::new(rest) };
    if r.tight() {
        r.bool_stream();
    }
    d.io(&mut r);
    (!r.bad).then_some(d)
}

/// The command id and responseRequired of a frame this broker could not read, to answer it.
fn peek(b: &[u8], tight: bool) -> (i32, bool) {
    let mut r = Reader::new(b.get(1..).unwrap_or_default());
    let (mut id, mut resp) = (0, false);
    if tight {
        r.bool_stream();
        resp = r.bits.first().copied().unwrap_or(false);
        r.i32(&mut id);
    } else {
        r.i32(&mut id);
        r.bit(&mut resp);
    }
    (id, resp)
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
    let mut w = Writer(vec![], vec![], None);
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
  sender TEXT NOT NULL DEFAULT '', via TEXT NOT NULL DEFAULT '', tx TEXT NOT NULL DEFAULT '', priority INTEGER NOT NULL DEFAULT 4,
  deliver_at INTEGER NOT NULL DEFAULT 0, expires_at INTEGER NOT NULL DEFAULT 0, grp TEXT NOT NULL DEFAULT '', origin TEXT NOT NULL DEFAULT '',
  job TEXT NOT NULL DEFAULT '', moved_to INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS amq_pending ON amq_messages(dest, state, seq);
CREATE TABLE IF NOT EXISTS amq_destinations(name TEXT PRIMARY KEY, created_at INTEGER NOT NULL, created_by TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS amq_settings(id INTEGER PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS amq_durable(id TEXT PRIMARY KEY, client_id TEXT NOT NULL, name TEXT NOT NULL, dest TEXT NOT NULL, selector TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL, last_seen INTEGER NOT NULL, no_local TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS amq_outbox(id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, seq INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS amq_tx(id TEXT PRIMARY KEY, kind TEXT NOT NULL, state TEXT NOT NULL, xa_format INTEGER, xa_gtid BLOB, xa_bq BLOB,
  conn TEXT NOT NULL DEFAULT '', user TEXT NOT NULL DEFAULT '', sends INTEGER NOT NULL DEFAULT 0, acks INTEGER NOT NULL DEFAULT 0, started_at INTEGER NOT NULL, ended_at INTEGER);
";

/// Columns added after 0.5.0: an environment created by 0.5.0 gets them here (on newer ones the
/// ALTER fails and is ignored), then the indexes that use them.
pub fn upgrade(db: &stuntman_core::store::Db) {
    for col in ["tx TEXT NOT NULL DEFAULT ''", "priority INTEGER NOT NULL DEFAULT 4", "deliver_at INTEGER NOT NULL DEFAULT 0", "expires_at INTEGER NOT NULL DEFAULT 0", "grp TEXT NOT NULL DEFAULT ''", "origin TEXT NOT NULL DEFAULT ''", "job TEXT NOT NULL DEFAULT ''", "moved_to INTEGER NOT NULL DEFAULT 0"] {
        let _ = db.migrate(&format!("ALTER TABLE amq_messages ADD COLUMN {col}"));
    }
    let _ = db.migrate("ALTER TABLE amq_durable ADD COLUMN no_local TEXT NOT NULL DEFAULT ''");
    let _ = db.migrate("CREATE INDEX IF NOT EXISTS amq_ready ON amq_messages(dest, state, priority DESC, seq); CREATE INDEX IF NOT EXISTS amq_timed ON amq_messages(state, deliver_at); CREATE INDEX IF NOT EXISTS amq_txs ON amq_messages(tx)");
}

pub const STATES: [&str; 8] = ["scheduled", "pending", "delivered", "acked", "dead_letter", "expired", "rolled_back", "discarded"];

/// The shared dead-letter queue, as ActiveMQ names it.
pub const DLQ: &str = "queue://ActiveMQ.DLQ";

fn dlq_for(dest: &str, individual: bool) -> String {
    match dest.split_once("://") {
        Some((scheme, name)) if individual => format!("queue://ActiveMQ.DLQ.{}.{name}", if scheme.contains("topic") { "Topic" } else { "Queue" }),
        _ => DLQ.into(),
    }
}

/// Settles one message as dead_letter or expired, ActiveMQ's way: a copy goes to the dead-letter
/// queue (with dlqDeliveryFailureCause and its original destination) and the original points at
/// it. An expired non-persistent message is only dropped; one already on a DLQ stays there.
fn dead_letter(c: &rusqlite::Connection, seq: i64, state: &str, cause: &str, individual: bool, now: i64) -> rusqlite::Result<Option<String>> {
    let Some((dest, headers, props)) = c.query_row("SELECT dest, headers, props FROM amq_messages WHERE seq=?1", [seq], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))).optional()? else {
        return Ok(None);
    };
    let mut hd: Map<String, Value> = serde_json::from_str(&headers).unwrap_or_default();
    let persistent = hd.get("persistent").and_then(|v| v.as_bool()).unwrap_or(true);
    if state == "expired" {
        outbox(c, "Expired", seq)?;
    }
    if dest.contains("ActiveMQ.DLQ") || (state == "expired" && !persistent) {
        c.execute("UPDATE amq_messages SET state=?2, acked_at=?3, tx='' WHERE seq=?1", params![seq, state, now])?;
        return Ok(None);
    }
    outbox(c, "MessageDLQd", seq)?;
    let to = dlq_for(&dest, individual);
    let mut pr: Map<String, Value> = serde_json::from_str(&props).unwrap_or_default();
    pr.insert("dlqDeliveryFailureCause".into(), json!(cause));
    hd.insert("originalDestination".into(), json!(dest));
    hd.insert("originalSeq".into(), json!(seq));
    hd.remove("expiration");
    c.execute(
        "INSERT INTO amq_messages(dest, target, kind, message_id, body, props, headers, state, enqueued_at, sender, via, priority, grp, origin)
         SELECT ?2, '', kind, message_id, body, ?3, ?4, 'pending', ?5, sender, via, priority, grp, origin FROM amq_messages WHERE seq=?1",
        params![seq, to, Value::Object(pr).to_string(), Value::Object(hd).to_string(), now],
    )?;
    let copy = c.last_insert_rowid();
    c.execute("UPDATE amq_messages SET state=?2, acked_at=?3, tx='', moved_to=?4 WHERE seq=?1", params![seq, state, now, copy])?;
    c.execute("INSERT OR IGNORE INTO amq_destinations(name, created_at, created_by) VALUES(?1, ?2, 'broker')", params![to, now])?;
    Ok(Some(to))
}

/// After a redelivery: past the redelivery limit, the message goes to the dead-letter queue.
fn over_limit(c: &rusqlite::Connection, seq: i64, s: &Settings, now: i64) -> rusqlite::Result<()> {
    let max = s.max_redeliveries();
    let n: i64 = c.query_row("SELECT redelivered FROM amq_messages WHERE seq=?1", [seq], |r| r.get(0)).optional()?.unwrap_or(0);
    if max >= 0 && n > max {
        dead_letter(c, seq, "dead_letter", &format!("Delivery[{}] exceeds redelivery policy limit: maximumRedeliveries = {max}", n + 1), s.individual_dlq, now)?;
    }
    Ok(())
}

/// A repeating or cron schedule (AMQ_SCHEDULED_PERIOD / _REPEAT / _CRON).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Job {
    pub period: i64,
    pub repeat: i64,
    /// Deliveries still due in the current series.
    pub left: i64,
    pub cron: String,
    pub delay: i64,
}

/// What set_activemq changes. Empty users: any user name and password log in.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub users: BTreeMap<String, String>,
    pub refuse_login: bool,
    pub drop_after: u64,
    pub slow_dispatch_ms: u64,
    pub redeliver: u64,
    pub fail_commits: u64,
    /// Deliveries after the first before a message goes to the dead-letter queue (None: 6, as
    /// ActiveMQ's redelivery policy; -1: never).
    pub max_redeliveries: Option<i64>,
    /// ActiveMQ.DLQ.Queue.<name> / ActiveMQ.DLQ.Topic.<name> instead of the shared ActiveMQ.DLQ.
    pub individual_dlq: bool,
    /// ActiveMQ.Advisory.MessageDelivered.* / MessageConsumed.* (off by default, as in ActiveMQ).
    pub advisory_delivered: bool,
    pub advisory_consumed: bool,
}

impl Settings {
    pub fn max_redeliveries(&self) -> i64 {
        self.max_redeliveries.unwrap_or(6)
    }
}

fn settings_sync(c: &rusqlite::Connection) -> Settings {
    c.query_row("SELECT v FROM amq_settings WHERE id=1", [], |r| r.get::<_, String>(0)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub async fn settings(ctx: &Ctx) -> Settings {
    ctx.db.call(|c| Ok(settings_sync(c))).await.unwrap_or_default()
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

/// Consumers connected now, for the tools and topic fan-out. `target` is the consumer itself, or
/// the durable subscription it reads ("durable:<clientId>:<name>").
#[derive(Clone, Debug)]
struct Live {
    env: String,
    key: String,
    target: String,
    durable: Option<String>,
    dest: String,
    selector: String,
    sel: Option<Arc<Sel>>,
    prefetch: i32,
    remote: String,
    user: String,
    since: i64,
    /// noLocal: not the messages its own connection (this id) sent.
    no_local: Option<String>,
    exclusive: bool,
    /// ConsumerInfo.priority (consumer.priority): higher ones get queue messages first.
    priority: i32,
    /// It has prefetch room (or a pull waiting) now: kept up to date by its connection's pump.
    room: bool,
    /// The negotiated wire format: "loose", "loose+cache", "tight" or "tight+cache".
    wire: String,
}

/// Connections logged in now: env, remote, user, client id, wire format, since.
fn conns() -> &'static Mutex<Vec<(String, String, String, String, String, i64)>> {
    static C: OnceLock<Mutex<Vec<(String, String, String, String, String, i64)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn live() -> &'static Mutex<Vec<Live>> {
    static L: OnceLock<Mutex<Vec<Live>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// Message groups: "env \0 destination \0 JMSXGroupID" → the consumer that owns the group.
fn groups() -> &'static Mutex<HashMap<String, String>> {
    static G: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    G.get_or_init(Default::default)
}

fn durable_id(client: &str, name: &str) -> String {
    format!("durable:{client}:{name}")
}

/// A header field or property, as a selector sees it.
fn field(k: &str, hd: &Map<String, Value>, props: &Map<String, Value>, message_id: &str, redelivered: i64) -> V {
    let j = |v: Option<&Value>| match v {
        Some(Value::String(s)) => V::Str(s.clone()),
        Some(Value::Bool(b)) => V::Bool(*b),
        Some(Value::Number(n)) => n.as_i64().map(V::Int).unwrap_or(V::Float(n.as_f64().unwrap_or(0.0))),
        _ => V::Null,
    };
    match k {
        "JMSType" => j(hd.get("type")),
        "JMSCorrelationID" => j(hd.get("correlationId")),
        "JMSPriority" => V::Int(hd.get("priority").and_then(|v| v.as_i64()).unwrap_or(4)),
        "JMSDeliveryMode" => V::Str(if hd.get("persistent").and_then(|v| v.as_bool()).unwrap_or(true) { "PERSISTENT" } else { "NON_PERSISTENT" }.into()),
        "JMSTimestamp" => j(hd.get("timestamp")),
        "JMSExpiration" => V::Int(hd.get("expiration").and_then(|v| v.as_i64()).unwrap_or(0)),
        "JMSMessageID" => V::Str(message_id.into()),
        "JMSRedelivered" => V::Bool(redelivered > 0),
        "JMSXDeliveryCount" => V::Int(redelivered + 1),
        "JMSXGroupID" => j(hd.get("groupId")),
        "JMSXGroupSeq" => j(hd.get("groupSeq")),
        "JMSXUserID" => j(hd.get("userId")),
        _ => j(props.get(k)),
    }
}

fn selects(s: &Sel, hd: &Map<String, Value>, props: &Map<String, Value>, message_id: &str, redelivered: i64) -> bool {
    s.matches(&|k| field(k, hd, props, message_id, redelivered))
}

fn row_selects(s: &Sel, r: &Row) -> bool {
    let hd: Map<String, Value> = serde_json::from_str(&r.headers).unwrap_or_default();
    let props: Map<String, Value> = serde_json::from_str(&r.props).unwrap_or_default();
    selects(s, &hd, &props, &r.message_id, r.redelivered)
}

/// Time-driven changes, on the environment's clock (`now`): scheduled messages whose time came
/// become pending, repeating and cron schedules put a copy out each time, and pending messages past
/// their JMSExpiration expire (to the dead-letter queue when persistent).
fn promote(c: &rusqlite::Connection, now: i64) -> rusqlite::Result<usize> {
    let mut n = c.execute("UPDATE amq_messages SET state='pending' WHERE state='scheduled' AND job='' AND deliver_at<=?1", [now])?;
    let due: Vec<(i64, String, i64)> = c.prepare("SELECT seq, job, deliver_at FROM amq_messages WHERE state='scheduled' AND job<>'' AND deliver_at<=?1")?.query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
    for (seq, job, mut at) in due {
        let mut j: Job = serde_json::from_str(&job).unwrap_or_default();
        let cron = (!j.cron.is_empty()).then(|| crate::cron::parse(&j.cron).ok()).flatten();
        // ponytail: catches up at most 100 missed deliveries per sweep (a clock moved far ahead).
        for _ in 0..100 {
            if at > now {
                break;
            }
            n += 1;
            if cron.is_none() && j.left <= 0 {
                // The last one of a plain repeat is the scheduled message itself.
                c.execute("UPDATE amq_messages SET state='pending', job='' WHERE seq=?1", [seq])?;
                at = i64::MAX;
                break;
            }
            c.execute(
                "INSERT INTO amq_messages(dest, target, kind, message_id, body, props, headers, state, enqueued_at, sender, via, priority, expires_at, grp, origin)
                 SELECT dest, target, kind, '', body, props, headers, 'pending', ?2, sender, via, priority, expires_at, grp, origin FROM amq_messages WHERE seq=?1",
                params![seq, util::now_ms()],
            )?;
            let copy = c.last_insert_rowid();
            c.execute("UPDATE amq_messages SET message_id=?1 WHERE seq=?2", params![format!("ID:stuntman-{}-0:0:0:{copy}", port()), copy])?;
            if j.left > 0 {
                j.left -= 1;
                at += j.period.max(0);
            } else if let Some(cr) = &cron {
                j.left = j.repeat.max(0);
                match cr.next(at) {
                    Some(t) => at = t + j.delay,
                    None => {
                        c.execute("UPDATE amq_messages SET state='discarded', job='' WHERE seq=?1", [seq])?;
                        at = i64::MAX;
                        break;
                    }
                }
            }
        }
        if at != i64::MAX {
            c.execute("UPDATE amq_messages SET deliver_at=?2, job=?3 WHERE seq=?1", params![seq, at, serde_json::to_string(&j).unwrap_or_default()])?;
        }
    }
    let expired: Vec<i64> = c.prepare("SELECT seq FROM amq_messages WHERE state='pending' AND expires_at>0 AND expires_at<=?1")?.query_map([now], |r| r.get(0))?.collect::<Result<_, _>>()?;
    if !expired.is_empty() {
        let s = settings_sync(c);
        for seq in &expired {
            dead_letter(c, *seq, "expired", "The message expired", s.individual_dlq, util::now_ms())?;
        }
    }
    Ok(n + expired.len())
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

/// A transaction's key: "local:<connection>:<n>" or "xa:<formatId>:<gtid hex>:<bqual hex>".
fn tx_key(o: &Obj) -> Option<String> {
    match o.as_deref()? {
        Ds::LocalTxId(l) => Some(format!("local:{}:{}", id_key(&l.connection_id), l.value)),
        Ds::XaTxId(x) => Some(format!("xa:{}:{}:{}", x.format_id, hex::encode(x.gtid.as_deref().unwrap_or_default()), hex::encode(x.bq.as_deref().unwrap_or_default()))),
        _ => None,
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

/// A compressed body (Java's Deflater: zlib, NMS 1.x: gzip; a Java BytesMessage puts the plain length first).
pub fn inflate(kind: &str, c: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut srcs = vec![c];
    if kind == "bytes" && c.len() > 4 && !c.starts_with(&[0x1f, 0x8b]) {
        srcs.insert(0, &c[4..]);
    }
    for s in srcs {
        let mut o = vec![];
        // Apache.NMS.ActiveMQ 1.x writes gzip.
        if s.starts_with(&[0x1f, 0x8b]) && flate2::read::GzDecoder::new(s).read_to_end(&mut o).is_ok() {
            return Some(o);
        }
        o.clear();
        if flate2::read::ZlibDecoder::new(s).read_to_end(&mut o).is_ok() {
            return Some(o);
        }
        o.clear();
        if flate2::read::DeflateDecoder::new(s).read_to_end(&mut o).is_ok() && !o.is_empty() {
            return Some(o);
        }
    }
    None
}

async fn register(ctx: &Ctx, dest: &str, by: &str) {
    if dest.is_empty() || dest.contains("ActiveMQ.Advisory") || dest.starts_with("temp-") {
        return;
    }
    let (d, by, now) = (dest.to_string(), by.to_string(), util::now_ms());
    if ctx.db.call(move |c| c.execute("INSERT OR IGNORE INTO amq_destinations(name, created_at, created_by) VALUES(?1, ?2, ?3)", params![d, now, by])).await.unwrap_or(0) > 0 {
        // Boxed: the advisory goes through publish, which registers (advisory topics are not).
        Box::pin(destination_advisory(ctx, dest, 0)).await;
    }
}

/// ActiveMQ.Advisory.Queue / .Topic: a DestinationInfo (op 0 added, 1 removed).
async fn destination_advisory(ctx: &Ctx, dest: &str, op: u8) {
    let t = if dest.starts_with("topic://") { "Topic" } else { "Queue" };
    advise(ctx, format!("{ADV}{t}"), Ds::DestinationInfo(DestinationInfo { destination: Some(Box::new(dest_ds(dest))), op, ..Default::default() }), Map::new()).await;
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
    /// The transaction it was sent in: invisible until the commit.
    pub tx: String,
    /// The sending connection's id (noLocal subscribers skip it); empty for the tools.
    pub origin: String,
    /// Delivered now even with AMQ_SCHEDULED_* properties (the scheduler's BROWSE copies).
    pub immediate: bool,
}

/// Puts a message on a queue, or a copy for each subscriber of a topic whose selector takes it:
/// the consumers connected now and the durable subscriptions (none: kept as discarded). With
/// AMQ_SCHEDULED_DELAY it waits as scheduled.
pub async fn publish(ctx: &Ctx, m: NewMsg) -> Result<Vec<i64>, ApiError> {
    register(ctx, &m.dest, &m.sender).await;
    let topic = m.dest.starts_with("topic://");
    let mut targets: Vec<String> = vec![];
    if topic {
        let env = ctx.env.to_string();
        let takes = |s: &Option<Arc<Sel>>| s.as_ref().is_none_or(|s| selects(s, &m.headers, &m.props, &m.message_id, 0));
        let mine = |nl: Option<&str>| !m.origin.is_empty() && nl == Some(m.origin.as_str());
        targets = live().lock().unwrap().iter().filter(|l| l.env == env && topic_match(&l.dest, &m.dest) && l.durable.is_none() && takes(&l.sel) && !mine(l.no_local.as_deref())).map(|l| l.key.clone()).collect();
        let subs: Vec<(String, String, String, String)> = ctx.db.call(move |c| c.prepare("SELECT id, selector, no_local, dest FROM amq_durable ORDER BY id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect()).await?;
        for (id, s, nl, d) in subs {
            if !topic_match(&d, &m.dest) || mine(Some(nl.as_str())) {
                continue;
            }
            if s.is_empty() || selector::parse(&s).is_ok_and(|e| takes(&Some(Arc::new(e)))) {
                targets.push(id);
            }
        }
    }
    if targets.is_empty() {
        if is_advisory(&m.dest) {
            // Nobody listens: an advisory is not kept.
            return Ok(vec![]);
        }
        targets.push(String::new());
    }
    let now = util::now_ms();
    let clock = stuntman_core::clock::now(ctx);
    let num = |k: &str| m.props.get(k).and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())).unwrap_or(0).max(0);
    let delay = if m.immediate { 0 } else { num("AMQ_SCHEDULED_DELAY") };
    let job = if m.immediate {
        Job::default()
    } else {
        Job { period: num("AMQ_SCHEDULED_PERIOD"), repeat: num("AMQ_SCHEDULED_REPEAT"), left: num("AMQ_SCHEDULED_REPEAT"), cron: m.props.get("AMQ_SCHEDULED_CRON").and_then(|v| v.as_str()).unwrap_or("").trim().to_string(), delay }
    };
    let (deliver_at, job) = if !job.cron.is_empty() {
        let c = crate::cron::parse(&job.cron).map_err(|e| ApiError::bad(format!("AMQ_SCHEDULED_CRON: {e}")))?;
        // The first delivery of a series is at the cron time; the repeats follow every period.
        let first = c.next(clock).ok_or_else(|| ApiError::bad(format!("AMQ_SCHEDULED_CRON “{}” never fires", job.cron)))?;
        (first + delay, serde_json::to_string(&job).unwrap_or_default())
    } else if job.repeat > 0 {
        (clock + delay, serde_json::to_string(&job).unwrap_or_default())
    } else {
        (if delay > 0 { clock + delay } else { 0 }, String::new())
    };
    let priority = m.headers.get("priority").and_then(|v| v.as_i64()).unwrap_or(4);
    let expires = m.headers.get("expiration").and_then(|v| v.as_i64()).unwrap_or(0).max(0);
    let grp = m.headers.get("groupId").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let dest = m.dest.clone();
    let seqs = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let mut seqs = vec![];
            for t in targets {
                let state = if topic && t.is_empty() { "discarded" } else if deliver_at > 0 || !job.is_empty() { "scheduled" } else { "pending" };
                tx.execute(
                    "INSERT INTO amq_messages(dest, target, kind, message_id, body, props, headers, state, enqueued_at, sender, via, tx, priority, deliver_at, expires_at, grp, origin, job) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
                    params![m.dest, t, m.kind, m.message_id, m.body, Value::Object(m.props.clone()).to_string(), Value::Object(m.headers.clone()).to_string(), state, now, m.sender, m.via, m.tx, priority, deliver_at, expires, grp, m.origin, job],
                )?;
                let seq = tx.last_insert_rowid();
                if m.message_id.is_empty() {
                    tx.execute("UPDATE amq_messages SET message_id=?1 WHERE seq=?2", params![format!("ID:stuntman-{}-0:0:0:{seq}", port()), seq])?;
                }
                seqs.push(seq);
            }
            if !m.tx.is_empty() {
                tx.execute("UPDATE amq_tx SET sends=sends+1 WHERE id=?1", [&m.tx])?;
            }
            tx.commit()?;
            Ok(seqs)
        })
        .await?;
    ring(ctx, &dest);
    Ok(seqs)
}

/// A stored message as the OpenWire message it was (body left out for advisory payloads).
fn row_message(r: &Row, body: bool) -> Ds {
    let hd: Map<String, Value> = serde_json::from_str(&r.headers).unwrap_or_default();
    let props: Map<String, Value> = serde_json::from_str(&r.props).unwrap_or_default();
    let compressed = hd.get("compressed").and_then(|v| v.as_bool()).unwrap_or(false);
    let s = |k: &str| hd.get(k).and_then(|v| v.as_str()).map(String::from);
    let id = mid(&r.message_id, r.seq);
    // An advisory carries its subject (ConnectionInfo, ConsumerInfo…) as the message's data structure.
    let mut data_structure: Obj = None;
    if let Some(b) = s("dataStructure").and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok()) {
        Reader::new(&b).nested(&mut data_structure);
    }
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
        original_destination: s("originalDestination").map(|d| Box::new(dest_ds(&d))),
        timestamp: hd.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(r.enqueued_at),
        kind: s("type"),
        content: body.then(|| if r.kind == "text" && !compressed { text_content(&String::from_utf8_lossy(&r.body)) } else { r.body.clone() }),
        props: (!props.is_empty()).then(|| map_encode(&props)),
        data_structure,
        compressed,
        redelivery: r.redelivered as i32,
        user_id: s("userId"),
        broker_in: r.enqueued_at,
        broker_out: util::now_ms(),
        ..Default::default()
    };
    match r.kind.as_str() {
        "text" => Ds::TextMessage(m),
        "bytes" => Ds::BytesMessage(m),
        "map" => Ds::MapMessage(m),
        "object" => Ds::ObjectMessage(m),
        "stream" => Ds::StreamMessage(m),
        "blob" => Ds::BlobMessage(BlobMsg { m, url: s("blobUrl"), mime: s("blobMime"), deleted: false }),
        _ => Ds::Message(m),
    }
}

async fn load_rows(ctx: &Ctx, seqs: Vec<i64>) -> Vec<Row> {
    ctx.db
        .call(move |c| {
            let mut out = vec![];
            for s in seqs {
                if let Some(r) = c.query_row(&format!("SELECT {ROW} FROM amq_messages WHERE seq=?1"), [s], row).optional()? {
                    out.push(r);
                }
            }
            Ok(out)
        })
        .await
        .unwrap_or_default()
}

// ---------------------------------------------------------------- advisories

const ADV: &str = "topic://ActiveMQ.Advisory.";

fn is_advisory(d: &str) -> bool {
    d.contains("ActiveMQ.Advisory.")
}

/// "Queue.orders", "Topic.prices", "TempQueue.ID:…": how advisory topic names spell a destination.
fn adv_name(dest: &str) -> String {
    let (scheme, name) = dest.split_once("://").unwrap_or(("queue", dest));
    let t = match scheme {
        "topic" => "Topic",
        "temp-queue" => "TempQueue",
        "temp-topic" => "TempTopic",
        _ => "Queue",
    };
    format!("{t}.{name}")
}

/// Topic wildcards: `*` one segment, `>` the rest (ActiveMQ's syntax). Without them, equality.
pub fn topic_match(pattern: &str, name: &str) -> bool {
    if !pattern.contains(['*', '>']) {
        return pattern == name;
    }
    let (Some((ps, p)), Some((ns, n))) = (pattern.split_once("://"), name.split_once("://")) else { return false };
    if ps != ns {
        return false;
    }
    let (p, n): (Vec<&str>, Vec<&str>) = (p.split('.').collect(), n.split('.').collect());
    for (i, seg) in p.iter().enumerate() {
        match *seg {
            ">" => return n.len() > i,
            "*" if i < n.len() => {}
            s if n.get(i) == Some(&s) => {}
            _ => return false,
        }
    }
    p.len() == n.len()
}

/// Someone listens on this advisory topic (a consumer connected now): only then is one made.
fn adv_wanted(env: &str, topic: &str) -> bool {
    live().lock().unwrap().iter().any(|l| l.env == env && is_advisory(&l.dest) && topic_match(&l.dest, topic))
}

/// Publishes an advisory: an ActiveMQMessage of type "Advisory" whose data structure is `ds`.
async fn advise(ctx: &Ctx, topic: String, mut ds: Ds, extra: Map<String, Value>) {
    if !adv_wanted(&ctx.env, &topic) {
        return;
    }
    let mut w = Writer::default();
    w.nested(&mut Some(Box::new(ds.clone())));
    let mut props = Map::new();
    props.insert("originBrokerId".into(), json!("ID:stuntman-1"));
    props.insert("originBrokerName".into(), json!("stuntman"));
    props.insert("originBrokerURL".into(), json!(format!("tcp://localhost:{}", port())));
    props.extend(extra);
    let mut headers = Map::new();
    headers.insert("type".into(), json!("Advisory"));
    headers.insert("persistent".into(), json!(false));
    headers.insert("priority".into(), json!(4));
    headers.insert("timestamp".into(), json!(util::now_ms()));
    headers.insert("advisory".into(), json!(ds.name()));
    headers.insert("dataStructure".into(), json!(base64::engine::general_purpose::STANDARD.encode(&w.0)));
    let _ = &mut ds;
    let _ = publish(ctx, NewMsg { dest: topic, kind: "message".into(), props, headers, sender: "broker".into(), via: "advisory".into(), ..Default::default() }).await;
}

/// Message advisories (Expired, MessageDLQd, MessageDelivered, MessageConsumed): the message
/// without its body, and its id as orignalMessageId (ActiveMQ's spelling).
async fn advise_rows(ctx: &Ctx, kind: &str, seqs: Vec<i64>) {
    if !live().lock().unwrap().iter().any(|l| l.env == *ctx.env && is_advisory(&l.dest)) {
        return;
    }
    for r in load_rows(ctx, seqs).await {
        let mut extra = Map::new();
        extra.insert("orignalMessageId".into(), json!(r.message_id));
        extra.insert("orignalDestination".into(), json!(r.dest));
        advise(ctx, format!("{ADV}{kind}.{}", adv_name(&r.dest)), row_message(&r, false), extra).await;
    }
}

/// Expired and MessageDLQd events noted by the synchronous store code (amq_outbox), sent now.
async fn flush_outbox(ctx: &Ctx) {
    let ev: Vec<(String, i64)> = ctx
        .db
        .call(|c| {
            let v: Vec<(String, i64)> = c.prepare("SELECT kind, seq FROM amq_outbox ORDER BY id")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            if !v.is_empty() {
                c.execute("DELETE FROM amq_outbox", [])?;
            }
            Ok(v)
        })
        .await
        .unwrap_or_default();
    for (k, seq) in ev {
        advise_rows(ctx, &k, vec![seq]).await;
    }
}

fn outbox(c: &rusqlite::Connection, kind: &str, seq: i64) -> rusqlite::Result<()> {
    if live().lock().unwrap().iter().any(|l| is_advisory(&l.dest)) {
        c.execute("INSERT INTO amq_outbox(kind, seq) VALUES(?1, ?2)", params![kind, seq])?;
    }
    Ok(())
}

/// Producers with a destination, connected now: env, producer key, destination.
fn producers() -> &'static Mutex<Vec<(String, String, String)>> {
    static P: OnceLock<Mutex<Vec<(String, String, String)>>> = OnceLock::new();
    P.get_or_init(Default::default)
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

fn group_key(env: &str, dest: &str, grp: &str) -> String {
    format!("{env}\0{dest}\0{grp}")
}

/// Marks the next pending message of a destination as delivered to this consumer: highest
/// JMSPriority first, then oldest; one its selector takes; not of a group another consumer owns.
/// `higher`: the selectors of higher-priority consumers with prefetch room (a message one of them
/// takes is left for it).
async fn claim(ctx: &Ctx, dest: &str, target: &str, consumer: &str, sel: Option<Arc<Sel>>, no_local: Option<String>, higher: Vec<Arc<Sel>>) -> Option<Row> {
    let topic = dest.starts_with("topic://");
    let (d, t, k, env, clock) = (dest.to_string(), target.to_string(), consumer.to_string(), ctx.env.to_string(), stuntman_core::clock::now(ctx));
    let (moved, got) = ctx
        .db
        .call(move |c| {
            let moved = promote(c, clock)?;
            let mut gs = groups().lock().unwrap();
            let pick = {
                // ponytail: scans pending rows in Rust (selectors, groups); an SQL prefilter when queues hold many thousands.
                // A topic subscriber reads its own copies (whatever topic a wildcard matched).
                let mut st = c.prepare(&format!(
                    "SELECT {ROW}, grp FROM amq_messages WHERE state='pending' AND tx='' AND ((?4 AND target=?2) OR (NOT ?4 AND dest=?1 AND target IN ('', ?2))) AND (?3 IS NULL OR origin<>?3) ORDER BY priority DESC, seq"
                ))?;
                let mut rows = st.query(params![d, t, no_local, topic])?;
                let mut pick = None;
                while let Some(r) = rows.next()? {
                    let grp: String = r.get(9)?;
                    if !grp.is_empty() && gs.get(&group_key(&env, &d, &grp)).is_some_and(|o| *o != k) {
                        continue;
                    }
                    let x = row(r)?;
                    if sel.as_ref().is_none_or(|s| row_selects(s, &x)) && !higher.iter().any(|h| row_selects(h, &x)) {
                        pick = Some((x, grp));
                        break;
                    }
                }
                pick
            };
            let Some((x, grp)) = pick else { return Ok((moved, None)) };
            c.execute("UPDATE amq_messages SET state='delivered', consumer=?2, delivered_at=?3 WHERE seq=?1 AND state='pending'", params![x.seq, k, util::now_ms()])?;
            if !grp.is_empty() {
                let gk = group_key(&env, &d, &grp);
                // JMSXGroupSeq -1 closes the group: the next message of it may go to anyone.
                if serde_json::from_str::<Value>(&x.headers).ok().and_then(|h| h["groupSeq"].as_i64()) == Some(-1) {
                    gs.remove(&gk);
                } else {
                    gs.entry(gk).or_insert(k);
                }
            }
            Ok((moved, Some(x)))
        })
        .await
        .ok()?;
    if moved > 0 {
        ring(ctx, dest);
    }
    got
}

async fn pending_rows(ctx: &Ctx, dest: &str, sel: Option<Arc<Sel>>) -> Vec<Row> {
    let (d, clock) = (dest.to_string(), stuntman_core::clock::now(ctx));
    let rows: Vec<Row> = ctx
        .db
        .call(move |c| {
            promote(c, clock)?;
            c.prepare(&format!("SELECT {ROW} FROM amq_messages WHERE dest=?1 AND state='pending' AND tx='' AND target='' ORDER BY priority DESC, seq LIMIT 1000"))?.query_map([d], row)?.collect()
        })
        .await
        .unwrap_or_default();
    rows.into_iter().filter(|r| sel.as_ref().is_none_or(|s| row_selects(s, r))).collect()
}

/// Settles messages: acked (or, while the redeliver provocation lasts, back to pending), dead_letter or expired.
async fn settle(ctx: &Ctx, seqs: Vec<i64>, state: &'static str, cause: String) {
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
            let s = settings_sync(&tx);
            for (i, seq) in seqs.iter().enumerate() {
                if i < again {
                    if tx.execute("UPDATE amq_messages SET state='pending', consumer='', delivered_at=NULL, redelivered=redelivered+1 WHERE seq=?1 AND state='delivered'", [seq])? > 0 {
                        over_limit(&tx, *seq, &s, now)?;
                    }
                } else if state == "acked" {
                    tx.execute("UPDATE amq_messages SET state=?2, acked_at=?3 WHERE seq=?1 AND state='delivered'", params![seq, state, now])?;
                } else if tx.query_row("SELECT state FROM amq_messages WHERE seq=?1", [seq], |r| r.get::<_, String>(0)).optional()?.as_deref() == Some("delivered") {
                    dead_letter(&tx, *seq, state, &cause, s.individual_dlq, now)?;
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
/// had seen it), except what a prepared transaction acked; a plain topic subscriber's copies are
/// discarded (a durable subscription keeps them).
async fn release(ctx: &Ctx, key: String, held: Vec<i64>, last_delivered: Option<i64>, discard: bool) {
    let _ = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            if discard {
                tx.execute("UPDATE amq_messages SET state='discarded' WHERE target=?1 AND (state IN ('pending','scheduled') OR (state='delivered' AND tx=''))", [&key])?;
            }
            let (s, now) = (settings_sync(&tx), util::now_ms());
            for seq in held {
                let bump = last_delivered.map(|l| seq <= l).unwrap_or(true) as i64;
                if tx.execute("UPDATE amq_messages SET state='pending', consumer='', delivered_at=NULL, redelivered=redelivered+?2 WHERE seq=?1 AND state='delivered' AND tx=''", params![seq, bump])? > 0 && bump > 0 {
                    over_limit(&tx, seq, &s, now)?;
                }
            }
            tx.commit()
        })
        .await;
    ring(ctx, "");
}

// ---------------------------------------------------------------- scheduler management

/// A message to topic://ActiveMQ.Scheduler.Management: AMQ_SCHEDULER_ACTION BROWSE (each scheduled
/// message, with scheduledJobId, to the JMSReplyTo), REMOVE (scheduledJobId) or REMOVEALL, BROWSE
/// and REMOVEALL limited by ACTION_START_TIME / ACTION_END_TIME (next delivery time, ms, on the
/// environment clock). A job's id is its message id. Returns how many jobs it touched.
pub async fn scheduler_management(ctx: &Ctx, props: &Map<String, Value>, reply_to: Option<String>) -> Result<usize, ApiError> {
    let s = |k: &str| props.get(k).and_then(|v| v.as_str()).map(String::from);
    let n = |k: &str| props.get(k).and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()));
    let (start, end) = (n("ACTION_START_TIME"), n("ACTION_END_TIME"));
    let action = s("AMQ_SCHEDULER_ACTION").unwrap_or_default();
    match action.as_str() {
        "BROWSE" => {
            let to = reply_to.ok_or_else(|| ApiError::bad("AMQ_SCHEDULER_ACTION BROWSE needs a JMSReplyTo"))?;
            let rows: Vec<(String, Row)> = ctx
                .db
                .call(move |c| {
                    c.prepare(&format!("SELECT {ROW}, job FROM amq_messages WHERE state='scheduled' AND (?1 IS NULL OR deliver_at>=?1) AND (?2 IS NULL OR deliver_at<=?2) ORDER BY deliver_at, seq"))?
                        .query_map(params![start, end], |r| Ok((r.get::<_, String>(9)?, row(r)?)))?
                        .collect()
                })
                .await?;
            let count = rows.len();
            for (_, r) in rows {
                let mut p: Map<String, Value> = serde_json::from_str(&r.props).unwrap_or_default();
                p.insert("scheduledJobId".into(), json!(r.message_id));
                let headers: Map<String, Value> = serde_json::from_str(&r.headers).unwrap_or_default();
                publish(ctx, NewMsg { dest: to.clone(), kind: r.kind, body: r.body, props: p, headers, message_id: r.message_id, sender: "broker".into(), via: "scheduler".into(), immediate: true, ..Default::default() }).await?;
            }
            Ok(count)
        }
        "REMOVE" | "REMOVEALL" => {
            let id = s("scheduledJobId");
            if action == "REMOVE" && id.is_none() {
                return Err(ApiError::bad("AMQ_SCHEDULER_ACTION REMOVE needs scheduledJobId"));
            }
            let n = ctx
                .db
                .call(move |c| {
                    c.execute(
                        "UPDATE amq_messages SET state='discarded', job='' WHERE state='scheduled' AND (?1 IS NULL OR message_id=?1) AND (?2 IS NULL OR deliver_at>=?2) AND (?3 IS NULL OR deliver_at<=?3)",
                        params![id, start, end],
                    )
                })
                .await?;
            ring(ctx, "");
            Ok(n)
        }
        a => Err(ApiError::bad(format!("AMQ_SCHEDULER_ACTION “{a}”: BROWSE, REMOVE or REMOVEALL"))),
    }
}

// ---------------------------------------------------------------- durable subscriptions

/// Creates or reuses a durable subscription. A different topic or selector replaces it, and what
/// it kept is discarded (ActiveMQ's rule).
async fn durable_open(ctx: &Ctx, id: &str, client: &str, name: &str, dest: &str, selector: &str, no_local: &str) {
    let (id, client, name, dest, sel, now, nl) = (id.to_string(), client.to_string(), name.to_string(), dest.to_string(), selector.to_string(), util::now_ms(), no_local.to_string());
    let _ = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let old: Option<(String, String)> = tx.query_row("SELECT dest, selector FROM amq_durable WHERE id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            if old.as_ref().is_some_and(|(d, s)| *d != dest || *s != sel) {
                tx.execute("UPDATE amq_messages SET state='discarded' WHERE target=?1 AND state IN ('pending','scheduled')", [&id])?;
            }
            tx.execute(
                "INSERT INTO amq_durable(id, client_id, name, dest, selector, created_at, last_seen, no_local) VALUES(?1,?2,?3,?4,?5,?6,?6,?7)
                 ON CONFLICT(id) DO UPDATE SET dest=?4, selector=?5, last_seen=?6, no_local=?7",
                params![id, client, name, dest, sel, now, nl],
            )?;
            tx.commit()
        })
        .await;
}

/// RemoveSubscriptionInfo (unsubscribe): false when there was none.
async fn durable_remove(ctx: &Ctx, id: &str) -> bool {
    let id = id.to_string();
    ctx.db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute("UPDATE amq_messages SET state='discarded' WHERE target=?1 AND state IN ('pending','scheduled','delivered') AND tx=''", [&id])?;
            let n = tx.execute("DELETE FROM amq_durable WHERE id=?1", [&id])?;
            tx.commit()?;
            Ok(n > 0)
        })
        .await
        .unwrap_or(false)
}

async fn durable_seen(ctx: &Ctx, id: &str) {
    let (id, now) = (id.to_string(), util::now_ms());
    let _ = ctx.db.call(move |c| c.execute("UPDATE amq_durable SET last_seen=?2 WHERE id=?1", params![id, now])).await;
}

// ---------------------------------------------------------------- transactions

/// Starts a transaction (BEGIN, or the first send or ack in it); a known one is left as it is.
async fn tx_open(ctx: &Ctx, id: &str, xid: Option<XaTxId>, conn: &str, user: &str) {
    let (id, conn, user, now) = (id.to_string(), conn.to_string(), user.to_string(), util::now_ms());
    let kind = if xid.is_some() { "xa" } else { "local" };
    let x = xid.unwrap_or_default();
    let _ = ctx
        .db
        .call(move |c| {
            c.execute(
                "INSERT INTO amq_tx(id, kind, state, xa_format, xa_gtid, xa_bq, conn, user, started_at) VALUES(?1,?2,'active',?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(id) DO UPDATE SET state='active', conn=?6, sends=0, acks=0, started_at=?8, ended_at=NULL WHERE state IN ('committed','rolled_back')",
                params![id, kind, x.format_id, x.gtid, x.bq, conn, user, now],
            )
        })
        .await;
}

async fn tx_state(ctx: &Ctx, id: &str) -> Option<(String, i64)> {
    let id = id.to_string();
    ctx.db.call(move |c| c.query_row("SELECT state, sends+acks FROM amq_tx WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()).await.ok().flatten()
}

async fn tx_set(ctx: &Ctx, id: &str, state: &'static str) {
    let (id, now) = (id.to_string(), util::now_ms());
    let ended = matches!(state, "committed" | "rolled_back").then_some(now);
    let _ = ctx.db.call(move |c| c.execute("UPDATE amq_tx SET state=?2, ended_at=?3 WHERE id=?1", params![id, state, ended])).await;
}

/// A transacted ack: the messages stay delivered, bound to the transaction, until its outcome.
async fn tx_ack(ctx: &Ctx, id: &str, seqs: Vec<i64>) {
    let id = id.to_string();
    let _ = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let mut n = 0;
            for s in seqs {
                n += tx.execute("UPDATE amq_messages SET tx=?1 WHERE seq=?2 AND state='delivered' AND tx=''", params![id, s])?;
            }
            tx.execute("UPDATE amq_tx SET acks=acks+?2 WHERE id=?1", params![id, n as i64])?;
            tx.commit()
        })
        .await;
}

/// Commit: its sends become visible (or scheduled), its acks take effect. Returns the acked seqs.
async fn tx_commit(ctx: &Ctx, id: &str) -> Vec<i64> {
    let (id, now) = (id.to_string(), util::now_ms());
    let acked = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let acked: Vec<i64> = tx.prepare("SELECT seq FROM amq_messages WHERE tx=?1 AND state='delivered'")?.query_map([&id], |r| r.get(0))?.collect::<Result<_, _>>()?;
            tx.execute("UPDATE amq_messages SET state='acked', acked_at=?2, tx='' WHERE tx=?1 AND state='delivered'", params![id, now])?;
            tx.execute("UPDATE amq_messages SET tx='' WHERE tx=?1", [&id])?;
            tx.execute("UPDATE amq_tx SET state='committed', ended_at=?2 WHERE id=?1", params![id, now])?;
            tx.commit()?;
            Ok(acked)
        })
        .await
        .unwrap_or_default();
    ring(ctx, "");
    acked
}

/// Rollback: its sends are dropped (rolled_back); what it acked is the consumer's again, to be
/// redelivered by the client (one more redelivery), or pending when that consumer is gone.
async fn tx_rollback(ctx: &Ctx, id: &str, bump: bool) {
    let (id, now) = (id.to_string(), util::now_ms());
    let env = ctx.env.to_string();
    let here: Vec<String> = live().lock().unwrap().iter().filter(|l| l.env == env).map(|l| l.key.clone()).collect();
    let _ = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute("UPDATE amq_messages SET state='rolled_back', tx='', acked_at=?2 WHERE tx=?1 AND state IN ('pending','scheduled')", params![id, now])?;
            let held: Vec<(i64, String)> = tx.prepare("SELECT seq, consumer FROM amq_messages WHERE tx=?1 AND state='delivered'")?.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            let s = settings_sync(&tx);
            for (seq, consumer) in held {
                let gone = !here.contains(&consumer);
                tx.execute(
                    "UPDATE amq_messages SET tx='', redelivered=redelivered+?2, state=CASE WHEN ?3 THEN 'pending' ELSE state END, consumer=CASE WHEN ?3 THEN '' ELSE consumer END WHERE seq=?1",
                    params![seq, bump as i64, gone],
                )?;
                if gone && bump {
                    over_limit(&tx, seq, &s, now)?;
                }
            }
            tx.execute("UPDATE amq_messages SET tx='' WHERE tx=?1", [&id])?;
            tx.execute("UPDATE amq_tx SET state='rolled_back', ended_at=?2 WHERE id=?1", params![id, now])?;
            tx.commit()
        })
        .await;
    ring(ctx, "");
}

/// XA RECOVER: the prepared transactions, from any connection.
async fn tx_recover(ctx: &Ctx) -> Vec<XaTxId> {
    ctx.db
        .call(|c| {
            c.prepare("SELECT xa_format, xa_gtid, xa_bq FROM amq_tx WHERE kind='xa' AND state='prepared' ORDER BY started_at")?
                .query_map([], |r| Ok(XaTxId { format_id: r.get::<_, Option<i32>>(0)?.unwrap_or(0), gtid: r.get(1)?, bq: r.get(2)? }))?
                .collect()
        })
        .await
        .unwrap_or_default()
}

/// The connection is gone: what it left open (not prepared) rolls back.
async fn tx_abandon(ctx: &Ctx, conn: &str) {
    let c2 = conn.to_string();
    let open: Vec<String> = ctx.db.call(move |c| c.prepare("SELECT id FROM amq_tx WHERE conn=?1 AND state IN ('active','ended')")?.query_map([c2], |r| r.get(0))?.collect()).await.unwrap_or_default();
    for id in open {
        tx_rollback(ctx, &id, false).await;
    }
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
    let mut w = Writer(11i32.to_be_bytes().to_vec(), vec![], None);
    // As a real broker: tight encoding and the marshal cache are offered; each side's AND decides.
    for (k, v) in [("TightEncodingEnabled", true), ("CacheEnabled", true), ("SizePrefixDisabled", false), ("StackTraceEnabled", false), ("TcpNoDelayEnabled", true)] {
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
    w.0.push(5);
    w.0.extend(CACHE_SIZE.to_be_bytes());
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
    /// Where its messages are addressed: the consumer itself, or its durable subscription.
    target: String,
    sel: Option<Arc<Sel>>,
    /// noLocal: the consumer's connection id, whose own messages it does not get.
    no_local: Option<String>,
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
    client_id: String,
    remote: String,
    cons: Vec<Cons>,
    /// Producers with a producerWindowSize: they get a ProducerAck for each async send.
    windows: HashMap<String, i32>,
    sent: u64,
    next: i32,
    fmt: Fmt,
    conn_id: Obj,
}

impl Conn {
    fn hdr(&mut self) -> Hdr {
        self.next += 1;
        Hdr { id: self.next, resp: false }
    }

    async fn send(&mut self, wr: &mut OwnedWriteHalf, mut d: Ds) -> Flow {
        if wr.write_all(&encode_with(&mut d, Some(&mut self.fmt))).await.is_err() {
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
        let message = r.map(|r| Box::new(row_message(r, true)));
        let redelivery = r.map(|r| r.redelivered as i32).unwrap_or(0);
        Ds::MessageDispatch(MessageDispatch { h: self.hdr(), consumer_id, destination, message, redelivery })
    }

    async fn handle(&mut self, wr: &mut OwnedWriteHalf, b: &[u8]) -> Flow {
        let typ = b[0];
        let (cid, resp) = peek(b, self.fmt.tight);
        let Some(d) = decode_with(b, Some(&mut self.fmt)) else {
            eprintln!("ActiveMQ: {}: cannot read command type {typ}", self.remote);
            return if resp { self.fail(wr, cid, "java.io.IOException", format!("Stuntman's ActiveMQ does not implement OpenWire command type {typ}")).await } else { Flow::Go };
        };
        let connected = self.ctx.is_some();
        if let Some(c) = self.ctx.as_ref().filter(|_| typ != 10) { stuntman_core::usage::hit(c, "queues"); }
        // A BlobMessage is a Message with a URL: handled with the others, its URL kept as a header.
        let (d, blob) = match d {
            Ds::BlobMessage(b) => (Ds::Message(b.m), Some((b.url, b.mime))),
            d => (d, None),
        };
        match d {
            Ds::ConnectionInfo(i) => {
                let Some(ctx) = self.envs.default_env() else { return self.fail(wr, cid, "javax.jms.JMSException", "No Stuntman environment".into()).await };
                let s = settings(&ctx).await;
                let (user, pass) = (i.user.clone().unwrap_or_default(), i.password.clone().unwrap_or_default());
                if s.refuse_login || (!s.users.is_empty() && s.users.get(&user) != Some(&pass)) {
                    return self.fail(wr, cid, "java.lang.SecurityException", format!("User name [{user}] or password is invalid.")).await;
                }
                self.ctx = Some(ctx);
                self.user = user;
                self.client_id = i.client_id.clone().unwrap_or_default();
                self.conn_id = i.connection_id.clone();
                let env = self.ctx.as_ref().unwrap().env.to_string();
                {
                    let mut cs = conns().lock().unwrap();
                    cs.retain(|c| c.1 != self.remote);
                    cs.push((env, self.remote.clone(), self.user.clone(), self.client_id.clone(), self.fmt.name(), util::now_ms()));
                }
                let ctx = self.ctx.clone().unwrap();
                advise(&ctx, format!("{ADV}Connection"), Ds::ConnectionInfo(ConnectionInfo { password: None, ..i }), Map::new()).await;
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
                if p.window > 0 {
                    self.windows.insert(id_key(&p.producer_id), p.window);
                }
                if let (Some(ctx), Some(d)) = (self.ctx.clone(), obj_dest(&p.destination).filter(|d| !is_advisory(d))) {
                    let key = id_key(&p.producer_id);
                    let n = {
                        let mut ps = producers().lock().unwrap();
                        ps.push((ctx.env.to_string(), key, d.clone()));
                        ps.iter().filter(|x| x.0 == *ctx.env && x.2 == d).count()
                    };
                    let mut x = Map::new();
                    x.insert("producerCount".into(), json!(n));
                    advise(&ctx, format!("{ADV}Producer.{}", adv_name(&d)), Ds::ProducerInfo(p.clone()), x).await;
                }
                self.reply(wr, cid, resp).await
            }
            Ds::ConsumerInfo(i) => {
                let ctx = self.ctx.clone().unwrap();
                let key = id_key(&i.consumer_id);
                let dest = obj_dest(&i.destination).unwrap_or_default();
                // Composite destinations (the clients' own TempQueue,TempTopic advisory watch) get nothing.
                let idle = i.browser || dest.is_empty() || dest.contains(',');
                let selector = i.selector.clone().filter(|s| !s.trim().is_empty());
                let sel = match selector.as_deref().map(selector::parse) {
                    Some(Err(e)) => return self.fail(wr, cid, "javax.jms.InvalidSelectorException", format!("Invalid selector '{}': {e}", selector.unwrap_or_default())).await,
                    Some(Ok(e)) => Some(Arc::new(e)),
                    None => None,
                };
                let (mut target, mut durable) = (key.clone(), None);
                let no_local = match i.consumer_id.as_deref() {
                    Some(Ds::ConsumerId(c)) if i.no_local => Some(c.connection.clone().unwrap_or_default()),
                    _ => None,
                };
                let exclusive = i.exclusive && !dest.starts_with("topic://");
                if let Some(name) = i.subscription.clone().filter(|n| !n.is_empty() && dest.starts_with("topic://")) {
                    let client = i.client_id.clone().filter(|c| !c.is_empty()).unwrap_or(self.client_id.clone());
                    if client.is_empty() {
                        return self.fail(wr, cid, "javax.jms.JMSException", "You cannot create a durable subscriber without specifying a unique clientID on a Connection".into()).await;
                    }
                    let id = durable_id(&client, &name);
                    if live().lock().unwrap().iter().any(|l| l.env == *ctx.env && l.target == id) {
                        return self.fail(wr, cid, "javax.jms.JMSException", format!("Durable consumer is in use for client: {client} and subscriptionName: {name}")).await;
                    }
                    durable_open(&ctx, &id, &client, &name, &dest, selector.as_deref().unwrap_or(""), no_local.as_deref().unwrap_or("")).await;
                    (target, durable) = (id, Some(name));
                }
                register(&ctx, &dest, &self.user).await;
                self.cons.push(Cons { key: key.clone(), target: target.clone(), sel: sel.clone(), no_local: no_local.clone(), id: i.consumer_id.clone(), dest: dest.clone(), prefetch: i.prefetch, held: vec![], pull: None, idle });
                let info = i.clone();
                if !idle {
                    live().lock().unwrap().push(Live { env: ctx.env.to_string(), key, target, durable, dest: dest.clone(), selector: selector.unwrap_or_default(), sel: sel.clone(), prefetch: i.prefetch, remote: self.remote.clone(), user: self.user.clone(), since: util::now_ms(), no_local, exclusive, priority: i.priority as i8 as i32, room: true, wire: self.fmt.name() });
                }
                if self.reply(wr, cid, resp).await == Flow::Stop {
                    return Flow::Stop;
                }
                if !idle && !is_advisory(&dest) {
                    let n = live().lock().unwrap().iter().filter(|l| l.env == *ctx.env && l.dest == dest).count();
                    let mut x = Map::new();
                    x.insert("consumerCount".into(), json!(n));
                    advise(&ctx, format!("{ADV}Consumer.{}", adv_name(&dest)), Ds::ConsumerInfo(info), x).await;
                }
                if i.browser {
                    // A QueueBrowser: copies of what is pending, then the null dispatch that ends it.
                    let n = self.cons.len() - 1;
                    for r in pending_rows(&ctx, &dest, sel).await {
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
                let tx = tx_key(&a.tx);
                if let Some(c) = self.cons.iter_mut().find(|c| c.key == key) {
                    let at = c.held.iter().position(|e| e.0 == last).or_else(|| (a.count > 0 && !c.held.is_empty()).then(|| (a.count as usize).min(c.held.len()) - 1));
                    if let (Some(id), 2 | 4, Some(i)) = (&tx, a.ack_type, at) {
                        // Transacted: the slots free up now, the ack counts at the commit.
                        let range = if a.ack_type == 4 { i..=i } else { 0..=i };
                        let seqs: Vec<i64> = c.held[range].iter_mut().map(|e| {
                            e.1 = true;
                            e.0
                        }).collect();
                        let xid = match a.tx.as_deref() {
                            Some(Ds::XaTxId(x)) => Some(x.clone()),
                            _ => None,
                        };
                        tx_open(&ctx, id, xid, &self.remote, &self.user).await;
                        tx_ack(&ctx, id, seqs).await;
                        return self.reply(wr, cid, resp).await;
                    }
                    let done: Vec<i64> = match (a.ack_type, at) {
                        (_, None) | (3, _) => vec![],
                        (0, Some(i)) => {
                            c.held[..=i].iter_mut().for_each(|e| e.1 = true);
                            vec![]
                        }
                        (4, Some(i)) => vec![c.held.remove(i).0],
                        (_, Some(i)) => c.held.drain(..=i).map(|e| e.0).collect(),
                    };
                    let cause = match a.ack_type {
                        6 => "The message expired".to_string(),
                        _ => a.poison.as_ref().and_then(|e| e.message.clone()).unwrap_or_else(|| "Poison ack from the consumer".into()),
                    };
                    let consumed = if matches!(a.ack_type, 2 | 4) { done.clone() } else { vec![] };
                    settle(&ctx, done, match a.ack_type {
                        1 => "dead_letter",
                        6 => "expired",
                        _ => "acked",
                    }, cause)
                    .await;
                    if !consumed.is_empty() && settings(&ctx).await.advisory_consumed {
                        advise_rows(&ctx, "MessageConsumed", consumed).await;
                    }
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
                    29 => "blob",
                    _ => "message",
                };
                let Some(dest) = obj_dest(&m.destination) else { return self.fail(wr, cid, "javax.jms.InvalidDestinationException", "The message has no destination".into()).await };
                if dest == "topic://ActiveMQ.Scheduler.Management" {
                    let props = m.props.as_deref().map(map_decode).unwrap_or_default();
                    let n = scheduler_management(self.ctx.as_ref().unwrap(), &props, obj_dest(&m.reply_to)).await;
                    if let Err(e) = n {
                        return self.fail(wr, cid, "javax.jms.JMSException", e.message).await;
                    }
                    return self.reply(wr, cid, resp).await;
                }
                let ctx = self.ctx.clone().unwrap();
                let tx = tx_key(&m.tx);
                if let Some(id) = &tx {
                    let xid = match m.tx.as_deref() {
                        Some(Ds::XaTxId(x)) => Some(x.clone()),
                        _ => None,
                    };
                    tx_open(&ctx, id, xid, &self.remote, &self.user).await;
                }
                let mut content = m.content.unwrap_or_default();
                // What the client counted against its producer window (Message.getSize()).
                let size = 1024 + content.len() + m.props.as_ref().map_or(0, |p| p.len());
                let mut compressed = m.compressed;
                if compressed {
                    if let Some(b) = inflate(kind, &content) {
                        (content, compressed) = (b, false);
                    }
                }
                let body = if kind == "text" && !compressed { content_text(&content).into_bytes() } else { content };
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
                put("compressed", json!(compressed));
                if let Some((url, mime)) = blob {
                    put("blobUrl", json!(url));
                    put("blobMime", json!(mime));
                }
                let origin = match m.producer_id.as_deref() {
                    Some(Ds::ProducerId(p)) => p.connection.clone().unwrap_or_default(),
                    _ => String::new(),
                };
                hd.insert("persistent".into(), json!(m.persistent));
                hd.insert("priority".into(), json!(m.priority));
                hd.insert("timestamp".into(), json!(m.timestamp));
                if m.expiration > 0 {
                    hd.insert("expiration".into(), json!(m.expiration));
                }
                if m.group_seq != 0 {
                    hd.insert("groupSeq".into(), json!(m.group_seq));
                }
                let n = NewMsg { dest, kind: kind.into(), body, props: m.props.as_deref().map(map_decode).unwrap_or_default(), headers: hd, message_id: mid_text(&m.message_id), sender: self.user.clone(), via: "openwire".into(), tx: tx.unwrap_or_default(), origin, immediate: false };
                if let Err(e) = publish(&ctx, n).await {
                    return self.fail(wr, cid, "javax.jms.JMSException", e.message).await;
                }
                if !resp && self.windows.get(&id_key(&m.producer_id)).is_some_and(|w| *w > 0) {
                    let h = self.hdr();
                    return self.send(wr, Ds::ProducerAck(ProducerAck { h, producer_id: m.producer_id, size: size as i32 })).await;
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
                        let pre = format!("{key}:");
                        self.drop_producers(|k| k.starts_with(&pre)).await;
                    }
                    Some(Ds::ProducerId(_)) => {
                        self.windows.remove(&key);
                        self.drop_producers(|k| *k == key).await;
                    }
                    Some(Ds::ConnectionId(_)) => self.close().await,
                    _ => {}
                }
                self.reply(wr, cid, resp).await
            }
            Ds::TransactionInfo(t) => self.transaction(wr, cid, resp, t).await,
            Ds::DestinationInfo(di) => {
                let ctx = self.ctx.clone().unwrap();
                if let Some(d) = obj_dest(&di.destination).filter(|d| !d.starts_with("temp-") && !is_advisory(d)) {
                    if di.op == 0 {
                        register(&ctx, &d, &self.user).await;
                    } else {
                        let d2 = d.clone();
                        if ctx.db.call(move |c| c.execute("DELETE FROM amq_destinations WHERE name=?1", [d2])).await.unwrap_or(0) > 0 {
                            destination_advisory(&ctx, &d, 1).await;
                        }
                        ring(&ctx, &d);
                    }
                }
                self.reply(wr, cid, resp).await
            }
            Ds::RemoveSubscriptionInfo(r) => {
                let ctx = self.ctx.clone().unwrap();
                let client = r.client_id.filter(|c| !c.is_empty()).unwrap_or(self.client_id.clone());
                let name = r.subscription.unwrap_or_default();
                let id = durable_id(&client, &name);
                if live().lock().unwrap().iter().any(|l| l.env == *ctx.env && l.target == id) {
                    return self.fail(wr, cid, "javax.jms.JMSException", format!("Durable consumer is in use for client: {client} and subscriptionName: {name}")).await;
                }
                if !durable_remove(&ctx, &id).await {
                    return self.fail(wr, cid, "javax.jms.InvalidDestinationException", format!("No durable subscription exists for clientID: {client} and subscriptionName: {name}")).await;
                }
                ring(&ctx, "");
                self.reply(wr, cid, resp).await
            }
            _ => self.reply(wr, cid, resp).await,
        }
    }

    /// TransactionInfo: local (LocalTransactionId) and XA (XATransactionId) transactions.
    async fn transaction(&mut self, wr: &mut OwnedWriteHalf, cid: i32, resp: bool, t: TransactionInfo) -> Flow {
        const XA_OK: i32 = 0;
        const XA_RDONLY: i32 = 3;
        let ctx = self.ctx.clone().unwrap();
        if t.kind == 5 {
            // RECOVER: the prepared XA transactions.
            let data = tx_recover(&ctx).await.into_iter().map(|x| Some(Box::new(Ds::XaTxId(x)))).collect();
            let h = self.hdr();
            return self.send(wr, Ds::DataArrayResponse(DataArrayResponse { h, correlation_id: cid, data: Some(data) })).await;
        }
        let Some(id) = tx_key(&t.tx) else { return self.fail(wr, cid, "javax.jms.JMSException", "TransactionInfo without a transaction id".into()).await };
        let xid = match t.tx.as_deref() {
            Some(Ds::XaTxId(x)) => Some(x.clone()),
            _ => None,
        };
        let err = if xid.is_some() { "javax.transaction.xa.XAException" } else { "javax.jms.TransactionRolledBackException" };
        let state = tx_state(&ctx, &id).await;
        let open = state.as_ref().is_some_and(|(s, _)| matches!(s.as_str(), "active" | "ended" | "prepared"));
        match t.kind {
            0 => tx_open(&ctx, &id, xid, &self.remote, &self.user).await,
            7 => {
                if state.is_some_and(|(s, _)| s == "active") {
                    tx_set(&ctx, &id, "ended").await;
                }
            }
            1 => {
                let Some((s, work)) = state.filter(|_| open) else { return self.fail(wr, cid, err, format!("Transaction '{id}' has not been started.")).await };
                if s == "prepared" {
                    return self.fail(wr, cid, err, format!("Transaction '{id}' is already prepared.")).await;
                }
                // Nothing in it: read-only, and done.
                let result = if work == 0 { XA_RDONLY } else { XA_OK };
                tx_set(&ctx, &id, if work == 0 { "committed" } else { "prepared" }).await;
                let h = self.hdr();
                return self.send(wr, Ds::IntegerResponse(IntegerResponse { h, correlation_id: cid, result })).await;
            }
            2 | 3 => {
                if !open {
                    // A local commit with nothing sent nor acked is fine; an unknown XA id is not.
                    if xid.is_some() {
                        return self.fail(wr, cid, err, format!("Transaction '{id}' has not been started.")).await;
                    }
                    return self.reply(wr, cid, resp).await;
                }
                if t.kind == 3 && state.as_ref().is_some_and(|(s, _)| s != "prepared") {
                    return self.fail(wr, cid, err, "Cannot do 2 phase commit if the transaction has not been prepared".into()).await;
                }
                let mut s = settings(&ctx).await;
                if s.fail_commits > 0 {
                    s.fail_commits -= 1;
                    let _ = save(&ctx, &s).await;
                    tx_rollback(&ctx, &id, true).await;
                    return self.fail(wr, cid, err, "Commit failed (set_activemq fail_commits): the transaction was rolled back".into()).await;
                }
                let acked = tx_commit(&ctx, &id).await;
                if settings(&ctx).await.advisory_consumed {
                    advise_rows(&ctx, "MessageConsumed", acked.clone()).await;
                }
                for c in &mut self.cons {
                    c.held.retain(|e| !acked.contains(&e.0));
                }
            }
            4 => {
                if open {
                    tx_rollback(&ctx, &id, true).await;
                }
            }
            6 => {
                let id2 = id.clone();
                let _ = ctx.db.call(move |c| c.execute("DELETE FROM amq_tx WHERE id=?1", [id2])).await;
            }
            _ => {}
        }
        self.reply(wr, cid, resp).await
    }

    async fn drop_consumer(&mut self, i: usize, last_delivered: Option<i64>) {
        let c = self.cons.remove(i);
        let Some(ctx) = self.ctx.clone() else { return };
        live().lock().unwrap().retain(|l| !(l.env == *ctx.env && l.key == c.key));
        groups().lock().unwrap().retain(|_, o| *o != c.key);
        let durable = c.target != c.key;
        if durable {
            durable_seen(&ctx, &c.target).await;
        }
        release(&ctx, c.key, c.held.into_iter().map(|e| e.0).collect(), last_delivered, !durable && c.dest.starts_with("topic://")).await;
        if !c.idle && !is_advisory(&c.dest) {
            let n = live().lock().unwrap().iter().filter(|l| l.env == *ctx.env && l.dest == c.dest).count();
            let mut x = Map::new();
            x.insert("consumerCount".into(), json!(n));
            advise(&ctx, format!("{ADV}Consumer.{}", adv_name(&c.dest)), Ds::RemoveInfo(RemoveInfo { object_id: c.id, ..Default::default() }), x).await;
        }
    }

    /// Producers gone (RemoveInfo of the producer or its session, or the connection closing).
    async fn drop_producers(&mut self, gone: impl Fn(&String) -> bool) {
        let Some(ctx) = self.ctx.clone() else { return };
        let left: Vec<(String, String)> = {
            let mut ps = producers().lock().unwrap();
            let mine: Vec<(String, String)> = ps.iter().filter(|x| x.0 == *ctx.env && x.1.starts_with(&self.conn_prefix()) && gone(&x.1)).map(|x| (x.1.clone(), x.2.clone())).collect();
            ps.retain(|x| !(x.0 == *ctx.env && mine.iter().any(|m| m.0 == x.1)));
            mine
        };
        for (key, d) in left {
            let n = producers().lock().unwrap().iter().filter(|x| x.0 == *ctx.env && x.2 == d).count();
            let mut parts = key.rsplitn(3, ':');
            let (value, session, conn) = (parts.next().and_then(|x| x.parse().ok()).unwrap_or(0), parts.next().and_then(|x| x.parse().ok()).unwrap_or(0), parts.next().unwrap_or("").to_string());
            let id = Some(Box::new(Ds::ProducerId(ProducerId { connection: Some(conn), value, session })));
            let mut x = Map::new();
            x.insert("producerCount".into(), json!(n));
            advise(&ctx, format!("{ADV}Producer.{}", adv_name(&d)), Ds::RemoveInfo(RemoveInfo { object_id: id, ..Default::default() }), x).await;
        }
    }

    /// The connection id this connection's producer and consumer keys start with.
    fn conn_prefix(&self) -> String {
        format!("{}:", id_key(&self.conn_id))
    }

    async fn close(&mut self) {
        conns().lock().unwrap().retain(|c| c.1 != self.remote);
        if let Some(ctx) = self.ctx.clone() {
            tx_abandon(&ctx, &self.remote).await;
        }
        while !self.cons.is_empty() {
            self.drop_consumer(0, None).await;
        }
        self.drop_producers(|_| true).await;
        if let (Some(ctx), Some(id)) = (self.ctx.clone(), self.conn_id.take()) {
            advise(&ctx, format!("{ADV}Connection"), Ds::RemoveInfo(RemoveInfo { object_id: Some(id), ..Default::default() }), Map::new()).await;
        }
    }

    /// Dispatches what each consumer has room for (its prefetch, or a pull), then answers pulls
    /// that timed out with an empty dispatch.
    async fn pump(&mut self, wr: &mut OwnedWriteHalf) -> Flow {
        let Some(ctx) = self.ctx.clone() else { return Flow::Go };
        flush_outbox(&ctx).await;
        let env = ctx.env.to_string();
        let set_room = |key: &str, room: bool| {
            if let Some(l) = live().lock().unwrap().iter_mut().find(|l| l.env == env && l.key == key) {
                l.room = room;
            }
        };
        let mut st: Option<Settings> = None;
        for i in 0..self.cons.len() {
            loop {
                let c = &self.cons[i];
                let room = if c.prefetch > 0 { c.held.iter().filter(|e| !e.1).count() < c.prefetch as usize } else { c.pull.is_some() };
                set_room(&c.key, room);
                if c.idle || !room {
                    break;
                }
                let mut higher = vec![];
                if !c.dest.starts_with("topic://") {
                    let ls = live().lock().unwrap();
                    let mine = ls.iter().find(|l| l.env == env && l.key == c.key).map(|l| l.priority).unwrap_or(0);
                    // An exclusive consumer on the queue (the highest priority, then the first connected) takes everything.
                    let owner = ls.iter().filter(|l| l.env == env && l.dest == c.dest && l.exclusive).fold(None::<&Live>, |b, l| if b.is_none_or(|b| l.priority > b.priority) { Some(l) } else { b });
                    if owner.is_some_and(|l| l.key != c.key) {
                        break;
                    }
                    // Higher-priority consumers with room come first: what they would take is theirs.
                    let mut gated = false;
                    for l in ls.iter().filter(|l| l.env == env && l.dest == c.dest && l.key != c.key && l.priority > mine && l.room) {
                        match &l.sel {
                            Some(s) => higher.push(s.clone()),
                            None => gated = true,
                        }
                    }
                    if gated {
                        break;
                    }
                }
                let Some(r) = claim(&ctx, &c.dest, &c.target, &c.key, c.sel.clone(), c.no_local.clone(), higher).await else { break };
                if st.is_none() {
                    st = Some(settings(&ctx).await);
                }
                let s = st.clone().unwrap();
                let c = &mut self.cons[i];
                c.held.push((r.seq, false));
                if c.prefetch <= 0 {
                    c.pull = None;
                }
                let room = if c.prefetch > 0 { c.held.iter().filter(|e| !e.1).count() < c.prefetch as usize } else { c.pull.is_some() };
                set_room(&c.key, room);
                if s.slow_dispatch_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(s.slow_dispatch_ms)).await;
                }
                let d = self.dispatch(i, Some(&r));
                if self.send(wr, d).await == Flow::Stop {
                    return Flow::Stop;
                }
                ring(&ctx, &r.dest);
                if s.advisory_delivered && !is_advisory(&r.dest) {
                    advise_rows(&ctx, "MessageDelivered", vec![r.seq]).await;
                }
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
    let mut c = Conn { envs, ctx: None, user: String::new(), client_id: String::new(), remote, cons: vec![], windows: HashMap::new(), sent: 0, next: 0, fmt: Fmt::default(), conn_id: None };
    if c.send(&mut wr, wire_format()).await == Flow::Go {
        let first = tokio::time::timeout(Duration::from_secs(30), rx.recv()).await.ok().flatten();
        match first.as_deref().and_then(decode) {
            Some(Ds::WireFormatInfo(w)) if &w.magic == b"ActiveMQ" && w.version >= VERSION => {
                let p = w.props.as_deref().map(map_decode).unwrap_or_default();
                let on = |k: &str| p.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
                c.fmt.tight = on("TightEncodingEnabled");
                c.fmt.cache = on("CacheEnabled");
                c.fmt.cache_size = p.get("CacheSize").and_then(|v| v.as_i64()).unwrap_or(0).clamp(0, CACHE_SIZE as i64) as u16;
                c.fmt.cache &= c.fmt.cache_size > 0;
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
    for (i, k) in [(16, "transaction")] {
        if let Some(t) = r.get::<_, Option<String>>(i)?.filter(|t| !t.is_empty()) {
            v[k] = json!(t);
        }
    }
    for (i, k) in [(17, "deliverAt"), (18, "expiresAt")] {
        let t: i64 = r.get(i)?;
        if t > 0 {
            v[k] = json!(t);
        }
    }
    let job: String = r.get(19)?;
    if !job.is_empty() {
        v["schedule"] = serde_json::from_str(&job).unwrap_or(Value::Null);
    }
    let moved: i64 = r.get(20)?;
    if moved > 0 {
        v["movedTo"] = json!({"seq": moved, "destination": r.get::<_, Option<String>>(21)?});
    }
    Ok(v)
}

async fn messages(ctx: &Ctx, dest: Option<String>, state: Option<String>, last: u32) -> Result<Vec<Value>, ApiError> {
    let clock = stuntman_core::clock::now(ctx);
    ctx.db
        .call(move |c| {
            promote(c, clock)?;
            c.prepare("SELECT m.seq, m.dest, m.target, m.kind, m.message_id, m.body, m.props, m.headers, m.state, m.consumer, m.redelivered, m.enqueued_at, m.delivered_at, m.acked_at, m.sender, m.via, m.tx, m.deliver_at, m.expires_at,
                m.job, m.moved_to, (SELECT d.dest FROM amq_messages d WHERE d.seq=m.moved_to) FROM amq_messages m WHERE (?1 IS NULL OR m.dest=?1) AND (?2 IS NULL OR m.state=?2) ORDER BY m.seq DESC LIMIT ?3")?
                .query_map(params![dest, state, last], view)?
                .collect()
        })
        .await
}

async fn counts(ctx: &Ctx, dest: &str) -> Map<String, Value> {
    let (d, clock) = (dest.to_string(), stuntman_core::clock::now(ctx));
    let rows: Vec<(String, i64)> = ctx
        .db
        .call(move |c| {
            promote(c, clock)?;
            c.prepare("SELECT state, count(*) FROM amq_messages WHERE dest=?1 GROUP BY state")?.query_map([d], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
        })
        .await
        .unwrap_or_default();
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
    json!({"users": s.users.keys().collect::<Vec<_>>(), "anyUser": s.users.is_empty(), "refuseLogin": s.refuse_login, "dropAfter": s.drop_after, "slowDispatchMs": s.slow_dispatch_ms, "redeliver": s.redeliver, "failCommits": s.fail_commits,
        "maxRedeliveries": s.max_redeliveries(), "individualDlq": s.individual_dlq, "advisoryDelivered": s.advisory_delivered, "advisoryConsumed": s.advisory_consumed, "deadLetterQueue": if s.individual_dlq { "queue://ActiveMQ.DLQ.Queue.<name> / ActiveMQ.DLQ.Topic.<name>" } else { DLQ }})
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("list_activemq_destinations", "ActiveMQ (OpenWire, port 61616) queues and topics with their messages per state (scheduled, pending, delivered, acked, dead_letter, expired, rolled_back, discarded), the consumers connected now (with their selector and durable subscription), each topic's durable subscriptions (online or offline, with what they keep), the open and recent transactions (local and XA, prepared ones included), the connections logged in now with their negotiated wire format (loose or tight, with or without the marshal cache), the broker URL and what set_activemq changed.", |ctx, _a| async move {
            let names: Vec<String> = ctx.db.call(|c| c.prepare("SELECT name FROM amq_destinations UNION SELECT DISTINCT dest FROM amq_messages WHERE dest NOT LIKE '%ActiveMQ.Advisory.%' ORDER BY 1")?.query_map([], |r| r.get(0))?.collect()).await?;
            let env = ctx.env.to_string();
            let conns: Vec<Live> = live().lock().unwrap().iter().filter(|l| l.env == env).cloned().collect();
            let subs: Vec<Value> = ctx
                .db
                .call(|c| {
                    c.prepare("SELECT d.id, d.client_id, d.name, d.dest, d.selector, d.created_at, d.last_seen, (SELECT count(*) FROM amq_messages m WHERE m.target=d.id AND m.state IN ('pending','scheduled')) FROM amq_durable d ORDER BY d.dest, d.id")?
                        .query_map([], |r| Ok(json!({"id": r.get::<_, String>(0)?, "clientId": r.get::<_, String>(1)?, "name": r.get::<_, String>(2)?, "destination": r.get::<_, String>(3)?,
                            "selector": r.get::<_, String>(4)?, "createdAt": r.get::<_, i64>(5)?, "lastSeen": r.get::<_, i64>(6)?, "pending": r.get::<_, i64>(7)?})))?
                        .collect()
                })
                .await?;
            let txs: Vec<Value> = ctx
                .db
                .call(|c| {
                    c.prepare("SELECT id, kind, state, user, conn, sends, acks, started_at, ended_at FROM amq_tx ORDER BY state IN ('committed','rolled_back'), started_at DESC LIMIT 30")?
                        .query_map([], |r| Ok(json!({"id": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?, "state": r.get::<_, String>(2)?, "user": r.get::<_, String>(3)?, "remote": r.get::<_, String>(4)?,
                            "sends": r.get::<_, i64>(5)?, "acks": r.get::<_, i64>(6)?, "startedAt": r.get::<_, i64>(7)?, "endedAt": r.get::<_, Option<i64>>(8)?})))?
                        .collect()
                })
                .await?;
            let mut out = vec![];
            for n in names {
                let consumers: Vec<Value> = conns.iter().filter(|l| l.dest == n).map(|l| json!({"id": l.key, "prefetch": l.prefetch, "remote": l.remote, "user": l.user, "since": l.since, "selector": l.selector, "durable": l.durable, "noLocal": l.no_local.is_some(), "exclusive": l.exclusive, "priority": l.priority, "wire": l.wire})).collect();
                let mut d = json!({"name": n, "kind": n.split("://").next().unwrap_or("queue"), "counts": counts(&ctx, &n).await, "consumers": consumers});
                if n.starts_with("topic://") {
                    d["subscriptions"] = json!(subs.iter().filter(|s| s["destination"] == json!(n)).map(|s| {
                        let mut s = s.clone();
                        s["active"] = json!(conns.iter().any(|l| json!(l.target) == s["id"]));
                        s
                    }).collect::<Vec<_>>());
                }
                out.push(d);
            }
            let connections: Vec<Value> = self::conns().lock().unwrap().iter().filter(|c| c.0 == env).map(|c| json!({"remote": c.1, "user": c.2, "clientId": c.3, "wire": c.4, "since": c.5})).collect();
            Ok(json!({"destinations": out, "transactions": txs, "connections": connections, "url": format!("activemq:tcp://localhost:{}", port()), "port": port(), "openwireVersion": VERSION,
                "bindError": BIND_ERROR.lock().unwrap().clone(), "settings": settings_view(&settings(&ctx).await)}))
        })
        .route("GET", "/activemq/destinations"),
        Tool::new("list_activemq_messages", "ActiveMQ messages, newest first, with their state: scheduled (AMQ_SCHEDULED_DELAY not over), pending (waiting), delivered (dispatched, not acked yet), acked, dead_letter (poison ack), expired (JMSExpiration passed), rolled_back (sent in a transaction that rolled back), discarded (topic copy whose subscriber left). A message still inside an open transaction carries its id; a repeating or cron one its schedule; one moved to the dead-letter queue (expired, or poisoned past max_redeliveries) points at its copy there (movedTo), and the copy names its originalDestination.", |ctx, a| async move {
            let dest = a.str("destination").or(a.str("queue")).map(|d| normalize(&d));
            let state = a.str("state").filter(|s| s != "all");
            let rows = messages(&ctx, dest.clone(), state, a.u64("last").unwrap_or(50) as u32).await?;
            Ok(json!({"destination": dest, "counts": match &dest { Some(d) => Value::Object(counts(&ctx, d).await), None => Value::Null }, "messages": rows}))
        })
        .param("destination", "queue://<name>, topic://<name> or a queue name (empty: all)").param("state", "scheduled | pending | delivered | acked | dead_letter | expired | rolled_back | discarded").param("last", "default 50")
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
                "blob" => vec![],
                k => return Err(ApiError::bad(format!("kind “{k}”: text, bytes or blob"))),
            };
            let mut headers = Map::new();
            if kind == "blob" {
                headers.insert("blobUrl".into(), json!(a.str("blob_url").ok_or_else(|| ApiError::bad("kind=blob: “blob_url” is required"))?));
                if let Some(m) = a.str("mime_type") {
                    headers.insert("blobMime".into(), json!(m));
                }
            }
            for (k, h) in [("correlation_id", "correlationId"), ("type", "type"), ("group_id", "groupId")] {
                if let Some(v) = a.str(k) {
                    headers.insert(h.into(), json!(v));
                }
            }
            if let Some(r) = a.str("reply_to") {
                headers.insert("replyTo".into(), json!(normalize(&r)));
            }
            headers.insert("persistent".into(), json!(a.bool("persistent").unwrap_or(true)));
            headers.insert("priority".into(), json!(a.u64("priority").unwrap_or(4).min(9)));
            headers.insert("timestamp".into(), json!(util::now_ms()));
            if let Some(t) = a.ms("ttl")?.filter(|t| *t > 0) {
                headers.insert("expiration".into(), json!(util::now_ms() + t as i64));
            }
            let (sender, via) = by(&a);
            let mut props = match a.get("properties") {
                Some(Value::Object(o)) => o.clone(),
                Some(Value::String(s)) => serde_json::from_str(s).map_err(|_| ApiError::bad("properties: a JSON object"))?,
                _ => Map::new(),
            };
            if let Some(d) = a.ms("delay")?.filter(|d| *d > 0) {
                props.insert("AMQ_SCHEDULED_DELAY".into(), json!(d as i64));
            }
            if let Some(d) = a.ms("period")?.filter(|d| *d > 0) {
                props.insert("AMQ_SCHEDULED_PERIOD".into(), json!(d as i64));
            }
            if let Some(r) = a.u64("repeat").filter(|r| *r > 0) {
                props.insert("AMQ_SCHEDULED_REPEAT".into(), json!(r as i64));
            }
            if let Some(c) = a.str("cron").filter(|c| !c.trim().is_empty()) {
                props.insert("AMQ_SCHEDULED_CRON".into(), json!(c));
            }
            let mut seqs = vec![];
            for _ in 0..a.u64("count").unwrap_or(1).clamp(1, 1000) {
                seqs.extend(publish(&ctx, NewMsg { dest: dest.clone(), kind: kind.clone(), body: body.clone(), props: props.clone(), headers: headers.clone(), message_id: String::new(), sender: sender.clone(), via: via.clone(), tx: String::new(), origin: String::new(), immediate: false }).await?);
            }
            Ok(json!({"destination": dest, "seqs": seqs, "counts": counts(&ctx, &dest).await}))
        })
        .req("destination", "queue://<name> (a bare name is a queue) or topic://<name>").param("text", "the TextMessage body (JSON is sent as its text)").param("kind", "text (default) | bytes | blob (a BlobMessage: blob_url, mime_type)")
        .param("blob_url", "kind=blob: the remote blob URL the consumer gets").param("mime_type", "kind=blob: its MIME type")
        .param("body_base64", "kind=bytes: the BytesMessage body").param("properties", "{key: value} message properties").param("correlation_id", "JMSCorrelationID").param("type", "JMSType")
        .param("reply_to", "JMSReplyTo destination").param("group_id", "JMSXGroupID").param("persistent", "default true").param("priority", "0-9, default 4 (higher goes first)").param("count", "send this many copies (default 1)")
        .param("delay", "scheduled delivery (AMQ_SCHEDULED_DELAY) like 5s: scheduled until then").param("ttl", "time to live like 30s (JMSExpiration): expired (and moved to the DLQ) if nobody took it by then")
        .param("period", "AMQ_SCHEDULED_PERIOD like 10s: the time between repeats").param("repeat", "AMQ_SCHEDULED_REPEAT: how many more deliveries")
        .param("cron", "AMQ_SCHEDULED_CRON, 5 fields in UTC on the environment clock, like \"*/5 * * * *\" (repeats until cancel_activemq_scheduled)")
        .route("POST", "/activemq/messages"),
        Tool::new("set_activemq", "ActiveMQ broker behaviour: users (name → password; empty: anyone logs in), refuse_login (every login fails with a SecurityException), drop_after (close the connection after N dispatched messages), slow_dispatch (wait before each dispatch), redeliver (the next N acks are lost: those messages come again with a higher redelivery count), fail_commits (the next N transaction commits fail with a TransactionRolledBackException, or an XAException for XA, and roll back), max_redeliveries (past it a message goes to the dead-letter queue; default 6), individual_dlq (one DLQ per destination instead of ActiveMQ.DLQ), advisory_delivered / advisory_consumed (the MessageDelivered / MessageConsumed advisory topics, off by default as in ActiveMQ). Only what you pass changes; reset=true goes back to defaults.", |ctx, a| async move {
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
            if let Some(Value::Number(_) | Value::String(_)) = a.0.get("fail_commits") {
                s.fail_commits = a.u64("fail_commits").unwrap_or(0);
            }
            match a.0.get("max_redeliveries") {
                Some(Value::Number(n)) => s.max_redeliveries = Some(n.as_i64().unwrap_or(6).max(-1)),
                Some(Value::String(t)) => s.max_redeliveries = Some(t.trim().parse::<i64>().map_err(|_| ApiError::bad("max_redeliveries: a number (-1: never)"))?.max(-1)),
                _ => {}
            }
            if let Some(b) = a.bool("individual_dlq") {
                s.individual_dlq = b;
            }
            if let Some(b) = a.bool("advisory_delivered") {
                s.advisory_delivered = b;
            }
            if let Some(b) = a.bool("advisory_consumed") {
                s.advisory_consumed = b;
            }
            save(&ctx, &s).await?;
            ctx.live.send("activemq.settings", "", "changed");
            Ok(json!({"settings": settings_view(&s)}))
        })
        .param("users", "{\"acme\": \"s3cret\"} or \"acme:s3cret,globex:pw\"; {} or \"\": anyone").param("refuse_login", "true: every login fails (java.lang.SecurityException)")
        .param("drop_after", "close the connection after this many dispatched messages; 0: never").param("slow_dispatch", "a delay like 2s before each dispatch; 0: none")
        .param("redeliver", "the next N acks are lost and those messages are delivered again")
        .param("fail_commits", "the next N commits fail and roll back (the app's retry path)")
        .param("max_redeliveries", "deliveries after the first before a message goes to the dead-letter queue (default 6; -1: never)")
        .param("individual_dlq", "true: ActiveMQ.DLQ.Queue.<name> / ActiveMQ.DLQ.Topic.<name> per destination; false: the shared ActiveMQ.DLQ")
        .param("advisory_delivered", "true: ActiveMQ.Advisory.MessageDelivered.<Queue|Topic>.<name> on each dispatch")
        .param("advisory_consumed", "true: ActiveMQ.Advisory.MessageConsumed.<Queue|Topic>.<name> on each ack")
        .param("reset", "true: back to defaults")
        .route("PUT", "/activemq/settings"),
        Tool::new("cancel_activemq_scheduled", "Cancels scheduled ActiveMQ messages (AMQ_SCHEDULED_DELAY, _PERIOD/_REPEAT, _CRON) before they go out: one (seq) or all of a destination. They are kept as discarded.", |ctx, a| async move {
            let seq = a.str("seq").map(|s| s.trim_start_matches('#').parse::<i64>().map_err(|_| ApiError::bad("seq: a number"))).transpose()?;
            let dest = a.str("destination").or(a.str("queue")).map(|d| normalize(&d));
            if seq.is_none() && dest.is_none() {
                return Err(ApiError::bad("“seq” or “destination” is required"));
            }
            let n = ctx.db.call(move |c| c.execute("UPDATE amq_messages SET state='discarded', job='' WHERE state='scheduled' AND (?1 IS NULL OR seq=?1) AND (?2 IS NULL OR dest=?2)", params![seq, dest])).await?;
            ring(&ctx, "");
            Ok(json!({"cancelled": n}))
        })
        .param("seq", "one scheduled message").param("destination", "every scheduled message of this destination")
        .route("POST", "/activemq/scheduled/cancel"),
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
                    (None, None) => num("scheduled") + num("pending") + num("delivered") == 0 && num("acked") > 0,
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

    fn fmt(tight: bool, cache: bool) -> Fmt {
        Fmt { tight, cache, cache_size: 1024, ..Default::default() }
    }

    /// Loose, loose with the marshal cache, tight with the cache: decode(encode(x)) == x in each,
    /// twice in a row (the cache indexes carry over between frames).
    fn roundtrip(mut d: Ds) {
        let f = encode(&mut d);
        assert_eq!(u32::from_be_bytes(f[..4].try_into().unwrap()) as usize, f.len() - 4, "size prefix of {d:?}");
        assert_eq!(decode(&f[4..]).as_ref(), Some(&d), "round trip of type {}", d.typ());
        for (tight, cache) in [(false, true), (true, false), (true, true)] {
            let (mut w, mut r) = (fmt(tight, cache), fmt(tight, cache));
            for _ in 0..2 {
                let f = encode_with(&mut d, Some(&mut w));
                assert_eq!(u32::from_be_bytes(f[..4].try_into().unwrap()) as usize, f.len() - 4);
                assert_eq!(decode_with(&f[4..], Some(&mut r)).as_ref(), Some(&d), "tight={tight} cache={cache} round trip of type {}", d.typ());
            }
        }
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
            Ds::BytesMessage(Msg { content: Some(vec![0, 1, 2, 255]), props: None, ..msg.clone() }),
            Ds::BlobMessage(BlobMsg { m: Msg { content: None, ..msg }, url: Some("http://files.acme.example/manifest-1043.pdf".into()), mime: Some("application/pdf".into()), deleted: false }),
            Ds::MessageDispatch(MessageDispatch { h: h(false), consumer_id: cid(), destination: q(), message: None, redelivery: -1 }),
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
        assert_eq!(p["TightEncodingEnabled"], json!(true));
        assert_eq!(p["CacheSize"], json!(1024));
        assert_eq!(p["MaxInactivityDuration"], json!(30_000));
        assert!(f.windows(25).any(|x| x == b"\x00\x15MaxInactivityDuration\x06\x00"), "MaxInactivityDuration goes as a long");
    }

    #[test]
    fn tight_known_bytes() {
        // Tight Response: type, BooleanStream (1 byte of bits: responseRequired false), commandId, correlationId.
        let f = encode_with(&mut Ds::Response(Response { h: Hdr { id: 1, resp: false }, correlation_id: 5 }), Some(&mut fmt(true, false)));
        assert_eq!(f, vec![0, 0, 0, 11, 30, 1, 0, 0, 0, 0, 1, 0, 0, 0, 5]);
        // Longs take 0, 2, 4 or 8 bytes after two bits; strings a present and an ASCII bit.
        let f = encode_with(&mut Ds::RemoveInfo(RemoveInfo { h: Hdr { id: 2, resp: true }, object_id: None, last_delivered: 300 }), Some(&mut fmt(true, false)));
        // bits: resp 1, object present 0, the long's (false, true): two bytes follow → 0b1001
        assert_eq!(f, vec![0, 0, 0, 9, 12, 1, 0b1001, 0, 0, 0, 2, 1, 44]);
        assert_eq!(peek(&f[4..], true), (2, true));
        let f = encode(&mut Ds::RemoveInfo(RemoveInfo { h: Hdr { id: 2, resp: true }, object_id: None, last_delivered: 300 }));
        assert_eq!(peek(&f[4..], false), (2, true));
        // A message nested in a dispatch carries one more bit (MarshallAware); 70 bits take 9 bytes.
        let bits = vec![true; 70];
        assert_eq!(bool_stream(&bits)[0], 9);
        assert_eq!(bool_stream(&vec![false; 8 * 64])[..2], [0xc0, 64]);
        assert_eq!(bool_stream(&vec![false; 8 * 300])[..3], [0x80, 1, 44]);
    }

    #[test]
    fn marshal_cache_hits_are_read() {
        // Loose RemoveInfo with the cache: commandId, responseRequired, then the object id as
        // "known" (false) and its index, then the long.
        let mut r = fmt(false, true);
        r.seen.insert(3, cid());
        let mut b = vec![12, 0, 0, 0, 9, 1, 0, 0, 3];
        b.extend(42i64.to_be_bytes());
        let Some(Ds::RemoveInfo(x)) = decode_with(&b, Some(&mut r)) else { panic!() };
        assert_eq!((x.object_id, x.last_delivered), (cid(), 42));
        // An index never sent is refused.
        b[8] = 4;
        assert!(decode_with(&b, Some(&mut r)).is_none());
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

    fn xid() -> XaTxId {
        XaTxId { format_id: 0x1234, gtid: Some(b"acme-gtid".to_vec()), bq: Some(vec![1, 2]) }
    }

    #[test]
    fn transactions_round_trip() {
        let local = Some(Box::new(Ds::LocalTxId(LocalTxId { value: 3, connection_id: Some(Box::new(Ds::ConnectionId(StrId { value: Some("ID:acme-1".into()) }))) })));
        let xa = Some(Box::new(Ds::XaTxId(xid())));
        for d in [
            Ds::TransactionInfo(TransactionInfo { h: h(true), connection_id: Some(Box::new(Ds::ConnectionId(StrId { value: Some("ID:acme-1".into()) }))), tx: local.clone(), kind: 2 }),
            Ds::TransactionInfo(TransactionInfo { h: h(true), tx: xa.clone(), kind: 1, ..Default::default() }),
            Ds::TransactionInfo(TransactionInfo { h: h(true), tx: None, kind: 5, ..Default::default() }),
            Ds::IntegerResponse(IntegerResponse { h: h(false), correlation_id: 7, result: 3 }),
            Ds::DataArrayResponse(DataArrayResponse { h: h(false), correlation_id: 7, data: Some(vec![xa.clone(), Some(Box::new(Ds::XaTxId(XaTxId { format_id: 1, gtid: Some(vec![]), bq: None })))]) }),
            Ds::DataArrayResponse(DataArrayResponse { h: h(false), correlation_id: 8, data: Some(vec![]) }),
            Ds::RemoveSubscriptionInfo(RemoveSubscriptionInfo { h: h(true), connection_id: None, subscription: Some("prices-sub".into()), client_id: Some("acme-dashboard".into()) }),
            Ds::ProducerAck(ProducerAck { h: h(false), producer_id: pid(), size: 1100 }),
            Ds::TextMessage(Msg { h: h(false), destination: q(), tx: local.clone(), group_id: Some("TRK-1".into()), group_seq: 2, expiration: 9, priority: 9, ..Default::default() }),
            Ds::MessageAck(MessageAck { h: h(false), destination: q(), tx: xa.clone(), consumer_id: cid(), ack_type: 2, count: 2, ..Default::default() }),
            Ds::ConsumerInfo(ConsumerInfo { h: h(true), consumer_id: cid(), destination: Some(Box::new(dest_ds("topic://prices"))), prefetch: 1, selector: Some("region = 'north'".into()), subscription: Some("prices-sub".into()), ..Default::default() }),
        ] {
            roundtrip(d);
        }
        assert_eq!(tx_key(&local).unwrap(), "local:ID:acme-1:3");
        assert_eq!(tx_key(&xa).unwrap(), "xa:4660:61636d652d67746964:0102");
    }

    #[test]
    fn xa_known_bytes() {
        // XATransactionId (112): int formatId, then each byte array as present flag, int length, bytes.
        let mut w = Writer::default();
        w.nested(&mut Some(Box::new(Ds::XaTxId(XaTxId { format_id: 1, gtid: Some(vec![0xaa]), bq: None }))));
        assert_eq!(w.0, vec![1, 112, 0, 0, 0, 1, 1, 0, 0, 0, 1, 0xaa, 0]);
        // IntegerResponse (34): the Response fields, then the int result (XA_RDONLY = 3).
        let f = encode(&mut Ds::IntegerResponse(IntegerResponse { h: Hdr { id: 1, resp: false }, correlation_id: 5, result: 3 }));
        assert_eq!(f, vec![0, 0, 0, 14, 34, 0, 0, 0, 1, 0, 0, 0, 0, 5, 0, 0, 0, 3]);
        // DataArrayResponse (33): a present flag, a short count, the nested objects.
        let f = encode(&mut Ds::DataArrayResponse(DataArrayResponse { h: Hdr { id: 1, resp: false }, correlation_id: 5, data: Some(vec![]) }));
        assert_eq!(&f[4..], &[33, 0, 0, 0, 1, 0, 0, 0, 0, 5, 1, 0, 0]);
    }

    #[test]
    fn compressed_bodies() {
        use std::io::Write;
        let z = |b: &[u8]| {
            let mut e = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::default());
            e.write_all(b).unwrap();
            e.finish().unwrap()
        };
        // A TextMessage deflates its whole content (length and modified UTF-8).
        let text = text_content("{\"truck\": \"TRK-100\"} ✓");
        assert_eq!(content_text(&inflate("text", &z(&text)).unwrap()), "{\"truck\": \"TRK-100\"} ✓");
        // A BytesMessage writes the plain length first, then the deflated bytes.
        let mut b = 4i32.to_be_bytes().to_vec();
        b.extend(z(&[9, 8, 7, 6]));
        assert_eq!(inflate("bytes", &b).unwrap(), vec![9, 8, 7, 6]);
        assert!(inflate("text", b"not deflated").is_none());
        // Apache.NMS.ActiveMQ 1.8 (gzip), as it sent "hello acme hello acme hello".
        let nms = base64::engine::general_purpose::STANDARD.decode("H4sIAAAAAAAAE2NgYJDOSM3JyVdITM5NVUBnAgDO1+OHHwAAAA==").unwrap();
        assert_eq!(content_text(&inflate("text", &nms).unwrap()), "hello acme hello acme hello");
    }

    #[test]
    fn selector_fields() {
        let hd = json!({"type": "order.created", "correlationId": "c-1", "priority": 7, "persistent": false, "timestamp": 5, "groupId": "TRK-1"});
        let props = json!({"region": "north", "amount": 250});
        let (hd, props) = (hd.as_object().unwrap(), props.as_object().unwrap());
        let s = selector::parse("JMSType = 'order.created' AND JMSCorrelationID = 'c-1' AND JMSPriority = 7 AND JMSDeliveryMode = 'NON_PERSISTENT' AND JMSTimestamp = 5 AND JMSMessageID = 'ID:x:1:1:1' AND JMSXGroupID = 'TRK-1' AND region = 'north' AND amount > 200 AND JMSRedelivered = TRUE").unwrap();
        assert!(selects(&s, hd, props, "ID:x:1:1:1", 1));
        assert!(!selects(&s, hd, props, "ID:x:1:1:1", 0));
    }

    #[test]
    fn schedules_and_dead_letters() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(SQL).unwrap();
        let add = |dest: &str, state: &str, deliver: i64, expires: i64, job: &Job, hd: Value| {
            let job = if *job == Job::default() { String::new() } else { serde_json::to_string(job).unwrap() };
            c.execute("INSERT INTO amq_messages(dest, kind, body, headers, state, enqueued_at, deliver_at, expires_at, job) VALUES(?1,'text',x'',?2,?3,0,?4,?5,?6)", params![dest, hd.to_string(), state, deliver, expires, job]).unwrap();
            c.last_insert_rowid()
        };
        let states = |dest: &str| -> Vec<String> { c.prepare("SELECT state FROM amq_messages WHERE dest=?1 ORDER BY seq").unwrap().query_map([dest], |r| r.get(0)).unwrap().map(|x| x.unwrap()).collect() };
        // Repeat 2, period 1 s, first at t=1000: by t=2500 two deliveries, the last one at 3000 is the message itself.
        let rep = add("queue://orders.rep", "scheduled", 1000, 0, &Job { period: 1000, repeat: 2, left: 2, ..Default::default() }, json!({}));
        promote(&c, 2500).unwrap();
        assert_eq!(states("queue://orders.rep"), ["scheduled", "pending", "pending"]);
        promote(&c, 3000).unwrap();
        assert_eq!(states("queue://orders.rep"), ["pending", "pending", "pending"]);
        let job: String = c.query_row("SELECT job FROM amq_messages WHERE seq=?1", [rep], |r| r.get(0)).unwrap();
        assert!(job.is_empty());
        // Cron every minute with one repeat 10 s later: two deliveries per minute, forever.
        let cron = add("queue://orders.cron", "scheduled", 60_000, 0, &Job { cron: "* * * * *".into(), repeat: 1, left: 1, period: 10_000, ..Default::default() }, json!({}));
        promote(&c, 125_000).unwrap();
        assert_eq!(states("queue://orders.cron").iter().filter(|s| *s == "pending").count(), 3, "at 60 s, 70 s and 120 s");
        let at: i64 = c.query_row("SELECT deliver_at FROM amq_messages WHERE seq=?1", [cron], |r| r.get(0)).unwrap();
        assert_eq!(at, 130_000);
        // Expired: a persistent message moves to ActiveMQ.DLQ, a non-persistent one is only dropped.
        let p = add("queue://orders.exp", "pending", 0, 500, &Job::default(), json!({"persistent": true}));
        let np = add("queue://orders.exp", "pending", 0, 500, &Job::default(), json!({"persistent": false}));
        promote(&c, 600).unwrap();
        let moved: (String, i64) = c.query_row("SELECT state, moved_to FROM amq_messages WHERE seq=?1", [p], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(moved.0, "expired");
        let (dest, hd, pr): (String, String, String) = c.query_row("SELECT dest, headers, props FROM amq_messages WHERE seq=?1", [moved.1], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((dest.as_str(), serde_json::from_str::<Value>(&hd).unwrap()["originalDestination"].clone()), (DLQ, json!("queue://orders.exp")));
        assert_eq!(serde_json::from_str::<Value>(&pr).unwrap()["dlqDeliveryFailureCause"], json!("The message expired"));
        assert_eq!(c.query_row("SELECT moved_to FROM amq_messages WHERE seq=?1", [np], |r| r.get::<_, i64>(0)).unwrap(), 0);
        // Past max_redeliveries, to the individual DLQ; on a DLQ already, it stays.
        let s = Settings { max_redeliveries: Some(1), individual_dlq: true, ..Default::default() };
        let t = add("topic://prices", "pending", 0, 0, &Job::default(), json!({}));
        c.execute("UPDATE amq_messages SET redelivered=2 WHERE seq=?1", [t]).unwrap();
        over_limit(&c, t, &s, 1).unwrap();
        let to: i64 = c.query_row("SELECT moved_to FROM amq_messages WHERE seq=?1", [t], |r| r.get(0)).unwrap();
        assert_eq!(c.query_row("SELECT dest FROM amq_messages WHERE seq=?1", [to], |r| r.get::<_, String>(0)).unwrap(), "queue://ActiveMQ.DLQ.Topic.prices");
        assert_eq!(dead_letter(&c, to, "dead_letter", "again", false, 2).unwrap(), None);
    }

    #[test]
    fn advisory_names_and_wildcards() {
        assert_eq!(adv_name("queue://orders"), "Queue.orders");
        assert_eq!(adv_name("topic://prices.eu"), "Topic.prices.eu");
        assert_eq!(adv_name("temp-queue://ID:x:1:1"), "TempQueue.ID:x:1:1");
        for (p, n, ok) in [
            ("topic://ActiveMQ.Advisory.Consumer.Queue.>", "topic://ActiveMQ.Advisory.Consumer.Queue.orders", true),
            ("topic://ActiveMQ.Advisory.Consumer.Queue.>", "topic://ActiveMQ.Advisory.Consumer.Queue.orders.eu", true),
            ("topic://ActiveMQ.Advisory.Consumer.Queue.>", "topic://ActiveMQ.Advisory.Consumer.Queue", false),
            ("topic://ActiveMQ.Advisory.Consumer.*.orders", "topic://ActiveMQ.Advisory.Consumer.Topic.orders", true),
            ("topic://prices.*", "topic://prices.eu.de", false),
            ("topic://prices.*", "queue://prices.eu", false),
            ("topic://prices", "topic://prices", true),
            ("topic://prices", "topic://prices.eu", false),
        ] {
            assert_eq!(topic_match(p, n), ok, "{p} vs {n}");
        }
        // An advisory's data structure survives the store (loose, nested) and comes back on dispatch.
        let info = Ds::ConsumerInfo(ConsumerInfo { consumer_id: cid(), destination: q(), prefetch: 1000, priority: 9, ..Default::default() });
        let mut w = Writer::default();
        w.nested(&mut Some(Box::new(info.clone())));
        let headers = json!({"type": "Advisory", "dataStructure": base64::engine::general_purpose::STANDARD.encode(&w.0)}).to_string();
        let r = Row { seq: 1, dest: format!("{ADV}Consumer.Queue.positions"), kind: "message".into(), message_id: String::new(), body: vec![], props: "{}".into(), headers, redelivered: 0, enqueued_at: 0 };
        let Ds::Message(m) = row_message(&r, false) else { panic!() };
        assert_eq!((m.data_structure.as_deref(), m.kind.as_deref(), m.content), (Some(&info), Some("Advisory"), None));
        assert_eq!(info.name(), "ConsumerInfo");
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
