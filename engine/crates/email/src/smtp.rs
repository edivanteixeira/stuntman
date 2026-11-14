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

/// Listeners by "<env>/<service>", so a port change or "intake off" takes effect at once.
fn listeners() -> &'static Mutex<HashMap<String, (u16, JoinHandle<()>)>> {
    static L: OnceLock<Mutex<HashMap<String, (u16, JoinHandle<()>)>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// Last bind error per "<env>/<service>", shown on E3.
pub fn bind_errors() -> &'static Mutex<HashMap<String, String>> {
    static E: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    E.get_or_init(Default::default)
}

/// Starts, moves or stops listeners to match the service files.
pub fn key(ctx: &Ctx, service: &str) -> String {
    format!("{}/{service}", ctx.env)
}

pub async fn reconcile(ctx: &Ctx) {
    let svcs = if ctx.state.gone() { vec![] } else { config::list(ctx).unwrap_or_default() };
    let want: HashMap<String, u16> = svcs.iter().filter(|s| s.intake.smtp.access.on).map(|s| (key(ctx, &s.name), s.intake.smtp.port)).collect();
    let mine = format!("{}/", ctx.env);
    let mut l = listeners().lock().unwrap();
    l.retain(|name, (port, h)| {
        let keep = !name.starts_with(&mine) || want.get(name) == Some(port);
        if !keep {
            h.abort();
        }
        keep
    });
    for (name, port) in want {
        if l.contains_key(&name) {
            continue;
        }
        let std = match std::net::TcpListener::bind(("0.0.0.0", port)) {
            Ok(s) => s,
            Err(e) => {
                // Held by another service of this Stuntman: say which, not just "address in use".
                let why = match l.iter().find(|(_, (p, _))| *p == port) {
                    Some((other, _)) => format!("port {port} is taken by {} in this Stuntman; pick another port", other.replace('/', " / ")),
                    None => format!("port {port}: {e}"),
                };
                bind_errors().lock().unwrap().insert(name.clone(), why);
                continue;
            }
        };
        bind_errors().lock().unwrap().remove(&name);
        std.set_nonblocking(true).unwrap();
        let listener = TcpListener::from_std(std).unwrap();
        let (ctx2, n2) = (ctx.clone(), name.split_once('/').map(|x| x.1).unwrap_or(&name).to_string());
        let h = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { continue };
                let (c, n) = (ctx2.clone(), n2.clone());
                tokio::spawn(async move {
                    let _ = session(c, n, sock).await;
                });
            }
        });
        l.insert(name, (port, h));
    }
}

fn tls() -> Arc<tokio_rustls::TlsAcceptor> {
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

async fn session(ctx: Ctx, service: String, sock: TcpStream) -> std::io::Result<()> {
    if ctx.state.paused() {
        let mut sock = sock;
        return sock.write_all(b"421 4.3.2 This environment is paused in Stuntman
").await;
    }
    let Ok(svc) = config::get(&ctx, &service) else { return Ok(()) };
    let smtp = svc.intake.smtp.clone();
    let host = ctx.host.to_string();
    let mut io: Box<dyn Io> = Box::new(sock);
    // Provoked: the provider is down.
    if smtp.access.mode == "refuse" {
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
    io.write_all(format!("220 {host} ESMTP Stuntman (stand-in for {service})\r\n").as_bytes()).await?;
    let mut tls_on = false;
    let mut authed: Option<String> = None;
    let mut from: Option<String> = None;
    let mut rcpts: Vec<String> = vec![];
    let mut rd = BufReader::new(io);
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
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            let mech = parts.get(1).map(|s| s.to_uppercase()).unwrap_or_default();
            let (user, pass) = match mech.as_str() {
                "PLAIN" => {
                    let b = match parts.get(2) {
                        Some(b) => b.to_string(),
                        None => {
                            w.write_all(b"334 \r\n").await?;
                            line.clear();
                            rd.read_line(&mut line).await?;
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
                            rd.read_line(&mut line).await?;
                            String::from_utf8_lossy(&B64.decode(line.trim()).unwrap_or_default()).to_string()
                        }
                    };
                    rd.get_mut().write_all(b"334 UGFzc3dvcmQ6\r\n").await?;
                    line.clear();
                    rd.read_line(&mut line).await?;
                    (u, String::from_utf8_lossy(&B64.decode(line.trim()).unwrap_or_default()).to_string())
                }
                _ => {
                    w.write_all(b"504 5.5.4 Use AUTH PLAIN or LOGIN\r\n").await?;
                    continue;
                }
            };
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
