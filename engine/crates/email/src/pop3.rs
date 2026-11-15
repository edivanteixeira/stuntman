//! POP3 for mailboxes the app reads (A16): CAPA, STLS, USER, PASS, AUTH PLAIN, STAT, LIST, UIDL,
//! RETR, TOP, DELE, RSET, NOOP, QUIT (deletions take effect on QUIT). INBOX only, as POP3 has.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use stuntman_core::{env::Envs, Ctx};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{imap::Io, mailbox};

type BoxIo = Box<dyn Io>;

async fn w(c: &mut BufReader<BoxIo>, s: &str) -> std::io::Result<()> {
    c.get_mut().write_all(format!("{s}\r\n").as_bytes()).await?;
    c.get_mut().flush().await
}

fn stuffed(raw: &[u8]) -> Vec<u8> {
    let t = String::from_utf8_lossy(raw).replace("\r\n", "\n");
    let mut out = String::new();
    for l in t.split('\n') {
        if l.starts_with('.') {
            out.push('.');
        }
        out.push_str(l);
        out.push_str("\r\n");
    }
    out.push_str(".\r\n");
    out.into_bytes()
}

async fn conn(ctx: Ctx, sock: tokio::net::TcpStream) {
    let mut c = BufReader::new(Box::new(sock) as BoxIo);
    let mut user = String::new();
    let mut b: Option<mailbox::Box_> = None;
    let mut msgs: Vec<mailbox::Msg> = vec![];
    let mut dele: Vec<i64> = vec![];
    let mut tls = false;
    if w(&mut c, "+OK Stuntman POP3 ready").await.is_err() {
        return;
    }
    loop {
        let mut line = String::new();
        match tokio::time::timeout(Duration::from_secs(600), c.read_line(&mut line)).await {
            Ok(Ok(0)) | Err(_) | Ok(Err(_)) => break,
            _ => {}
        }
        let line = line.trim_end().to_string();
        let mut p = line.splitn(3, ' ');
        let cmd = p.next().unwrap_or("").to_uppercase();
        stuntman_core::usage::hit(&ctx, "email");
        let a1 = p.next().unwrap_or("").to_string();
        let a2 = p.next().unwrap_or("").to_string();
        if let Some(bx) = &b {
            mailbox::log(&ctx, &bx.address, "POP3", &line, "").await;
        }
        let live: Vec<(usize, &mailbox::Msg)> = msgs.iter().enumerate().filter(|(_, m)| !dele.contains(&m.uid)).map(|(i, m)| (i + 1, m)).collect();
        let pick = |n: &str| -> Option<&mailbox::Msg> { let n: usize = n.parse().ok()?; msgs.get(n.checked_sub(1)?).filter(|m| !dele.contains(&m.uid)) };
        let r = match cmd.as_str() {
            "CAPA" => "+OK\r\nUSER\r\nUIDL\r\nTOP\r\nSTLS\r\nSASL PLAIN\r\n.".to_string(),
            "STLS" => {
                if w(&mut c, "+OK Begin TLS").await.is_err() { break }
                let inner = c.into_inner();
                match crate::smtp::tls().accept(inner).await {
                    Ok(t) => { c = BufReader::new(Box::new(t) as BoxIo); tls = true; continue }
                    Err(_) => return,
                }
            }
            "USER" => { user = a1; "+OK".into() }
            "PASS" | "AUTH" => {
                let (u, pw) = if cmd == "AUTH" {
                    let raw = String::from_utf8_lossy(&B64.decode(a2.trim()).unwrap_or_default()).to_string();
                    let x: Vec<&str> = raw.split('\0').collect();
                    (x.get(1).unwrap_or(&"").to_string(), x.get(2).unwrap_or(&"").to_string())
                } else {
                    (user.clone(), if a2.is_empty() { a1 } else { format!("{a1} {a2}") })
                };
                match mailbox::get(&ctx, &u).await.ok().flatten() {
                    Some(bx) if bx.provoke == "tls_required" && !tls => "-ERR [AUTH] Use STLS first".into(),
                    Some(bx) if bx.provoke == "login_refused" => { mailbox::log(&ctx, &u, "POP3", "PASS ****", "refused (provoked)").await; "-ERR [AUTH] Authentication failed".into() }
                    Some(bx) if bx.provoke == "busy" => "-ERR [IN-USE] Mailbox locked by another session".into(),
                    Some(bx) if mailbox::password_ok(&ctx, &bx, &pw) => {
                        msgs = mailbox::messages(&ctx, &bx.address, "INBOX").await.unwrap_or_default();
                        mailbox::log(&ctx, &u, "POP3", "PASS ****", "OK").await;
                        b = Some(bx);
                        format!("+OK {} messages", msgs.len())
                    }
                    _ => "-ERR [AUTH] Authentication failed".into(),
                }
            }
            "QUIT" => {
                if let Some(bx) = &b {
                    for uid in &dele {
                        let _ = mailbox::flags(&ctx, &bx.address, *uid, None, Some(true), None).await;
                    }
                    let _ = mailbox::expunge(&ctx, &bx.address, "INBOX").await;
                }
                let _ = w(&mut c, "+OK Bye").await;
                break;
            }
            "NOOP" => "+OK".into(),
            _ if b.is_none() => "-ERR Log in first".into(),
            "STAT" => format!("+OK {} {}", live.len(), live.iter().map(|(_, m)| m.raw.len()).sum::<usize>()),
            "LIST" | "UIDL" => {
                let one = |n: usize, m: &mailbox::Msg| if cmd == "LIST" { format!("{n} {}", m.raw.len()) } else { format!("{n} {}", m.uid) };
                if a1.is_empty() {
                    format!("+OK\r\n{}.", live.iter().map(|(n, m)| format!("{}\r\n", one(*n, m))).collect::<String>())
                } else {
                    match a1.parse::<usize>().ok().and_then(|n| pick(&a1).map(|m| one(n, m))) { Some(x) => format!("+OK {x}"), None => "-ERR No such message".into() }
                }
            }
            "RETR" | "TOP" => match pick(&a1) {
                Some(m) => {
                    let drop = b.as_ref().map(|x| x.provoke == "drop_fetch").unwrap_or(false);
                    let mut body = m.raw.clone();
                    if cmd == "TOP" {
                        let n: usize = a2.trim().parse().unwrap_or(0);
                        let t = String::from_utf8_lossy(&m.raw).to_string();
                        let (h, rest) = t.split_once("\r\n\r\n").unwrap_or((&t, ""));
                        body = format!("{h}\r\n\r\n{}", rest.split("\r\n").take(n).collect::<Vec<_>>().join("\r\n")).into_bytes();
                    }
                    let mut out = format!("+OK {} octets\r\n", m.raw.len()).into_bytes();
                    out.extend(stuffed(&body));
                    if drop {
                        let _ = c.get_mut().write_all(&out[..out.len() / 2]).await;
                        let _ = c.get_mut().flush().await;
                        if let Some(bx) = &b { mailbox::log(&ctx, &bx.address, "POP3", &line, "connection dropped mid-RETR (provoked)").await; }
                        return;
                    }
                    if c.get_mut().write_all(&out).await.is_err() { break }
                    if cmd == "RETR" {
                        if let Some(bx) = &b { let _ = mailbox::flags(&ctx, &bx.address, m.uid, Some(true), None, None).await; }
                    }
                    continue;
                }
                None => "-ERR No such message".into(),
            },
            "DELE" => match pick(&a1) { Some(m) => { dele.push(m.uid); format!("+OK Message {a1} deleted") } None => "-ERR No such message".into() },
            "RSET" => { dele.clear(); "+OK".into() }
            _ => "-ERR Unknown command".into(),
        };
        if w(&mut c, &r).await.is_err() {
            break;
        }
    }
}

pub fn serve(envs: Envs, port: u16) {
    tokio::spawn(async move {
        let Ok(l) = tokio::net::TcpListener::bind(("0.0.0.0", port)).await else {
            eprintln!("POP3: port {port} is taken");
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
