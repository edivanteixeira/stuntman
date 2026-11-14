//! What happens after the app sends: the message goes sent → delivered → read (or fails) as the
//! recipient's steps say, the person may answer, and every change reaches the app as Meta's own
//! webhook, signed with the app secret (X-Hub-Signature-256), retried 3 times in a minute.

use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use stuntman_core::{trips, util, ApiError, Ctx};
use hmac::{Hmac, Mac};
use rand::Rng;
use serde_json::{json, Value};
use sha2::Sha256;

use crate::{config, store};

pub const WINDOW_MS: i64 = 24 * 3_600_000;

pub fn wamid() -> String {
    let b: [u8; 24] = rand::thread_rng().gen();
    format!("wamid.HBgL{}", B64.encode(b))
}

pub fn title(code: u32) -> &'static str {
    match code {
        131026 => "Message undeliverable",
        131047 => "Re-engagement message",
        131049 => "This message was not delivered to maintain healthy ecosystem engagement.",
        131051 => "Unsupported message type",
        130429 => "Rate limit hit",
        131048 => "Spam rate limit hit",
        131056 => "(Business Account, Consumer Account) pair rate limit hit",
        132000 => "Number of parameters does not match the expected number of params",
        132001 => "Template name does not exist in the translation",
        132015 => "Template is Paused",
        132016 => "Template is Disabled",
        190 => "Invalid OAuth access token",
        _ => "Something went wrong",
    }
}

pub fn detail(code: u32) -> &'static str {
    match code {
        131026 => "The recipient is not on WhatsApp, blocked the business, or uses an old app.",
        131047 => "More than 24 hours have passed since the recipient last replied to the sender number.",
        131049 => "Meta chose not to deliver this message to keep the ecosystem healthy.",
        131051 => "Message type unknown or not supported.",
        _ => "",
    }
}

pub fn window_json(last_in: Option<i64>) -> Value {
    match last_in {
        Some(t) if util::now_ms() - t < WINDOW_MS => json!({"open": true, "since": t, "until": t + WINDOW_MS, "leftMs": t + WINDOW_MS - util::now_ms()}),
        Some(t) => json!({"open": false, "since": t, "closedAt": t + WINDOW_MS}),
        None => json!({"open": false, "since": null}),
    }
}

fn secs() -> String {
    (util::now_ms() / 1000).to_string()
}

fn envelope(n: &config::Number, field: &str, value: Value) -> Value {
    json!({"object": "whatsapp_business_account", "entry": [{"id": n.waba_id, "changes": [{"value": value, "field": field}]}]})
}

fn metadata(n: &config::Number) -> Value {
    json!({"display_phone_number": config::digits(&n.display.number), "phone_number_id": n.phone_number_id})
}

pub fn status_body(n: &config::Number, m: &Value, status: &str, code: Option<u32>) -> Value {
    let mut s = json!({"id": m["id"], "status": status, "timestamp": secs(), "recipient_id": m["number"]});
    if status == "sent" || status == "delivered" {
        let cat = if m["category"].as_str().unwrap_or("").is_empty() { "service" } else { m["category"].as_str().unwrap() };
        s["conversation"] = json!({"id": format!("conv_{}", &m["id"].as_str().unwrap_or("")[10..22.min(m["id"].as_str().unwrap_or("").len())]), "origin": {"type": cat}});
        s["pricing"] = json!({"billable": true, "pricing_model": "PMP", "category": cat});
    }
    if let Some(c) = code {
        s["errors"] = json!([{"code": c, "title": title(c), "message": title(c), "error_data": {"details": detail(c)}}]);
    }
    envelope(n, "messages", json!({"messaging_product": "whatsapp", "metadata": metadata(n), "statuses": [s]}))
}

pub fn url(ctx: &Ctx, n: &config::Number) -> String {
    if n.webhook.url.trim().is_empty() { ctx.state.callback_default() } else { n.webhook.url.trim().to_string() }
}

pub fn sign(secret: &str, body: &[u8]) -> String {
    let mut m = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac");
    m.update(body);
    format!("sha256={}", hex::encode(m.finalize().into_bytes()))
}

/// Posts one webhook once; (status, ms, error).
pub async fn post_once(ctx: &Ctx, url: &str, body: &str, sig: Option<&str>) -> (Option<u16>, i64, String) {
    let t = util::now_ms();
    let mut r = ctx.http.post(url).header("content-type", "application/json").header("user-agent", "facebookexternalua").timeout(Duration::from_secs(10)).body(body.to_string());
    if let Some(s) = sig {
        r = r.header("X-Hub-Signature-256", s);
    }
    match r.send().await {
        Ok(x) => (Some(x.status().as_u16()), util::now_ms() - t, String::new()),
        Err(e) => (None, util::now_ms() - t, e.to_string()),
    }
}

/// Sends a webhook to the app with Meta's retries (here 3 more in a minute); notes each answer.
pub fn deliver(ctx: &Ctx, kind: &str, wamid: &str, status: &str, body: Value) {
    let (ctx, kind, wamid, status) = (ctx.clone(), kind.to_string(), wamid.to_string(), status.to_string());
    tokio::spawn(async move {
        let Ok(n) = config::number(&ctx) else { return };
        let u = url(&ctx, &n);
        let text = body.to_string();
        if u.is_empty() {
            let _ = store::hook(&ctx, &kind, &wamid, "", None, 0, "no webhook URL set (Number settings)", 1, &text, "").await;
            if !wamid.is_empty() { let _ = store::note_hook(&ctx, &wamid, &status, json!({"error": "no webhook URL set"})).await; }
            return;
        }
        let secret = util::resolve_secret(&n.webhook.app_secret, &ctx.data_dir);
        let sig = n.webhook.sign.then(|| sign(&secret, text.as_bytes()));
        for attempt in 1..=4u32 {
            let (code, ms, err) = post_once(&ctx, &u, &text, sig.as_deref()).await;
            let _ = store::hook(&ctx, &kind, &wamid, &u, code, ms, &err, attempt, &text, sig.as_deref().unwrap_or("")).await;
            if !wamid.is_empty() {
                let _ = store::note_hook(&ctx, &wamid, &status, json!({"status": code, "ms": ms, "attempt": attempt, "error": err, "url": u})).await;
            }
            if code.map(|c| (200..300).contains(&c)).unwrap_or(false) || ctx.state.gone() {
                return;
            }
            tokio::time::sleep(Duration::from_secs(20)).await;
        }
    });
}

/// Changes a message's status and tells the app.
pub async fn status(ctx: &Ctx, wamid: &str, st: &str, code: Option<u32>, by: &str) -> Result<Value, ApiError> {
    let m = store::set_status(ctx, wamid, st, code, by).await?;
    let n = config::number(ctx)?;
    deliver(ctx, "status", wamid, st, status_body(&n, &m, st, code));
    let label = format!("{} to +{}, {st}", if m["template"].as_str().unwrap_or("").is_empty() { m["kind"].as_str().unwrap_or("").to_string() } else { m["template"].as_str().unwrap_or("").to_string() }, m["number"].as_str().unwrap_or(""));
    let _ = trips::record(ctx, trips::Trip { id: wamid, module: "whatsapp", label: &label, status: st, trace_id: m["traceId"].as_str(), link: &format!("/whatsapp?number={}", m["number"].as_str().unwrap_or("")), host: "graph.facebook.com" , actor: "", outcome: if st == "failed" { "failed" } else if by.starts_with("mcp:") || by.starts_with("api:") || by == "you" { "provoked" } else { "ok" }, detail: &format!("status {st}{}", if by.is_empty() { String::new() } else { format!(" by {}", by.trim_start_matches("mcp:").trim_start_matches("api:")) }) }).await;
    Ok(m)
}

/// The steps after the app's message was accepted (runs in the background).
pub fn lifecycle(ctx: &Ctx, m: Value, buttons: Vec<config::Button>) {
    let ctx = ctx.clone();
    tokio::spawn(async move {
        let id = m["id"].as_str().unwrap_or("").to_string();
        let number = m["number"].as_str().unwrap_or("").to_string();
        let kind = m["kind"].as_str().unwrap_or("").to_string();
        let r = config::recipient_for(&ctx, &number);
        let sleep = |ms: u64| tokio::time::sleep(Duration::from_millis(ms));
        let blocked = match r.block_after() {
            Some(nn) => store::count_out(&ctx, &number).await.unwrap_or(0) > nn as i64,
            None => false,
        };
        sleep(300).await;
        if !r.whatsapp || blocked {
            let _ = status(&ctx, &id, "failed", Some(131026), "recipient rule").await;
            return;
        }
        if kind != "template" {
            let w = window_json(store::last_inbound(&ctx, &number).await.ok().flatten());
            if w["open"] != true {
                let _ = status(&ctx, &id, "failed", Some(131047), "24 h window").await;
                return;
            }
        }
        let _ = status(&ctx, &id, "sent", None, "recipient rule").await;
        person(&ctx, &r, &number, &id, &kind, &buttons);
        if let Some(code) = r.fail_code() {
            sleep(500).await;
            let _ = status(&ctx, &id, "failed", Some(code), "recipient rule").await;
            return;
        }
        let Some(d) = r.delivered_ms() else { return };
        sleep(d).await;
        if current(&ctx, &id).await != "sent" { return; }
        let _ = status(&ctx, &id, "delivered", None, "recipient rule").await;
        let Some(rd) = r.read_ms() else { return };
        sleep(rd.saturating_sub(d)).await;
        if current(&ctx, &id).await != "delivered" { return; }
        let _ = status(&ctx, &id, "read", None, "recipient rule").await;
    });
}

async fn current(ctx: &Ctx, id: &str) -> String {
    store::get(ctx, id).await.ok().and_then(|m| m["status"].as_str().map(String::from)).unwrap_or_default()
}

/// The person's side of the steps: tap a button, reply to a text, send an image or a location.
fn person(ctx: &Ctx, r: &config::Recipient, number: &str, ctx_id: &str, kind: &str, buttons: &[config::Button]) {
    person_from(ctx, r, number, ctx_id, kind, buttons, 0, util::now_ms());
}

/// The person's steps, `elapsed` ms after the message was sent; a step whose reply already exists
/// (an incoming message of that kind after `sent_at`) is skipped, so a restart never answers twice.
fn person_from(ctx: &Ctx, r: &config::Recipient, number: &str, ctx_id: &str, kind: &str, buttons: &[config::Button], elapsed: u64, sent_at: i64) {
    for s in &r.steps {
        let mut todo: Vec<(u64, Reply)> = vec![];
        if let (Some(a), true) = (&s.on_buttons, buttons.iter().any(|b| !b.quick_reply.is_empty())) {
            let b = buttons.iter().find(|b| !b.quick_reply.is_empty() && (a.tap.is_empty() || b.text.eq_ignore_ascii_case(&a.tap))).or(buttons.iter().find(|b| !b.quick_reply.is_empty()));
            if let Some(b) = b {
                todo.push((util::parse_ms(&a.after).unwrap_or(10_000), Reply::Button { text: b.text.clone(), payload: b.quick_reply.clone(), context: ctx_id.to_string() }));
            }
        }
        if let (Some(a), true) = (&s.on_text, kind == "text") {
            todo.push((util::parse_ms(&a.after).unwrap_or(20_000), Reply::Text(a.reply.clone())));
        }
        if let Some(a) = &s.send_image {
            todo.push((util::parse_ms(&a.after).unwrap_or(15_000), Reply::Image { caption: a.caption.clone() }));
        }
        if let Some(a) = &s.send_location {
            todo.push((util::parse_ms(&a.after).unwrap_or(15_000), Reply::Location { latitude: a.latitude.unwrap_or(40.7484), longitude: a.longitude.unwrap_or(-73.9857), name: if a.name.is_empty() { "Home".into() } else { a.name.clone() } }));
        }
        for (after, rep) in todo {
            let (ctx, number, ctx_id) = (ctx.clone(), number.to_string(), ctx_id.to_string());
            let k = match &rep { Reply::Text(_) => "text", Reply::Button { .. } => "button", Reply::Image { .. } => "image", Reply::Location { .. } => "location" };
            tokio::spawn(async move {
                if elapsed > 0 {
                    // Resumed: done already? (a button is tied to its message; the rest to the time after it)
                    let f = store::Filter { number: Some(number.clone()), dir: Some("in".into()), since_ms: Some(sent_at), limit: 50, ..Default::default() };
                    let done = store::list(&ctx, f).await.unwrap_or_default().iter().any(|m| m["kind"] == k && (k != "button" || m["contextId"] == ctx_id.as_str()));
                    if done { return; }
                }
                tokio::time::sleep(Duration::from_millis(after.saturating_sub(elapsed))).await;
                let _ = reply(&ctx, &number, rep, if elapsed > 0 { "recipient rule (resumed)" } else { "recipient rule" }).await;
            });
        }
    }
}

pub enum Reply {
    Text(String),
    Button { text: String, payload: String, context: String },
    Image { caption: String },
    Location { latitude: f64, longitude: f64, name: String },
}

/// The person writes back: stored as an incoming message, sent to the app as the messages webhook.
pub async fn reply(ctx: &Ctx, number: &str, r: Reply, by: &str) -> Result<Value, ApiError> {
    let n = config::number(ctx)?;
    let rec = config::recipient_for(ctx, number);
    let from = config::digits(number);
    let id = wamid();
    let disp = config::digits(&n.display.number);
    let (kind, text, msg, ctx_id) = match r {
        Reply::Text(t) => ("text", t.clone(), json!({"type": "text", "text": {"body": t}}), String::new()),
        Reply::Button { text, payload, context } => ("button", text.clone(), json!({"type": "button", "context": {"from": disp, "id": context}, "button": {"payload": payload, "text": text}}), context),
        Reply::Image { caption } => {
            let media = media_put(ctx)?;
            ("image", caption.clone(), json!({"type": "image", "image": {"caption": caption, "mime_type": "image/png", "sha256": media.1, "id": media.0}}), String::new())
        }
        Reply::Location { latitude, longitude, name } => ("location", format!("{latitude}, {longitude} · {name}"), json!({"type": "location", "location": {"latitude": latitude, "longitude": longitude, "name": name}}), String::new()),
    };
    let mut msg = msg;
    msg["from"] = json!(from);
    msg["id"] = json!(id);
    msg["timestamp"] = json!(secs());
    store::insert(ctx, store::Msg { id: id.clone(), dir: "in", number: from.clone(), kind: kind.into(), template: String::new(), lang: String::new(), text, status: "received".into(),
        body: msg.clone(), trace_id: None, caller: String::new(), context_id: ctx_id, by: by.into(), params: vec![], category: String::new() }).await?;
    let body = envelope(&n, "messages", json!({"messaging_product": "whatsapp", "metadata": metadata(&n),
        "contacts": [{"profile": {"name": rec.profile_name}, "wa_id": from}], "messages": [msg]}));
    deliver(ctx, "message", &id, "received", body);
    store::get(ctx, &id).await
}

/// A small PNG stands in for any picture the person sends; (media id, sha256).
pub fn media_put(ctx: &Ctx) -> Result<(String, String), ApiError> {
    const PNG: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0, 0, 0, 0, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82];
    let id: String = format!("{}", rand::thread_rng().gen_range(1_000_000_000_000_000u64..9_999_999_999_999_999));
    let dir = ctx.path("whatsapp/media");
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::internal(e.to_string()))?;
    std::fs::write(dir.join(format!("{id}.png")), PNG).map_err(|e| ApiError::internal(e.to_string()))?;
    use sha2::Digest;
    Ok((id, hex::encode(sha2::Sha256::digest(PNG))))
}

pub fn template_event(ctx: &Ctx, t: &config::Template, event: &str, reason: &str) {
    let Ok(n) = config::number(ctx) else { return };
    for lang in &t.languages {
        let body = envelope(&n, "message_template_status_update", json!({"event": event.to_uppercase(), "message_template_id": t.id(), "message_template_name": t.name,
            "message_template_language": lang, "reason": if reason.is_empty() { "NONE" } else { reason }}));
        deliver(ctx, "template", "", "", body);
    }
}

pub fn category_event(ctx: &Ctx, t: &config::Template, old: &str) {
    let Ok(n) = config::number(ctx) else { return };
    let body = envelope(&n, "template_category_update", json!({"message_template_id": t.id(), "message_template_name": t.name,
        "message_template_language": t.languages.first().cloned().unwrap_or_default(), "previous_category": old.to_uppercase(), "new_category": t.category.to_uppercase()}));
    deliver(ctx, "template", "", "", body);
}

pub fn quality_event(ctx: &Ctx, n: &config::Number, old: &str) {
    let ev = match n.quality.as_str() { "red" => "DOWNGRADE", "flagged" => "FLAGGED", "restricted" => "FLAGGED", _ if old == "red" || old == "yellow" => "UPGRADE", _ => "UNFLAGGED" };
    let body = envelope(n, "phone_number_quality_update", json!({"display_phone_number": config::digits(&n.display.number), "event": ev, "current_limit": format!("TIER_{}", n.limits.tier.to_uppercase())}));
    deliver(ctx, "quality", "", "", body);
}

/// After a restart: messages still on their way (accepted, sent, delivered) pick up their
/// remaining steps from where the recipient rule says they should be now.
pub fn resume(ctx: &Ctx) {
    let ctx = ctx.clone();
    tokio::spawn(async move {
        let since = util::now_ms() - 86_400_000;
        let Ok(msgs) = store::list(&ctx, store::Filter { dir: Some("out".into()), since_ms: Some(since), limit: 2000, ..Default::default() }).await else { return };
        let tpls = config::templates(&ctx).unwrap_or_default();
        for m in msgs.iter().filter(|m| ["sent", "delivered", "read"].contains(&m["status"].as_str().unwrap_or("")) && util::now_ms() - m["at"].as_i64().unwrap_or(0) < 3_600_000) {
            let number = m["number"].as_str().unwrap_or("").to_string();
            let r = config::recipient_for(&ctx, &number);
            let sent_at = m["history"].as_array().and_then(|h| h.iter().find(|e| e["status"] == "sent")).and_then(|e| e["at"].as_i64()).unwrap_or(util::now_ms());
            let buttons = tpls.iter().find(|t| t.name == m["template"].as_str().unwrap_or("")).map(|t| t.buttons.clone()).unwrap_or_default();
            person_from(&ctx, &r, &number, m["id"].as_str().unwrap_or(""), m["kind"].as_str().unwrap_or(""), &buttons, (util::now_ms() - sent_at).max(1) as u64, sent_at);
        }
        for m in msgs.into_iter().filter(|m| ["accepted", "sent", "delivered"].contains(&m["status"].as_str().unwrap_or(""))) {
            let (id, number, st) = (m["id"].as_str().unwrap_or("").to_string(), m["number"].as_str().unwrap_or("").to_string(), m["status"].as_str().unwrap_or("").to_string());
            if st == "accepted" {
                let buttons = config::templates(&ctx).ok().and_then(|ts| ts.into_iter().find(|t| t.name == m["template"].as_str().unwrap_or("")).map(|t| t.buttons)).unwrap_or_default();
                lifecycle(&ctx, m, buttons);
                continue;
            }
            let r = config::recipient_for(&ctx, &number);
            let sent_at = m["history"].as_array().and_then(|h| h.iter().find(|e| e["status"] == "sent")).and_then(|e| e["at"].as_i64()).unwrap_or(util::now_ms());
            let elapsed = (util::now_ms() - sent_at).max(1) as u64;
            let ctx = ctx.clone();
            tokio::spawn(async move {
                if st == "sent" {
                    let Some(d) = r.delivered_ms() else { return };
                    tokio::time::sleep(Duration::from_millis(d.saturating_sub(elapsed))).await;
                    if current(&ctx, &id).await != "sent" { return; }
                    let _ = status(&ctx, &id, "delivered", None, "recipient rule (resumed)").await;
                }
                let Some(rd) = r.read_ms() else { return };
                let since_sent = (util::now_ms() - sent_at).max(0) as u64;
                tokio::time::sleep(Duration::from_millis(rd.saturating_sub(since_sent))).await;
                if current(&ctx, &id).await != "delivered" { return; }
                let _ = status(&ctx, &id, "read", None, "recipient rule (resumed)").await;
            });
        }
    });
}
