//! The collectors' intakes: PostHog (/posthog: capture, batch, e, i/v0/e, decide, flags), Segment
//! (/segment/v1: track, identify, page, screen, group, alias, batch), Mixpanel (/mixpanel: track,
//! import, engage) and GA4 (/ga4/mp/collect, /ga4/debug/mp/collect). Bodies may be JSON, gzip, or
//! form-encoded base64 (data=…), as the SDKs send them.

use std::{io::Read, time::Duration};

use axum::{
    body::Bytes,
    extract::{Path, RawQuery, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use base64::Engine;
use serde_json::{json, Value};
use stuntman_core::Ctx;

use crate::store::{self, Ev};

pub const SOURCES: [&str; 4] = ["posthog", "segment", "mixpanel", "ga4"];
pub const FAILURES: [(&str, &str); 5] = [("400", "the batch is refused"), ("413", "the batch is too large"), ("429", "rate limited, Retry-After"), ("slow", "3 s before answering"), ("drop_half", "half the events are lost, 200 anyway")];

pub fn routes() -> Router<Ctx> {
    let mut r = Router::new();
    for s in SOURCES {
        r = r.route(&format!("/{s}/{{*rest}}"), any(move |State(ctx): State<Ctx>, Path(rest): Path<String>, headers: HeaderMap, RawQuery(q): RawQuery, body: Bytes| async move {
            handle(ctx, s, rest, headers, q.unwrap_or_default(), body).await
        }));
    }
    r
}

fn decode(headers: &HeaderMap, query: &str, body: &[u8]) -> Value {
    let mut b = body.to_vec();
    let gz = headers.get("content-encoding").and_then(|v| v.to_str().ok()).map(|v| v.contains("gzip")).unwrap_or(false) || query.contains("compression=gzip");
    if gz || b.starts_with(&[0x1f, 0x8b]) {
        let mut out = vec![];
        if flate2::read::GzDecoder::new(&b[..]).read_to_end(&mut out).is_ok() {
            b = out;
        }
    }
    let text = String::from_utf8_lossy(&b).to_string();
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        return v;
    }
    // Form bodies: data=<base64 JSON> (posthog-js legacy, Mixpanel).
    for kv in text.split('&') {
        if let Some(d) = kv.strip_prefix("data=") {
            let d = percent(d);
            let raw = base64::engine::general_purpose::STANDARD.decode(d.trim()).or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(d.trim())).unwrap_or_else(|_| d.into_bytes());
            if let Ok(v) = serde_json::from_slice::<Value>(&raw) {
                return v;
            }
        }
    }
    if let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(text.trim()) {
        if let Ok(v) = serde_json::from_slice::<Value>(&raw) {
            return v;
        }
    }
    Value::Null
}

fn percent(s: &str) -> String {
    let b = s.replace('+', " ").into_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&String::from_utf8_lossy(&b[i + 1..i + 3]), 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Personal data in properties: e-mail addresses, CPFs, card numbers.
pub fn pii(props: &Value) -> Vec<String> {
    let mut out = vec![];
    let email = regex::Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap();
    let cpf = regex::Regex::new(r"\b\d{3}\.?\d{3}\.?\d{3}-?\d{2}\b").unwrap();
    let card = regex::Regex::new(r"\b(?:\d[ -]?){13,19}\b").unwrap();
    fn walk(v: &Value, path: String, f: &mut dyn FnMut(&str, &str)) {
        match v {
            Value::Object(o) => o.iter().for_each(|(k, x)| walk(x, if path.is_empty() { k.clone() } else { format!("{path}.{k}") }, f)),
            Value::Array(a) => a.iter().enumerate().for_each(|(i, x)| walk(x, format!("{path}[{i}]"), f)),
            Value::String(s) => f(&path, s),
            Value::Number(n) => f(&path, &n.to_string()),
            _ => {}
        }
    }
    walk(props, String::new(), &mut |path, s| {
        if path.starts_with('$') && !path.contains("email") {
            return;
        }
        if email.is_match(s) {
            out.push(format!("{path}: e-mail"));
        } else if cpf.is_match(s) && s.chars().filter(|c| c.is_ascii_digit()).count() == 11 {
            out.push(format!("{path}: CPF"));
        } else if let Some(m) = card.find(s) {
            let d: Vec<u32> = m.as_str().chars().filter_map(|c| c.to_digit(10)).collect();
            let luhn = d.iter().rev().enumerate().map(|(i, x)| if i % 2 == 1 { let y = x * 2; if y > 9 { y - 9 } else { y } } else { *x }).sum::<u32>() % 10 == 0;
            if luhn {
                out.push(format!("{path}: card number"));
            }
        }
    });
    out
}

fn ev(source: &str, kind: &str, name: &str, user: &str, props: Value, sent_at: &str) -> Ev {
    let mut e = Ev { source: source.into(), kind: kind.into(), name: name.into(), user: user.into(), pii: pii(&props), props, sent_at: sent_at.into() };
    if e.user.contains('@') && !e.pii.iter().any(|p| p.starts_with("user")) {
        e.pii.push("user id: e-mail".into());
    }
    e
}

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Null) | None => String::new(),
        Some(o) => o.to_string(),
    }
}

/// GA4 Measurement Protocol checks (what /debug/mp/collect reports).
pub fn ga4_validate(b: &Value) -> Vec<Value> {
    let mut out = vec![];
    let name_ok = regex::Regex::new(r"^[A-Za-z][A-Za-z0-9_]{0,39}$").unwrap();
    let reserved = ["ad_activeview", "ad_click", "ad_exposure", "ad_query", "adunit_exposure", "app_clear_data", "app_install", "app_update", "app_remove", "error", "first_open", "first_visit", "in_app_purchase", "notification_dismiss", "notification_foreground", "notification_open", "notification_receive", "os_update", "session_start", "user_engagement"];
    if b.get("client_id").is_none() && b.get("app_instance_id").is_none() {
        out.push(json!({"fieldPath": "client_id", "description": "Measurement requires a client_id.", "validationCode": "VALUE_REQUIRED"}));
    }
    let evs = b["events"].as_array().cloned().unwrap_or_default();
    if evs.len() > 25 {
        out.push(json!({"fieldPath": "events", "description": "A request can have at most 25 events.", "validationCode": "VALUE_OUT_OF_BOUNDS"}));
    }
    for (i, e) in evs.iter().enumerate() {
        let n = s(e, "name");
        if !name_ok.is_match(&n) {
            out.push(json!({"fieldPath": format!("events[{i}].name"), "description": format!("Event name {n} is invalid. Names must start with a letter and use only letters, digits and underscores, up to 40 characters."), "validationCode": "NAME_INVALID"}));
        } else if reserved.contains(&n.as_str()) || n.starts_with("ga_") || n.starts_with("google_") || n.starts_with("firebase_") {
            out.push(json!({"fieldPath": format!("events[{i}].name"), "description": format!("Event name {n} is reserved."), "validationCode": "NAME_RESERVED"}));
        }
        if e["params"].as_object().map(|p| p.len() > 25).unwrap_or(false) {
            out.push(json!({"fieldPath": format!("events[{i}].params"), "description": "An event can have at most 25 parameters.", "validationCode": "VALUE_OUT_OF_BOUNDS"}));
        }
    }
    out
}

fn events_of(source: &str, rest: &str, b: &Value) -> Vec<Ev> {
    let mut out = vec![];
    match source {
        "posthog" => {
            let list = match b {
                Value::Array(a) => a.clone(),
                o if o.get("batch").is_some() => o["batch"].as_array().cloned().unwrap_or_default(),
                o if o.get("event").is_some() => vec![o.clone()],
                _ => vec![],
            };
            for e in list {
                let name = s(&e, "event");
                let props = e.get("properties").cloned().unwrap_or(json!({}));
                let user = { let d = s(&e, "distinct_id"); if d.is_empty() { s(&props, "distinct_id") } else { d } };
                let kind = match name.as_str() { "$identify" => "identify", "$pageview" => "page", "$groupidentify" => "group", _ => "track" };
                let mut p = props.clone();
                if let Some(set) = e.get("$set") {
                    p["$set"] = set.clone();
                }
                out.push(ev(source, kind, &name, &user, p, &s(&e, "timestamp")));
            }
        }
        "segment" => {
            let list = if b.get("batch").is_some() { b["batch"].as_array().cloned().unwrap_or_default() } else { vec![b.clone()] };
            let path_kind = rest.rsplit('/').next().unwrap_or("track");
            for e in list {
                let kind = { let t = s(&e, "type"); if t.is_empty() { path_kind.to_string() } else { t } };
                let user = { let u = s(&e, "userId"); if u.is_empty() { s(&e, "anonymousId") } else { u } };
                let (name, props) = match kind.as_str() {
                    "track" => (s(&e, "event"), e.get("properties").cloned().unwrap_or(json!({}))),
                    "identify" => ("identify".into(), e.get("traits").cloned().unwrap_or(json!({}))),
                    "page" | "screen" => (format!("{kind}{}", { let n = s(&e, "name"); if n.is_empty() { String::new() } else { format!(": {n}") } }), e.get("properties").cloned().unwrap_or(json!({}))),
                    "group" => ("group".into(), json!({"groupId": e["groupId"], "traits": e["traits"]})),
                    "alias" => ("alias".into(), json!({"previousId": e["previousId"]})),
                    _ => (kind.clone(), e.get("properties").cloned().unwrap_or(json!({}))),
                };
                out.push(ev(source, &kind, &name, &user, props, &s(&e, "timestamp")));
            }
        }
        "mixpanel" => {
            let list = match b { Value::Array(a) => a.clone(), o => vec![o.clone()] };
            for e in list {
                if rest.starts_with("engage") {
                    let user = s(&e, "$distinct_id");
                    out.push(ev(source, "identify", "$engage", &user, e.clone(), ""));
                    continue;
                }
                let props = e.get("properties").cloned().unwrap_or(json!({}));
                out.push(ev(source, "track", &s(&e, "event"), &s(&props, "distinct_id"), props, ""));
            }
        }
        "ga4" => {
            let user = { let u = s(b, "user_id"); if u.is_empty() { s(b, "client_id") } else { u } };
            for e in b["events"].as_array().cloned().unwrap_or_default() {
                out.push(ev(source, "track", &s(&e, "name"), &user, e.get("params").cloned().unwrap_or(json!({})), &s(b, "timestamp_micros")));
            }
            if let Some(up) = b.get("user_properties") {
                out.push(ev(source, "identify", "user_properties", &user, up.clone(), ""));
            }
        }
        _ => {}
    }
    out
}

async fn handle(ctx: Ctx, source: &'static str, rest: String, headers: HeaderMap, query: String, body: Bytes) -> Response {
    let rest = rest.trim_matches('/').to_string();
    let caller = headers.get("user-agent").and_then(|v| v.to_str().ok()).and_then(|u| u.split([' ', '/']).next()).filter(|s| !s.is_empty()).unwrap_or("app").to_string();
    ctx.live.seen(&caller, "Events");
    // Feature-flag lookups PostHog SDKs make: no flags.
    if source == "posthog" && (rest.starts_with("decide") || rest.starts_with("flags")) {
        return Json(json!({"featureFlags": {}, "flags": {}, "featureFlagPayloads": {}, "errorsWhileComputingFlags": false, "sessionRecording": false, "supportedCompression": ["gzip", "gzip-js"]})).into_response();
    }
    let debug = source == "ga4" && rest.starts_with("debug");
    let b = decode(&headers, &query, &body);
    let mut evs = events_of(source, &rest, &b);
    if !debug {
        if let Ok(Some(kind)) = store::take_force(&ctx, source).await {
            match kind.as_str() {
                "400" => return (StatusCode::BAD_REQUEST, Json(json!({"type": "validation_error", "detail": "Invalid batch (forced in Stuntman)"}))).into_response(),
                "413" => return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({"error": "Payload too large (forced in Stuntman)"}))).into_response(),
                "429" => return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "2")], Json(json!({"error": "Rate limited (forced in Stuntman)"}))).into_response(),
                "slow" => tokio::time::sleep(Duration::from_secs(3)).await,
                "drop_half" => {
                    let keep = evs.len() / 2;
                    evs.truncate(keep);
                }
                _ => {}
            }
        }
    }
    if debug {
        return Json(json!({"validationMessages": ga4_validate(&b)})).into_response();
    }
    let _ = store::add(&ctx, evs, &caller).await;
    match source {
        "posthog" => Json(json!({"status": 1})).into_response(),
        "segment" => Json(json!({"success": true})).into_response(),
        "mixpanel" => if query.contains("verbose=1") { Json(json!({"status": 1, "error": null})).into_response() } else { "1".into_response() },
        _ => StatusCode::NO_CONTENT.into_response(),
    }
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn sources_and_pii() {
        let p = events_of("posthog", "batch", &json!({"api_key": "phc", "batch": [{"event": "signup_completed", "distinct_id": "u1", "properties": {"plan": "pro", "email": "ana@globex.example"}}]}));
        assert_eq!((p[0].name.as_str(), p[0].user.as_str(), p[0].pii.len()), ("signup_completed", "u1", 1));
        let s = events_of("segment", "v1/batch", &json!({"batch": [{"type": "track", "event": "Order Completed", "userId": "u2", "properties": {"card": "4111 1111 1111 1111"}}, {"type": "identify", "userId": "u2", "traits": {"cpf": "111.444.777-35"}}]}));
        assert_eq!((s.len(), s[0].pii[0].as_str(), s[1].pii[0].as_str()), (2, "card: card number", "cpf: CPF"));
        let g = events_of("ga4", "mp/collect", &json!({"client_id": "c.1", "events": [{"name": "purchase", "params": {"value": 10}}]}));
        assert_eq!(g[0].name, "purchase");
        assert_eq!(ga4_validate(&json!({"client_id": "x", "events": [{"name": "first_visit"}, {"name": "9bad"}]})).len(), 2);
    }
}
