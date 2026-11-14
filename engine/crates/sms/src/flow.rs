//! What happens after the app sends: the text goes SENT → DELIVERED (or NOT_DELIVERED with a reason)
//! as the recipient's steps say, the person may reply (STOP opts out), and every change reaches the
//! app as Zenvia's own MESSAGE_STATUS / MESSAGE webhooks, retried 3 times in a minute.

use std::time::Duration;

use serde_json::{json, Value};
use stuntman_core::{trips, util, ApiError, Ctx};

use crate::{config, store};

pub fn url(ctx: &Ctx, s: &config::Service, kind: &str) -> String {
    let u = if kind == "MESSAGE" { &s.callbacks.inbound } else { &s.callbacks.status };
    if u.trim().is_empty() { ctx.state.callback_default() } else { u.trim().to_string() }
}

/// "2026-11-13T22:41:10.784Z", as Zenvia stamps its webhooks.
pub fn iso() -> String {
    let ms = util::now_ms();
    let (secs, milli) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // days since 1970-01-01 → civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z", sod / 3600, sod / 60 % 60, sod % 60)
}

/// Zenvia's MESSAGE_STATUS body.
pub fn status_body(m: &Value, code: &str, reason: &str) -> Value {
    let mut st = json!({"timestamp": iso(), "code": code, "description": match code { "SENT" => "The message was sent to the carrier", "DELIVERED" => "The message was delivered", "NOT_DELIVERED" => "The message was not delivered", _ => code }});
    if code == "NOT_DELIVERED" {
        st["causes"] = json!([{"channelErrorCode": reason, "reason": config::reason_text(reason), "details": config::reason_text(reason)}]);
    }
    json!({"id": util::uuid(), "timestamp": iso(), "type": "MESSAGE_STATUS", "subscriptionId": "stuntman-status", "channel": "sms",
        "messageId": m["id"], "externalId": m["externalId"], "contentIndex": 0, "messageStatus": st})
}

/// Sends a webhook with retries; notes each answer.
pub fn deliver(ctx: &Ctx, kind: &str, message_id: &str, status: &str, body: Value) {
    let (ctx, kind, mid, status) = (ctx.clone(), kind.to_string(), message_id.to_string(), status.to_string());
    tokio::spawn(async move {
        let Ok(s) = config::service(&ctx) else { return };
        let u = url(&ctx, &s, &kind);
        let text = body.to_string();
        if u.is_empty() {
            let _ = store::hook(&ctx, &kind, &mid, "", None, 0, "no callback URL set (Service settings)", 1, &text).await;
            let _ = store::note_hook(&ctx, &mid, &status, json!({"error": "no callback URL set"})).await;
            return;
        }
        for attempt in 1..=4u32 {
            let t = util::now_ms();
            let r = ctx.http.post(&u).header("content-type", "application/json").header("user-agent", "Zenvia-Webhook").timeout(Duration::from_secs(10)).body(text.clone()).send().await;
            let (code, err) = match r { Ok(x) => (Some(x.status().as_u16()), String::new()), Err(e) => (None, if e.is_connect() { "connection refused".into() } else { e.to_string() }) };
            let ms = util::now_ms() - t;
            let _ = store::hook(&ctx, &kind, &mid, &u, code, ms, &err, attempt, &text).await;
            let _ = store::note_hook(&ctx, &mid, &status, json!({"status": code, "ms": ms, "attempt": attempt, "error": err, "url": u})).await;
            if code.map(|c| (200..300).contains(&c)).unwrap_or(false) || ctx.state.gone() {
                return;
            }
            tokio::time::sleep(Duration::from_secs(20)).await;
        }
    });
}

/// Changes a message's status and tells the app.
pub async fn status(ctx: &Ctx, id: &str, code: &str, reason: &str, by: &str) -> Result<Value, ApiError> {
    let m = store::set_status(ctx, id, code, reason, by).await?;
    deliver(ctx, "MESSAGE_STATUS", id, code, status_body(&m, code, reason));
    let number = m["number"].as_str().unwrap_or("").to_string();
    let label = format!("SMS to +{number}, {}{}", code.to_lowercase().replace('_', " "), if reason.is_empty() { String::new() } else { format!(" ({})", config::reason_text(reason)) });
    let _ = trips::record(ctx, trips::Trip { id, module: "sms", label: &label, status: code, trace_id: m["traceId"].as_str(), link: &format!("/sms?id={id}"), host: "api.zenvia.com", actor: "",
        outcome: if code == "NOT_DELIVERED" { "failed" } else if by.starts_with("mcp:") || by.starts_with("api:") || by == "you" { "provoked" } else { "ok" },
        detail: &format!("{code}{}", if by.is_empty() { String::new() } else { format!(" by {}", by.trim_start_matches("mcp:").trim_start_matches("api:")) }) }).await;
    Ok(m)
}

async fn current(ctx: &Ctx, id: &str) -> String {
    store::get(ctx, id).await.ok().and_then(|m| m["status"].as_str().map(String::from)).unwrap_or_default()
}

/// The steps after the app's text was accepted (in the background).
pub fn lifecycle(ctx: &Ctx, m: Value) {
    let ctx = ctx.clone();
    tokio::spawn(async move {
        let id = m["id"].as_str().unwrap_or("").to_string();
        let number = m["number"].as_str().unwrap_or("").to_string();
        let r = config::recipient_for(&ctx, &number);
        tokio::time::sleep(Duration::from_millis(400)).await;
        if current(&ctx, &id).await != "accepted" { return; }
        let _ = status(&ctx, &id, "SENT", "", "recipient rule").await;
        if let Some(reason) = r.not_delivered() {
            tokio::time::sleep(Duration::from_millis(r.delivered_ms())).await;
            if current(&ctx, &id).await == "SENT" { let _ = status(&ctx, &id, "NOT_DELIVERED", &reason, "recipient rule").await; }
            return;
        }
        if let Some(after) = r.never_delivered() {
            tokio::time::sleep(Duration::from_millis(after)).await;
            if current(&ctx, &id).await == "SENT" { let _ = status(&ctx, &id, "NOT_DELIVERED", "phone_off", "recipient rule (expired)").await; }
            return;
        }
        tokio::time::sleep(Duration::from_millis(r.delivered_ms())).await;
        if current(&ctx, &id).await != "SENT" { return; }
        let _ = status(&ctx, &id, "DELIVERED", "", "recipient rule").await;
        // Replies the recipient's steps describe.
        let text = m["text"].as_str().unwrap_or("").to_lowercase();
        let nth = store::count_today(&ctx, &number).await.unwrap_or(0) as u32;
        let reply = r.steps.iter().find_map(|s| {
            s.on_nth.as_ref().filter(|x| x.n == Some(nth))
                .or(s.on_question.as_ref().filter(|_| text.trim_end().ends_with('?') || text.contains("reply") || text.contains("responda")))
                .or(s.on_text.as_ref())
                .cloned()
        });
        if let Some(rp) = reply {
            tokio::time::sleep(Duration::from_millis(util::parse_ms(&rp.after).unwrap_or(5_000))).await;
            let _ = reply_from(&ctx, &number, &rp.reply, m["sender"].as_str().unwrap_or(""), "recipient rule").await;
        }
    });
}

/// The person texts back: MESSAGE webhook to the app; STOP-like keywords opt the number out, START opts it in.
pub async fn reply_from(ctx: &Ctx, number: &str, text: &str, to: &str, by: &str) -> Result<Value, ApiError> {
    let s = config::service(ctx)?;
    let n = config::digits(number);
    let to = if to.is_empty() { s.senders.first().map(|x| x.id.clone()).unwrap_or_default() } else { to.to_string() };
    let id = util::uuid();
    let word = text.trim().to_uppercase();
    store::insert(ctx, store::Msg { id: id.clone(), dir: "in", number: n.clone(), sender: to.clone(), text: text.to_string(), status: "received".into(), reason: String::new(),
        body: Value::Null, trace_id: None, caller: String::new(), flavour: s.flavour.clone(), external_id: String::new(), by: by.to_string() }).await?;
    if s.opt_out.keywords.iter().any(|k| k.eq_ignore_ascii_case(&word)) {
        store::opt_out(ctx, &n, &format!("texted {word}")).await?;
    } else if !s.opt_out.back_in.is_empty() && word == s.opt_out.back_in.to_uppercase() {
        store::opt_in(ctx, &n).await?;
    }
    let body = json!({"id": util::uuid(), "timestamp": iso(), "type": "MESSAGE", "subscriptionId": "stuntman-inbound", "channel": "sms", "direction": "IN",
        "message": {"id": id, "from": n, "to": to, "direction": "IN", "channel": "sms", "contents": [{"type": "text", "text": text}], "timestamp": iso()}});
    deliver(ctx, "MESSAGE", &id, "received", body);
    let _ = trips::record(ctx, trips::Trip { id: &id, module: "sms", label: &format!("+{n} replied “{}”", text.chars().take(40).collect::<String>()), status: "received", trace_id: None, link: &format!("/sms?id={id}"),
        host: "api.zenvia.com", actor: "", outcome: if by == "recipient rule" { "ok" } else { "provoked" }, detail: &format!("reply by {}", by.trim_start_matches("mcp:").trim_start_matches("api:")) }).await;
    store::get(ctx, &id).await
}

/// After a restart: messages still on their way continue.
pub fn resume(ctx: &Ctx) {
    let ctx = ctx.clone();
    tokio::spawn(async move {
        let Ok(msgs) = store::list(&ctx, store::Filter { dir: Some("out".into()), since_ms: Some(util::now_ms() - 3_600_000), limit: 500, ..Default::default() }).await else { return };
        for m in msgs.into_iter().filter(|m| ["accepted", "SENT"].contains(&m["status"].as_str().unwrap_or(""))) {
            if m["status"] == "SENT" {
                let _ = store::set_status(&ctx, m["id"].as_str().unwrap_or(""), "accepted", "", "resumed").await;
            }
            lifecycle(&ctx, m);
        }
    });
}
