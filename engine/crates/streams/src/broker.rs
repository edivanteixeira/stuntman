//! A one-node Kafka broker (A17) that clients such as librdkafka (Confluent.Kafka), the Java client
//! and kafka-python talk to: ApiVersions, Metadata, Produce, Fetch, ListOffsets, FindCoordinator,
//! JoinGroup, SyncGroup, Heartbeat, LeaveGroup, OffsetCommit, OffsetFetch, InitProducerId,
//! CreateTopics, SaslHandshake and SaslAuthenticate (PLAIN, SCRAM-SHA-256, SCRAM-SHA-512).
//! Only non-flexible request versions are offered, which every client still speaks. One port for
//! every environment: the default one answers.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use stuntman_core::{env::Envs, util, Ctx};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{config, store, wire::{read_batches, write_batch, R, Res, W}};

/// (api key, min, max) offered.
pub const APIS: [(i16, i16, i16); 16] = [(0, 3, 8), (1, 4, 11), (2, 1, 5), (3, 1, 8), (8, 2, 7), (9, 1, 5), (10, 0, 2), (11, 0, 5), (12, 0, 3), (13, 0, 3), (14, 0, 3),
    (17, 0, 1), (18, 0, 2), (19, 0, 4), (22, 0, 1), (36, 0, 1)];

#[derive(Clone, Debug, Default)]
pub struct Member {
    pub id: String,
    pub client: String,
    pub host: String,
    pub protocols: Vec<(String, Vec<u8>)>,
    pub assignment: Vec<u8>,
    pub seen: Option<Instant>,
    pub session_ms: i32,
    pub round: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Group {
    pub generation: i32,
    /// Empty | PreparingRebalance | CompletingRebalance | Stable
    pub state: String,
    pub leader: String,
    pub protocol: String,
    pub protocol_type: String,
    pub members: BTreeMap<String, Member>,
    pub deadline: Option<Instant>,
    pub round: u64,
}

fn groups() -> &'static Mutex<HashMap<String, Group>> {
    static G: OnceLock<Mutex<HashMap<String, Group>>> = OnceLock::new();
    G.get_or_init(Default::default)
}

fn key(ctx: &Ctx, g: &str) -> String {
    format!("{}/{g}", ctx.env)
}

pub fn group_view(ctx: &Ctx, g: &str) -> Option<Group> {
    groups().lock().unwrap().get(&key(ctx, g)).cloned()
}

pub fn group_names(ctx: &Ctx) -> Vec<String> {
    let p = format!("{}/", ctx.env);
    groups().lock().unwrap().keys().filter_map(|k| k.strip_prefix(&p).map(String::from)).collect()
}

/// Starts a rebalance: members get REBALANCE_IN_PROGRESS on their next heartbeat and join again.
pub fn rebalance(ctx: &Ctx, g: &str) -> bool {
    let mut gs = groups().lock().unwrap();
    match gs.get_mut(&key(ctx, g)) {
        Some(x) if !x.members.is_empty() => {
            x.state = "PreparingRebalance".into();
            x.deadline = Some(Instant::now() + Duration::from_millis(3000));
            x.round += 1;
            true
        }
        _ => false,
    }
}

struct Conn {
    ctx: Ctx,
    authed: bool,
    mech: String,
    scram: Option<(String, String, String, Vec<u8>)>,
    client: String,
    peer: String,
    user: String,
}

fn hmac(alg: &str, key: &[u8], msg: &[u8]) -> Vec<u8> {
    use hmac::Mac;
    if alg.ends_with("512") {
        let mut m = hmac::Hmac::<sha2::Sha512>::new_from_slice(key).unwrap();
        m.update(msg);
        m.finalize().into_bytes().to_vec()
    } else {
        let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(key).unwrap();
        m.update(msg);
        m.finalize().into_bytes().to_vec()
    }
}

fn hash(alg: &str, d: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    if alg.ends_with("512") { sha2::Sha512::digest(d).to_vec() } else { sha2::Sha256::digest(d).to_vec() }
}

fn pbkdf2(alg: &str, pw: &[u8], salt: &[u8], iters: u32) -> Vec<u8> {
    let mut u = hmac(alg, pw, &[salt, &1u32.to_be_bytes()].concat());
    let mut out = u.clone();
    for _ in 1..iters {
        u = hmac(alg, pw, &u);
        for (o, x) in out.iter_mut().zip(u.iter()) {
            *o ^= x;
        }
    }
    out
}

fn b64(d: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(d)
}

fn unb64(s: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).unwrap_or_default()
}

impl Conn {
    fn provoke(&self) -> String {
        config::load(&self.ctx).provoke
    }

    async fn ensure_topic(&self, t: &str, auto: bool) -> Option<i32> {
        if let Ok(Some(p)) = store::topic(&self.ctx, t).await {
            return Some(p);
        }
        if auto && config::load(&self.ctx).auto_create {
            let n = config::load(&self.ctx).default_partitions.max(1);
            let _ = store::create_topic(&self.ctx, t, n).await;
            return Some(n);
        }
        None
    }

    /// SaslAuthenticate: PLAIN in one step, SCRAM in two.
    fn sasl(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let cfg = config::load(&self.ctx);
        if cfg.provoke == "sasl_refused" {
            return Err("Authentication failed: Invalid username or password (provoked in Stuntman)".into());
        }
        let pw_of = |u: &str| -> Option<String> {
            if cfg.sasl.users.is_empty() { return None }
            cfg.sasl.users.iter().find(|x| x.user == u).map(|x| util::resolve_secret(&x.password, &self.ctx.data_dir))
        };
        if self.mech == "PLAIN" {
            let p: Vec<&[u8]> = data.split(|b| *b == 0).collect();
            let (u, pw) = (String::from_utf8_lossy(p.get(1).unwrap_or(&&b""[..])).to_string(), String::from_utf8_lossy(p.get(2).unwrap_or(&&b""[..])).to_string());
            let ok = match pw_of(&u) { Some(want) => want == pw, None => cfg.sasl.users.is_empty() && !pw.is_empty() && pw != "wrong" };
            if !ok { return Err("Authentication failed: Invalid username or password".into()) }
            self.user = u;
            self.authed = true;
            return Ok(vec![]);
        }
        // SCRAM-SHA-256 / -512.
        let text = String::from_utf8_lossy(data).to_string();
        match self.scram.take() {
            None => {
                let bare = text.splitn(3, ',').nth(2).unwrap_or("").to_string();
                let attrs: HashMap<&str, &str> = bare.split(',').filter_map(|kv| kv.split_once('=')).collect();
                let user = attrs.get("n").copied().unwrap_or("").to_string();
                let cnonce = attrs.get("r").copied().unwrap_or("").to_string();
                let snonce = format!("{cnonce}{}", util::uuid().replace('-', ""));
                let salt: Vec<u8> = util::uuid().into_bytes()[..16].to_vec();
                let first = format!("r={snonce},s={},i=4096", b64(&salt));
                self.scram = Some((user, bare, first.clone(), salt));
                Ok(first.into_bytes())
            }
            Some((user, bare, first, salt)) => {
                let (without_proof, proof) = text.rsplit_once(",p=").ok_or("SCRAM: no proof")?;
                let pw = match pw_of(&user) { Some(p) => p, None if cfg.sasl.users.is_empty() => String::new(), None => return Err("Authentication failed: Invalid username or password".into()) };
                let alg = self.mech.clone();
                let auth_msg = format!("{bare},{first},{without_proof}");
                let check = |pw: &str| -> bool {
                    let salted = pbkdf2(&alg, pw.as_bytes(), &salt, 4096);
                    let ck = hmac(&alg, &salted, b"Client Key");
                    let sig = hmac(&alg, &hash(&alg, &ck), auth_msg.as_bytes());
                    let mine: Vec<u8> = ck.iter().zip(sig.iter()).map(|(a, b)| a ^ b).collect();
                    mine == unb64(proof)
                };
                // Without configured users any password but "wrong" works: accept the proof the
                // client could only make from the password it used, unless it is "wrong".
                let ok = if pw.is_empty() { !check("wrong") } else { check(&pw) };
                if !ok { return Err("Authentication failed during authentication due to invalid credentials with SASL mechanism SCRAM".into()) }
                self.user = user;
                self.authed = true;
                let salted = pbkdf2(&alg, if pw.is_empty() { b"" } else { pw.as_bytes() }, &salt, 4096);
                let server_sig = hmac(&alg, &hmac(&alg, &salted, b"Server Key"), auth_msg.as_bytes());
                Ok(format!("v={}", b64(&server_sig)).into_bytes())
            }
        }
    }

    async fn handle(&mut self, api: i16, v: i16, r: &mut R<'_>) -> Res<Option<Vec<u8>>> {
        let ctx = self.ctx.clone();
        let cfg = config::load(&ctx);
        let mut w = W::default();
        let host = cfg.advertised_host.clone();
        let port = crate::kafka_port() as i32;
        let lna = cfg.provoke == "leader_not_available";
        if cfg.sasl.required && !self.authed && ![17, 18, 36].contains(&api) {
            return Err("SASL authentication required".into());
        }
        match api {
            18 => {
                // ApiVersions (v0-2; a higher version gets error 35 in the v0 shape).
                let unsupported = v > 2;
                w.i16(if unsupported { 35 } else { 0 });
                w.arr(&APIS, |w, (k, a, b)| { w.i16(*k).i16(*a).i16(*b); });
                if v >= 1 && !unsupported { w.i32(0); }
            }
            17 => {
                let m = r.str()?;
                let ok = cfg.sasl.mechanisms.iter().any(|x| x.eq_ignore_ascii_case(&m));
                self.mech = m.to_uppercase();
                w.i16(if ok { 0 } else { 33 });
                w.arr(&cfg.sasl.mechanisms, |w, x| { w.str(x); });
            }
            36 => {
                let data = r.bytes()?.unwrap_or(&[]).to_vec();
                match self.sasl(&data) {
                    Ok(out) => { w.i16(0).nstr(None).bytes(&out); }
                    Err(e) => { w.i16(58).nstr(Some(&e)).bytes(&[]); }
                }
                if v >= 1 { w.i64(if self.authed { 3_600_000 } else { 0 }); }
            }
            3 => {
                // Metadata.
                let n = r.count()?;
                let mut want: Vec<String> = vec![];
                for _ in 0..n.max(0) { want.push(r.str()?); }
                let auto = if v >= 4 { r.i8()? != 0 } else { true };
                let all = n < 0 || (n == 0 && v == 0);
                let mut topics: Vec<(String, Option<i32>)> = vec![];
                if all {
                    for (t, p) in store::topics(&ctx).await.map_err(|e| e.message)? { topics.push((t, Some(p))); }
                } else {
                    for t in want { let p = self.ensure_topic(&t, auto).await; topics.push((t, p)); }
                }
                if v >= 3 { w.i32(0); }
                w.i32(1).i32(0).str(&host).i32(port);
                if v >= 1 { w.nstr(None); }
                if v >= 2 { w.nstr(Some("stuntman-kafka")); }
                if v >= 1 { w.i32(0); }
                w.i32(topics.len() as i32);
                for (t, p) in &topics {
                    w.i16(match p { None => 3, Some(_) if lna => 5, _ => 0 }).str(t);
                    if v >= 1 { w.bool(false); }
                    let parts = p.unwrap_or(0);
                    w.i32(parts);
                    for i in 0..parts {
                        w.i16(if lna { 5 } else { 0 }).i32(i).i32(if lna { -1 } else { 0 });
                        if v >= 7 { w.i32(0); }
                        w.i32(1).i32(0).i32(1).i32(0);
                        if v >= 5 { w.i32(0); }
                    }
                    if v >= 8 { w.i32(-2147483648); }
                }
                if v >= 8 { w.i32(-2147483648); }
            }
            0 => {
                // Produce.
                if v >= 3 { let _ = r.nstr()?; }
                let acks = r.i16()?;
                let _timeout = r.i32()?;
                let nt = r.count()?;
                let mut out: Vec<(String, Vec<(i32, i16, i64, String)>)> = vec![];
                for _ in 0..nt {
                    let t = r.str()?;
                    let np = r.count()?;
                    let mut ps = vec![];
                    let parts = self.ensure_topic(&t, true).await;
                    for _ in 0..np {
                        let idx = r.i32()?;
                        let data = r.bytes()?.unwrap_or(&[]).to_vec();
                        let res = match (parts, lna) {
                            (_, true) => (5, -1, String::from("leader not available (provoked)")),
                            (None, _) => (3, -1, String::from("unknown topic")),
                            (Some(p), _) if idx >= p => (3, -1, String::from("unknown partition")),
                            _ => match read_batches(&data) {
                                Err(e) => (if e.contains("compression") { 76 } else { 2 }, -1, e),
                                Ok(mut recs) => {
                                    if cfg.provoke == "duplicate" { let again = recs.clone(); recs.extend(again); }
                                    match store::append(&ctx, &t, idx, recs, &self.client).await { Ok(b) => (0, b, String::new()), Err(e) => (2, -1, e.message) }
                                }
                            },
                        };
                        ps.push((idx, res.0, res.1, res.2));
                    }
                    out.push((t, ps));
                }
                if acks == 0 { return Ok(None); }
                w.i32(out.len() as i32);
                for (t, ps) in &out {
                    w.str(t).i32(ps.len() as i32);
                    for (i, e, b, msg) in ps {
                        w.i32(*i).i16(*e).i64(*b);
                        if v >= 2 { w.i64(-1); }
                        if v >= 5 { w.i64(0); }
                        if v >= 8 { w.i32(0).nstr(if msg.is_empty() { None } else { Some(msg) }); }
                    }
                }
                w.i32(0);
            }
            1 => {
                // Fetch.
                let _replica = r.i32()?;
                let max_wait = r.i32()?;
                let _min = r.i32()?;
                let max_bytes = if v >= 3 { r.i32()? } else { i32::MAX };
                if v >= 4 { r.i8()?; }
                let (sid, _se) = if v >= 7 { (r.i32()?, r.i32()?) } else { (0, 0) };
                let nt = r.count()?;
                let mut asks: Vec<(String, Vec<(i32, i64, i32)>)> = vec![];
                for _ in 0..nt {
                    let t = r.str()?;
                    let np = r.count()?;
                    let mut ps = vec![];
                    for _ in 0..np {
                        let p = r.i32()?;
                        if v >= 9 { r.i32()?; }
                        let off = r.i64()?;
                        if v >= 5 { r.i64()?; }
                        let pmax = r.i32()?;
                        ps.push((p, off, pmax));
                    }
                    asks.push((t, ps));
                }
                if cfg.provoke == "slow_fetch" { tokio::time::sleep(Duration::from_secs(3)).await; }
                // Long poll: wait up to max_wait for something to read.
                let end = Instant::now() + Duration::from_millis(max_wait.clamp(0, 5000) as u64);
                loop {
                    let mut any = false;
                    for (t, ps) in &asks {
                        for (p, off, _) in ps {
                            if let Ok((_, hw)) = store::marks(&ctx, t, *p).await { if hw > *off { any = true } }
                        }
                    }
                    if any || Instant::now() >= end { break }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                if v >= 1 { w.i32(0); }
                if v >= 7 { w.i16(0).i32(sid); }
                w.i32(asks.len() as i32);
                let mut budget = max_bytes.max(1024) as usize;
                for (t, ps) in &asks {
                    w.str(t).i32(ps.len() as i32);
                    let known = store::topic(&ctx, t).await.ok().flatten();
                    for (p, off, pmax) in ps {
                        let (start, hw) = store::marks(&ctx, t, *p).await.unwrap_or((0, 0));
                        let err: i16 = if known.is_none() { 3 } else if lna { 5 } else if *off > hw || (*off < start && hw > 0) { 1 } else { 0 };
                        let recs = if err == 0 { store::read(&ctx, t, *p, *off, (*pmax as usize).min(budget)).await.unwrap_or_default() } else { vec![] };
                        let batch = write_batch(&recs);
                        budget = budget.saturating_sub(batch.len());
                        w.i32(*p).i16(err).i64(hw);
                        if v >= 4 { w.i64(hw); }
                        if v >= 5 { w.i64(start); }
                        if v >= 4 { w.i32(-1); }
                        if v >= 11 { w.i32(-1); }
                        w.bytes(&batch);
                    }
                }
            }
            2 => {
                // ListOffsets.
                r.i32()?;
                if v >= 2 { r.i8()?; }
                let nt = r.count()?;
                let mut out = vec![];
                for _ in 0..nt {
                    let t = r.str()?;
                    let np = r.count()?;
                    let mut ps = vec![];
                    for _ in 0..np {
                        let p = r.i32()?;
                        if v >= 4 { r.i32()?; }
                        let ts = r.i64()?;
                        let (start, hw) = store::marks(&ctx, &t, p).await.unwrap_or((0, 0));
                        let (off, at) = match ts { -1 => (hw, -1), -2 => (start, -1), x => store::offset_for_time(&ctx, &t, p, x).await.ok().flatten().unwrap_or((hw, -1)) };
                        ps.push((p, off, at));
                    }
                    out.push((t, ps));
                }
                if v >= 2 { w.i32(0); }
                w.i32(out.len() as i32);
                for (t, ps) in &out {
                    w.str(t).i32(ps.len() as i32);
                    for (p, off, at) in ps {
                        w.i32(*p).i16(0).i64(*at).i64(*off);
                        if v >= 4 { w.i32(0); }
                    }
                }
            }
            10 => {
                let _key = r.str()?;
                if v >= 1 { w.i32(0); }
                w.i16(0);
                if v >= 1 { w.nstr(None); }
                w.i32(0).str(&host).i32(port);
            }
            11 => {
                // JoinGroup.
                let gid = r.str()?;
                let session = r.i32()?;
                if v >= 1 { r.i32()?; }
                let mut mid = r.str()?;
                if v >= 5 { r.nstr()?; }
                let ptype = r.str()?;
                let np = r.count()?;
                let mut protos = vec![];
                for _ in 0..np { let n = r.str()?; let m = r.bytes()?.unwrap_or(&[]).to_vec(); protos.push((n, m)); }
                if mid.is_empty() { mid = format!("{}-{}", if self.client.is_empty() { "consumer" } else { &self.client }, util::uuid()); }
                let k = key(&ctx, &gid);
                let round = {
                    let mut gs = groups().lock().unwrap();
                    let g = gs.entry(k.clone()).or_default();
                    if g.state != "PreparingRebalance" {
                        // Others in the group get until their next heartbeat (3 s) to join again.
                        let others = g.members.keys().any(|k| k != &mid);
                        g.state = "PreparingRebalance".into();
                        g.deadline = Some(Instant::now() + Duration::from_millis(if others { 3500 } else { 300 }));
                        g.round += 1;
                    }
                    g.protocol_type = ptype;
                    let round = g.round;
                    g.members.insert(mid.clone(), Member { id: mid.clone(), client: self.client.clone(), host: self.peer.clone(), protocols: protos, assignment: vec![], seen: Some(Instant::now()), session_ms: session, round });
                    round
                };
                let _ = round;
                // Wait for the round to close, then the first to see it completes the rebalance.
                let g = loop {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let mut gs = groups().lock().unwrap();
                    let g = gs.get_mut(&k).unwrap();
                    let all_in = g.members.values().all(|m| m.round == g.round);
                    if g.state == "PreparingRebalance" && (all_in || g.deadline.map(|d| Instant::now() >= d).unwrap_or(true)) {
                        let r = g.round;
                        g.members.retain(|_, m| m.round == r);
                        g.generation += 1;
                        g.leader = g.members.keys().next().cloned().unwrap_or_default();
                        g.protocol = g.members.values().next().and_then(|m| m.protocols.first().map(|p| p.0.clone())).unwrap_or_default();
                        g.state = "CompletingRebalance".into();
                        for m in g.members.values_mut() { m.assignment.clear(); }
                    }
                    if g.state != "PreparingRebalance" { break g.clone() }
                };
                ctx.live.send("streams.groups", &gid, "updated");
                if v >= 2 { w.i32(0); }
                let known = g.members.contains_key(&mid);
                w.i16(if known { 0 } else { 25 }).i32(g.generation).str(&g.protocol).str(&g.leader).str(&mid);
                let members: Vec<&Member> = if g.leader == mid { g.members.values().collect() } else { vec![] };
                w.i32(members.len() as i32);
                for m in members {
                    w.str(&m.id);
                    if v >= 5 { w.nstr(None); }
                    let meta = m.protocols.iter().find(|p| p.0 == g.protocol).map(|p| p.1.clone()).unwrap_or_default();
                    w.bytes(&meta);
                }
            }
            14 => {
                // SyncGroup.
                let gid = r.str()?;
                let gen = r.i32()?;
                let mid = r.str()?;
                if v >= 3 { r.nstr()?; }
                let n = r.count()?;
                let mut asg = vec![];
                for _ in 0..n { let m = r.str()?; let a = r.bytes()?.unwrap_or(&[]).to_vec(); asg.push((m, a)); }
                let k = key(&ctx, &gid);
                let mut err: i16 = 0;
                {
                    let mut gs = groups().lock().unwrap();
                    match gs.get_mut(&k) {
                        None => err = 25,
                        Some(g) if g.generation != gen => err = 22,
                        Some(g) if !g.members.contains_key(&mid) => err = 25,
                        Some(g) if g.leader == mid => {
                            for (m, a) in asg { if let Some(x) = g.members.get_mut(&m) { x.assignment = a; } }
                            g.state = "Stable".into();
                        }
                        _ => {}
                    }
                }
                let mut assignment = vec![];
                if err == 0 {
                    let end = Instant::now() + Duration::from_secs(10);
                    loop {
                        {
                            let gs = groups().lock().unwrap();
                            let g = gs.get(&k).unwrap();
                            if g.state == "Stable" { assignment = g.members.get(&mid).map(|m| m.assignment.clone()).unwrap_or_default(); break }
                            if g.state == "PreparingRebalance" || g.generation != gen { err = 27; break }
                        }
                        if Instant::now() > end { err = 27; break }
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                }
                ctx.live.send("streams.groups", &gid, "updated");
                if v >= 1 { w.i32(0); }
                w.i16(err).bytes(&assignment);
            }
            12 => {
                // Heartbeat.
                let gid = r.str()?;
                let gen = r.i32()?;
                let mid = r.str()?;
                let mut gs = groups().lock().unwrap();
                let err: i16 = match gs.get_mut(&key(&ctx, &gid)) {
                    None => 25,
                    Some(g) => match g.members.get_mut(&mid) {
                        None => 25,
                        Some(m) => {
                            m.seen = Some(Instant::now());
                            if g.state == "PreparingRebalance" { 27 } else if g.generation != gen { 22 } else { 0 }
                        }
                    },
                };
                if v >= 1 { w.i32(0); }
                w.i16(err);
            }
            13 => {
                // LeaveGroup.
                let gid = r.str()?;
                let mut ids = vec![];
                if v >= 3 { let n = r.count()?; for _ in 0..n { ids.push(r.str()?); r.nstr()?; } } else { ids.push(r.str()?); }
                {
                    let mut gs = groups().lock().unwrap();
                    if let Some(g) = gs.get_mut(&key(&ctx, &gid)) {
                        for id in &ids { g.members.remove(id); }
                        if !g.members.is_empty() {
                            g.state = "PreparingRebalance".into();
                            g.deadline = Some(Instant::now() + Duration::from_millis(3000));
                            g.round += 1;
                        } else {
                            g.state = "Empty".into();
                        }
                    }
                }
                ctx.live.send("streams.groups", &gid, "updated");
                if v >= 1 { w.i32(0); }
                w.i16(0);
                if v >= 3 { w.arr(&ids, |w, id| { w.str(id).nstr(None).i16(0); }); }
            }
            8 => {
                // OffsetCommit.
                let gid = r.str()?;
                let _gen = r.i32()?;
                let _mid = r.str()?;
                if v >= 7 { r.nstr()?; }
                if (2..=4).contains(&v) { r.i64()?; }
                let nt = r.count()?;
                let mut out = vec![];
                for _ in 0..nt {
                    let t = r.str()?;
                    let np = r.count()?;
                    let mut ps = vec![];
                    for _ in 0..np {
                        let p = r.i32()?;
                        let off = r.i64()?;
                        if v >= 6 { r.i32()?; }
                        let meta = r.nstr()?.unwrap_or_default();
                        let e = if store::commit(&ctx, &gid, &t, p, off, &meta).await.is_ok() { 0 } else { 2 };
                        ps.push((p, e));
                    }
                    out.push((t, ps));
                }
                if v >= 3 { w.i32(0); }
                w.i32(out.len() as i32);
                for (t, ps) in &out { w.str(t).arr(ps, |w, (p, e)| { w.i32(*p).i16(*e); }); }
            }
            9 => {
                // OffsetFetch.
                let gid = r.str()?;
                let nt = r.count()?;
                let mut asks: Vec<(String, Vec<i32>)> = vec![];
                for _ in 0..nt.max(0) {
                    let t = r.str()?;
                    let np = r.count()?;
                    let mut ps = vec![];
                    for _ in 0..np { ps.push(r.i32()?); }
                    asks.push((t, ps));
                }
                let have = store::committed(&ctx, &gid).await.unwrap_or_default();
                if nt < 0 {
                    let mut m: BTreeMap<String, Vec<i32>> = BTreeMap::new();
                    for (t, p, _, _) in &have { m.entry(t.clone()).or_default().push(*p); }
                    asks = m.into_iter().collect();
                }
                if v >= 3 { w.i32(0); }
                w.i32(asks.len() as i32);
                for (t, ps) in &asks {
                    w.str(t).i32(ps.len() as i32);
                    for p in ps {
                        let c = have.iter().find(|x| &x.0 == t && x.1 == *p);
                        w.i32(*p).i64(c.map(|x| x.2).unwrap_or(-1));
                        if v >= 5 { w.i32(-1); }
                        w.nstr(Some(c.map(|x| x.3.as_str()).unwrap_or(""))).i16(0);
                    }
                }
                if v >= 2 { w.i16(0); }
            }
            22 => {
                r.nstr()?;
                r.i32()?;
                w.i32(0).i16(0).i64((util::now_ms() % 1_000_000) + 1000).i16(0);
            }
            19 => {
                // CreateTopics.
                let n = r.count()?;
                let mut out = vec![];
                for _ in 0..n {
                    let t = r.str()?;
                    let parts = r.i32()?;
                    let _rf = r.i16()?;
                    let na = r.count()?;
                    for _ in 0..na { r.i32()?; let k = r.count()?; for _ in 0..k { r.i32()?; } }
                    let nc = r.count()?;
                    for _ in 0..nc { r.str()?; r.nstr()?; }
                    let made = store::create_topic(&ctx, &t, if parts > 0 { parts } else { cfg.default_partitions.max(1) }).await.unwrap_or(false);
                    out.push((t, if made { 0i16 } else { 36 }));
                }
                if v >= 2 { w.i32(0); }
                w.i32(out.len() as i32);
                for (t, e) in &out { w.str(t).i16(*e); if v >= 1 { w.nstr(None); } }
            }
            _ => return Err(format!("API key {api} is not offered")),
        }
        Ok(Some(w.0))
    }
}

async fn conn(ctx: Ctx, mut sock: tokio::net::TcpStream) {
    let peer = sock.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let mut c = Conn { ctx: ctx.clone(), authed: false, mech: String::new(), scram: None, client: String::new(), peer, user: String::new() };
    loop {
        let Ok(len) = sock.read_i32().await else { break };
        if !(0..=64 << 20).contains(&len) { break }
        let mut buf = vec![0u8; len as usize];
        if sock.read_exact(&mut buf).await.is_err() { break }
        let mut r = R::new(&buf);
        let (Ok(api), Ok(v), Ok(corr)) = (r.i16(), r.i16(), r.i32()) else { break };
        let client = r.nstr().ok().flatten().unwrap_or_default();
        if c.client.is_empty() && !client.is_empty() {
            c.client = client.clone();
            ctx.live.seen(&client, "Streams");
        }
        // ApiVersions v3+ has a flexible header (a tag buffer after client_id): skip it.
        if api == 18 && v >= 3 { let _ = r.i8(); }
        let body = match c.handle(api, v, &mut r).await {
            Ok(Some(b)) => b,
            Ok(None) => continue,
            Err(_) => break,
        };
        let mut out = W::default();
        out.i32(body.len() as i32 + 4).i32(corr);
        out.0.extend(body);
        if sock.write_all(&out.0).await.is_err() { break }
    }
}

/// Members that stopped sending heartbeats leave their group (a rebalance follows).
fn sweep(envs: &Envs) {
    let _ = envs;
    let mut gs = groups().lock().unwrap();
    for g in gs.values_mut() {
        let before = g.members.len();
        g.members.retain(|_, m| m.seen.map(|s| s.elapsed() < Duration::from_millis(m.session_ms.max(6000) as u64)).unwrap_or(true));
        if g.members.len() != before {
            if g.members.is_empty() { g.state = "Empty".into(); } else { g.state = "PreparingRebalance".into(); g.deadline = Some(Instant::now() + Duration::from_millis(3000)); g.round += 1; }
        }
    }
}

pub fn serve(envs: Envs, port: u16) {
    let e2 = envs.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            sweep(&e2);
        }
    });
    tokio::spawn(async move {
        let Ok(l) = tokio::net::TcpListener::bind(("0.0.0.0", port)).await else {
            eprintln!("Kafka: port {port} is taken");
            return;
        };
        loop {
            let Ok((sock, _)) = l.accept().await else { continue };
            let _ = sock.set_nodelay(true);
            if let Some(ctx) = envs.default_env() {
                tokio::spawn(conn(ctx, sock));
            }
        }
    });
}
