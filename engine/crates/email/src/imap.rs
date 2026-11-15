//! IMAP4rev1 for mailboxes the app reads (A16): CAPABILITY, STARTTLS, LOGIN, AUTHENTICATE PLAIN and
//! XOAUTH2, LIST, LSUB, STATUS, SELECT, EXAMINE, CREATE, DELETE, FETCH (FLAGS, UID, INTERNALDATE,
//! RFC822.SIZE, ENVELOPE, BODYSTRUCTURE, BODY[...] sections and partials), STORE, SEARCH, COPY,
//! MOVE, EXPUNGE, CLOSE, IDLE, NOOP, LOGOUT — with UID variants. One port for every environment:
//! logins go to the default one.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use mail_parser::{MessageParser, MimeHeaders, PartType};
use stuntman_core::{env::Envs, Ctx};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::mailbox::{self, Msg};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type BoxIo = Box<dyn Io>;

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn nq(s: Option<&str>) -> String {
    match s {
        Some(x) if !x.is_empty() => q(x),
        _ => "NIL".into(),
    }
}

fn addrs(a: Option<&mail_parser::Address>) -> String {
    let Some(a) = a else { return "NIL".into() };
    let list: Vec<String> = a.iter().map(|x| {
        let (mb, host) = x.address().and_then(|e| e.split_once('@')).map(|(m, h)| (m.to_string(), h.to_string())).unwrap_or_default();
        format!("({} NIL {} {})", nq(x.name()), nq(Some(&mb)), nq(Some(&host)))
    }).collect();
    if list.is_empty() { "NIL".into() } else { format!("({})", list.concat()) }
}

pub fn envelope(m: &mail_parser::Message) -> String {
    let date = m.header_raw("Date").map(|d| d.trim().to_string());
    let from = addrs(m.from());
    let sender = m.sender().map(|s| addrs(Some(s))).unwrap_or(from.clone());
    let reply = m.reply_to().map(|s| addrs(Some(s))).unwrap_or(from.clone());
    format!("({} {} {} {} {} {} {} {} {} {})", nq(date.as_deref()), nq(m.subject()), from, sender, reply, addrs(m.to()), addrs(m.cc()), addrs(m.bcc()),
        nq(m.header_raw("In-Reply-To").map(|x| x.trim())), nq(m.message_id().map(|x| format!("<{x}>")).as_deref()))
}

fn part_structure(m: &mail_parser::Message, id: u32, ext: bool) -> String {
    let Some(p) = m.part(id) else { return "NIL".into() };
    let ct = p.content_type();
    let ty = ct.map(|c| c.ctype().to_lowercase()).unwrap_or("text".into());
    let sub = ct.and_then(|c| c.subtype()).map(|s| s.to_lowercase()).unwrap_or("plain".into());
    if let PartType::Multipart(children) = &p.body {
        let kids: String = children.iter().map(|c| part_structure(m, *c, ext)).collect();
        let b = ct.and_then(|c| c.attribute("boundary")).unwrap_or("");
        return if ext { format!("({kids} {} (\"boundary\" {}) NIL NIL)", q(&sub), q(b)) } else { format!("({kids} {})", q(&sub)) };
    }
    let attrs: Vec<String> = ct.and_then(|c| c.attributes.as_ref()).map(|a| a.iter().map(|x| format!("{} {}", q(&x.name), q(&x.value))).collect()).unwrap_or_default();
    let params = if attrs.is_empty() { if ty == "text" { "(\"charset\" \"us-ascii\")".into() } else { "NIL".to_string() } } else { format!("({})", attrs.join(" ")) };
    let enc = p.content_transfer_encoding().map(|e| e.to_uppercase()).unwrap_or("7BIT".into());
    let size = p.offset_end.saturating_sub(p.offset_body);
    let raw_body = &m.raw_message[p.offset_body as usize..p.offset_end as usize];
    let lines = raw_body.iter().filter(|b| **b == b'\n').count();
    let base = format!("{} {} {params} {} {} {} {size}", q(&ty), q(&sub), nq(p.content_id()), nq(p.content_description()), q(&enc));
    let disp = p.content_disposition().map(|d| format!("({} {})", q(&d.ctype().to_uppercase()), d.attributes.as_ref().map(|a| format!("({})", a.iter().map(|x| format!("{} {}", q(&x.name), q(&x.value))).collect::<Vec<_>>().join(" "))).unwrap_or("NIL".into()))).unwrap_or("NIL".into());
    if ty == "text" {
        if ext { format!("({base} {lines} NIL {disp} NIL NIL)") } else { format!("({base} {lines})") }
    } else if ext {
        format!("({base} NIL {disp} NIL NIL)")
    } else {
        format!("({base})")
    }
}

/// The MIME part a section path names ("1", "2.1"): its id in the parsed message.
fn part_id(m: &mail_parser::Message, path: &str) -> Option<u32> {
    let mut id = 0u32;
    for n in path.split('.') {
        let n: usize = n.parse().ok()?;
        match &m.part(id)?.body {
            PartType::Multipart(ch) => id = *ch.get(n - 1)?,
            PartType::Message(_) => return None,
            _ if n == 1 && id == 0 => {}
            _ => return None,
        }
    }
    Some(id)
}

/// The bytes of a BODY[section].
fn section(raw: &[u8], sec: &str) -> Vec<u8> {
    let Some(m) = MessageParser::default().parse(raw) else { return raw.to_vec() };
    let up = sec.to_uppercase();
    let root = m.root_part();
    let headers = &raw[..root.offset_body as usize];
    if up.is_empty() {
        return raw.to_vec();
    }
    if up == "HEADER" {
        return headers.to_vec();
    }
    if up == "TEXT" {
        return raw[root.offset_body as usize..].to_vec();
    }
    if let Some(list) = up.strip_prefix("HEADER.FIELDS.NOT").or(up.strip_prefix("HEADER.FIELDS")) {
        let not = up.starts_with("HEADER.FIELDS.NOT");
        let names: Vec<String> = list.trim().trim_start_matches('(').trim_end_matches(')').split_whitespace().map(|x| x.trim_matches('"').to_lowercase()).collect();
        let text = String::from_utf8_lossy(headers);
        let mut out = String::new();
        let mut keep = false;
        for line in text.split_inclusive("\r\n").flat_map(|l| l.split_inclusive('\n')) {
            if !line.starts_with(' ') && !line.starts_with('\t') {
                let name = line.split(':').next().unwrap_or("").trim().to_lowercase();
                keep = !line.trim().is_empty() && (names.contains(&name) != not);
            }
            if keep {
                out.push_str(line);
            }
        }
        out.push_str("\r\n");
        return out.into_bytes();
    }
    let (path, what) = match up.rsplit_once('.') {
        Some((p, w)) if ["MIME", "HEADER", "TEXT"].contains(&w) => (p.to_string(), w.to_string()),
        _ => (up.clone(), String::new()),
    };
    let Some(id) = part_id(&m, &path) else { return vec![] };
    let Some(p) = m.part(id) else { return vec![] };
    match what.as_str() {
        "MIME" | "HEADER" => raw[p.offset_header as usize..p.offset_body as usize].to_vec(),
        _ => raw[p.offset_body as usize..p.offset_end as usize].to_vec(),
    }
}

/// "1:3,5,7:*" against the sequence numbers (or UIDs) that exist.
fn in_set(set: &str, n: i64, max: i64) -> bool {
    set.split(',').any(|r| {
        let (a, b) = r.split_once(':').unwrap_or((r, r));
        let v = |x: &str| if x == "*" { max } else { x.parse().unwrap_or(0) };
        let (lo, hi) = (v(a).min(v(b)), v(a).max(v(b)));
        n >= lo && n <= hi
    })
}

/// Splits "a (b c) "d e" {lit}" into tokens, keeping parentheses as one token.
fn tokens(s: &str) -> Vec<String> {
    let c: Vec<char> = s.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            ' ' => i += 1,
            '"' => {
                let mut t = String::new();
                i += 1;
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' && i + 1 < c.len() {
                        i += 1;
                    }
                    t.push(c[i]);
                    i += 1;
                }
                i += 1;
                out.push(t);
            }
            '(' | '[' => {
                let (open, close) = if c[i] == '(' { ('(', ')') } else { ('[', ']') };
                let mut depth = 0;
                let st = i;
                while i < c.len() {
                    if c[i] == open { depth += 1 }
                    if c[i] == close { depth -= 1; if depth == 0 { i += 1; break } }
                    i += 1;
                }
                out.push(c[st..i].iter().collect());
            }
            _ => {
                let st = i;
                let mut depth = 0;
                while i < c.len() && (c[i] != ' ' || depth > 0) {
                    if c[i] == '[' { depth += 1 }
                    if c[i] == ']' { depth -= 1 }
                    i += 1;
                }
                out.push(c[st..i].iter().collect());
            }
        }
    }
    out
}

struct S {
    ctx: Ctx,
    user: Option<mailbox::Box_>,
    folder: Option<String>,
    readonly: bool,
    view: Vec<Msg>,
    tls: bool,
}

async fn w(c: &mut BufReader<BoxIo>, s: &str) -> std::io::Result<()> {
    c.get_mut().write_all(s.as_bytes()).await?;
    c.get_mut().write_all(b"\r\n").await?;
    c.get_mut().flush().await
}

/// One command line, with its literals ({n}) read in.
async fn read_cmd(c: &mut BufReader<BoxIo>) -> Option<String> {
    let mut out = String::new();
    loop {
        let mut line = Vec::new();
        let n = tokio::time::timeout(Duration::from_secs(1800), c.read_until(b'\n', &mut line)).await.ok()?.ok()?;
        if n == 0 {
            return None;
        }
        let l = String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string();
        if let Some(st) = l.rfind('{').filter(|_| l.ends_with('}')) {
            let spec = &l[st + 1..l.len() - 1];
            let plus = spec.ends_with('+');
            let len: usize = spec.trim_end_matches('+').parse().ok()?;
            out.push_str(&l[..st]);
            if !plus {
                w(c, "+ Ready for literal data").await.ok()?;
            }
            let mut buf = vec![0u8; len];
            c.read_exact(&mut buf).await.ok()?;
            out.push_str(&q(&String::from_utf8_lossy(&buf)));
            continue;
        }
        out.push_str(&l);
        return Some(out);
    }
}

fn flags(m: &Msg) -> String {
    let mut f = vec![];
    if m.seen { f.push("\\Seen") }
    if m.deleted { f.push("\\Deleted") }
    if m.flagged { f.push("\\Flagged") }
    format!("({})", f.join(" "))
}

async fn refresh(s: &mut S) {
    if let (Some(b), Some(f)) = (&s.user, &s.folder) {
        s.view = mailbox::messages(&s.ctx, &b.address, f).await.unwrap_or_default();
    }
}

async fn fetch(c: &mut BufReader<BoxIo>, s: &mut S, set: &str, items: &str, by_uid: bool) -> std::io::Result<bool> {
    let up = items.to_uppercase();
    let mut want: Vec<String> = match up.trim() {
        "ALL" => vec!["FLAGS".into(), "INTERNALDATE".into(), "RFC822.SIZE".into(), "ENVELOPE".into()],
        "FAST" => vec!["FLAGS".into(), "INTERNALDATE".into(), "RFC822.SIZE".into()],
        "FULL" => vec!["FLAGS".into(), "INTERNALDATE".into(), "RFC822.SIZE".into(), "ENVELOPE".into(), "BODY".into()],
        x => tokens(x.trim_start_matches('(').trim_end_matches(')')),
    };
    if by_uid && !want.iter().any(|x| x == "UID") {
        want.insert(0, "UID".into());
    }
    let addr = s.user.as_ref().map(|b| b.address.clone()).unwrap_or_default();
    let drop = s.user.as_ref().map(|b| b.provoke == "drop_fetch").unwrap_or(false);
    let max = if by_uid { s.view.last().map(|m| m.uid).unwrap_or(0) } else { s.view.len() as i64 };
    let mut seen_now = vec![];
    for (i, m) in s.view.iter().enumerate() {
        let seq = i as i64 + 1;
        if !in_set(set, if by_uid { m.uid } else { seq }, max) {
            continue;
        }
        let parsed = MessageParser::default().parse(&m.raw);
        let mut parts: Vec<Vec<u8>> = vec![];
        let mut sets_seen = false;
        for it in &want {
            let u = it.to_uppercase();
            match u.as_str() {
                "UID" => parts.push(format!("UID {}", m.uid).into_bytes()),
                "FLAGS" => parts.push(format!("FLAGS {}", flags(m)).into_bytes()),
                "INTERNALDATE" => parts.push(format!("INTERNALDATE \"{}\"", mailbox::imap_date(m.at)).into_bytes()),
                "RFC822.SIZE" => parts.push(format!("RFC822.SIZE {}", m.raw.len()).into_bytes()),
                "ENVELOPE" => parts.push(format!("ENVELOPE {}", parsed.as_ref().map(envelope).unwrap_or("NIL".into())).into_bytes()),
                "BODYSTRUCTURE" | "BODY" => parts.push(format!("{u} {}", parsed.as_ref().map(|p| part_structure(p, 0, u == "BODYSTRUCTURE")).unwrap_or("NIL".into())).into_bytes()),
                x if x.starts_with("BODY[") || x.starts_with("BODY.PEEK[") || x.starts_with("RFC822") => {
                    let peek = x.starts_with("BODY.PEEK[") || x == "RFC822.HEADER";
                    let (sec, name) = if let Some(r) = x.strip_prefix("RFC822") {
                        match r { ".HEADER" => ("HEADER".to_string(), "RFC822.HEADER".to_string()), ".TEXT" => ("TEXT".to_string(), "RFC822.TEXT".to_string()), _ => (String::new(), "RFC822".to_string()) }
                    } else {
                        let inner = it[it.find('[').unwrap() + 1..it.rfind(']').unwrap_or(it.len())].to_string();
                        (inner.clone(), format!("BODY[{inner}]"))
                    };
                    let mut data = section(&m.raw, &sec);
                    let mut label = name;
                    if let Some(p) = it.rfind('<') {
                        let r = it[p + 1..].trim_end_matches('>');
                        let (st, len) = r.split_once('.').map(|(a, b)| (a.parse::<usize>().unwrap_or(0), b.parse::<usize>().unwrap_or(usize::MAX))).unwrap_or((r.parse().unwrap_or(0), usize::MAX));
                        data = data.into_iter().skip(st).take(len).collect();
                        label = format!("{label}<{st}>");
                    }
                    if !peek && !s.readonly {
                        sets_seen = true;
                    }
                    let mut v = format!("{label} {{{}}}\r\n", data.len()).into_bytes();
                    v.extend(data);
                    parts.push(v);
                }
                _ => {}
            }
        }
        if sets_seen && !m.seen {
            seen_now.push(m.uid);
            parts.push(b"FLAGS (\\Seen)".to_vec());
        }
        let mut line = format!("* {seq} FETCH (").into_bytes();
        for (k, p) in parts.iter().enumerate() {
            if k > 0 {
                line.push(b' ');
            }
            line.extend(p);
        }
        line.extend(b")\r\n");
        if drop {
            let half = line.len() / 2;
            let _ = c.get_mut().write_all(&line[..half]).await;
            let _ = c.get_mut().flush().await;
            mailbox::log(&s.ctx, &addr, "IMAP", "FETCH", "connection dropped mid-FETCH (provoked)").await;
            return Ok(false);
        }
        c.get_mut().write_all(&line).await?;
    }
    for uid in seen_now {
        let _ = mailbox::flags(&s.ctx, &addr, uid, Some(true), None, None).await;
    }
    refresh(s).await;
    Ok(true)
}

fn search(s: &S, args: &[String], by_uid: bool) -> Vec<i64> {
    let max = s.view.last().map(|m| m.uid).unwrap_or(0);
    let mut out = vec![];
    for (i, m) in s.view.iter().enumerate() {
        let p = MessageParser::default().parse(&m.raw);
        let mut ok = true;
        let mut k = 0;
        let mut neg = false;
        while k < args.len() {
            let a = args[k].to_uppercase();
            let next = args.get(k + 1).cloned().unwrap_or_default();
            let hit = match a.as_str() {
                "NOT" => { neg = !neg; k += 1; continue }
                "ALL" => true,
                "SEEN" => m.seen,
                "UNSEEN" | "NEW" => !m.seen,
                "DELETED" => m.deleted,
                "UNDELETED" => !m.deleted,
                "FLAGGED" => m.flagged,
                "UNFLAGGED" => !m.flagged,
                "UID" => { k += 1; in_set(&next, m.uid, max) }
                "FROM" => { k += 1; p.as_ref().and_then(|x| x.from()).map(|f| f.iter().any(|a| a.address().unwrap_or("").to_lowercase().contains(&next.to_lowercase()) || a.name().unwrap_or("").to_lowercase().contains(&next.to_lowercase()))).unwrap_or(false) }
                "SUBJECT" => { k += 1; p.as_ref().and_then(|x| x.subject()).map(|x| x.to_lowercase().contains(&next.to_lowercase())).unwrap_or(false) }
                "TEXT" | "BODY" => { k += 1; String::from_utf8_lossy(&m.raw).to_lowercase().contains(&next.to_lowercase()) }
                "SINCE" | "BEFORE" | "ON" | "SENTSINCE" | "SENTBEFORE" | "LARGER" | "SMALLER" => { k += 1; true }
                x if x.chars().next().map(|c| c.is_ascii_digit() || c == '*').unwrap_or(false) => in_set(x, i as i64 + 1, s.view.len() as i64),
                _ => true,
            };
            ok &= hit != neg;
            neg = false;
            k += 1;
        }
        if ok {
            out.push(if by_uid { m.uid } else { i as i64 + 1 });
        }
    }
    out
}

async fn conn(ctx: Ctx, sock: tokio::net::TcpStream) {
    let mut c = BufReader::new(Box::new(sock) as BoxIo);
    let mut s = S { ctx: ctx.clone(), user: None, folder: None, readonly: false, view: vec![], tls: false };
    if w(&mut c, "* OK [CAPABILITY IMAP4rev1 STARTTLS AUTH=PLAIN AUTH=XOAUTH2 IDLE UIDPLUS MOVE] Stuntman IMAP ready").await.is_err() {
        return;
    }
    while let Some(line) = read_cmd(&mut c).await {
        let t = tokens(&line);
        if t.len() < 2 {
            if w(&mut c, "* BAD empty command").await.is_err() { break }
            continue;
        }
        let tag = t[0].clone();
        let mut cmd = t[1].to_uppercase();
        let mut args: Vec<String> = t[2..].to_vec();
        let by_uid = cmd == "UID";
        if by_uid {
            cmd = args.first().map(|x| x.to_uppercase()).unwrap_or_default();
            args.remove(0);
        }
        let addr = s.user.as_ref().map(|b| b.address.clone()).unwrap_or_default();
        if !addr.is_empty() {
            mailbox::log(&ctx, &addr, "IMAP", &format!("{}{cmd} {}", if by_uid { "UID " } else { "" }, args.join(" ")), "").await;
        }
        let authed = s.user.is_some();
        let res: Result<String, String> = match cmd.as_str() {
            "CAPABILITY" => {
                let tls_needed = !s.tls && config_requires_tls(&ctx).await;
                let _ = w(&mut c, &format!("* CAPABILITY IMAP4rev1 {}STARTTLS AUTH=PLAIN AUTH=XOAUTH2 IDLE UIDPLUS MOVE", if tls_needed { "LOGINDISABLED " } else { "" })).await;
                Ok("CAPABILITY completed".into())
            }
            "NOOP" | "CHECK" => {
                refresh(&mut s).await;
                if s.folder.is_some() {
                    let _ = w(&mut c, &format!("* {} EXISTS", s.view.len())).await;
                }
                Ok(format!("{cmd} completed"))
            }
            "LOGOUT" => {
                let _ = w(&mut c, "* BYE Stuntman IMAP logging out").await;
                let _ = w(&mut c, &format!("{tag} OK LOGOUT completed")).await;
                break;
            }
            "STARTTLS" => {
                if w(&mut c, &format!("{tag} OK Begin TLS negotiation now")).await.is_err() { break }
                let inner = c.into_inner();
                match crate::smtp::tls().accept(inner).await {
                    Ok(t) => {
                        c = BufReader::new(Box::new(t) as BoxIo);
                        s.tls = true;
                        continue;
                    }
                    Err(_) => return,
                }
            }
            "LOGIN" | "AUTHENTICATE" => {
                let (user, pw, xoauth) = if cmd == "LOGIN" {
                    (args.first().cloned().unwrap_or_default(), args.get(1).cloned().unwrap_or_default(), false)
                } else {
                    let mech = args.first().map(|x| x.to_uppercase()).unwrap_or_default();
                    let data = match args.get(1) {
                        Some(d) => d.clone(),
                        None => {
                            if w(&mut c, "+ ").await.is_err() { break }
                            let mut l = String::new();
                            if c.read_line(&mut l).await.unwrap_or(0) == 0 { break }
                            l.trim().to_string()
                        }
                    };
                    let raw = String::from_utf8_lossy(&B64.decode(data.trim()).unwrap_or_default()).to_string();
                    if mech == "XOAUTH2" {
                        let u = raw.split('\x01').find_map(|x| x.strip_prefix("user=")).unwrap_or("").to_string();
                        let tok = raw.split('\x01').find_map(|x| x.strip_prefix("auth=Bearer ")).unwrap_or("").to_string();
                        (u, tok, true)
                    } else {
                        let p: Vec<&str> = raw.split('\0').collect();
                        (p.get(1).unwrap_or(&"").to_string(), p.get(2).unwrap_or(&"").to_string(), false)
                    }
                };
                match mailbox::get(&ctx, &user).await.ok().flatten() {
                    Some(b) if b.provoke == "tls_required" && !s.tls && !xoauth => {
                        mailbox::log(&ctx, &user, "IMAP", &format!("{cmd} {user}"), "NO [PRIVACYREQUIRED] (provoked)").await;
                        Err("[PRIVACYREQUIRED] Use STARTTLS first".into())
                    }
                    Some(b) if b.provoke == "login_refused" => {
                        mailbox::log(&ctx, &user, "IMAP", &format!("{cmd} {user}"), "NO [AUTHENTICATIONFAILED] (provoked)").await;
                        Err("[AUTHENTICATIONFAILED] Invalid credentials".into())
                    }
                    Some(b) if xoauth && b.provoke == "token_expired" => {
                        let _ = w(&mut c, &format!("+ {}", B64.encode(r#"{"status":"401","schemes":"Bearer","scope":"https://mail.google.com/"}"#))).await;
                        let mut l = String::new();
                        let _ = c.read_line(&mut l).await;
                        mailbox::log(&ctx, &user, "IMAP", "AUTHENTICATE XOAUTH2", "token expired (provoked)").await;
                        Err("[AUTHENTICATIONFAILED] Invalid credentials (Failure)".into())
                    }
                    Some(b) if (xoauth && !pw.is_empty()) || mailbox::password_ok(&ctx, &b, &pw) => {
                        mailbox::log(&ctx, &user, "IMAP", &format!("{cmd} {user}"), "OK").await;
                        s.user = Some(b);
                        Ok(format!("{cmd} completed"))
                    }
                    Some(_) => {
                        mailbox::log(&ctx, &user, "IMAP", &format!("{cmd} {user}"), "NO wrong password").await;
                        Err("[AUTHENTICATIONFAILED] Invalid credentials".into())
                    }
                    None => Err("[AUTHENTICATIONFAILED] Invalid credentials".into()),
                }
            }
            _ if !authed => Err("Not authenticated".into()),
            "LIST" | "LSUB" => {
                let b = mailbox::get(&ctx, &addr).await.ok().flatten().unwrap_or_default();
                let pat = args.get(1).cloned().unwrap_or("*".into());
                for f in &b.folders {
                    if pat == "*" || pat == "%" || f.eq_ignore_ascii_case(&pat) || (pat.ends_with('*') && f.starts_with(pat.trim_end_matches('*'))) {
                        let _ = w(&mut c, &format!("* {cmd} (\\HasNoChildren) \"/\" {}", q(f))).await;
                    }
                }
                Ok(format!("{cmd} completed"))
            }
            "CREATE" | "DELETE" | "SUBSCRIBE" | "UNSUBSCRIBE" => {
                let name = args.first().cloned().unwrap_or_default();
                let mut b = mailbox::get(&ctx, &addr).await.ok().flatten().unwrap_or_default();
                if cmd == "CREATE" && !b.folders.iter().any(|f| f.eq_ignore_ascii_case(&name)) {
                    b.folders.push(name);
                } else if cmd == "DELETE" && !name.eq_ignore_ascii_case("INBOX") {
                    b.folders.retain(|f| !f.eq_ignore_ascii_case(&name));
                }
                let _ = mailbox::set_folders(&ctx, &addr, &b.folders).await;
                Ok(format!("{cmd} completed"))
            }
            "STATUS" => {
                let f = args.first().cloned().unwrap_or("INBOX".into());
                let v = mailbox::messages(&ctx, &addr, &f).await.unwrap_or_default();
                let b = mailbox::get(&ctx, &addr).await.ok().flatten().unwrap_or_default();
                let _ = w(&mut c, &format!("* STATUS {} (MESSAGES {} UNSEEN {} UIDNEXT {} UIDVALIDITY {} RECENT 0)", q(&f), v.len(), v.iter().filter(|m| !m.seen).count(), v.last().map(|m| m.uid + 1).unwrap_or(1), b.uidvalidity)).await;
                Ok("STATUS completed".into())
            }
            "SELECT" | "EXAMINE" => {
                let b = mailbox::get(&ctx, &addr).await.ok().flatten().unwrap_or_default();
                let f = args.first().cloned().unwrap_or("INBOX".into());
                let f = b.folders.iter().find(|x| x.eq_ignore_ascii_case(&f)).cloned();
                match f {
                    _ if b.provoke == "busy" => Err("[INUSE] Mailbox is locked by another session".into()),
                    None => Err("[NONEXISTENT] No such mailbox".into()),
                    Some(f) => {
                        s.folder = Some(f);
                        s.readonly = cmd == "EXAMINE";
                        refresh(&mut s).await;
                        let unseen = s.view.iter().position(|m| !m.seen).map(|p| p + 1);
                        let _ = w(&mut c, "* FLAGS (\\Seen \\Deleted \\Flagged \\Answered \\Draft)").await;
                        let _ = w(&mut c, &format!("* {} EXISTS", s.view.len())).await;
                        let _ = w(&mut c, "* 0 RECENT").await;
                        if let Some(u) = unseen {
                            let _ = w(&mut c, &format!("* OK [UNSEEN {u}] First unseen")).await;
                        }
                        let _ = w(&mut c, &format!("* OK [UIDVALIDITY {}] UIDs valid", b.uidvalidity)).await;
                        let _ = w(&mut c, &format!("* OK [UIDNEXT {}] Predicted next UID", s.view.last().map(|m| m.uid + 1).unwrap_or(1))).await;
                        let _ = w(&mut c, "* OK [PERMANENTFLAGS (\\Seen \\Deleted \\Flagged)] Limited").await;
                        Ok(format!("[{}] {cmd} completed", if s.readonly { "READ-ONLY" } else { "READ-WRITE" }))
                    }
                }
            }
            _ if s.folder.is_none() => Err("Select a mailbox first".into()),
            "FETCH" => {
                let set = args.first().cloned().unwrap_or_default();
                let items = args[1.min(args.len())..].join(" ");
                match fetch(&mut c, &mut s, &set, &items, by_uid).await {
                    Ok(true) => Ok(format!("{}FETCH completed", if by_uid { "UID " } else { "" })),
                    _ => break,
                }
            }
            "STORE" => {
                let set = args.first().cloned().unwrap_or_default();
                let op = args.get(1).map(|x| x.to_uppercase()).unwrap_or_default();
                let fl = args.get(2).map(|x| x.to_uppercase()).unwrap_or_default();
                let silent = op.ends_with(".SILENT");
                let val = !op.starts_with('-');
                let replace = op.starts_with("FLAGS");
                let max = if by_uid { s.view.last().map(|m| m.uid).unwrap_or(0) } else { s.view.len() as i64 };
                let hits: Vec<(i64, i64)> = s.view.iter().enumerate().filter(|(i, m)| in_set(&set, if by_uid { m.uid } else { *i as i64 + 1 }, max)).map(|(i, m)| (i as i64 + 1, m.uid)).collect();
                for (_, uid) in &hits {
                    let has = |f: &str| fl.contains(f);
                    let pick = |f: &str| if replace { Some(has(f)) } else if has(f) { Some(val) } else { None };
                    let _ = mailbox::flags(&ctx, &addr, *uid, pick("\\SEEN"), pick("\\DELETED"), pick("\\FLAGGED")).await;
                }
                refresh(&mut s).await;
                if !silent {
                    for (seq, uid) in hits {
                        if let Some(m) = s.view.iter().find(|m| m.uid == uid) {
                            let _ = w(&mut c, &format!("* {seq} FETCH ({}FLAGS {})", if by_uid { format!("UID {uid} ") } else { String::new() }, flags(m))).await;
                        }
                    }
                }
                Ok("STORE completed".into())
            }
            "SEARCH" => {
                let r = search(&s, &args, by_uid);
                let _ = w(&mut c, &format!("* SEARCH{}", r.iter().map(|x| format!(" {x}")).collect::<String>())).await;
                Ok("SEARCH completed".into())
            }
            "COPY" | "MOVE" => {
                let set = args.first().cloned().unwrap_or_default();
                let to = args.get(1).cloned().unwrap_or_default();
                let b = mailbox::get(&ctx, &addr).await.ok().flatten().unwrap_or_default();
                match b.folders.iter().find(|f| f.eq_ignore_ascii_case(&to)).cloned() {
                    None => Err("[TRYCREATE] No such mailbox".into()),
                    Some(to) => {
                        let max = if by_uid { s.view.last().map(|m| m.uid).unwrap_or(0) } else { s.view.len() as i64 };
                        let hits: Vec<(i64, i64)> = s.view.iter().enumerate().filter(|(i, m)| in_set(&set, if by_uid { m.uid } else { *i as i64 + 1 }, max)).map(|(i, m)| (i as i64 + 1, m.uid)).collect();
                        let mut removed = 0;
                        for (seq, uid) in &hits {
                            let _ = mailbox::move_to(&ctx, &addr, *uid, &to, cmd == "COPY").await;
                            if cmd == "MOVE" {
                                let _ = w(&mut c, &format!("* {} EXPUNGE", seq - removed)).await;
                                removed += 1;
                            }
                        }
                        refresh(&mut s).await;
                        Ok(format!("{cmd} completed"))
                    }
                }
            }
            "EXPUNGE" | "CLOSE" => {
                let before: Vec<i64> = s.view.iter().map(|m| m.uid).collect();
                let gone = if s.readonly { vec![] } else { mailbox::expunge(&ctx, &addr, s.folder.as_deref().unwrap_or("INBOX")).await.unwrap_or_default() };
                if cmd == "EXPUNGE" {
                    let mut removed = 0;
                    for (i, uid) in before.iter().enumerate() {
                        if gone.contains(uid) {
                            let _ = w(&mut c, &format!("* {} EXPUNGE", i as i64 + 1 - removed)).await;
                            removed += 1;
                        }
                    }
                    refresh(&mut s).await;
                } else {
                    s.folder = None;
                    s.view.clear();
                }
                Ok(format!("{cmd} completed"))
            }
            "IDLE" => {
                if w(&mut c, "+ idling").await.is_err() { break }
                let mut known = s.view.len();
                loop {
                    let mut l = String::new();
                    match tokio::time::timeout(Duration::from_secs(1), c.read_line(&mut l)).await {
                        Ok(Ok(0)) | Ok(Err(_)) => return,
                        Ok(Ok(_)) if l.trim().eq_ignore_ascii_case("DONE") => break,
                        _ => {
                            refresh(&mut s).await;
                            if s.view.len() != known {
                                known = s.view.len();
                                if w(&mut c, &format!("* {known} EXISTS")).await.is_err() { return }
                            }
                        }
                    }
                }
                Ok("IDLE terminated".into())
            }
            _ => Err(format!("{cmd} is not supported")),
        };
        let r = match res {
            Ok(m) => w(&mut c, &format!("{tag} OK {m}")).await,
            Err(m) if m.starts_with("Not") || m.ends_with("supported") => w(&mut c, &format!("{tag} BAD {m}")).await,
            Err(m) => w(&mut c, &format!("{tag} NO {m}")).await,
        };
        if r.is_err() {
            break;
        }
    }
}

async fn config_requires_tls(ctx: &Ctx) -> bool {
    mailbox::list(ctx).await.unwrap_or_default().iter().any(|b| b["provoke"] == "tls_required")
}

pub fn serve(envs: Envs, port: u16) {
    tokio::spawn(async move {
        let Ok(l) = tokio::net::TcpListener::bind(("0.0.0.0", port)).await else {
            eprintln!("IMAP: port {port} is taken");
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

#[cfg(test)]
mod t {
    #[test]
    fn structure_and_sections() {
        let raw = crate::mailbox::build("Fornecedor <fornecedor@globex.example>", "nfe@acme.example", "NF-e 3526", "Segue a nota.", None, &[("nfe.xml".into(), "application/xml".into(), b"<nfeProc/>".to_vec())]);
        let m = mail_parser::MessageParser::default().parse(&raw).unwrap();
        let bs = super::part_structure(&m, 0, true);
        assert!(bs.contains("\"mixed\"") && bs.contains("\"application\" \"xml\"") && bs.contains("\"BASE64\""), "{bs}");
        let env = super::envelope(&m);
        assert!(env.contains("\"NF-e 3526\"") && env.contains("\"fornecedor\" \"globex.example\""), "{env}");
        let p2 = super::section(&raw, "2");
        assert_eq!(base64::Engine::decode(&base64::engine::general_purpose::STANDARD, String::from_utf8_lossy(&p2).trim()).unwrap(), b"<nfeProc/>");
        assert!(String::from_utf8_lossy(&super::section(&raw, "HEADER.FIELDS (SUBJECT FROM)")).starts_with("From: "));
        assert!(super::in_set("1:3,5", 2, 9) && super::in_set("4:*", 9, 9) && !super::in_set("1:3", 4, 9));
    }
}
