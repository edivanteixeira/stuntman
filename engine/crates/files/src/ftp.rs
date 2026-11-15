//! FTP and explicit FTPS (AUTH TLS, PROT P), passive mode (PASV, EPSV). The commands EDI clients
//! use: USER PASS SYST FEAT OPTS PWD CWD CDUP TYPE MODE STRU PASV EPSV LIST NLST MLSD RETR STOR APPE
//! DELE MKD RMD RNFR RNTO SIZE MDTM REST NOOP QUIT AUTH PBSZ PROT. One port for every environment:
//! logins go to the default one.

use std::{sync::Arc, time::Duration};

use stuntman_core::{env::Envs, Ctx};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

use crate::{config, store};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type BoxIo = Box<dyn Io>;

#[derive(Debug)]
struct Fixed(Arc<rustls::sign::CertifiedKey>);
impl rustls::server::ResolvesServerCert for Fixed {
    fn resolve(&self, _: rustls::server::ClientHello) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

pub fn acceptor() -> Option<tokio_rustls::TlsAcceptor> {
    let leaf = stuntman_apis::ca::get().leaf("localhost")?;
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider())).with_safe_default_protocol_versions().ok()?.with_no_client_auth().with_cert_resolver(Arc::new(Fixed(leaf)));
    Some(tokio_rustls::TlsAcceptor::from(Arc::new(cfg)))
}

struct S {
    ctx: Ctx,
    user: String,
    server: Option<config::Server>,
    acct: Option<config::User>,
    logged: bool,
    cwd: String,
    pasv: Option<TcpListener>,
    rest: u64,
    rnfr: Option<String>,
    prot_p: bool,
    tls: bool,
}

impl S {
    fn srv(&self) -> String {
        self.server.as_ref().map(|s| s.name.clone()).unwrap_or_default()
    }
    fn provoke(&self) -> String {
        config::load(&self.ctx).servers.into_iter().find(|s| Some(&s.name) == self.server.as_ref().map(|x| &x.name)).map(|s| s.provoke).unwrap_or_default()
    }
    fn proto(&self) -> &'static str {
        if self.tls { "FTPS" } else { "FTP" }
    }
}

async fn reply(c: &mut BufReader<BoxIo>, s: &str) -> std::io::Result<()> {
    c.get_mut().write_all(format!("{s}\r\n").as_bytes()).await?;
    c.get_mut().flush().await
}

fn listing_line(name: &str, m: &std::fs::Metadata) -> String {
    let t: chrono_like::T = m.modified().ok().map(chrono_like::T::from).unwrap_or_default();
    format!("{}{} 1 ftp ftp {:>12} {} {}", if m.is_dir() { "d" } else { "-" }, if m.is_dir() { "rwxr-xr-x" } else { "rw-r--r--" }, m.len(), t.ls(), name)
}

/// Dates as ls and MDTM show them, without a date library.
mod chrono_like {
    #[derive(Default)]
    pub struct T(pub i64);
    impl From<std::time::SystemTime> for T {
        fn from(s: std::time::SystemTime) -> Self {
            T(s.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0))
        }
    }
    fn civil(days: i64) -> (i64, u32, u32) {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (if m <= 2 { y + 1 } else { y }, m, d)
    }
    impl T {
        pub fn parts(&self) -> (i64, u32, u32, i64, i64, i64) {
            let (y, m, d) = civil(self.0.div_euclid(86_400));
            let s = self.0.rem_euclid(86_400);
            (y, m, d, s / 3600, s % 3600 / 60, s % 60)
        }
        pub fn ls(&self) -> String {
            const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            let (_, m, d, h, mi, _) = self.parts();
            format!("{} {:>2} {:02}:{:02}", M[(m - 1) as usize], d, h, mi)
        }
        pub fn mdtm(&self) -> String {
            let (y, m, d, h, mi, s) = self.parts();
            format!("{y:04}{m:02}{d:02}{h:02}{mi:02}{s:02}")
        }
    }
}

async fn data_conn(s: &mut S) -> Option<BoxIo> {
    let l = s.pasv.take()?;
    let (sock, _) = tokio::time::timeout(Duration::from_secs(15), l.accept()).await.ok()?.ok()?;
    if s.prot_p {
        let a = acceptor()?;
        return a.accept(sock).await.ok().map(|t| Box::new(t) as BoxIo);
    }
    Some(Box::new(sock))
}

/// Copies with the server's provocations: slow (20 KB/s) or drop_mid (stop after 4 KB).
async fn copy(mut from: impl AsyncRead + Unpin, mut to: impl AsyncWrite + Unpin, provoke: &str) -> (u64, bool) {
    let mut buf = vec![0u8; if provoke == "slow" { 2048 } else { 32 * 1024 }];
    let mut n = 0u64;
    loop {
        let k = match from.read(&mut buf).await { Ok(0) | Err(_) => break, Ok(k) => k };
        let k = if provoke == "drop_mid" && n + k as u64 > 4096 { (4096 - n) as usize } else { k };
        if to.write_all(&buf[..k]).await.is_err() {
            return (n, false);
        }
        n += k as u64;
        if provoke == "drop_mid" && n >= 4096 {
            return (n, false);
        }
        if provoke == "slow" {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let _ = to.flush().await;
    (n, true)
}

async fn conn(ctx: Ctx, sock: TcpStream) {
    let local_ip = sock.local_addr().map(|a| a.ip()).unwrap_or(std::net::IpAddr::from([127, 0, 0, 1]));
    let mut c = BufReader::new(Box::new(sock) as BoxIo);
    let mut s = S { ctx: ctx.clone(), user: String::new(), server: None, acct: None, logged: false, cwd: "/".into(), pasv: None, rest: 0, rnfr: None, prot_p: false, tls: false };
    if reply(&mut c, "220 Stuntman FTP (simulated partner server)").await.is_err() {
        return;
    }
    loop {
        let mut line = String::new();
        match tokio::time::timeout(Duration::from_secs(600), c.read_line(&mut line)).await {
            Ok(Ok(0)) | Err(_) | Ok(Err(_)) => break,
            _ => {}
        }
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        let (cmd, arg) = line.split_once(' ').map(|(a, b)| (a.to_uppercase(), b.to_string())).unwrap_or((line.to_uppercase(), String::new()));
        let needs_login = !["USER", "PASS", "AUTH", "PBSZ", "PROT", "FEAT", "SYST", "NOOP", "QUIT", "OPTS"].contains(&cmd.as_str());
        if needs_login && !s.logged {
            let _ = reply(&mut c, "530 Please log in with USER and PASS").await;
            continue;
        }
        let r: String = match cmd.as_str() {
            "AUTH" if arg.eq_ignore_ascii_case("TLS") || arg.eq_ignore_ascii_case("SSL") => {
                let Some(a) = acceptor() else { let _ = reply(&mut c, "431 TLS is not available").await; continue };
                if reply(&mut c, "234 Proceed with negotiation").await.is_err() {
                    break;
                }
                let inner = c.into_inner();
                match a.accept(inner).await {
                    Ok(t) => {
                        c = BufReader::new(Box::new(t) as BoxIo);
                        s.tls = true;
                        continue;
                    }
                    Err(_) => return,
                }
            }
            "PBSZ" => "200 PBSZ=0".into(),
            "PROT" => {
                s.prot_p = arg.eq_ignore_ascii_case("P");
                format!("200 Protection level set to {}", if s.prot_p { "Private" } else { "Clear" })
            }
            "USER" => {
                s.user = arg.clone();
                s.logged = false;
                "331 Password required".into()
            }
            "PASS" => match config::login(&ctx, &s.user) {
                Some((srv, _)) if srv.provoke == "login_refused" => {
                    store::log(&ctx, &srv.name, &s.user, s.proto(), "login", "", 0, false, "530 login refused (provoked)").await;
                    "530 Login incorrect".into()
                }
                Some((srv, u)) if config::password_ok(&ctx, &u, &arg) => {
                    let _ = std::fs::create_dir_all(config::root(&ctx, &srv.name));
                    store::log(&ctx, &srv.name, &u.user, s.proto(), "login", "", 0, true, "logged in").await;
                    s.server = Some(srv);
                    s.acct = Some(u);
                    s.logged = true;
                    "230 Logged in".into()
                }
                Some((srv, _)) => {
                    store::log(&ctx, &srv.name, &s.user, s.proto(), "login", "", 0, false, "530 wrong password").await;
                    "530 Login incorrect".into()
                }
                None => "530 Login incorrect".into(),
            },
            "SYST" => "215 UNIX Type: L8".into(),
            "FEAT" => "211-Features:\r\n UTF8\r\n PASV\r\n EPSV\r\n SIZE\r\n MDTM\r\n REST STREAM\r\n MLSD\r\n AUTH TLS\r\n PBSZ\r\n PROT\r\n211 End".into(),
            "OPTS" | "TYPE" | "MODE" | "STRU" | "NOOP" | "ALLO" => format!("200 {cmd} ok"),
            "PWD" | "XPWD" => format!("257 \"{}\" is the current directory", s.cwd),
            "CWD" | "XCWD" | "CDUP" => {
                let to = config::resolve(&s.cwd, if cmd == "CDUP" { ".." } else { &arg });
                if config::disk(&ctx, &s.srv(), &to).is_dir() {
                    s.cwd = to;
                    "250 Directory changed".into()
                } else {
                    "550 No such directory".into()
                }
            }
            "PASV" | "EPSV" => match passive(local_ip).await {
                Ok(l) => {
                    let port = l.local_addr().map(|a| a.port()).unwrap_or(0);
                    s.pasv = Some(l);
                    if cmd == "EPSV" {
                        format!("229 Entering Extended Passive Mode (|||{port}|)")
                    } else {
                        // In a container the local address is not the one the client reached: STUNTMAN_FTP_PASSIVE_HOST.
                        let adv = std::env::var("STUNTMAN_FTP_PASSIVE_HOST").ok().and_then(|h| h.parse().ok()).unwrap_or(local_ip);
                        let ip = match adv { std::net::IpAddr::V4(v) => v.octets(), _ => [127, 0, 0, 1] };
                        format!("227 Entering Passive Mode ({},{},{},{},{},{})", ip[0], ip[1], ip[2], ip[3], port >> 8, port & 255)
                    }
                }
                Err(_) => "425 Cannot open a data connection".into(),
            },
            "PORT" | "EPRT" => "502 Active mode is not offered: use passive (PASV/EPSV)".into(),
            "REST" => {
                s.rest = arg.trim().parse().unwrap_or(0);
                format!("350 Restarting at {}", s.rest)
            }
            "SIZE" | "MDTM" => {
                let p = config::resolve(&s.cwd, &arg);
                match std::fs::metadata(config::disk(&ctx, &s.srv(), &p)) {
                    Ok(m) if m.is_file() => if cmd == "SIZE" { format!("213 {}", m.len()) } else { format!("213 {}", m.modified().ok().map(chrono_like::T::from).unwrap_or_default().mdtm()) },
                    _ => "550 No such file".into(),
                }
            }
            "LIST" | "NLST" | "MLSD" => {
                let target = arg.split_whitespace().find(|a| !a.starts_with('-')).map(|a| config::resolve(&s.cwd, a)).unwrap_or(s.cwd.clone());
                if s.provoke() == "permission_denied" {
                    "550 Permission denied".into()
                } else {
                    let dir = config::disk(&ctx, &s.srv(), &target);
                    let mut rows: Vec<(String, std::fs::Metadata)> = std::fs::read_dir(&dir).map(|r| r.flatten().filter_map(|e| Some((e.file_name().to_string_lossy().to_string(), e.metadata().ok()?))).collect()).unwrap_or_default();
                    rows.sort_by(|a, b| a.0.cmp(&b.0));
                    let text: String = rows.iter().map(|(n, m)| match cmd.as_str() {
                        "NLST" => format!("{n}\r\n"),
                        "MLSD" => format!("type={};size={};modify={}; {n}\r\n", if m.is_dir() { "dir" } else { "file" }, m.len(), m.modified().ok().map(chrono_like::T::from).unwrap_or_default().mdtm()),
                        _ => format!("{}\r\n", listing_line(n, m)),
                    }).collect();
                    let _ = reply(&mut c, "150 Here comes the listing").await;
                    match data_conn(&mut s).await {
                        Some(mut d) => {
                            let _ = d.write_all(text.as_bytes()).await;
                            let _ = d.shutdown().await;
                            "226 Listing sent".into()
                        }
                        None => "425 No data connection (use PASV first)".into(),
                    }
                }
            }
            "RETR" => {
                let p = config::resolve(&s.cwd, &arg);
                let f = config::disk(&ctx, &s.srv(), &p);
                let prov = s.provoke();
                if prov == "permission_denied" {
                    store::log(&ctx, &s.srv(), &s.user, s.proto(), "get", &p, 0, false, "550 permission denied (provoked)").await;
                    "550 Permission denied".into()
                } else if !f.is_file() {
                    "550 No such file".into()
                } else {
                    let _ = reply(&mut c, "150 Opening data connection").await;
                    match (data_conn(&mut s).await, tokio::fs::File::open(&f).await) {
                        (Some(mut d), Ok(mut file)) => {
                            if s.rest > 0 {
                                use tokio::io::AsyncSeekExt;
                                let _ = file.seek(std::io::SeekFrom::Start(s.rest)).await;
                                s.rest = 0;
                            }
                            let (n, done) = copy(file, &mut d, &prov).await;
                            if done {
                                let _ = d.shutdown().await;
                            }
                            drop(d);
                            store::log(&ctx, &s.srv(), &s.user, s.proto(), "get", &p, n, done, if done { "downloaded" } else { "426 dropped mid-transfer (provoked)" }).await;
                            if done { "226 Transfer complete".into() } else { "426 Connection closed; transfer aborted".into() }
                        }
                        _ => "425 No data connection".into(),
                    }
                }
            }
            "STOR" | "APPE" => {
                let p = config::resolve(&s.cwd, &arg);
                let f = config::disk(&ctx, &s.srv(), &p);
                let prov = s.provoke();
                let ro = s.acct.as_ref().map(|a| a.read_only).unwrap_or(false);
                if prov == "permission_denied" || ro {
                    store::log(&ctx, &s.srv(), &s.user, s.proto(), "put", &p, 0, false, if ro { "550 read-only user" } else { "550 permission denied (provoked)" }).await;
                    "550 Permission denied".into()
                } else if prov == "disk_full" {
                    store::log(&ctx, &s.srv(), &s.user, s.proto(), "put", &p, 0, false, "552 disk full (provoked)").await;
                    "552 Requested file action aborted: exceeded storage allocation".into()
                } else {
                    if let Some(parent) = f.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = reply(&mut c, "150 Ok to send data").await;
                    let file = if cmd == "APPE" || s.rest > 0 { tokio::fs::OpenOptions::new().create(true).append(true).open(&f).await } else { tokio::fs::File::create(&f).await };
                    s.rest = 0;
                    match (data_conn(&mut s).await, file) {
                        (Some(mut d), Ok(mut file)) => {
                            let (n, done) = copy(&mut d, &mut file, &prov).await;
                            let _ = file.flush().await;
                            // TLS: answer the client's close_notify before closing.
                            if done {
                                let _ = d.shutdown().await;
                            }
                            drop(d);
                            store::log(&ctx, &s.srv(), &s.user, s.proto(), "put", &p, n, done, if done { "uploaded" } else { "426 dropped mid-transfer (provoked); the partial file stays" }).await;
                            if done { "226 Transfer complete".into() } else { "426 Connection closed; transfer aborted".into() }
                        }
                        _ => "425 No data connection".into(),
                    }
                }
            }
            "DELE" | "RMD" | "XRMD" | "MKD" | "XMKD" | "RNFR" | "RNTO" => {
                let p = config::resolve(&s.cwd, &arg);
                let f = config::disk(&ctx, &s.srv(), &p);
                let ro = s.acct.as_ref().map(|a| a.read_only).unwrap_or(false);
                if s.provoke() == "permission_denied" || ro {
                    "550 Permission denied".into()
                } else {
                    match cmd.as_str() {
                        "DELE" => match std::fs::remove_file(&f) {
                            Ok(_) => { store::log(&ctx, &s.srv(), &s.user, s.proto(), "delete", &p, 0, true, "deleted").await; "250 Deleted".into() }
                            Err(_) => "550 No such file".into(),
                        },
                        "RMD" | "XRMD" => if std::fs::remove_dir(&f).is_ok() { "250 Removed".into() } else { "550 Cannot remove (not empty?)".into() },
                        "MKD" | "XMKD" => if std::fs::create_dir_all(&f).is_ok() { format!("257 \"{p}\" created") } else { "550 Cannot create".into() },
                        "RNFR" => if f.exists() { s.rnfr = Some(p); "350 Ready for RNTO".into() } else { "550 No such file".into() },
                        _ => match s.rnfr.take() {
                            Some(from) => match std::fs::rename(config::disk(&ctx, &s.srv(), &from), &f) {
                                Ok(_) => { store::log(&ctx, &s.srv(), &s.user, s.proto(), "rename", &format!("{from} → {p}"), 0, true, "renamed").await; "250 Renamed".into() }
                                Err(_) => "550 Rename failed".into(),
                            },
                            None => "503 RNFR first".into(),
                        },
                    }
                }
            }
            "QUIT" => {
                let _ = reply(&mut c, "221 Goodbye").await;
                break;
            }
            _ => format!("502 {cmd} not implemented"),
        };
        if reply(&mut c, &r).await.is_err() {
            break;
        }
    }
}

pub fn serve(envs: Envs, port: u16) {
    tokio::spawn(async move {
        let Ok(l) = TcpListener::bind(("0.0.0.0", port)).await else {
            eprintln!("FTP: port {port} is taken");
            return;
        };
        loop {
            let Ok((sock, _)) = l.accept().await else { continue };
            if let Some(ctx) = envs.default_env() {
                tokio::spawn(conn(ctx, sock));
            }
        }
    });
}

/// A data listener: any free port, or one from STUNTMAN_FTP_PASSIVE_PORTS ("30000-30009") so a
/// container can publish the range.
async fn passive(local_ip: std::net::IpAddr) -> std::io::Result<TcpListener> {
    let Some((a, b)) = std::env::var("STUNTMAN_FTP_PASSIVE_PORTS").ok().and_then(|r| {
        let (a, b) = r.split_once('-')?;
        Some((a.trim().parse::<u16>().ok()?, b.trim().parse::<u16>().ok()?))
    }) else {
        return TcpListener::bind((local_ip, 0)).await;
    };
    for p in a..=b {
        if let Ok(l) = TcpListener::bind(("0.0.0.0", p)).await {
            return Ok(l);
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "every passive port is taken"))
}
