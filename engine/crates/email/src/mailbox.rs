//! A16: mailboxes the app reads (IMAP, POP3). You put a message in one ("a supplier sent an NF-e
//! XML"); the app finds it on its next poll; what it did (LOGIN, SELECT, FETCH, STORE…) is kept.
//! Each mailbox has folders (INBOX first), flags and the failures provoked on it.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx, Tool};

pub const PROVOKE: [(&str, &str); 5] = [("login_refused", "the login is refused"), ("busy", "the mailbox is in use (NO [INUSE])"), ("drop_fetch", "the connection drops during FETCH / RETR"),
    ("tls_required", "plain logins refused before STARTTLS"), ("token_expired", "XOAUTH2 tokens refused")];

#[derive(Clone, Debug, Default)]
pub struct Box_ {
    pub address: String,
    pub password: String,
    pub provoke: String,
    pub uidvalidity: i64,
    pub folders: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Msg {
    pub uid: i64,
    pub at: i64,
    pub raw: Vec<u8>,
    pub seen: bool,
    pub deleted: bool,
    pub flagged: bool,
}

pub async fn get(ctx: &Ctx, address: &str) -> Result<Option<Box_>, ApiError> {
    let a = address.to_lowercase();
    ctx.db
        .call(move |cn| {
            cn.query_row("SELECT address, password, provoke, uidvalidity, folders FROM email_mailboxes WHERE address=?1", [a], |r| {
                Ok(Box_ { address: r.get(0)?, password: r.get(1)?, provoke: r.get(2)?, uidvalidity: r.get(3)?, folders: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or(vec!["INBOX".into()]) })
            })
            .optional()
        })
        .await
}

pub async fn set(ctx: &Ctx, address: &str, password: Option<String>, provoke: Option<String>) -> Result<(), ApiError> {
    let a = address.to_lowercase();
    ctx.db
        .call(move |cn| {
            cn.execute("INSERT OR IGNORE INTO email_mailboxes(address, uidvalidity, created) VALUES(?1,?2,?3)", params![a, util::now_ms() / 1000, util::now_ms()])?;
            if let Some(p) = password {
                cn.execute("UPDATE email_mailboxes SET password=?2 WHERE address=?1", params![a, p])?;
            }
            if let Some(p) = provoke {
                cn.execute("UPDATE email_mailboxes SET provoke=?2 WHERE address=?1", params![a, if p == "none" { String::new() } else { p }])?;
            }
            Ok(())
        })
        .await?;
    ctx.live.send("email.mailboxes", address, "updated");
    Ok(())
}

pub async fn set_folders(ctx: &Ctx, address: &str, folders: &[String]) -> Result<(), ApiError> {
    let (a, f) = (address.to_lowercase(), serde_json::to_string(folders).unwrap_or_default());
    ctx.db.call(move |cn| cn.execute("UPDATE email_mailboxes SET folders=?2 WHERE address=?1", params![a, f]).map(|_| ())).await
}

pub async fn list(ctx: &Ctx) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(|cn| {
            let mut st = cn.prepare("SELECT b.address, b.provoke, b.folders, b.password<>'', (SELECT COUNT(*) FROM email_mailbox_msgs m WHERE m.mailbox=b.address AND m.deleted=0), (SELECT COUNT(*) FROM email_mailbox_msgs m WHERE m.mailbox=b.address AND m.seen=0 AND m.deleted=0) FROM email_mailboxes b ORDER BY b.created")?;
            let v = st
                .query_map([], |r| Ok(json!({"address": r.get::<_, String>(0)?, "provoke": r.get::<_, String>(1)?, "folders": serde_json::from_str::<Value>(&r.get::<_, String>(2)?).unwrap_or(json!(["INBOX"])),
                    "password": if r.get::<_, bool>(3)? { "set" } else { "any (not “wrong”)" }, "messages": r.get::<_, i64>(4)?, "unread": r.get::<_, i64>(5)?})))?
                .filter_map(|x| x.ok())
                .collect();
            Ok(v)
        })
        .await
}

pub async fn messages(ctx: &Ctx, address: &str, folder: &str) -> Result<Vec<Msg>, ApiError> {
    let (a, f) = (address.to_lowercase(), folder.to_string());
    ctx.db
        .call(move |cn| {
            let mut st = cn.prepare("SELECT uid, at, raw, seen, deleted, flagged FROM email_mailbox_msgs WHERE mailbox=?1 AND folder=?2 ORDER BY uid")?;
            let v = st.query_map(params![a, f], |r| Ok(Msg { uid: r.get(0)?, at: r.get(1)?, raw: r.get(2)?, seen: r.get(3)?, deleted: r.get(4)?, flagged: r.get(5)? }))?.filter_map(|x| x.ok()).collect();
            Ok(v)
        })
        .await
}

pub async fn put_raw(ctx: &Ctx, address: &str, folder: &str, raw: Vec<u8>) -> Result<i64, ApiError> {
    let (a, f) = (address.to_lowercase(), folder.to_string());
    let p = mail_parser::MessageParser::default().parse(&raw);
    let sender = p.as_ref().and_then(|m| m.from()).and_then(|x| x.first()).and_then(|x| x.address()).unwrap_or("").to_string();
    let subject = p.as_ref().and_then(|m| m.subject()).unwrap_or("").to_string();
    let uid = ctx
        .db
        .call(move |cn| {
            let uid: i64 = cn.query_row("SELECT next_uid FROM email_mailboxes WHERE address=?1", [&a], |r| r.get(0))?;
            cn.execute("UPDATE email_mailboxes SET next_uid=next_uid+1 WHERE address=?1", [&a])?;
            cn.execute("INSERT INTO email_mailbox_msgs(mailbox, folder, uid, at, raw, sender, subject) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![a, f, uid, util::now_ms(), raw, sender, subject])?;
            Ok(uid)
        })
        .await?;
    ctx.live.send("email.mailboxes", address, "message");
    Ok(uid)
}

pub async fn flags(ctx: &Ctx, address: &str, uid: i64, seen: Option<bool>, deleted: Option<bool>, flagged: Option<bool>) -> Result<(), ApiError> {
    let a = address.to_lowercase();
    ctx.db
        .call(move |cn| {
            if let Some(x) = seen { cn.execute("UPDATE email_mailbox_msgs SET seen=?3 WHERE mailbox=?1 AND uid=?2", params![a, uid, x])?; }
            if let Some(x) = deleted { cn.execute("UPDATE email_mailbox_msgs SET deleted=?3 WHERE mailbox=?1 AND uid=?2", params![a, uid, x])?; }
            if let Some(x) = flagged { cn.execute("UPDATE email_mailbox_msgs SET flagged=?3 WHERE mailbox=?1 AND uid=?2", params![a, uid, x])?; }
            Ok(())
        })
        .await
}

pub async fn expunge(ctx: &Ctx, address: &str, folder: &str) -> Result<Vec<i64>, ApiError> {
    let (a, f) = (address.to_lowercase(), folder.to_string());
    ctx.db
        .call(move |cn| {
            let mut st = cn.prepare("SELECT uid FROM email_mailbox_msgs WHERE mailbox=?1 AND folder=?2 AND deleted=1 ORDER BY uid")?;
            let gone: Vec<i64> = st.query_map(params![a, f], |r| r.get(0))?.filter_map(|x| x.ok()).collect();
            cn.execute("DELETE FROM email_mailbox_msgs WHERE mailbox=?1 AND folder=?2 AND deleted=1", params![a, f])?;
            Ok(gone)
        })
        .await
}

pub async fn move_to(ctx: &Ctx, address: &str, uid: i64, to: &str, copy: bool) -> Result<i64, ApiError> {
    let (a, t) = (address.to_lowercase(), to.to_string());
    ctx.db
        .call(move |cn| {
            if copy {
                let nu: i64 = cn.query_row("SELECT next_uid FROM email_mailboxes WHERE address=?1", [&a], |r| r.get(0))?;
                cn.execute("UPDATE email_mailboxes SET next_uid=next_uid+1 WHERE address=?1", [&a])?;
                cn.execute("INSERT INTO email_mailbox_msgs(mailbox, folder, uid, at, raw, seen, sender, subject) SELECT mailbox, ?3, ?4, at, raw, seen, sender, subject FROM email_mailbox_msgs WHERE mailbox=?1 AND uid=?2", params![a, uid, t, nu])?;
                Ok(nu)
            } else {
                cn.execute("UPDATE email_mailbox_msgs SET folder=?3 WHERE mailbox=?1 AND uid=?2", params![a, uid, t])?;
                Ok(uid)
            }
        })
        .await
}

pub async fn log(ctx: &Ctx, address: &str, proto: &str, line: &str, result: &str) {
    // Passwords and tokens never go to the log.
    let mut l = line.to_string();
    let up = l.to_uppercase();
    if up.contains(" LOGIN ") || up.starts_with("PASS ") || up.contains("AUTHENTICATE") {
        let keep: Vec<&str> = l.split_whitespace().take(if up.contains(" LOGIN ") { 3 } else { 2 }).collect();
        l = format!("{} ****", keep.join(" "));
    }
    let (a, p, r) = (address.to_lowercase(), proto.to_string(), result.to_string());
    let _ = ctx.db.call(move |cn| cn.execute("INSERT INTO email_mailbox_log(at, mailbox, proto, line, result) VALUES(?1,?2,?3,?4,?5)", params![util::now_ms(), a, p, l.chars().take(300).collect::<String>(), r]).map(|_| ())).await;
    ctx.live.send("email.mailboxes", address, "log");
}

pub fn password_ok(ctx: &Ctx, b: &Box_, pw: &str) -> bool {
    if b.password.is_empty() { !pw.is_empty() && pw != "wrong" } else { util::resolve_secret(&b.password, &ctx.data_dir) == pw }
}

/// A MIME message from parts: text, optional HTML, attachments.
pub fn build(from: &str, to: &str, subject: &str, text: &str, html: Option<&str>, attachments: &[(String, String, Vec<u8>)]) -> Vec<u8> {
    let date = { let t = std::time::SystemTime::now(); httpdate(t) };
    let mid = format!("<{}@stuntman.local>", util::uuid());
    let mut out = format!("From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nDate: {date}\r\nMessage-ID: {mid}\r\nMIME-Version: 1.0\r\n");
    let body_part = match html {
        Some(h) => {
            let b = format!("alt-{}", &util::uuid()[..8]);
            format!("Content-Type: multipart/alternative; boundary=\"{b}\"\r\n\r\n--{b}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{text}\r\n--{b}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{h}\r\n--{b}--\r\n")
        }
        None => format!("Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{text}\r\n"),
    };
    if attachments.is_empty() {
        out.push_str(&body_part);
        return out.into_bytes();
    }
    let b = format!("mix-{}", &util::uuid()[..8]);
    out.push_str(&format!("Content-Type: multipart/mixed; boundary=\"{b}\"\r\n\r\n--{b}\r\n{body_part}"));
    for (name, ct, data) in attachments {
        let enc = B64.encode(data);
        let lines: Vec<String> = enc.as_bytes().chunks(76).map(|c| String::from_utf8_lossy(c).to_string()).collect();
        out.push_str(&format!("--{b}\r\nContent-Type: {ct}; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n", lines.join("\r\n")));
    }
    out.push_str(&format!("--{b}--\r\n"));
    out.into_bytes()
}

fn httpdate(t: std::time::SystemTime) -> String {
    let s = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let days = s.div_euclid(86_400);
    let (y, m, d) = {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let dd = doy - (153 * mp + 2) / 5 + 1;
        let mm = if mp < 10 { mp + 3 } else { mp - 9 };
        (yoe + era * 400 + if mm <= 2 { 1 } else { 0 }, mm, dd)
    };
    const W: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let r = s.rem_euclid(86_400);
    format!("{}, {:02} {} {} {:02}:{:02}:{:02} +0000", W[days.rem_euclid(7) as usize], d, M[(m - 1) as usize], y, r / 3600, r % 3600 / 60, r % 60)
}

pub fn imap_date(ms: i64) -> String {
    // "15-Nov-2026 14:02:11 +0000"
    let h = httpdate(std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms as u64));
    let p: Vec<&str> = h.split_whitespace().collect();
    format!("{}-{}-{} {} +0000", p[1], p[2], p[3], p[4])
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("list_mailboxes", "The mailboxes the app reads over IMAP or POP3, with their messages and unread counts, and how to connect.", |ctx, _a| async move {
            let ports = (std::env::var("STUNTMAN_IMAP_PORT").unwrap_or("1143".into()), std::env::var("STUNTMAN_POP3_PORT").unwrap_or("1110".into()));
            Ok(json!({"mailboxes": list(&ctx).await?, "connect": {"imap": format!("localhost:{} (STARTTLS)", ports.0), "pop3": format!("localhost:{} (STLS)", ports.1)},
                "provocations": PROVOKE.iter().map(|p| json!({"kind": p.0, "label": p.1})).collect::<Vec<_>>()}))
        })
        .route("GET", "/mailboxes"),
        Tool::new("set_mailbox", "Creates or changes a mailbox the app reads: its address (the login), a password (empty: any but \"wrong\") and a provocation: login_refused, busy, drop_fetch, tls_required, token_expired (none clears).", |ctx, a| async move {
            let address = a.need("address")?;
            if !address.contains('@') {
                return Err(ApiError::bad("address: name@domain (a synthetic one, e.g. nfe@acme.example)"));
            }
            let prov = a.str("provoke");
            if let Some(p) = &prov {
                if p != "none" && !PROVOKE.iter().any(|x| x.0 == p) {
                    return Err(ApiError::bad(format!("provoke: {} | none", PROVOKE.iter().map(|x| x.0).collect::<Vec<_>>().join(" | "))));
                }
            }
            set(&ctx, &address, a.str("password"), prov).await?;
            Ok(json!({"mailboxes": list(&ctx).await?}))
        })
        .req("address", "nfe@acme.example").param("password", "secret://name or a synthetic literal").param("provoke", "login_refused | busy | drop_fetch | tls_required | token_expired | none")
        .route("PUT", "/mailboxes/{address}"),
        Tool::new("put_mail", "Puts a message in a mailbox as if someone sent it: from, subject, text (and html), attachments [{filename, content_type, base64 or text}] — or raw (a whole RFC 822 message). The app finds it on its next poll.", |ctx, a| async move {
            let address = a.need("address")?;
            if get(&ctx, &address).await?.is_none() {
                set(&ctx, &address, None, None).await?;
            }
            let raw = match a.str("raw") {
                Some(r) => r.into_bytes(),
                None => {
                    let mut atts = vec![];
                    for x in a.get("attachments").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                        let data = match (x["base64"].as_str(), x["text"].as_str()) { (Some(b), _) => B64.decode(b).map_err(|_| ApiError::bad("attachments: base64"))?, (None, Some(t)) => t.as_bytes().to_vec(), _ => vec![] };
                        atts.push((x["filename"].as_str().unwrap_or("file.bin").to_string(), x["content_type"].as_str().unwrap_or("application/octet-stream").to_string(), data));
                    }
                    build(&a.str("from").unwrap_or("someone@globex.example".into()), &address, &a.str("subject").unwrap_or_default(), &a.str("text").unwrap_or_default(), a.str("html").as_deref(), &atts)
                }
            };
            let folder = a.str("folder").unwrap_or("INBOX".into());
            let uid = put_raw(&ctx, &address, &folder, raw).await?;
            log(&ctx, &address, "Stuntman", &format!("put by you in {folder}"), &format!("uid {uid}")).await;
            Ok(json!({"address": address, "folder": folder, "uid": uid}))
        })
        .req("address", "the mailbox").param("from", "sender").param("subject", "subject").param("text", "plain text").param("html", "HTML").param("attachments", "[{filename, content_type, base64 | text}]")
        .param("raw", "a whole RFC 822 message").param("folder", "INBOX")
        .route("POST", "/mailboxes/{address}/messages"),
        Tool::new("list_mailbox_messages", "The messages in a mailbox (a folder): sender, subject, flags (read, deleted), size; and what the app did on it (IMAP/POP3 commands, without passwords).", |ctx, a| async move {
            let address = a.need("address")?;
            let folder = a.str("folder").unwrap_or("INBOX".into());
            let msgs = messages(&ctx, &address, &folder).await?;
            let rows: Vec<Value> = msgs.iter().map(|m| {
                let p = mail_parser::MessageParser::default().parse(&m.raw);
                json!({"uid": m.uid, "at": m.at, "from": p.as_ref().and_then(|x| x.from()).and_then(|x| x.first()).and_then(|x| x.address()).unwrap_or(""), "subject": p.as_ref().and_then(|x| x.subject()).unwrap_or(""),
                    "attachments": p.as_ref().map(|x| x.attachments().map(|t| mail_parser::MimeHeaders::attachment_name(t).unwrap_or("").to_string()).collect::<Vec<_>>()).unwrap_or_default(),
                    "seen": m.seen, "deleted": m.deleted, "flagged": m.flagged, "size": m.raw.len()})
            }).collect();
            let a2 = address.to_lowercase();
            let logs: Vec<Value> = ctx.db.call(move |cn| {
                let mut st = cn.prepare("SELECT at, proto, line, result FROM email_mailbox_log WHERE mailbox=?1 ORDER BY seq DESC LIMIT 100")?;
                let v = st.query_map([a2], |r| Ok(json!({"at": r.get::<_, i64>(0)?, "proto": r.get::<_, String>(1)?, "line": r.get::<_, String>(2)?, "result": r.get::<_, String>(3)?})))?.filter_map(|x| x.ok()).collect();
                Ok(v)
            }).await?;
            Ok(json!({"address": address, "folder": folder, "messages": rows, "log": logs}))
        })
        .req("address", "the mailbox").param("folder", "INBOX")
        .route("GET", "/mailboxes/{address}/messages"),
    ]
}
