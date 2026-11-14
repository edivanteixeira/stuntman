//! One path for every intake (SMTP, SendGrid API): parse the MIME message, find the inbox, extract
//! values, store one message per recipient, tell the app "processed", start the lifecycle.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use stuntman_core::{extract, util, ApiError, Ctx};
use mail_parser::{MessageParser, MimeHeaders};
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::{config, events, inbox, lifecycle, message};

pub struct Envelope {
    pub service: config::Service,
    /// smtp | sendgrid
    pub via: String,
    pub mail_from: String,
    pub rcpts: Vec<String>,
    pub raw: Vec<u8>,
    pub trace_id: Option<String>,
    pub custom_args: Value,
    /// Who sent it, for the "Connected" list.
    pub client: String,
}

/// Per-service sends in the current minute, for "600 / min, then 429".
pub fn rate_ok(svc: &config::Service) -> bool {
    static R: OnceLock<Mutex<HashMap<String, (i64, u64)>>> = OnceLock::new();
    let mut m = R.get_or_init(Default::default).lock().unwrap();
    let minute = util::now_ms() / 60_000;
    let e = m.entry(svc.name.clone()).or_insert((minute, 0));
    if e.0 != minute {
        *e = (minute, 0);
    }
    e.1 += 1;
    e.1 <= svc.limits.per_minute()
}

pub fn trace_from(traceparent: &str) -> Option<String> {
    let p: Vec<&str> = traceparent.trim().split('-').collect();
    (p.len() >= 3 && p[1].len() == 32).then(|| p[1].to_string())
}

/// "Action required: NDA · Acme Corp" → "Action required: ‹document› · ‹company›" is the idea; we
/// mask what varies: numbers, e-mail addresses, and the part after ": " split on " · ".
pub fn kind_of(subject: &str, headers: &[(String, String)]) -> String {
    for (k, v) in headers {
        if ["x-template-id", "x-stuntman-kind", "x-duble-kind", "x-kind"].contains(&k.to_lowercase().as_str()) && !v.trim().is_empty() {
            return v.trim().to_string();
        }
    }
    let re_mail = Regex::new(r"[\w.+-]+@[\w-]+\.[\w.-]+").unwrap();
    let re_num = Regex::new(r"\d[\d.,/-]*").unwrap();
    let s = re_mail.replace_all(subject, "‹email›");
    let s = re_num.replace_all(&s, "‹n›").to_string();
    match s.split_once(": ") {
        Some((head, tail)) if !tail.trim().is_empty() => {
            let parts: Vec<&str> = tail.split(" · ").collect();
            let masked: Vec<&str> = parts.iter().map(|_| "‹…›").collect();
            format!("{head}: {}", masked.join(" · "))
        }
        _ => s,
    }
}

/// Tiny stop-word vote; Content-Language wins when the app sets it.
pub fn lang_of(text: &str, headers: &[(String, String)]) -> String {
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("content-language") && !v.trim().is_empty() {
            return v.trim().to_string();
        }
    }
    let t = format!(" {} ", text.to_lowercase());
    let score = |ws: &[&str]| ws.iter().map(|w| t.matches(&format!(" {w} ")).count()).sum::<usize>();
    let pt = score(&["você", "não", "seu", "sua", "para", "com", "uma", "código", "é", "documento", "assinatura", "obrigado"]);
    let es = score(&["usted", "tu", "su", "para", "con", "una", "código", "es", "está", "documento", "firma", "gracias", "el", "los"]);
    let en = score(&["the", "you", "your", "to", "and", "is", "this", "code", "for", "if", "it", "please"]);
    let best = [(pt, "pt-BR"), (es, "es"), (en, "en")].into_iter().max_by_key(|x| x.0).unwrap();
    if best.0 == 0 { String::new() } else { best.1.to_string() }
}

fn html_to_text(html: &str) -> String {
    let d = scraper::Html::parse_document(html);
    d.root_element().text().collect::<Vec<_>>().join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
}

pub struct Parsed {
    pub subject: String,
    pub from: String,
    pub from_name: String,
    pub text: String,
    pub html: String,
    pub headers: Vec<(String, String)>,
    pub attachments: Vec<message::Attachment>,
}

pub fn parse(raw: &[u8]) -> Result<Parsed, ApiError> {
    let m = MessageParser::default().parse(raw).ok_or_else(|| ApiError::bad("Not a MIME message"))?;
    let (from, from_name) = m
        .from()
        .and_then(|a| a.first())
        .map(|a| (a.address().unwrap_or("").to_string(), a.name().unwrap_or("").to_string()))
        .unwrap_or_default();
    // mail-parser renders text-only mail as HTML too; keep HTML only when the sender wrote some.
    let has_html = m.parts.iter().any(|p| p.content_type().map(|c| c.ctype().eq_ignore_ascii_case("text") && c.subtype().map(|s| s.eq_ignore_ascii_case("html")).unwrap_or(false)).unwrap_or(false));
    let html = if has_html { m.body_html(0).map(|s| s.to_string()).unwrap_or_default() } else { String::new() };
    let mut text = m.body_text(0).map(|s| s.to_string()).filter(|_| m.text_body_count() > 0).unwrap_or_default();
    if text.trim().is_empty() && !html.is_empty() {
        text = html_to_text(&html);
    }
    let headers: Vec<(String, String)> = m
        .headers_raw()
        .map(|(k, v)| (k.to_string(), v.trim().replace("\r\n", "").replace('\n', "")))
        .collect();
    let mut attachments = vec![];
    for (i, a) in m.attachments().enumerate() {
        let data = a.contents().to_vec();
        let ctype = a.content_type().map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream"))).unwrap_or("application/octet-stream".into());
        let inline = a.content_disposition().map(|d| d.ctype() == "inline").unwrap_or(false) && a.content_id().is_some();
        attachments.push(message::Attachment {
            index: i as u32,
            name: a.attachment_name().unwrap_or(&format!("attachment-{}", i + 1)).to_string(),
            content_type: ctype,
            size: data.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&data)),
            inline,
            data,
        });
    }
    Ok(Parsed { subject: m.subject().unwrap_or("").to_string(), from, from_name, text, html, headers, attachments })
}

/// Runs built-in and user extractors on a parsed message. Returns (extracted, links, names that hit).
pub async fn extract_all(ctx: &Ctx, p: &Parsed, kind: &str, to: &str) -> Result<(Value, Vec<extract::Link>, Vec<String>), ApiError> {
    let cfg = config::extractors(ctx)?;
    let mut links = extract::links(&p.text, &p.html);
    if cfg.extractors.iter().any(|e| e.follow_redirects && e.from == extract::Source::Link) {
        let futs: Vec<_> = links.iter_mut().take(10).map(|l| extract::follow(&ctx.http, l)).collect();
        futures::future::join_all(futs).await;
    }
    let doc = extract::Doc {
        subject: p.subject.clone(),
        text: p.text.clone(),
        html: p.html.clone(),
        links: links.clone(),
        headers: p.headers.clone(),
        attachments: p.attachments.iter().filter(|a| !a.inline).map(|a| a.name.clone()).collect(),
        kind: kind.into(),
        from: p.from.clone(),
        to: to.into(),
    };
    let mut out = Map::new();
    let mut hits = vec![];
    for ex in &cfg.extractors {
        let o = extract::run(ex, &doc);
        if let Some(v) = o.value {
            out.insert(ex.name.clone(), json!({"value": v, "by": "yours", "linkIndex": o.link_index}));
            hits.push(ex.name.clone());
        }
    }
    if let Some(code) = extract::code(&cfg.code, &doc) {
        if !out.values().any(|v| v["value"] == json!(code)) {
            out.insert("code".into(), json!({"value": code, "by": "built-in"}));
        } else {
            out.insert("code".into(), json!({"value": code, "by": "built-in", "same": true}));
        }
        hits.push("code".into());
    }
    if !links.is_empty() {
        hits.push("links".into());
    }
    if !doc.attachments.is_empty() {
        hits.push("attachments".into());
    }
    Ok((Value::Object(out), links, hits))
}

pub fn domain_of(addr: &str) -> String {
    addr.rsplit_once('@').map(|x| x.1.to_lowercase()).unwrap_or_default()
}

/// Whether Stuntman takes mail for this address at all (SMTP asks this at RCPT TO).
pub async fn accepts(ctx: &Ctx, svc: &config::Service, rcpt: &str) -> bool {
    svc.addresses.catch_all || domain_of(rcpt) == svc.addresses.domain.to_lowercase() || inbox::get(ctx, rcpt).await.ok().flatten().is_some()
}

pub async fn ingest(ctx: &Ctx, env: Envelope) -> Result<Vec<message::Message>, ApiError> {
    let svc = env.service;
    let p = parse(&env.raw)?;
    let kind = kind_of(&p.subject, &p.headers);
    let lang = lang_of(&format!("{} {}", p.subject, p.text), &p.headers);
    let trace = env.trace_id.or_else(|| p.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("traceparent")).and_then(|(_, v)| trace_from(v)));
    let too_big = env.raw.len() as u64 > svc.limits.size_bytes();
    let sg_id = format!("{:022x}", rand::random::<u128>() >> 40);
    let mut out = vec![];
    for rcpt in env.rcpts.iter().map(|r| r.trim().to_lowercase()) {
        let ib = inbox::get(ctx, &rcpt).await?.filter(|i| !i.expired());
        let inbox_kind = match &ib {
            Some(i) => i.kind.clone(),
            None => "catch-all".into(),
        };
        let (extracted, links, hits) = extract_all(ctx, &p, &kind, &rcpt).await?;
        message::hit(ctx, hits).await?;
        let refused = !accepts(ctx, &svc, &rcpt).await;
        let m = message::Message {
            service: svc.name.clone(),
            via: env.via.clone(),
            at: util::now_ms(),
            from: p.from.clone(),
            from_name: p.from_name.clone(),
            to: rcpt.clone(),
            subject: p.subject.clone(),
            kind: kind.clone(),
            lang: lang.clone(),
            status: "processed".into(),
            status_by: env.client.clone(),
            inbox: inbox_kind,
            trace_id: trace.clone(),
            extracted,
            links: serde_json::to_value(&links).unwrap(),
            sg_message_id: sg_id.clone(),
            custom_args: env.custom_args.clone(),
            size: env.raw.len() as u64,
            raw: env.raw.clone(),
            ..Default::default()
        };
        let headers = json!(p.headers.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>());
        let m = message::insert(ctx, m, p.text.clone(), p.html.clone(), headers, p.attachments.clone()).await?;
        ctx.live.send("email.message", &m.id, "created");
        let label = format!("{} {} → {}", if m.via == "smtp" { "SMTP" } else { "SendGrid POST /v3/mail/send" }, m.subject, m.to);
        stuntman_core::trips::record(ctx, stuntman_core::trips::Trip {
            id: &m.id, module: "email", label: &label, status: "processed", trace_id: m.trace_id.as_deref(),
            link: &format!("/notifications/{}", m.id), host: if m.via == "smtp" { "smtp" } else { "sendgrid" },
            actor: &env.client, outcome: if m.provoked { "provoked" } else { "ok" }, detail: &format!("{}  ·  {}", env.client, m.service),
        }).await?;
        events::emit(ctx, &svc, &m, "processed", json!({}), &env.client).await?;
        if too_big {
            lifecycle::drop_it(ctx, &svc, &m, &format!("Message larger than {}", svc.limits.message_size), "rule").await?;
        } else if refused {
            lifecycle::drop_it(ctx, &svc, &m, "Catch-all is off: mail to other domains is refused", "rule").await?;
        } else {
            lifecycle::start(ctx.clone(), svc.clone(), m.clone(), ib);
        }
        out.push(m);
    }
    Ok(out)
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn kinds_mask_what_varies() {
        assert_eq!(kind_of("Action required: NDA · Acme Corp", &[]), "Action required: ‹…› · ‹…›");
        assert_eq!(kind_of("Your verification code", &[]), "Your verification code");
        assert_eq!(kind_of("Order 58213 shipped", &[]), "Order ‹n› shipped");
        assert_eq!(kind_of("x", &[("X-Template-Id".into(), "d-123".into())]), "d-123");
    }

    #[test]
    fn languages() {
        assert_eq!(lang_of("Your verification code. Use this code to confirm your signature", &[]), "en");
        assert_eq!(lang_of("Seu código de verificação. Você não precisa fazer nada com este documento", &[]), "pt-BR");
        assert_eq!(lang_of("Tu exportación está lista. Gracias por usar el servicio con su cuenta", &[]), "es");
    }

    #[test]
    fn parses_mime_with_attachment() {
        let raw = b"From: Acme <no-reply@acme.example.com>\r\nTo: ops@acme.example.com\r\nSubject: Tu exportacion\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nHola, tu archivo: https://acme.example.com/exports/9f2c\r\n--b\r\nContent-Type: text/csv; name=grupos.csv\r\nContent-Disposition: attachment; filename=grupos.csv\r\n\r\nid,nombre\r\n31,Ventas\r\n--b--\r\n";
        let p = parse(raw).unwrap();
        assert_eq!(p.from, "no-reply@acme.example.com");
        assert_eq!(p.from_name, "Acme");
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].name, "grupos.csv");
        assert!(p.text.contains("exports/9f2c"));
        assert!(p.html.is_empty(), "text-only mail must not get an invented HTML part");
    }
}
