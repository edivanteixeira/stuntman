//! SendGrid v3 Mail Send, as the app calls it. Answers like SendGrid (202, X-Message-Id, its error
//! shapes) and turns each personalization into a MIME message for the common ingest path.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use stuntman_core::{util, Ctx};
use serde_json::{json, Value};

use crate::{config, ingest, smtp};

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route("/sendgrid/v3/mail/send", post(|s: State<Ctx>, h: HeaderMap, b: Bytes| send(s, None, h, b)))
        .route("/sendgrid/{service}/v3/mail/send", post(|s: State<Ctx>, Path(n): Path<String>, h: HeaderMap, b: Bytes| send(s, Some(n), h, b)))
}

fn sg_error(code: StatusCode, msg: &str, field: Option<&str>) -> Response {
    (code, Json(json!({"errors": [{"message": msg, "field": field, "help": null}]}))).into_response()
}

async fn send(State(ctx): State<Ctx>, service: Option<String>, h: HeaderMap, body: Bytes) -> Response {
    let svc = match service.as_deref().map(|n| config::get(&ctx, n)).unwrap_or_else(|| config::default_service(&ctx)) {
        Ok(s) => s,
        Err(e) => return sg_error(StatusCode::NOT_FOUND, &e.message, None),
    };
    let a = &svc.intake.sendgrid_api;
    if !a.on {
        return sg_error(StatusCode::SERVICE_UNAVAILABLE, "SendGrid API intake is off for this service", None);
    }
    if a.mode == "refuse" {
        match a.how.as_deref().unwrap_or("timeout") {
            "timeout" => {
                tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                return StatusCode::GATEWAY_TIMEOUT.into_response();
            }
            "429" => return (StatusCode::TOO_MANY_REQUESTS, [("Retry-After", "60")], Json(json!({"errors": [{"message": "too many requests"}]}))).into_response(),
            // A shared HTTP port cannot refuse the TCP connection or break TLS; the closest honest answer.
            "refused" | "tls" => return (StatusCode::BAD_GATEWAY, [("X-Stuntman-Note", "connection-level failure simulated as 502 on the shared HTTP port")], "").into_response(),
            _ => return sg_error(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable", None),
        }
    }
    let key = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").trim_start_matches("Bearer ").trim().to_string();
    if key.is_empty() {
        return sg_error(StatusCode::UNAUTHORIZED, "The provided authorization grant is invalid, expired, or revoked", None);
    }
    if a.mode == "validate" && !smtp::key_ok(a, &key) {
        return sg_error(StatusCode::UNAUTHORIZED, "The provided authorization grant is invalid, expired, or revoked", None);
    }
    if !ingest::rate_ok(&svc) {
        return (StatusCode::TOO_MANY_REQUESTS, [("Retry-After", "60")], Json(json!({"errors": [{"message": "too many requests"}]}))).into_response();
    }
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return sg_error(StatusCode::BAD_REQUEST, "Bad Request", None),
    };
    let from = &v["from"];
    if from["email"].as_str().unwrap_or("").is_empty() {
        return sg_error(StatusCode::BAD_REQUEST, "The from object must be provided for every email send. It is an object that requires the email parameter, but may also contain a name parameter.", Some("from"));
    }
    let Some(ps) = v["personalizations"].as_array().filter(|p| !p.is_empty()) else {
        return sg_error(StatusCode::BAD_REQUEST, "The personalizations field is required and must have at least one personalization.", Some("personalizations"));
    };
    let template = v["template_id"].as_str().unwrap_or("");
    if v["content"].as_array().map(|c| c.is_empty()).unwrap_or(true) && template.is_empty() && v["subject"].is_null() && ps.iter().all(|p| p["subject"].is_null()) {
        return sg_error(StatusCode::BAD_REQUEST, "Unless a valid template_id is provided, the content parameter is required.", Some("content"));
    }
    let trace = h.get("traceparent").and_then(|t| t.to_str().ok()).and_then(ingest::trace_from);
    let client = h.get("user-agent").and_then(|u| u.to_str().ok()).unwrap_or("app").to_string();
    ctx.live.seen(&client, "SendGrid API");
    let mut first_id = String::new();
    for (i, p) in ps.iter().enumerate() {
        let list = |k: &str| -> Vec<String> { p[k].as_array().map(|a| a.iter().filter_map(|t| t["email"].as_str().map(String::from)).collect()).unwrap_or_default() };
        let (to, cc, bcc) = (list("to"), list("cc"), list("bcc"));
        if to.is_empty() {
            return sg_error(StatusCode::BAD_REQUEST, "The to array is required for all personalization objects, and must have at least one email object with a valid email address.", Some(&format!("personalizations.{i}.to")));
        }
        let mut custom = v["custom_args"].as_object().cloned().unwrap_or_default();
        if let Some(c) = p["custom_args"].as_object() {
            custom.extend(c.clone());
        }
        let raw = build_mime(&v, p, &to, &cc);
        let env = ingest::Envelope {
            service: svc.clone(),
            via: "sendgrid".into(),
            mail_from: from["email"].as_str().unwrap_or("").into(),
            rcpts: to.into_iter().chain(cc).chain(bcc).collect(),
            raw,
            trace_id: trace.clone(),
            custom_args: Value::Object(custom),
            client: format!("api:{client}"),
        };
        match ingest::ingest(&ctx, env).await {
            Ok(ms) => {
                if first_id.is_empty() {
                    first_id = ms.first().map(|m| m.sg_message_id.clone()).unwrap_or_default();
                }
            }
            Err(e) => return sg_error(StatusCode::BAD_REQUEST, &e.message, None),
        }
    }
    (StatusCode::ACCEPTED, [("X-Message-Id", first_id)]).into_response()
}

fn hdr_addr(v: &Value) -> String {
    match (v["name"].as_str(), v["email"].as_str()) {
        (Some(n), Some(e)) if !n.is_empty() => format!("\"{}\" <{e}>", n.replace('"', "")),
        (_, Some(e)) => e.to_string(),
        _ => String::new(),
    }
}

fn enc_header(s: &str) -> String {
    if s.is_ascii() {
        s.to_string()
    } else {
        format!("=?UTF-8?B?{}?=", B64.encode(s.as_bytes()))
    }
}

fn b64_lines(data: &[u8]) -> String {
    let s = B64.encode(data);
    s.as_bytes().chunks(76).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("\r\n")
}

/// A MIME message equivalent to what SendGrid would deliver, so .eml download and the common
/// parser work the same for both intakes.
pub fn build_mime(v: &Value, p: &Value, to: &[String], cc: &[String]) -> Vec<u8> {
    let subject = p["subject"].as_str().or(v["subject"].as_str()).unwrap_or("");
    let mut h = String::new();
    h += &format!("From: {}\r\n", hdr_addr(&v["from"]));
    h += &format!("To: {}\r\n", p["to"].as_array().map(|a| a.iter().map(hdr_addr).collect::<Vec<_>>().join(", ")).unwrap_or(to.join(", ")));
    if !cc.is_empty() {
        h += &format!("Cc: {}\r\n", cc.join(", "));
    }
    if v["reply_to"].is_object() {
        h += &format!("Reply-To: {}\r\n", hdr_addr(&v["reply_to"]));
    }
    h += &format!("Subject: {}\r\n", enc_header(subject));
    h += &format!("Message-ID: <{}@stuntman>\r\n", util::id("sg"));
    h += "MIME-Version: 1.0\r\n";
    if let Some(t) = v["template_id"].as_str() {
        h += &format!("X-Template-Id: {t}\r\n");
    }
    for src in [&v["headers"], &p["headers"]] {
        if let Some(o) = src.as_object() {
            for (k, val) in o {
                h += &format!("{k}: {}\r\n", val.as_str().unwrap_or(&val.to_string()));
            }
        }
    }
    let mut parts: Vec<(String, String)> = v["content"]
        .as_array()
        .map(|c| c.iter().map(|x| (x["type"].as_str().unwrap_or("text/plain").to_string(), x["value"].as_str().unwrap_or("").to_string())).collect())
        .unwrap_or_default();
    if parts.is_empty() {
        let data = serde_json::to_string_pretty(&p["dynamic_template_data"]).unwrap_or_default();
        parts.push(("text/plain".into(), format!("Template {} rendered by SendGrid, with this data:\n{data}", v["template_id"].as_str().unwrap_or(""))));
    }
    let b = format!("dbl{:x}", rand::random::<u64>());
    let alt = format!("alt{:x}", rand::random::<u64>());
    let mut out = h;
    out += &format!("Content-Type: multipart/mixed; boundary=\"{b}\"\r\n\r\n");
    out += &format!("--{b}\r\nContent-Type: multipart/alternative; boundary=\"{alt}\"\r\n\r\n");
    for (t, val) in &parts {
        out += &format!("--{alt}\r\nContent-Type: {t}; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n", b64_lines(val.as_bytes()));
    }
    out += &format!("--{alt}--\r\n");
    if let Some(atts) = v["attachments"].as_array() {
        for a in atts {
            let name = a["filename"].as_str().unwrap_or("attachment");
            let t = a["type"].as_str().unwrap_or("application/octet-stream");
            let disp = a["disposition"].as_str().unwrap_or("attachment");
            let data = B64.decode(a["content"].as_str().unwrap_or("").replace(['\r', '\n'], "")).unwrap_or_default();
            out += &format!("--{b}\r\nContent-Type: {t}; name=\"{name}\"\r\nContent-Disposition: {disp}; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n");
            if let Some(cid) = a["content_id"].as_str() {
                out += &format!("Content-ID: <{cid}>\r\n");
            }
            out += &format!("\r\n{}\r\n", b64_lines(&data));
        }
    }
    out += &format!("--{b}--\r\n");
    out.into_bytes()
}
