//! The inboxes' own HTTP port. Written by hand so the failures are real: a reset is a TCP reset,
//! an empty reply closes without a byte, a hang notices when the client gives up, and the traps
//! (private redirect, huge and slow answers) record whether the app fell for them.

use std::time::{Duration, Instant};

use stuntman_core::{env::Envs, util};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

use crate::{config, sign, store};

const MAX_BODY: usize = 10 << 20;

pub async fn serve(envs: Envs, port: u16) {
    let l = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("webhook inboxes: port {port}: {e}");
            *crate::BIND_ERROR.lock().unwrap() = Some(format!("port {port}: {e}"));
            return;
        }
    };
    loop {
        let Ok((sock, addr)) = l.accept().await else { continue };
        let e = envs.clone();
        tokio::spawn(async move {
            let _ = handle(e, sock, addr.ip().to_string(), port).await;
        });
    }
}

struct Req {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

async fn read_req(s: &mut TcpStream) -> std::io::Result<Option<Req>> {
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 8192];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > 64 * 1024 {
            return Ok(None);
        }
        let n = s.read(&mut tmp).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/").to_string());
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    let get = |k: &str| headers.iter().find(|(h, _)| h.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone());
    let mut rest = buf[head_end + 4..].to_vec();
    if get("expect").map(|e| e.eq_ignore_ascii_case("100-continue")).unwrap_or(false) {
        s.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
    }
    let body = if get("transfer-encoding").map(|t| t.to_lowercase().contains("chunked")).unwrap_or(false) {
        // Chunked: read chunk sizes until the 0 chunk.
        let mut out = vec![];
        loop {
            while !rest.windows(2).any(|w| w == b"\r\n") {
                let n = s.read(&mut tmp).await?;
                if n == 0 {
                    return Ok(None);
                }
                rest.extend_from_slice(&tmp[..n]);
            }
            let p = rest.windows(2).position(|w| w == b"\r\n").unwrap();
            let size = usize::from_str_radix(String::from_utf8_lossy(&rest[..p]).split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
            rest.drain(..p + 2);
            if size == 0 {
                break;
            }
            while rest.len() < size + 2 {
                let n = s.read(&mut tmp).await?;
                if n == 0 {
                    return Ok(None);
                }
                rest.extend_from_slice(&tmp[..n]);
            }
            out.extend_from_slice(&rest[..size]);
            rest.drain(..size + 2);
            if out.len() > MAX_BODY {
                return Ok(None);
            }
        }
        out
    } else {
        let len: usize = get("content-length").and_then(|l| l.parse().ok()).unwrap_or(0).min(MAX_BODY);
        while rest.len() < len {
            let n = s.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            rest.extend_from_slice(&tmp[..n]);
        }
        rest.truncate(len);
        rest
    };
    Ok(Some(Req { method, target, headers, body }))
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        410 => "Gone",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Status",
    }
}

async fn respond(s: &mut TcpStream, code: u16, body: &str, extra: &str) -> std::io::Result<()> {
    let r = format!("HTTP/1.1 {code} {}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", reason(code), body.len());
    s.write_all(r.as_bytes()).await?;
    s.flush().await
}

/// Waits `ms`, returning early with the elapsed time if the client closes the connection.
async fn wait_or_gave_up(s: &mut TcpStream, ms: u64, t0: Instant) -> Option<i64> {
    if ms == 0 {
        return None;
    }
    let mut b = [0u8; 256];
    let sleep = tokio::time::sleep(Duration::from_millis(ms));
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            _ = &mut sleep => return None,
            r = s.read(&mut b) => match r {
                Ok(0) | Err(_) => return Some(t0.elapsed().as_millis() as i64),
                Ok(_) => continue,
            }
        }
    }
}

async fn handle(envs: Envs, mut s: TcpStream, remote: String, port: u16) -> std::io::Result<()> {
    let Some(mut req) = read_req(&mut s).await? else { return Ok(()) };
    let t0 = Instant::now();
    // The environment: /e/<name>/ prefix, else the Host header (*.<base_domain>), else the default.
    let ctx = match req.target.strip_prefix("/e/").and_then(|r| r.split_once('/')) {
        Some((name, rest)) => {
            let c = envs.get(name);
            req.target = format!("/{rest}");
            c
        }
        None => envs.by_host(req.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("host")).map(|(_, v)| v.as_str())),
    };
    let Some(ctx) = ctx else {
        return respond(&mut s, 404, "No Stuntman environment answers for this host\n", "").await;
    };
    if ctx.state.paused() {
        return respond(&mut s, 503, "This environment is paused in Stuntman\n", "").await;
    }
    let path = req.target.split('?').next().unwrap_or("/").to_string();

    // A followed redirect trap: the app called the "private" address it was sent to.
    if let Some(id) = path.strip_prefix("/_trap/") {
        let _ = store::set_trap(&ctx, id, "redirect_private", "failed", "followed the redirect to 127.0.0.1, a private address: an SSRF hole").await;
        return respond(&mut s, 200, "you followed a redirect to a private address", "").await;
    }
    let Some(name) = path.strip_prefix("/in/").map(|r| r.split('/').next().unwrap_or("").to_string()).filter(|n| !n.is_empty()) else {
        return respond(&mut s, 404, "Stuntman webhook inboxes: send to /in/<inbox>\n", "").await;
    };
    ctx.live.seen(req.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("user-agent")).map(|(_, v)| v.split('/').next().unwrap_or("app")).unwrap_or("app"), "Webhooks");

    let inbox = config::get(&ctx, &name).await.ok().flatten();
    let check = inbox.as_ref().map(|i| sign::check(i, &req.headers, &req.body, util::now_ms(), &ctx.data_dir)).unwrap_or_default();
    let (id, attempt, _) = match store::insert(&ctx, store::New { inbox: name.clone(), method: req.method.clone(), path: req.target.clone(), headers: req.headers.clone(), body: req.body.clone(), signature: json!(check), remote }).await {
        Ok(x) => x,
        Err(_) => return respond(&mut s, 500, "Stuntman could not record the delivery", "").await,
    };
    let done = |answer: String, gave: Option<i64>, traps: serde_json::Value| {
        let (c, i) = (ctx.clone(), id.clone());
        let ms = t0.elapsed().as_millis() as i64;
        async move {
            let _ = store::finish(&c, &i, &answer, Some(ms), gave, traps).await;
        }
    };

    let Some(inbox) = inbox.filter(|i| !i.expired()) else {
        let gone = config::gone(&ctx, &name).await.unwrap_or(false) || config::get(&ctx, &name).await.ok().flatten().is_some();
        let code = if gone { 410 } else { 404 };
        let _ = respond(&mut s, code, if gone { "This inbox expired or was removed" } else { "No such inbox" }, "").await;
        done(code.to_string(), None, json!([])).await;
        return Ok(());
    };

    if inbox.signature.reject_bad && matches!(check.result.as_str(), "invalid" | "old_secret" | "missing") {
        let _ = respond(&mut s, 401, "Invalid signature", "").await;
        done("401".into(), None, json!([])).await;
        return Ok(());
    }

    // One trap per delivery, taking turns when several are on.
    let seq: usize = id.trim_start_matches("dlv_").parse().unwrap_or(0);
    let trap = (!inbox.traps.is_empty()).then(|| inbox.traps[seq % inbox.traps.len()].clone());

    if let Some(g) = wait_or_gave_up(&mut s, inbox.delay_ms(), t0).await {
        done("no answer".into(), Some(g), json!([])).await;
        return Ok(());
    }

    match trap.as_deref() {
        Some("redirect_private") if !inbox.redirect_to.is_empty() => {
            let loc = inbox.redirect_to.clone();
            let _ = respond(&mut s, 302, "", &format!("Location: {loc}\r\n")).await;
            done("302".into(), None, json!([{"trap": "redirect_private", "result": "unobserved",
                "detail": format!("302 → {loc}; Stuntman cannot see that address, so whether the app followed is not known. Leave the target empty to observe it.")}])).await;
            return Ok(());
        }
        Some("redirect_private") => {
            let loc = format!("http://127.0.0.1:{port}{}/_trap/{id}", ctx.prefix());
            let _ = respond(&mut s, 302, "", &format!("Location: {loc}\r\n")).await;
            done("302".into(), None, json!([{"trap": "redirect_private", "result": "pending", "detail": format!("302 → {loc}")}])).await;
            let (c, i) = (ctx.clone(), id.clone());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(20)).await;
                if let Ok(d) = store::get(&c, &i).await {
                    if d["traps"].as_array().map(|a| a.iter().any(|t| t["result"] == "pending")).unwrap_or(false) {
                        let _ = store::set_trap(&c, &i, "redirect_private", "passed", "not followed ✓").await;
                    }
                }
            });
            return Ok(());
        }
        Some("huge") => {
            let total: usize = 50 << 20;
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n");
            let chunk = vec![b'x'; 64 * 1024];
            let mut sent = 0usize;
            let mut ok = s.write_all(head.as_bytes()).await.is_ok();
            while ok && sent < total {
                ok = s.write_all(&chunk).await.is_ok();
                if ok {
                    sent += chunk.len();
                }
            }
            let (res, det) = if sent >= total { ("failed", "read all 50 MB of the answer".to_string()) } else { ("passed", format!("stopped reading after {:.1} MB ✓", sent as f64 / (1 << 20) as f64)) };
            done("200".into(), None, json!([{"trap": "huge", "result": res, "detail": det}])).await;
            return Ok(());
        }
        Some("slow") => {
            let n = 60;
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n");
            let _ = s.write_all(head.as_bytes()).await;
            let mut gave = None;
            for _ in 0..n {
                if s.write_all(b".").await.is_err() {
                    gave = Some(t0.elapsed().as_millis() as i64);
                    break;
                }
                let _ = s.flush().await;
                if let Some(g) = wait_or_gave_up(&mut s, 1000, t0).await {
                    gave = Some(g);
                    break;
                }
            }
            let (res, det) = match gave { Some(g) => ("passed", format!("gave up after {:.1} s ✓", g as f64 / 1000.0)), None => ("failed", "waited 60 s for 60 bytes".to_string()) };
            done("200".into(), gave, json!([{"trap": "slow", "result": res, "detail": det}])).await;
            return Ok(());
        }
        _ => {}
    }

    let a = &inbox.answer;
    if a.mode != "always_200" && a.mode != "accept_202" {
        let _ = stuntman_core::trips::set_rule(&ctx, &id, &format!("{} · {}", inbox.name, inbox.summary())).await;
    }
    let code: u16 = match a.mode.as_str() {
        "accept_202" => 202,
        "error" => a.status,
        "fail_then" => {
            let key = if a.per == "inbox" { "inbox".to_string() } else { format!("event:{}", sign::header_like(&req.headers, "event-id").map(|h| h.1.clone()).unwrap_or_else(|| store::event_of(&req.headers, &req.body).1)) };
            let n = store::count_and_bump(&ctx, &name, &key).await.unwrap_or(1);
            if n as u32 <= a.times { a.status } else { a.then }
        }
        "fail_share" => {
            if rand::random::<u32>() % 100 < a.share { a.status } else { a.then }
        }
        "hang" => {
            if let Some(g) = wait_or_gave_up(&mut s, inbox.hang_ms(), t0).await {
                done("no answer".into(), Some(g), json!([])).await;
                return Ok(());
            }
            200
        }
        "reset" => {
            // A zero linger makes close() send RST at once: the reset the app must handle.
            #[allow(deprecated)]
            let _ = s.set_linger(Some(Duration::ZERO));
            drop(s);
            done("reset".into(), None, json!([])).await;
            return Ok(());
        }
        "empty" => {
            let _ = s.shutdown().await;
            done("empty".into(), None, json!([])).await;
            return Ok(());
        }
        _ => 200,
    };
    let _ = attempt;
    let gave = respond(&mut s, code, if code < 300 { "ok" } else { "error" }, "").await.err().map(|_| t0.elapsed().as_millis() as i64);
    done(code.to_string(), gave, json!([])).await;
    Ok(())
}
