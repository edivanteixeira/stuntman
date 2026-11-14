//! The provider's event webhook back to the app, in SendGrid's format and signed like SendGrid
//! (ECDSA over timestamp + body). Every event is also kept with the message, with the app's reply.

use stuntman_core::{callback, util, ApiError, Ctx};
use serde_json::{json, Value};

use crate::{config, message};

/// Our event name → (SendGrid event, message status).
pub fn names(event: &str) -> (&'static str, &'static str) {
    match event {
        "processed" => ("processed", "processed"),
        "delivered" => ("delivered", "delivered"),
        "deferred" => ("deferred", "deferred"),
        "bounce" | "bounced" => ("bounce", "bounced"),
        "dropped" => ("dropped", "dropped"),
        "open" | "opened" => ("open", "opened"),
        "click" | "clicked" => ("click", "clicked"),
        "spamreport" => ("spamreport", "spamreport"),
        "unsubscribe" => ("unsubscribe", "unsubscribed"),
        _ => ("", ""),
    }
}

/// Builds one SendGrid event object for a message.
pub fn sendgrid_event(m: &message::Message, ev: &str, extra: &Value) -> Value {
    let mut e = json!({
        "email": m.to,
        "timestamp": util::now_ms() / 1000,
        "smtp-id": format!("<{}@stuntman>", m.sg_message_id),
        "event": ev,
        "category": [],
        "sg_event_id": format!("{:016x}", rand::random::<u64>()),
        "sg_message_id": format!("{}.filter0001.stuntman.0", m.sg_message_id),
    });
    if let (Value::Object(o), Value::Object(ca)) = (&mut e, &m.custom_args) {
        for (k, v) in ca {
            o.entry(k.clone()).or_insert(v.clone());
        }
    }
    if let (Value::Object(o), Value::Object(x)) = (&mut e, extra) {
        for (k, v) in x {
            o.insert(k.clone(), v.clone());
        }
    }
    e
}

/// Sends one event if the service asks for that type, records it, returns what the app replied.
pub async fn emit(ctx: &Ctx, svc: &config::Service, m: &message::Message, event: &str, extra: Value, by: &str) -> Result<Value, ApiError> {
    let (sg, _) = names(event);
    let body = sendgrid_event(m, sg, &extra);
    let detail = extra.get("detail").and_then(|d| d.as_str()).unwrap_or("").to_string();
    let wanted = !target(ctx, svc).is_empty() && svc.events.types.iter().any(|t| t == sg);
    let reply = if wanted { Some(post(ctx, svc, vec![body.clone()]).await) } else { None };
    let rv = reply.as_ref().map(|r| serde_json::to_value(r).unwrap());
    message::add_event(ctx, &m.id, sg, by, &detail, rv.clone()).await?;
    Ok(json!({"event": body, "sent": wanted, "reply": rv}))
}

pub async fn post(ctx: &Ctx, svc: &config::Service, events: Vec<Value>) -> callback::Reply {
    let payload = serde_json::to_vec(&events).unwrap();
    let mut headers = vec![];
    if svc.events.sign == "ecdsa" {
        let ts = (util::now_ms() / 1000).to_string();
        headers.push(("X-Twilio-Email-Event-Webhook-Signature".to_string(), ctx.signer.sign(&ts, &payload)));
        headers.push(("X-Twilio-Email-Event-Webhook-Timestamp".to_string(), ts));
    }
    headers.push(("User-Agent".to_string(), "SendGrid Event API (Stuntman)".to_string()));
    callback::post(&ctx.http, &target(ctx, svc), payload, &headers).await
}

/// The service's own events URL, else the environment's default callback target (N3).
pub fn target(ctx: &Ctx, svc: &config::Service) -> String {
    if svc.events.to.is_empty() { ctx.state.callback_default() } else { svc.events.to.clone() }
}

/// "Send test event" on E3.
pub async fn test(ctx: &Ctx, svc: &mut config::Service, event: &str) -> Result<Value, ApiError> {
    if target(ctx, svc).is_empty() {
        return Err(ApiError::bad("Set where to send events first"));
    }
    let m = message::Message {
        to: "test@example.com".into(),
        sg_message_id: util::id("test"),
        custom_args: json!({}),
        ..Default::default()
    };
    let (sg, _) = names(event);
    if sg.is_empty() {
        return Err(ApiError::bad(format!("Unknown event type “{event}”")));
    }
    let body = sendgrid_event(&m, sg, &json!({"test": true}));
    let reply = post(ctx, svc, vec![body.clone()]).await;
    let out = json!({"event": body, "reply": reply, "at": util::now_ms()});
    svc.events.last_test = Some(json!({"at": util::now_ms(), "status": reply.status, "ms": reply.ms, "error": reply.error}));
    config::save(ctx, svc)?;
    Ok(out)
}
