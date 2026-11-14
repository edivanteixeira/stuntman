//! Zenvia API v2, as the app calls api.zenvia.com: POST /v2/channels/sms/messages with X-API-TOKEN.
//! Point the app's base URL at <stuntman>/zenvia (for example SMS_API_URL =
//! http://localhost:8025/e/<env>/zenvia/v2/channels/{0}/messages). Contents may be text or a template
//! (templateId + fields). Refusals answer Zenvia's error body and are listed as "refused".

use axum::{
    body::Bytes,
    extract::{Extension, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use stuntman_core::{force, netserve::Kill, trips, util, Ctx};

use crate::{config, flow, store};

pub fn routes() -> Router<Ctx> {
    Router::new().route("/zenvia/v2/channels/{channel}/messages", post(send))
}

fn err(http: StatusCode, code: &str, message: &str, path: Option<(&str, &str)>) -> Response {
    let mut b = json!({"code": code, "message": message});
    if let Some((p, c)) = path {
        b["details"] = json!([{"code": c, "path": p, "message": message}]);
    }
    (http, Json(b)).into_response()
}

fn caller(h: &HeaderMap) -> String {
    h.get("x-client-name").or(h.get("user-agent")).and_then(|v| v.to_str().ok()).map(|s| s.split(['/', ' ']).next().unwrap_or("app").to_string()).filter(|s| !s.is_empty()).unwrap_or("app".into())
}

fn trace(h: &HeaderMap) -> Option<String> {
    h.get("traceparent").and_then(|t| t.to_str().ok()).and_then(|t| t.split('-').nth(1)).filter(|t| t.len() == 32).map(String::from)
}

/// The text the person reads, from text or template contents.
fn text_of(s: &config::Service, c: &Value) -> Result<(String, Value), String> {
    match c["type"].as_str().unwrap_or("") {
        "text" => Ok((c["text"].as_str().unwrap_or("").to_string(), Value::Null)),
        "template" => {
            let id = c["templateId"].as_str().unwrap_or("").to_string();
            let fields = c["fields"].clone();
            let tpl = json!({"templateId": id, "fields": fields});
            match s.templates.iter().find(|t| t.id == id) {
                Some(t) => Ok((config::render(&t.text, &fields), json!({"templateId": id, "name": t.name, "fields": fields}))),
                None if s.strict_templates => Err(format!("Template {id} not found")),
                None => {
                    let shown = match &fields {
                        Value::Object(o) => o.iter().map(|(k, v)| format!("{k}: {}", v.as_str().map(String::from).unwrap_or(v.to_string()))).collect::<Vec<_>>().join(", "),
                        Value::String(x) => x.clone(),
                        _ => String::new(),
                    };
                    Ok((format!("[template {}] {shown}", &id[..8.min(id.len())]), tpl))
                }
            }
        }
        other => Err(format!("contents type “{other}” is not sent by SMS (text or template)")),
    }
}

async fn refuse(ctx: &Ctx, s: &config::Service, to: &str, from: &str, text: &str, body: &Value, who: &str, tr: Option<String>, status: StatusCode, code: &str, message: &str, path: Option<(&str, &str)>) -> Response {
    let id = util::uuid();
    let _ = store::insert(ctx, store::Msg { id: id.clone(), dir: "out", number: config::digits(to), sender: from.to_string(), text: text.to_string(), status: "refused".into(),
        reason: format!("{} {message}", status.as_u16()), body: body.clone(), trace_id: tr.clone(), caller: who.to_string(), flavour: s.flavour.clone(),
        external_id: body["externalId"].as_str().unwrap_or("").to_string(), by: String::new() }).await;
    let _ = trips::record(ctx, trips::Trip { id: &id, module: "sms", label: &format!("SMS to +{} refused: {message}", config::digits(to)), status: "refused", trace_id: tr.as_deref(), link: &format!("/sms?id={id}"),
        host: "api.zenvia.com", actor: who, outcome: "failed", detail: &format!("{} {code}", status.as_u16()) }).await;
    err(status, code, message, path)
}

/// A failure forced with force_sms_failure: Zenvia's 500 or 400 body, or the connection fails.
/// Listed as refused, like the other refusals.
async fn forced(ctx: &Ctx, s: &config::Service, v: &Value, who: &str, tr: Option<String>, answer: &str, by: &str, kill: Option<Kill>) -> Response {
    let to = v["to"].as_str().map(String::from).or_else(|| v["to"].as_u64().map(|x| x.to_string())).unwrap_or_default();
    let from = v["from"].as_str().unwrap_or("");
    let text = v["contents"][0]["text"].as_str().unwrap_or("");
    let by = by.trim_start_matches("api:").trim_start_matches("mcp:");
    let (status, code, message, net) = match answer {
        "500" => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", "Internal Server Error".to_string(), false),
        "400" => (StatusCode::BAD_REQUEST, "VALIDATION_ERROR", "Validation error".to_string(), false),
        n => (StatusCode::BAD_GATEWAY, n, format!("connection {n}"), true),
    };
    let id = util::uuid();
    let reason = if net { format!("{message} (forced by {by})") } else { format!("{} {message} (forced by {by})", status.as_u16()) };
    let _ = store::insert(ctx, store::Msg { id: id.clone(), dir: "out", number: config::digits(&to), sender: from.to_string(), text: text.to_string(), status: "refused".into(),
        reason, body: v.clone(), trace_id: tr.clone(), caller: who.to_string(), flavour: s.flavour.clone(), external_id: v["externalId"].as_str().unwrap_or("").to_string(), by: by.to_string() }).await;
    let _ = trips::record(ctx, trips::Trip { id: &id, module: "sms", label: &format!("SMS to +{} refused: {message}", config::digits(&to)), status: "refused", trace_id: tr.as_deref(), link: &format!("/sms?id={id}"),
        host: "api.zenvia.com", actor: who, outcome: "provoked", detail: &format!("{answer} forced by {by}") }).await;
    if net {
        return force::network(answer, kill).await;
    }
    err(status, code, &message, (answer == "400").then_some(("to", "INVALID_VALUE")))
}

async fn send(State(ctx): State<Ctx>, Path(channel): Path<String>, kill: Option<Extension<Kill>>, h: HeaderMap, body: Bytes) -> Response {
    let s = match config::service(&ctx) { Ok(s) => s, Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", &e.message, None) };
    let tok = h.get("x-api-token").and_then(|v| v.to_str().ok()).unwrap_or("").trim().to_string();
    if tok.is_empty() || (!s.access.any_token && tok != util::resolve_secret(&s.access.token, &ctx.data_dir)) {
        return err(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", "Unauthorized: missing or invalid X-API-TOKEN", None);
    }
    let who = caller(&h);
    ctx.live.seen(&who, "Zenvia SMS");
    let tr = trace(&h);
    let v: Value = match serde_json::from_slice(&body) { Ok(v) => v, Err(_) => return err(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", "Malformed JSON", None) };
    let to = v["to"].as_str().map(String::from).or_else(|| v["to"].as_u64().map(|x| x.to_string())).unwrap_or_default();
    let from = v["from"].as_str().unwrap_or("").to_string();
    if let Some((answer, by)) = force::take(&ctx, "sms").await {
        return forced(&ctx, &s, &v, &who, tr, &answer, &by, kill.map(|k| k.0)).await;
    }
    if channel != "sms" {
        return err(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", &format!("Channel {channel} is not simulated here (SMS only; WhatsApp is its own service)"), Some(("channel", "INVALID_VALUE")));
    }
    let contents = v["contents"].as_array().cloned().unwrap_or_default();
    let (text, tpl) = match contents.first().map(|c| text_of(&s, c)) {
        Some(Ok(x)) => x,
        Some(Err(e)) => return refuse(&ctx, &s, &to, &from, "", &v, &who, tr, StatusCode::BAD_REQUEST, "VALIDATION_ERROR", &e, Some(("contents[0]", "INVALID_VALUE"))).await,
        None => return refuse(&ctx, &s, &to, &from, "", &v, &who, tr, StatusCode::BAD_REQUEST, "VALIDATION_ERROR", "contents is required", Some(("contents", "REQUIRED"))).await,
    };
    let digits = config::digits(&to);
    if to.is_empty() || digits.len() < 10 || digits.len() > 15 || to.chars().any(|c| c.is_alphabetic()) {
        return refuse(&ctx, &s, &to, &from, &text, &v, &who, tr, StatusCode::BAD_REQUEST, "VALIDATION_ERROR", &format!("invalid “to”: {to}"), Some(("to", "INVALID_FORMAT"))).await;
    }
    if from.is_empty() {
        return refuse(&ctx, &s, &to, &from, &text, &v, &who, tr, StatusCode::BAD_REQUEST, "VALIDATION_ERROR", "“from” is required", Some(("from", "REQUIRED"))).await;
    }
    if s.strict_senders && !s.senders.iter().any(|x| x.id == from) {
        return refuse(&ctx, &s, &to, &from, &text, &v, &who, tr, StatusCode::BAD_REQUEST, "VALIDATION_ERROR", &format!("unknown sender “{from}”"), Some(("from", "INVALID_VALUE"))).await;
    }
    if store::opted_out(&ctx, &digits).await.ok().flatten().is_some() {
        return refuse(&ctx, &s, &to, &from, &text, &v, &who, tr, StatusCode::BAD_REQUEST, "VALIDATION_ERROR", &format!("+{digits} opted out (texted STOP)"), Some(("to", "OPTED_OUT"))).await;
    }
    if store::sent_last_second(&ctx).await.unwrap_or(0) >= s.limits.throughput.max(1) as i64 {
        return refuse(&ctx, &s, &to, &from, &text, &v, &who, tr, StatusCode::TOO_MANY_REQUESTS, "TOO_MANY_REQUESTS", &format!("Over {} messages/s", s.limits.throughput), None).await;
    }
    let (_, _, _, seg) = config::encoding(&text);
    if seg as u32 > s.limits.max_segments.max(1) {
        return refuse(&ctx, &s, &to, &from, &text, &v, &who, tr, StatusCode::BAD_REQUEST, "VALIDATION_ERROR", &format!("message too long: {seg} segments (max {})", s.limits.max_segments), Some(("contents[0].text", "MAX_LENGTH"))).await;
    }
    let id = util::uuid();
    let ext = v["externalId"].as_str().unwrap_or("").to_string();
    let mut stored = v.clone();
    if !tpl.is_null() { stored["template"] = tpl; }
    let m = store::Msg { id: id.clone(), dir: "out", number: digits.clone(), sender: from.clone(), text: text.clone(), status: "accepted".into(), reason: String::new(), body: stored,
        trace_id: tr.clone(), caller: who.clone(), flavour: s.flavour.clone(), external_id: ext.clone(), by: String::new() };
    if let Err(e) = store::insert(&ctx, m).await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", &e.message, None);
    }
    let _ = trips::record(&ctx, trips::Trip { id: &id, module: "sms", label: &format!("SMS to +{digits}: {}", text.chars().take(50).collect::<String>()), status: "accepted", trace_id: tr.as_deref(),
        link: &format!("/sms?id={id}"), host: "api.zenvia.com", actor: &who, outcome: "ok", detail: &format!("{seg} segment(s)  ·  from {from}") }).await;
    if let Ok(saved) = store::get(&ctx, &id).await {
        flow::lifecycle(&ctx, saved);
    }
    let mut out = json!({"id": id, "from": from, "to": digits, "direction": "OUT", "channel": "sms", "contents": contents, "timestamp": flow::iso()});
    if !ext.is_empty() { out["externalId"] = json!(ext); }
    (StatusCode::OK, Json(out)).into_response()
}
