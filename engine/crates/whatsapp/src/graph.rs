//! Meta's WhatsApp Cloud API, as the app calls graph.facebook.com: send messages, fetch media,
//! manage templates. Point the app's Graph base URL at <stuntman>/whatsapp. Errors carry Meta's
//! codes (190 token, 100 parameter, 130429 rate, 131056 tier, 131048 quality, 132000/132001/
//! 132015/132016 templates); refusals that Meta reports later (131026, 131047) come as status webhooks.

use axum::{
    body::Bytes,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use stuntman_core::{force, netserve::Kill, trips, util, Ctx};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{config, flow, store};

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route("/whatsapp/{ver}/{id}/messages", post(send))
        .route("/whatsapp/{ver}/{id}/message_templates", get(templates_list).post(template_create).delete(template_delete))
        .route("/whatsapp/{ver}/{id}", get(media_info))
        .route("/whatsapp/media/{id}/file", get(media_file))
}

pub fn err(http: StatusCode, code: u32, message: &str, sub: Option<u32>) -> Response {
    let mut e = json!({"message": message, "type": if code == 190 { "OAuthException" } else { "OAuthException" }, "code": code,
        "error_data": {"messaging_product": "whatsapp", "details": message}, "fbtrace_id": format!("A{}", &util::uuid().replace('-', "")[..22])});
    if let Some(s) = sub { e["error_subcode"] = json!(s); }
    (http, Json(json!({"error": e}))).into_response()
}

fn caller(h: &HeaderMap) -> String {
    h.get("x-client-name").or(h.get("user-agent")).and_then(|v| v.to_str().ok()).map(|s| s.split(['/', ' ']).next().unwrap_or("app").to_string()).filter(|s| !s.is_empty()).unwrap_or("app".into())
}

/// Bearer token check: expired on purpose (190/463), strict token (190), or any token.
fn auth(ctx: &Ctx, n: &config::Number, h: &HeaderMap) -> Result<(), Response> {
    let tok = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").trim_start_matches("Bearer ").trim().to_string();
    if n.tokens_expired_at.is_some() {
        return Err(err(StatusCode::UNAUTHORIZED, 190, "Error validating access token: Session has expired", Some(463)));
    }
    if tok.is_empty() {
        return Err(err(StatusCode::UNAUTHORIZED, 190, "Invalid OAuth access token - Cannot parse access token", None));
    }
    if !n.access.any_token && tok != util::resolve_secret(&n.access.token, &ctx.data_dir) {
        return Err(err(StatusCode::UNAUTHORIZED, 190, "Invalid OAuth access token.", None));
    }
    Ok(())
}

fn trace(h: &HeaderMap) -> Option<String> {
    h.get("traceparent").and_then(|t| t.to_str().ok()).and_then(|t| t.split('-').nth(1)).filter(|t| t.len() == 32).map(String::from)
}

fn param_text(p: &Value) -> String {
    match p["type"].as_str().unwrap_or("text") {
        "currency" => p["currency"]["fallback_value"].as_str().map(String::from).unwrap_or_else(|| format!("{} {}", p["currency"]["code"].as_str().unwrap_or(""), p["currency"]["amount_1000"].as_f64().unwrap_or(0.0) / 1000.0)),
        "date_time" => p["date_time"]["fallback_value"].as_str().unwrap_or("").to_string(),
        "text" => p["text"].as_str().unwrap_or("").to_string(),
        other => p[other].as_str().map(String::from).unwrap_or_else(|| format!("({other})")),
    }
}

/// "any", or a range such as "v18.0-v21.0": other versions get Meta's "unknown path" answer.
pub fn version_ok(range: &str, ver: &str) -> bool {
    let num = |s: &str| s.trim().trim_start_matches('v').parse::<f64>().ok();
    match range.split_once('-') {
        Some((lo, hi)) => match (num(lo), num(hi), num(ver)) { (Some(l), Some(h), Some(v)) => v >= l && v <= h, _ => false },
        None => range.trim() == "any" || range.trim().is_empty() || num(range) == num(ver),
    }
}

/// A failure forced with force_whatsapp_failure: Meta's 500 (131000) or 400 (#100) body, or the
/// connection fails. Kept as a refused message so the conversation shows it.
async fn forced(ctx: &Ctx, v: &Value, to: &str, who: &str, h: &HeaderMap, answer: &str, by: &str, kill: Option<Kill>) -> Response {
    let by = by.trim_start_matches("api:").trim_start_matches("mcp:");
    let num = config::digits(to);
    let kind = v["type"].as_str().unwrap_or("text").to_string();
    let text = v["text"]["body"].as_str().unwrap_or("").to_string();
    let tr = trace(h);
    let id = flow::wamid();
    let detail = match answer { "500" => "500 (#131000) Something went wrong", "400" => "400 (#100) Invalid parameter", n => n };
    let _ = store::insert(ctx, store::Msg { id: id.clone(), dir: "out", number: num.clone(), kind, template: v["template"]["name"].as_str().unwrap_or("").to_string(), lang: String::new(), text,
        status: "refused".into(), body: v.clone(), trace_id: tr.clone(), caller: who.to_string(), context_id: String::new(), by: by.to_string(), params: vec![], category: String::new() }).await;
    let _ = trips::record(ctx, trips::Trip { id: &id, module: "whatsapp", label: &format!("WhatsApp to +{num} refused: {detail}"), status: "refused", trace_id: tr.as_deref(), link: &format!("/whatsapp?number={num}"),
        host: "graph.facebook.com", actor: who, outcome: "provoked", detail: &format!("{answer} forced by {by}") }).await;
    match answer {
        "500" => err(StatusCode::INTERNAL_SERVER_ERROR, 131000, "(#131000) Something went wrong", None),
        "400" => err(StatusCode::BAD_REQUEST, 100, "(#100) Invalid parameter", None),
        n => force::network(n, kill).await,
    }
}

async fn send(State(ctx): State<Ctx>, Path((ver, id)): Path<(String, String)>, kill: Option<Extension<Kill>>, h: HeaderMap, body: Bytes) -> Response {
    let n = match config::number(&ctx) { Ok(n) => n, Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, 1, &e.message, None) };
    if !version_ok(&n.api_versions, &ver) {
        return err(StatusCode::BAD_REQUEST, 2500, &format!("Unknown path components: /{ver}/{id}/messages (this number accepts {})", n.api_versions), None);
    }
    if let Err(r) = auth(&ctx, &n, &h) { return r; }
    if id != n.phone_number_id {
        return err(StatusCode::BAD_REQUEST, 100, &format!("Unsupported post request. Object with ID '{id}' does not exist, cannot be loaded due to missing permissions, or does not support this operation."), Some(33));
    }
    let v: Value = match serde_json::from_slice(&body) { Ok(v) => v, Err(_) => return err(StatusCode::BAD_REQUEST, 100, "(#100) Invalid parameter", None) };
    let to = v["to"].as_str().map(String::from).or_else(|| v["to"].as_u64().map(|x| x.to_string())).unwrap_or_default();
    if to.is_empty() { return err(StatusCode::BAD_REQUEST, 100, "(#100) The parameter to is required.", None); }
    let who = caller(&h);
    ctx.live.seen(&who, "WhatsApp API");
    if let Some((answer, by)) = force::take(&ctx, "whatsapp").await {
        return forced(&ctx, &v, &to, &who, &h, &answer, &by, kill.map(|k| k.0)).await;
    }
    if n.quality == "restricted" { return err(StatusCode::BAD_REQUEST, 131048, flow::title(131048), None); }
    if store::sent_last_second(&ctx).await.unwrap_or(0) >= n.limits.throughput.max(1) as i64 { return err(StatusCode::BAD_REQUEST, 130429, flow::title(130429), None); }
    let num = config::digits(&to);
    if let Some(max) = n.limits.tier_max() {
        let seen = store::numbers_24h(&ctx).await.unwrap_or_default();
        if !seen.contains(&num) && seen.len() >= max { return err(StatusCode::BAD_REQUEST, 131056, flow::title(131056), None); }
    }
    let kind = v["type"].as_str().unwrap_or("text").to_string();
    let text: String;
    let (mut tpl, mut lang, mut params, mut buttons, mut category) = (String::new(), String::new(), vec![], vec![], String::new());
    match kind.as_str() {
        "template" => {
            tpl = v["template"]["name"].as_str().unwrap_or("").to_string();
            lang = v["template"]["language"]["code"].as_str().unwrap_or("").to_string();
            for c in v["template"]["components"].as_array().cloned().unwrap_or_default() {
                if c["type"].as_str().map(|t| t.eq_ignore_ascii_case("body")).unwrap_or(false) {
                    params = c["parameters"].as_array().cloned().unwrap_or_default().iter().map(param_text).collect();
                }
            }
            let all = config::templates(&ctx).unwrap_or_default();
            let t = all.iter().find(|t| t.name == tpl && t.languages.iter().any(|l| l == &lang));
            let Some(t) = t else {
                let _ = store::undeclared(&ctx, &tpl, &lang, params.len(), &v, &who).await;
                return err(StatusCode::NOT_FOUND, 132001, &format!("(#132001) {} — template name ({tpl}) does not exist in {lang}", flow::title(132001)), None);
            };
            match t.status.as_str() {
                "paused" => return err(StatusCode::BAD_REQUEST, 132015, &format!("(#132015) {}", flow::title(132015)), None),
                "disabled" => return err(StatusCode::BAD_REQUEST, 132016, &format!("(#132016) {}", flow::title(132016)), None),
                "approved" => {}
                _ => return err(StatusCode::NOT_FOUND, 132001, &format!("(#132001) {} — {tpl} is {}", flow::title(132001), t.status), None),
            }
            if params.len() != t.body_params() {
                return err(StatusCode::BAD_REQUEST, 132000, &format!("(#132000) {}: expected {}, got {}", flow::title(132000), t.body_params(), params.len()), None);
            }
            text = config::render(&t.body, &params);
            buttons = t.buttons.clone();
            category = t.category.clone();
        }
        "text" => text = v["text"]["body"].as_str().unwrap_or("").to_string(),
        "image" | "document" | "video" | "audio" | "sticker" => {
            let o = &v[kind.as_str()];
            text = o["caption"].as_str().map(String::from).unwrap_or_else(|| o["link"].as_str().or(o["id"].as_str()).unwrap_or("").to_string());
        }
        "interactive" => text = v["interactive"]["body"]["text"].as_str().unwrap_or("").to_string(),
        "location" => text = format!("{}, {}", v["location"]["latitude"], v["location"]["longitude"]),
        "reaction" => text = v["reaction"]["emoji"].as_str().unwrap_or("").to_string(),
        _ => return err(StatusCode::BAD_REQUEST, 100, &format!("(#100) Param type must be one of {{TEXT, TEMPLATE, IMAGE, …}} - got \"{kind}\"."), None),
    }
    let id = flow::wamid();
    let tr = trace(&h);
    let m = store::Msg { id: id.clone(), dir: "out", number: num.clone(), kind: kind.clone(), template: tpl.clone(), lang, text, status: "accepted".into(), body: v,
        trace_id: tr.clone(), caller: who.clone(), context_id: String::new(), by: who.clone(), params, category };
    if let Err(e) = store::insert(&ctx, m).await { return err(StatusCode::INTERNAL_SERVER_ERROR, 1, &e.message, None); }
    let label = if tpl.is_empty() { format!("{kind} to +{num}") } else { format!("{tpl} to +{num}") };
    let _ = trips::record(&ctx, trips::Trip { id: &id, module: "whatsapp", label: &label, status: "accepted", trace_id: tr.as_deref(), link: &format!("/whatsapp?number={num}"), host: "graph.facebook.com" , actor: &who, outcome: "ok", detail: &who }).await;
    if let Ok(saved) = store::get(&ctx, &id).await { flow::lifecycle(&ctx, saved, buttons); }
    let mut msg = json!({"id": id});
    if kind == "template" { msg["message_status"] = json!("accepted"); }
    Json(json!({"messaging_product": "whatsapp", "contacts": [{"input": to, "wa_id": num}], "messages": [msg]})).into_response()
}

async fn media_info(State(ctx): State<Ctx>, Path((_ver, id)): Path<(String, String)>, h: HeaderMap) -> Response {
    let n = match config::number(&ctx) { Ok(n) => n, Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, 1, &e.message, None) };
    if let Err(r) = auth(&ctx, &n, &h) { return r; }
    let p = ctx.path(&format!("whatsapp/media/{id}.png"));
    let Ok(bytes) = std::fs::read(&p) else { return err(StatusCode::NOT_FOUND, 100, &format!("Unsupported get request. Object with ID '{id}' does not exist"), Some(33)) };
    use sha2::Digest;
    let host = h.get("host").and_then(|v| v.to_str().ok()).unwrap_or("localhost");
    Json(json!({"messaging_product": "whatsapp", "url": format!("http://{host}{}/whatsapp/media/{id}/file", ctx.prefix()), "mime_type": "image/png",
        "sha256": hex::encode(sha2::Sha256::digest(&bytes)), "file_size": bytes.len(), "id": id})).into_response()
}

async fn media_file(State(ctx): State<Ctx>, Path(id): Path<String>, h: HeaderMap) -> Response {
    let n = match config::number(&ctx) { Ok(n) => n, Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, 1, &e.message, None) };
    if let Err(r) = auth(&ctx, &n, &h) { return r; }
    match std::fs::read(ctx.path(&format!("whatsapp/media/{id}.png"))) {
        Ok(b) => ([("content-type", "image/png")], b).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

pub fn meta_template(t: &config::Template, lang: &str) -> Value {
    let mut comps = vec![];
    if !t.header.is_empty() { comps.push(json!({"type": "HEADER", "format": "TEXT", "text": t.header})); }
    comps.push(json!({"type": "BODY", "text": t.body}));
    if !t.footer.is_empty() { comps.push(json!({"type": "FOOTER", "text": t.footer})); }
    if !t.buttons.is_empty() {
        comps.push(json!({"type": "BUTTONS", "buttons": t.buttons.iter().map(|b| if b.url.is_empty() { json!({"type": "QUICK_REPLY", "text": b.text}) } else { json!({"type": "URL", "text": b.text, "url": b.url}) }).collect::<Vec<_>>()}));
    }
    json!({"name": t.name, "language": lang, "status": t.status.to_uppercase(), "category": t.category.to_uppercase(), "id": t.id(), "components": comps,
           "quality_score": {"score": match t.quality.as_str() { "low" => "RED", "medium" => "YELLOW", _ => "GREEN" }}})
}

async fn templates_list(State(ctx): State<Ctx>, Path((_ver, waba)): Path<(String, String)>, Query(q): Query<HashMap<String, String>>, h: HeaderMap) -> Response {
    let n = match config::number(&ctx) { Ok(n) => n, Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, 1, &e.message, None) };
    if let Err(r) = auth(&ctx, &n, &h) { return r; }
    if waba != n.waba_id { return err(StatusCode::BAD_REQUEST, 100, &format!("Unsupported get request. Object with ID '{waba}' does not exist"), Some(33)); }
    let data: Vec<Value> = config::templates(&ctx).unwrap_or_default().iter().filter(|t| q.get("name").map(|x| x == &t.name).unwrap_or(true))
        .flat_map(|t| t.languages.iter().map(move |l| meta_template(t, l))).collect();
    Json(json!({"data": data, "paging": {"cursors": {"before": "", "after": ""}}})).into_response()
}

async fn template_create(State(ctx): State<Ctx>, Path((_ver, waba)): Path<(String, String)>, h: HeaderMap, body: Bytes) -> Response {
    let n = match config::number(&ctx) { Ok(n) => n, Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, 1, &e.message, None) };
    if let Err(r) = auth(&ctx, &n, &h) { return r; }
    if waba != n.waba_id { return err(StatusCode::BAD_REQUEST, 100, &format!("Unsupported post request. Object with ID '{waba}' does not exist"), Some(33)); }
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (name, lang) = (v["name"].as_str().unwrap_or("").to_string(), v["language"].as_str().unwrap_or("en_US").to_string());
    if name.is_empty() { return err(StatusCode::BAD_REQUEST, 100, "(#100) The parameter name is required.", None); }
    let mut all = config::templates(&ctx).unwrap_or_default();
    if all.iter().any(|t| t.name == name && t.languages.contains(&lang)) {
        return err(StatusCode::BAD_REQUEST, 100, &format!("Message template \"{name}\" already exists in {lang}"), Some(2388024));
    }
    let mut t = config::Template { name: name.clone(), languages: vec![lang], status: "pending".into(), category: v["category"].as_str().unwrap_or("utility").to_lowercase(), created_at: Some(util::now_ms()), ..Default::default() };
    for c in v["components"].as_array().cloned().unwrap_or_default() {
        match c["type"].as_str().unwrap_or("").to_uppercase().as_str() {
            "HEADER" => t.header = c["text"].as_str().unwrap_or("").to_string(),
            "BODY" => t.body = c["text"].as_str().unwrap_or("").to_string(),
            "FOOTER" => t.footer = c["text"].as_str().unwrap_or("").to_string(),
            "BUTTONS" => t.buttons = c["buttons"].as_array().cloned().unwrap_or_default().iter().map(|b| config::Button { text: b["text"].as_str().unwrap_or("").into(), url: b["url"].as_str().unwrap_or("").into(),
                quick_reply: if b["type"].as_str().unwrap_or("").eq_ignore_ascii_case("QUICK_REPLY") { b["text"].as_str().unwrap_or("").to_uppercase().replace(' ', "_") } else { String::new() } }).collect(),
            _ => {}
        }
    }
    let id = t.id();
    let cat = t.category.to_uppercase();
    match all.iter_mut().find(|x| x.name == name) {
        Some(x) => x.languages.extend(t.languages),
        None => all.push(t),
    }
    if let Err(e) = config::save_templates(&ctx, &all) { return err(StatusCode::INTERNAL_SERVER_ERROR, 1, &e.message, None); }
    let _ = store::forget_undeclared(&ctx, &name).await;
    ctx.live.send("whatsapp.templates", &name, "created");
    Json(json!({"id": id, "status": "PENDING", "category": cat})).into_response()
}

async fn template_delete(State(ctx): State<Ctx>, Path((_ver, _waba)): Path<(String, String)>, Query(q): Query<HashMap<String, String>>, h: HeaderMap) -> Response {
    let n = match config::number(&ctx) { Ok(n) => n, Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, 1, &e.message, None) };
    if let Err(r) = auth(&ctx, &n, &h) { return r; }
    let name = q.get("name").cloned().unwrap_or_default();
    let mut all = config::templates(&ctx).unwrap_or_default();
    let before = all.len();
    all.retain(|t| t.name != name);
    if all.len() == before { return err(StatusCode::BAD_REQUEST, 100, &format!("Message template \"{name}\" not found"), Some(2593002)); }
    if let Err(e) = config::save_templates(&ctx, &all) { return err(StatusCode::INTERNAL_SERVER_ERROR, 1, &e.message, None); }
    ctx.live.send("whatsapp.templates", &name, "deleted");
    Json(json!({"success": true})).into_response()
}

#[cfg(test)]
mod t {
    #[test]
    fn versions() {
        assert!(super::version_ok("any", "v25.0"));
        assert!(super::version_ok("v18.0-v21.0", "v21.0") && super::version_ok("v18.0-v21.0", "v18.0"));
        assert!(!super::version_ok("v18.0-v21.0", "v25.0") && !super::version_ok("v18.0-v21.0", "latest"));
    }
}
