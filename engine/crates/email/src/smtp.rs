//! SMTP intake: one listener per e-mail service, with STARTTLS (self-signed), AUTH PLAIN/LOGIN,
//! and the provoked failures of E3 (timeout, refused, TLS failure, 421, RCPT 550/451, no STARTTLS).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use stuntman_core::Ctx;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

use crate::{config, ingest};

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// One listener per port. Several environments (or services) may want the same port: then a
/// session finds its environment by the SMTP login (or the STARTTLS server name).
struct PortL {
    handle: JoinHandle<()>,
    members: Arc<Mutex<Vec<(Ctx, String)>>>,
}

fn ports() -> &'static Mutex<HashMap<u16, PortL>> {
    static L: OnceLock<Mutex<HashMap<u16, PortL>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// Last bind error per "<env>/<service>", shown on E3.
pub fn bind_errors() -> &'static Mutex<HashMap<String, String>> {
    static E: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    E.get_or_init(Default::default)
}

pub fn key(ctx: &Ctx, service: &str) -> String {
    format!("{}/{service}", ctx.env)
}

/// The environments listening on a port, by name.
pub fn sharers(port: u16) -> Vec<String> {
    let l = ports().lock().unwrap();
    let mut v: Vec<String> = l.get(&port).map(|p| p.members.lock().unwrap().iter().map(|(c, _)| c.env.to_string()).collect()).unwrap_or_default();
    v.sort();
    v.dedup();
    v
}

/// Starts, joins, leaves or stops listeners to match this environment's service files.
pub async fn reconcile(ctx: &Ctx) {
    let svcs = if ctx.state.gone() { vec![] } else { config::list(ctx).unwrap_or_default() };
    let want: HashMap<String, u16> = svcs.iter().filter(|s| s.intake.smtp.access.on).map(|s| (s.name.clone(), s.intake.smtp.port)).collect();
    let mut l = ports().lock().unwrap();
    // Leave the ports this environment no longer wants; a port nobody wants closes.
    l.retain(|port, pl| {
        let mut m = pl.members.lock().unwrap();
        m.retain(|(c, svc)| c.env != ctx.env || want.get(svc) == Some(port));
        let keep = !m.is_empty();
        if !keep {
            pl.handle.abort();
        }
        keep
    });
    for (svc, port) in want {
        let k = key(ctx, &svc);
        if let Some(pl) = l.get(&port) {
            let mut m = pl.members.lock().unwrap();
            if !m.iter().any(|(c, s)| c.env == ctx.env && *s == svc) {
                m.push((ctx.clone(), svc));
            }
            bind_errors().lock().unwrap().remove(&k);
            continue;
        }
        let std = match std::net::TcpListener::bind(("0.0.0.0", port)) {
            Ok(s) => s,
            Err(e) => {
                bind_errors().lock().unwrap().insert(k, format!("port {port}: {e}"));
                continue;
            }
        };
        bind_errors().lock().unwrap().remove(&k);
        std.set_nonblocking(true).unwrap();
        let listener = TcpListener::from_std(std).unwrap();
        let members = Arc::new(Mutex::new(vec![(ctx.clone(), svc)]));
        let m2 = members.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { continue };
                let cands = m2.lock().unwrap().clone();
                tokio::spawn(async move {
                    let _ = match cands.len() {
                        0 => Ok(()),
                        1 => {
                            let (c, n) = cands.into_iter().next().unwrap();
                            session(c, n, BufReader::new(Box::new(sock) as Box<dyn Io>), None).await
                        }
                        _ => shared(cands, sock).await,
                    };
                });
            }
        });
        l.insert(port, PortL { handle, members });
    }
}

/// What a session on a shared port already did before its environment was known.
struct Started {
    tls_on: bool,
    authed: String,
}

/// A port several environments share: greet, offer what any of them offers, and pick the
/// environment from the AUTH user (<env> or <env>/<service>) or the STARTTLS server name.
async fn shared(cands: Vec<(Ctx, String)>, sock: TcpStream) -> std::io::Result<()> {
    let names: Vec<String> = { let mut v: Vec<String> = cands.iter().map(|(c, _)| c.env.to_string()).collect(); v.sort(); v.dedup(); v };
    let host = cands[0].0.host.to_string();
    let svcs: Vec<Option<config::Service>> = cands.iter().map(|(c, n)| config::get(c, n).ok()).collect();
    let starttls = svcs.iter().flatten().any(|s| s.intake.smtp.starttls);
    let size = svcs.iter().flatten().map(|s| s.limits.size_bytes()).max().unwrap_or(10 << 20);
    let mut rd = BufReader::new(Box::new(sock) as Box<dyn Io>);
    rd.get_mut().write_all(format!("220 {host} ESMTP Stuntman (shared by {}: log in as the environment's name, or any address at its base domain)\r\n", names.join(", ")).as_bytes()).await?;
    let mut tls_on = false;
    let mut sni: Option<String> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if rd.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let cmd = line.trim_end().to_string();
        let up = cmd.to_uppercase();
        if up.starts_with("EHLO") || up.starts_with("HELO") {
            let helo = cmd.get(5..).unwrap_or("").trim().to_string();
            let w = rd.get_mut();
            if up.starts_with("HELO") {
                w.write_all(format!("250 {host}\r\n").as_bytes()).await?;
            } else {
                let mut caps = vec![format!("{host} hello {helo}"), format!("SIZE {size}"), "8BITMIME".into(), "PIPELINING".into()];
                if starttls && !tls_on { caps.push("STARTTLS".into()); }
                caps.push("AUTH PLAIN LOGIN".into());
                for (i, c) in caps.iter().enumerate() {
                    w.write_all(format!("250{}{c}\r\n", if i + 1 == caps.len() { ' ' } else { '-' }).as_bytes()).await?;
                }
            }
        } else if up == "STARTTLS" && starttls && !tls_on {
            rd.get_mut().write_all(b"220 2.0.0 Ready to start TLS\r\n").await?;
            rd.get_mut().flush().await?;
            let stream = tls().accept(rd.into_inner()).await?;
            sni = stream.get_ref().1.server_name().map(|s| s.to_lowercase());
            rd = BufReader::new(Box::new(stream) as Box<dyn Io>);
            tls_on = true;
        } else if up.starts_with("AUTH") {
            let Some((user, pass)) = read_auth(&cmd, &mut rd, &mut line).await? else { continue };
            // <env>, <env>/<service>; else the STARTTLS name's environment.
            let (env, svc) = user.split_once('/').map(|(e, s)| (e.to_lowercase(), Some(s.to_string()))).unwrap_or((user.to_lowercase(), None));
            let by_sni = sni.as_deref().and_then(|h| cands[0].0.envs.by_host(Some(h))).map(|c| c.env.to_string());
            // An app that logs in with its sender address: <anyone>@<the environment's base domain>.
            let by_domain = user.rsplit_once('@').map(|(_, d)| d.trim().trim_end_matches('.').to_lowercase()).and_then(|d| {
                cands.iter().position(|(c, _)| { let b = c.state.file.read().unwrap().base_domain.to_lowercase(); d == b || d.ends_with(&format!(".{b}")) || d == format!("{}.localhost", c.env) })
            });
            let pick = cands.iter().position(|(c, s)| *c.env == *env && svc.as_ref().map(|x| x == s).unwrap_or(true))
                .or(by_domain)
                .or_else(|| by_sni.as_ref().and_then(|e| cands.iter().position(|(c, _)| *c.env == **e)));
            let w = rd.get_mut();
            let Some(i) = pick else {
                w.write_all(format!("535 5.7.8 No environment “{user}” on this port; log in as one of: {} (or any address at its base domain)\r\n", names.join(", ")).as_bytes()).await?;
                continue;
            };
            let (ctx, name) = cands[i].clone();
            let ok = svcs[i].as_ref().map(|s| s.intake.smtp.access.mode != "validate" || key_ok(&s.intake.smtp.access, &pass)).unwrap_or(false);
            if !ok {
                w.write_all(b"535 5.7.8 Authentication failed\r\n").await?;
                continue;
            }
            w.write_all(b"235 2.7.0 Authentication successful\r\n").await?;
            w.flush().await?;
            return session(ctx, name, rd, Some(Started { tls_on, authed: user })).await;
        } else if up == "NOOP" || up == "RSET" {
            rd.get_mut().write_all(b"250 2.0.0 OK\r\n").await?;
        } else if up == "QUIT" {
            rd.get_mut().write_all(b"221 2.0.0 Bye\r\n").await?;
            return Ok(());
        } else if up.starts_with("MAIL") || up.starts_with("RCPT") || up == "DATA" {
            rd.get_mut().write_all(format!("530 5.7.0 Log in first: this port serves {}; the SMTP user names the environment\r\n", names.join(", ")).as_bytes()).await?;
        } else {
            rd.get_mut().write_all(b"500 5.5.2 Command not recognized\r\n").await?;
        }
        rd.get_mut().flush().await?;
    }
}

/// AUTH PLAIN or LOGIN, the user and password; None after answering a bad mechanism.
async fn read_auth(cmd: &str, rd: &mut BufReader<Box<dyn Io>>, line: &mut String) -> std::io::Result<Option<(String, String)>> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let mech = parts.get(1).map(|s| s.to_uppercase()).unwrap_or_default();
    Ok(Some(match mech.as_str() {
        "PLAIN" => {
            let b = match parts.get(2) {
                Some(b) => b.to_string(),
                None => {
                    rd.get_mut().write_all(b"334 \r\n").await?;
                    line.clear();
                    rd.read_line(line).await?;
                    line.trim().to_string()
                }
            };
            let d = B64.decode(b.trim()).unwrap_or_default();
            let f: Vec<&[u8]> = d.split(|c| *c == 0).collect();
            (String::from_utf8_lossy(f.get(1).copied().unwrap_or(b"")).to_string(), String::from_utf8_lossy(f.get(2).copied().unwrap_or(b"")).to_string())
        }
        "LOGIN" => {
            // .NET SmtpClient (and smtplib) send the user on the AUTH line itself.
            let u = match parts.get(2) {
                Some(b) => String::from_utf8_lossy(&B64.decode(b.trim()).unwrap_or_default()).to_string(),
                None => {
                    rd.get_mut().write_all(b"334 VXNlcm5hbWU6\r\n").await?;
                    line.clear();
                    rd.read_line(line).await?;
                    String::from_utf8_lossy(&B64.decode(line.trim()).unwrap_or_default()).to_string()
                }
            };
            rd.get_mut().write_all(b"334 UGFzc3dvcmQ6\r\n").await?;
            line.clear();
            rd.read_line(line).await?;
            (u, String::from_utf8_lossy(&B64.decode(line.trim()).unwrap_or_default()).to_string())
        }
        _ => {
            rd.get_mut().write_all(b"504 5.5.4 Use AUTH PLAIN or LOGIN\r\n").await?;
            return Ok(None);
        }
    }))
}

pub(crate) fn tls() -> Arc<tokio_rustls::TlsAcceptor> {
    static T: OnceLock<Arc<tokio_rustls::TlsAcceptor>> = OnceLock::new();
    T.get_or_init(|| {
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".into(), "stuntman.local".into()]).unwrap();
        let cert = rustls::pki_types::CertificateDer::from(ck.cert.der().to_vec());
        let key = rustls::pki_types::PrivateKeyDer::try_from(ck.key_pair.serialize_der()).unwrap();
        let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();
        Arc::new(tokio_rustls::TlsAcceptor::from(Arc::new(cfg)))
    })
    .clone()
}

pub fn key_ok(access: &config::Access, secret: &str) -> bool {
    let h = format!("{:x}", Sha256::digest(secret.as_bytes()));
    access.keys.iter().any(|k| k.sha256 == h)
}

async fn session(ctx: Ctx, service: String, mut rd: BufReader<Box<dyn Io>>, started: Option<Started>) -> std::io::Result<()> {
    if ctx.state.paused() {
        return rd.get_mut().write_all(b"421 4.3.2 This environment is paused in Stuntman\r\n").await;
    }
    let Ok(svc) = config::get(&ctx, &service) else { return Ok(()) };
    let smtp = svc.intake.smtp.clone();
    let host = ctx.host.to_string();
    // Provoked: the provider is down.
    if smtp.access.mode == "refuse" {
        let io = rd.get_mut();
        match smtp.access.how.as_deref().unwrap_or("timeout") {
            "refused" => return Ok(()),
            "timeout" => {
                tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                return Ok(());
            }
            "503" => {
                io.write_all(b"421 4.3.2 Service not available, closing transmission channel\r\n").await?;
                return Ok(());
            }
            "429" => {
                io.write_all(b"421 4.7.0 Too many connections, try again later\r\n").await?;
                return Ok(());
            }
            _ => {} // "tls": greet, offer STARTTLS, break the handshake.
        }
    }
    let break_tls = smtp.access.mode == "refuse" && smtp.access.how.as_deref() == Some("tls");
    let mut tls_on = started.as_ref().map(|s| s.tls_on).unwrap_or(false);
    let mut authed: Option<String> = started.as_ref().map(|s| s.authed.clone());
    if started.is_none() {
        rd.get_mut().write_all(format!("220 {host} ESMTP Stuntman (stand-in for {service})\r\n").as_bytes()).await?;
    }
    let mut from: Option<String> = None;
    let mut rcpts: Vec<String> = vec![];
    let mut line = String::new();
    loop {
        line.clear();
        if rd.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let cmd = line.trim_end().to_string();
        let up = cmd.to_uppercase();
        let w = rd.get_mut();
        if up.starts_with("EHLO") || up.starts_with("HELO") {
            let helo = cmd.get(5..).unwrap_or("").trim().to_string();
            if up.starts_with("HELO") {
                w.write_all(format!("250 {host}\r\n").as_bytes()).await?;
                continue;
            }
            let mut caps = vec![format!("{host} hello {helo}"), format!("SIZE {}", svc.limits.size_bytes()), "8BITMIME".into(), "PIPELINING".into()];
            if (smtp.starttls || break_tls) && !tls_on {
                caps.push("STARTTLS".into());
            }
            caps.push("AUTH PLAIN LOGIN".into());
            for (i, c) in caps.iter().enumerate() {
                let sep = if i + 1 == caps.len() { ' ' } else { '-' };
                w.write_all(format!("250{sep}{c}\r\n").as_bytes()).await?;
            }
        } else if up == "STARTTLS" {
            if !(smtp.starttls || break_tls) || tls_on {
                w.write_all(b"502 5.5.1 STARTTLS not offered\r\n").await?;
                continue;
            }
            w.write_all(b"220 2.0.0 Ready to start TLS\r\n").await?;
            w.flush().await?;
            if break_tls {
                // TLS starts and breaks: the client sees a handshake failure.
                w.write_all(b"\x15\x03\x03\x00\x02\x02\x28").await?;
                return Ok(());
            }
            let inner = rd.into_inner();
            let stream = tls().accept(inner).await?;
            rd = BufReader::new(Box::new(stream) as Box<dyn Io>);
            tls_on = true;
            authed = None;
            from = None;
            rcpts.clear();
        } else if up.starts_with("AUTH") {
            let Some((user, pass)) = read_auth(&cmd, &mut rd, &mut line).await? else { continue };
            let w = rd.get_mut();
            if smtp.access.mode == "validate" && !key_ok(&smtp.access, &pass) {
                w.write_all(b"535 5.7.8 Authentication failed\r\n").await?;
            } else {
                authed = Some(if user.is_empty() { "smtp".into() } else { user });
                w.write_all(b"235 2.7.0 Authentication successful\r\n").await?;
            }
        } else if up.starts_with("MAIL FROM:") {
            if smtp.access.mode == "validate" && authed.is_none() {
                w.write_all(b"530 5.7.0 Authentication required\r\n").await?;
                continue;
            }
            if !ingest::rate_ok(&svc) {
                w.write_all(b"451 4.7.1 Rate limit reached, try again later\r\n").await?;
                continue;
            }
            from = Some(addr(&cmd[10..]));
            rcpts.clear();
            w.write_all(b"250 2.1.0 OK\r\n").await?;
        } else if up.starts_with("RCPT TO:") {
            if from.is_none() {
                w.write_all(b"503 5.5.1 MAIL first\r\n").await?;
                continue;
            }
            let r = addr(&cmd[8..]);
            match smtp.refuse_recipients.as_str() {
                "550" => w.write_all(b"550 5.1.1 Recipient address rejected: mailbox unavailable\r\n").await?,
                "451" => w.write_all(b"451 4.3.0 Recipient temporarily unavailable, try again later\r\n").await?,
                _ if !ingest::accepts(&ctx, &svc, &r).await => w.write_all(b"550 5.7.1 Relaying denied: catch-all is off\r\n").await?,
                _ => {
                    rcpts.push(r);
                    w.write_all(b"250 2.1.5 OK\r\n").await?;
                }
            }
        } else if up == "DATA" {
            if rcpts.is_empty() {
                w.write_all(b"503 5.5.1 RCPT first\r\n").await?;
                continue;
            }
            w.write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n").await?;
            let limit = svc.limits.size_bytes() as usize + 64 * 1024;
            let mut data: Vec<u8> = vec![];
            let mut l = Vec::new();
            loop {
                l.clear();
                if rd.read_until(b'\n', &mut l).await? == 0 {
                    return Ok(());
                }
                if l == b".\r\n" || l == b".\n" {
                    break;
                }
                let s = if l.starts_with(b"..") { &l[1..] } else { &l[..] };
                if data.len() < limit {
                    data.extend_from_slice(s);
                }
            }
            let env = ingest::Envelope {
                service: svc.clone(),
                via: "smtp".into(),
                mail_from: from.clone().unwrap_or_default(),
                rcpts: std::mem::take(&mut rcpts),
                raw: data,
                trace_id: None,
                custom_args: serde_json::json!({}),
                client: format!("smtp:{}", authed.clone().unwrap_or("client".into())),
            };
            // The login names the sender; an EHLO name is usually a host name nobody recognizes.
            ctx.live.seen(&authed.clone().unwrap_or("SMTP client".into()), "SMTP");
            let w = rd.get_mut();
            match ingest::ingest(&ctx, env).await {
                Ok(ms) => w.write_all(format!("250 2.0.0 OK queued as {}\r\n", ms.first().map(|m| m.id.as_str()).unwrap_or("-")).as_bytes()).await?,
                Err(e) => w.write_all(format!("554 5.6.0 {}\r\n", e.message).as_bytes()).await?,
            }
            from = None;
        } else if up == "RSET" {
            from = None;
            rcpts.clear();
            w.write_all(b"250 2.0.0 OK\r\n").await?;
        } else if up == "NOOP" {
            w.write_all(b"250 2.0.0 OK\r\n").await?;
        } else if up.starts_with("VRFY") {
            w.write_all(b"252 2.1.5 Cannot verify, but will accept\r\n").await?;
        } else if up == "QUIT" {
            w.write_all(b"221 2.0.0 Bye\r\n").await?;
            return Ok(());
        } else {
            w.write_all(b"500 5.5.2 Command not recognized\r\n").await?;
        }
        rd.get_mut().flush().await?;
    }
}

fn addr(s: &str) -> String {
    let s = s.trim();
    let s = s.split_whitespace().next().unwrap_or("");
    s.trim_start_matches('<').trim_end_matches('>').trim().to_lowercase()
}

#[allow(dead_code)]
async fn read_all(r: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
    let mut v = vec![];
    let _ = r.read_to_end(&mut v).await;
    v
}
