//! Event Hubs on an AMQP 1.0 connection: senders to "<hub>" or "<hub>/Partitions/<n>" (one event
//! or a batch, format 0x80013700), receivers on "<hub>/ConsumerGroups/<group>/Partitions/<n>"
//! starting where their selector filter says, with an owner level (com.microsoft:epoch), the
//! $management node (com.microsoft:eventhub and com.microsoft:partition READ) and $cbs tokens
//! checked against the Event Hubs namespace. A connection is in Event Hubs mode once its host, a
//! token's audience or a link address names an Event Hubs namespace (or, for "localhost", a hub).

use fe2o3_amqp_types::messaging::DeliveryAnnotations;

use super::*;
use crate::eventhubs as eh;

const BATCH: u32 = 0x8001_3700;
const METRIC: &str = "com.microsoft:enable-receiver-runtime-metric";

pub(super) fn set(conn: &Conn, ctx: &Ctx, ns: &str) {
    {
        let mut g = conn.st.lock().unwrap();
        if g.eh.is_some() {
            return;
        }
        g.eh = Some((ctx.clone(), ns.to_string()));
    }
    eh::conn_open(ctx, ns, &conn.remote, conn.kill.clone());
    ctx.live.seen(conn.client.split(" :").next().unwrap_or(&conn.client), "Event Hubs");
}

fn state(conn: &Conn) -> Option<(Ctx, eh::Namespace)> {
    let (ctx, ns) = conn.st.lock().unwrap().eh.clone()?;
    let n = eh::get(&ctx, &ns)?;
    Some((ctx, n))
}

/// The connection's Event Hubs namespace, or the one this address names (the connection then
/// stays in Event Hubs mode).
fn mode(conn: &Conn, addr: &str) -> Option<(Ctx, eh::Namespace)> {
    if conn.st.lock().unwrap().eh.is_some() {
        return state(conn);
    }
    if !addr.contains("://") {
        return None;
    }
    let (c, n) = eh::resolve(&conn.envs, addr)?;
    set(conn, &c, &n.namespace);
    Some((c, n))
}

fn path_of(addr: &str) -> String {
    let a = addr.split_once("://").map(|(_, r)| r.split_once('/').map(|(_, p)| p).unwrap_or("")).unwrap_or(addr);
    a.trim_matches('/').to_string()
}

pub(super) fn cbs(conn: &Arc<Conn>, ch: u16, m: &Message<Body<Value>>, audience: &str) -> bool {
    let Some((ctx, n)) = mode(conn, audience) else { return false };
    let token = match &m.body {
        Body::Value(AmqpValue(Value::String(s))) => s.clone(),
        b => String::from_utf8_lossy(&body_bytes(b)).into(),
    };
    let ok = eh::token_ok(&ctx, &n, &token);
    *conn.authed.lock().unwrap() = ok;
    if ok {
        respond(conn, ch, m, 202, "Accepted", Value::Null);
    } else {
        respond(conn, ch, m, 401, "InvalidSignature: The token has an invalid signature.", Value::Null);
    }
    true
}

enum Kind {
    Mgmt,
    Reply,
    Send(String, Option<String>),
    Recv(String, String, String),
    Bad,
}

/// Handles the attach when it is an Event Hubs link; false leaves it to Service Bus.
pub(super) async fn attach(conn: &Arc<Conn>, ch: u16, a: &Attach, target: &str, source: &str) -> bool {
    let they_send = a.role == Role::Sender;
    let addr = if they_send { target } else { source };
    let path = path_of(addr);
    if path == "$cbs" || source == "$cbs" {
        return false;
    }
    let Some((ctx, n)) = mode(conn, if path.starts_with('$') { "" } else { addr }).or_else(|| state(conn)) else { return false };
    let segs: Vec<&str> = path.split('/').collect();
    let kind = if path.eq_ignore_ascii_case("$management") {
        if they_send { Kind::Mgmt } else { Kind::Reply }
    } else if they_send {
        match segs.as_slice() {
            [h] => Kind::Send(h.to_string(), None),
            [h, k, p] if k.eq_ignore_ascii_case("partitions") => Kind::Send(h.to_string(), Some(p.to_string())),
            _ => Kind::Bad,
        }
    } else {
        match segs.as_slice() {
            [h, k, g, k2, p] if k.eq_ignore_ascii_case("consumergroups") && k2.eq_ignore_ascii_case("partitions") => Kind::Recv(h.to_string(), g.to_string(), p.to_string()),
            _ => Kind::Bad,
        }
    };
    let ns = n.namespace.clone();
    let authed = *conn.authed.lock().unwrap() || n.any_key;
    let mut refuse: Option<AErr> = None;
    // The hub and partition a send or receive link names, checked.
    let mut hub_part: Option<(String, Option<u32>, eh::Hub)> = None;
    let check = |h: &str, p: Option<&String>| -> Result<(String, Option<u32>, eh::Hub), AErr> {
        let hub = n.hub(h).cloned().ok_or_else(|| err("amqp:not-found", &eh::not_found_text(&ns, h)))?;
        let name = n.hub_name(h).unwrap_or(h.to_string());
        let part = match p {
            None => None,
            Some(p) => match p.parse::<u32>() {
                Ok(x) if x < hub.partitions => Some(x),
                _ => return Err(err("com.microsoft:argument-out-of-range", &format!("The specified partition is invalid for an EventHub partition sender or receiver. It should be between 0 and {}.", hub.partitions - 1))),
            },
        };
        Ok((name, part, hub))
    };
    match &kind {
        Kind::Bad => refuse = Some(err("amqp:not-found", &format!("The messaging entity 'sb://{ns}.servicebus.windows.net/{path}' could not be found. To know more visit https://aka.ms/sbResourceMgrExceptions."))),
        Kind::Send(h, p) => match check(h, p.as_ref()) {
            Ok(x) => hub_part = Some(x),
            Err(e) => refuse = Some(e),
        },
        Kind::Recv(h, g, p) => match check(h, Some(p)) {
            Ok(x) if !x.2.consumer_groups.iter().any(|c| c.eq_ignore_ascii_case(g)) => refuse = Some(err("amqp:not-found", &format!("The messaging entity '{ns}:eventhub:{}~32766|{g}' could not be found. To know more visit https://aka.ms/sbResourceMgrExceptions.", x.0))),
            Ok(x) => hub_part = Some(x),
            Err(e) => refuse = Some(e),
        },
        _ => {}
    }
    if refuse.is_none() && matches!(kind, Kind::Send(..) | Kind::Recv(..)) && !authed {
        refuse = Some(err("amqp:unauthorized-access", "Unauthorized access. 'Send' or 'Listen' claim(s) are required to perform this operation: put a valid token on $cbs first."));
    }

    // A receiver joins its partition (owner level rules) before it is answered.
    let wake = Arc::new(Notify::new());
    let mut reader: Option<eh::Reader> = None;
    if let (Kind::Recv(_, g, _), Some((hub, Some(p), _)), None) = (&kind, &hub_part, &refuse) {
        let epoch = a.properties.as_ref().and_then(|x| x.iter().find(|(k, _)| k.as_str() == "com.microsoft:epoch").map(|(_, v)| v.clone())).and_then(|v| match v {
            Value::Long(n) => Some(n),
            Value::Int(n) => Some(n as i64),
            Value::Ulong(n) => Some(n as i64),
            Value::Uint(n) => Some(n as i64),
            _ => None,
        });
        match eh::join(&eh::reader_key(&ctx, &ns, hub, g, *p), epoch, &conn.client, &conn.remote, wake.clone()) {
            Ok(r) => reader = Some(r),
            Err((c, t)) => refuse = Some(err(&c, &t)),
        }
    }
    let metric = a.desired_capabilities.as_ref().map(|c| c.iter().any(|s| s.as_str() == METRIC)).unwrap_or(false);
    let reply = Attach {
        name: a.name.clone(),
        handle: a.handle.clone(),
        role: if they_send { Role::Receiver } else { Role::Sender },
        snd_settle_mode: a.snd_settle_mode.clone(),
        rcv_settle_mode: a.rcv_settle_mode.clone(),
        // Echoed even when refused: pyamqp drops a link without them before it reads the detach's error.
        source: a.source.clone(),
        target: a.target.clone(),
        unsettled: None,
        incomplete_unsettled: false,
        initial_delivery_count: if they_send { None } else { Some(0) },
        max_message_size: Some(1024 * 1024),
        offered_capabilities: if metric { Some(Array::from(vec![Symbol::from(METRIC)])) } else { None },
        desired_capabilities: None,
        properties: None,
    };
    conn.send(perf(ch, &Performative::Attach(reply)));
    if let Some(e) = refuse {
        conn.send(perf(ch, &Performative::Detach(Detach { handle: a.handle.clone(), closed: true, error: Some(e) })));
        return true;
    }
    let node = match &kind {
        Kind::Mgmt => Node::EhMgmt,
        Kind::Reply => Node::Reply(target.to_string()),
        Kind::Send(..) => Node::EhSend(hub_part.as_ref().map(|x| x.0.clone()).unwrap_or_default(), hub_part.as_ref().and_then(|x| x.1)),
        _ => Node::EhRecv,
    };
    let initial = a.initial_delivery_count.unwrap_or(0);
    conn.st.lock().unwrap().links.insert((ch, a.handle.0), Link {
        name: a.name.clone(),
        node,
        we_send: !they_send,
        settled_mode: a.snd_settle_mode == SenderSettleMode::Settled,
        credit: 0,
        delivery_count: if they_send { initial } else { 0 },
        drain: false,
        received: 0,
        consumer: String::new(),
        wake: wake.clone(),
        session: None,
    });
    if they_send {
        conn.flow(ch, a.handle.0, 1000, initial, false);
    }
    if let (Some(r), Some((hub, Some(p), _))) = (reader, hub_part) {
        let sel = a.source.as_ref().and_then(|s| s.filter.as_ref()).and_then(|f| f.iter().find(|(k, _)| k.as_str().contains("selector-filter")).map(|(_, v)| v.clone()));
        let text = match sel {
            Some(Value::Described(d)) => match d.value { Value::String(s) => Some(s), _ => None },
            Some(Value::String(s)) => Some(s),
            _ => None,
        };
        let from = text.as_deref().and_then(eh::parse_selector).unwrap_or(eh::From::Start);
        let start = eh::start_seq(&ctx, &ns, &hub, p, &from).await.unwrap_or(0);
        tokio::spawn(deliver(conn.clone(), ch, a.handle.0, ctx, ns, hub, p, start, r, a.snd_settle_mode == SenderSettleMode::Settled, metric));
    }
    true
}

/// The stored event as the receiver gets it: the app's message with the x-opt annotations.
fn message(e: &eh::Event, info: Option<&eh::Part>) -> Vec<u8> {
    let mut m = decode(&e.raw).unwrap_or_else(|| Message {
        header: None,
        delivery_annotations: None,
        message_annotations: None,
        properties: None,
        application_properties: None,
        body: Body::Data(Batch::new(vec![Data(Binary::from(e.body.clone()))])),
        footer: None,
    });
    let mut a = m.message_annotations.take().map(|x| x.0).unwrap_or_default();
    a.insert(OwnedKey::Symbol(Symbol::from("x-opt-sequence-number")), Value::Long(e.seq));
    a.insert(OwnedKey::Symbol(Symbol::from("x-opt-offset")), Value::String(e.offset.to_string()));
    a.insert(OwnedKey::Symbol(Symbol::from("x-opt-enqueued-time")), Value::Timestamp(Timestamp::from_milliseconds(e.at)));
    if !e.pkey.is_empty() {
        a.insert(OwnedKey::Symbol(Symbol::from("x-opt-partition-key")), Value::String(e.pkey.clone()));
    }
    m.message_annotations = Some(MessageAnnotations(a));
    m.delivery_annotations = info.map(|i| {
        let mut d: OrderedMap<OwnedKey, Value> = OrderedMap::new();
        d.insert(OwnedKey::Symbol(Symbol::from("last_enqueued_sequence_number")), Value::Long(i.last_enqueued_sequence_number));
        d.insert(OwnedKey::Symbol(Symbol::from("last_enqueued_offset")), Value::String(i.last_enqueued_offset.clone()));
        d.insert(OwnedKey::Symbol(Symbol::from("last_enqueued_time_utc")), Value::Timestamp(Timestamp::from_milliseconds(i.last_enqueued_time_utc)));
        d.insert(OwnedKey::Symbol(Symbol::from("runtime_info_retrieval_time_utc")), Value::Timestamp(Timestamp::from_milliseconds(util::now_ms())));
        DeliveryAnnotations(d)
    });
    encode(m)
}

/// Sends a partition's events on one receive link while the app gives credit.
#[allow(clippy::too_many_arguments)]
async fn deliver(conn: Arc<Conn>, ch: u16, handle: u32, ctx: Ctx, ns: String, hub: String, part: u32, mut cur: i64, r: eh::Reader, settled: bool, metric: bool) {
    let bell = eh::wake(&ctx);
    loop {
        let (credit, drain, alive) = {
            let g = conn.st.lock().unwrap();
            match g.links.get(&(ch, handle)) {
                Some(l) => (l.credit, l.drain, true),
                None => (0, false, false),
            }
        };
        if !alive || conn.tx.is_closed() || ctx.state.gone() {
            break;
        }
        let kicked = r.kick.lock().unwrap().clone();
        if let Some((cond, text)) = kicked {
            conn.send(perf(ch, &Performative::Detach(Detach { handle: Handle(handle), closed: true, error: Some(err(&cond, &text)) })));
            conn.st.lock().unwrap().links.remove(&(ch, handle));
            break;
        }
        if credit == 0 {
            tokio::select! { _ = r.wake.notified() => {} _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {} }
            continue;
        }
        let evs = eh::read(&ctx, &ns, &hub, part, cur, credit.min(100) as usize).await.unwrap_or_default();
        if !evs.is_empty() { stuntman_core::usage::hit(&ctx, "queues"); }
        if evs.is_empty() {
            if drain {
                let dc = {
                    let mut g = conn.st.lock().unwrap();
                    let Some(l) = g.links.get_mut(&(ch, handle)) else { break };
                    l.delivery_count = l.delivery_count.wrapping_add(l.credit);
                    l.credit = 0;
                    l.drain = false;
                    l.delivery_count
                };
                conn.flow(ch, handle, 0, dc, true);
            } else {
                tokio::select! { _ = bell.notified() => {} _ = r.wake.notified() => {} _ = tokio::time::sleep(std::time::Duration::from_millis(1000)) => {} }
            }
            continue;
        }
        let info = if metric { eh::part(&ctx, &ns, &hub, part).await.ok() } else { None };
        for e in evs {
            conn.transfer(ch, handle, e.seq.to_be_bytes().to_vec(), settled, message(&e, info.as_ref()));
            let mut g = conn.st.lock().unwrap();
            if let Some(l) = g.links.get_mut(&(ch, handle)) {
                l.credit = l.credit.saturating_sub(1);
                l.delivery_count = l.delivery_count.wrapping_add(1);
            }
            cur = e.seq + 1;
            r.pos.store(e.seq, std::sync::atomic::Ordering::Relaxed);
        }
    }
    eh::leave(r.id);
}

fn partition_key(m: &Message<Body<Value>>) -> Option<String> {
    match ann(m, "x-opt-partition-key") {
        Some(Value::String(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

pub(super) async fn publish(conn: &Arc<Conn>, ch: u16, p: &Partial, msg: Message<Body<Value>>, hub: &str, part: Option<u32>) {
    let state = match state(conn) {
        None => DeliveryState::Rejected(Rejected { error: Some(err("amqp:unauthorized-access", "Put a token on $cbs first")) }),
        Some((ctx, n)) => match accept(conn, &ctx, &n, p, msg, hub, part).await {
            Ok(()) => DeliveryState::Accepted(Accepted {}),
            // info written out: pyamqp reads the error's three fields by position.
            Err((c, t)) => DeliveryState::Rejected(Rejected { error: Some(AErr { info: Some(Default::default()), ..err(c, &t) }) }),
        },
    };
    reply_state(conn, ch, p.delivery_id, p.settled, state);
}

async fn accept(conn: &Conn, ctx: &Ctx, n: &eh::Namespace, p: &Partial, msg: Message<Body<Value>>, hub: &str, part: Option<u32>) -> Result<(), (&'static str, String)> {
    stuntman_core::usage::hit(ctx, "queues");
    let h = n.hub(hub).cloned().ok_or_else(|| ("amqp:not-found", eh::not_found_text(&n.namespace, hub)))?;
    if n.provoke.slow_sends_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(n.provoke.slow_sends_ms)).await;
    }
    if let Some(r) = eh::send_refusal(ctx, &n.namespace, false) {
        return Err(r);
    }
    // A batch carries each event, encoded, in a Data section of its own.
    let inner: Vec<(Vec<u8>, Message<Body<Value>>)> = match (&msg.body, p.format) {
        (Body::Data(b), BATCH) => b.iter().filter_map(|d| decode(&d.0).map(|m| (d.0.to_vec(), m))).collect(),
        _ => vec![(p.payload.clone(), msg.clone())],
    };
    let key = partition_key(&msg).or_else(|| inner.first().and_then(|(_, m)| partition_key(m))).unwrap_or_default();
    let target = part.unwrap_or_else(|| eh::pick_partition(&h, &key));
    let evs = inner.into_iter().map(|(raw, m)| {
        let o = outgoing(&m, &conn.client);
        let mut sys = o.sys;
        sys.remove("partitionKey");
        if !o.message_id.is_empty() { sys.insert("messageId".into(), json!(o.message_id)); }
        if !o.subject.is_empty() { sys.insert("subject".into(), json!(o.subject)); }
        if !o.content_type.is_empty() { sys.insert("contentType".into(), json!(o.content_type)); }
        eh::New { raw, body: o.body, props: o.props, sys, pkey: key.clone(), sender: conn.client.clone(), via: "amqp".into() }
    }).collect();
    eh::append(ctx, &n.namespace, &n.hub_name(hub).unwrap_or(hub.to_string()), target, evs).await.map(|_| ()).map_err(|e| ("amqp:internal-error", e.message))
}

pub(super) async fn management(conn: &Arc<Conn>, ch: u16, m: Message<Body<Value>>) {
    let Some((ctx, n)) = state(conn) else {
        respond(conn, ch, &m, 401, "Unauthorized", Value::Null);
        return;
    };
    let token = app_prop(&m, "security_token");
    if !(*conn.authed.lock().unwrap() || token.map(|t| eh::token_ok(&ctx, &n, &t)).unwrap_or(false) || n.any_key) {
        respond(conn, ch, &m, 401, "InvalidSignature: The token has an invalid signature.", Value::Null);
        return;
    }
    if let Some((_, t)) = eh::send_refusal(&ctx, &n.namespace, true) {
        respond(conn, ch, &m, 503, &format!("ServerBusy: {t}"), Value::Null);
        return;
    }
    let name = app_prop(&m, "name").unwrap_or_default();
    let Some(h) = n.hub(&name).cloned() else {
        respond(conn, ch, &m, 404, &eh::not_found_text(&n.namespace, &name), Value::Null);
        return;
    };
    let hub = n.hub_name(&name).unwrap_or(name);
    match app_prop(&m, "type").unwrap_or_default().as_str() {
        "com.microsoft:eventhub" => {
            let ids: Vec<Value> = (0..h.partitions).map(|p| Value::String(p.to_string())).collect();
            respond(conn, ch, &m, 200, "OK", vmap(vec![
                ("name", Value::String(hub)),
                ("type", Value::String("com.microsoft:eventhub".into())),
                ("created_at", Value::Timestamp(Timestamp::from_milliseconds(h.created_at))),
                ("partition_count", Value::Int(h.partitions as i32)),
                ("partition_ids", Value::Array(Array::from(ids))),
            ]));
        }
        "com.microsoft:partition" => {
            let p = app_prop(&m, "partition").unwrap_or_default();
            let Some(pid) = p.parse::<u32>().ok().filter(|x| *x < h.partitions) else {
                respond(conn, ch, &m, 400, &format!("ArgumentOutOfRange: the partition is invalid; it should be between 0 and {}.", h.partitions - 1), Value::Null);
                return;
            };
            let i = eh::part(&ctx, &n.namespace, &hub, pid).await.unwrap_or_default();
            respond(conn, ch, &m, 200, "OK", vmap(vec![
                ("name", Value::String(hub)),
                ("type", Value::String("com.microsoft:partition".into())),
                ("partition", Value::String(p)),
                ("begin_sequence_number", Value::Long(i.begin_sequence_number)),
                ("last_enqueued_sequence_number", Value::Long(i.last_enqueued_sequence_number)),
                ("last_enqueued_offset", Value::String(i.last_enqueued_offset)),
                ("last_enqueued_time_utc", Value::Timestamp(Timestamp::from_milliseconds(i.last_enqueued_time_utc))),
                ("is_partition_empty", Value::Bool(i.is_empty)),
            ]));
        }
        other => respond(conn, ch, &m, 400, &format!("Stuntman's Event Hubs management answers com.microsoft:eventhub and com.microsoft:partition, not {other}"), Value::Null),
    }
}
