//! A14: what Microsoft Teams does around a bot. The Bot Connector's OpenID metadata and keys (the
//! app checks the JWT on each activity with them), the activities Teams sends to the bot's messaging
//! endpoint — signed, with the serviceurl claim — and a connector (the serviceUrl) where the bot
//! posts its replies and cards, kept to read. The bot's own token comes from any provider's token
//! endpoint under the tenant botframework.com (client credentials).

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx, Tool};

use crate::{store, token};

const ISSUER: &str = "https://api.botframework.com";
const KID: &str = "stuntman-bf";

pub fn base(ctx: &Ctx) -> String {
    format!("http://localhost:{}{}/botframework", std::env::var("STUNTMAN_PORT").unwrap_or("8025".into()), ctx.prefix())
}

async fn key(ctx: &Ctx) -> Result<String, ApiError> {
    if let Some(k) = store::key(ctx, "__botframework", KID).await? {
        return Ok(k);
    }
    let pem = tokio::task::spawn_blocking(|| stuntman_keyvault::crypto::generate(2048)).await.map_err(|e| ApiError::internal(e.to_string()))?.map_err(ApiError::internal)?;
    store::add_key(ctx, "__botframework", KID, &pem).await?;
    Ok(pem)
}

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route("/botframework/v1/.well-known/openidconfiguration", get(|State(ctx): State<Ctx>| async move {
            let b = base(&ctx);
            Json(json!({"issuer": ISSUER, "authorization_endpoint": "https://invalid.botframework.com", "jwks_uri": format!("{b}/v1/.well-known/keys"),
                "id_token_signing_alg_values_supported": ["RS256"], "token_endpoint_auth_methods_supported": ["private_key_jwt"]}))
        }))
        .route("/botframework/v1/.well-known/keys", get(|State(ctx): State<Ctx>| async move {
            match key(&ctx).await.and_then(|pem| stuntman_keyvault::crypto::jwk(&pem, KID, &[]).map_err(ApiError::internal)) {
                Ok(mut k) => {
                    k["use"] = json!("sig");
                    k["endorsements"] = json!(["msteams"]);
                    Json(json!({"keys": [k]})).into_response()
                }
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.message).into_response(),
            }
        }))
        .route("/botframework/connector/v3/conversations", post(create))
        .route("/botframework/connector/v3/conversations/{id}/activities", post(activity))
        .route("/botframework/connector/v3/conversations/{id}/activities/{aid}", post(activity_reply).put(activity_reply))
        .route("/botframework/connector/v3/conversations/{id}/members", get(|Path(id): Path<String>| async move {
            Json(json!([{"id": "29:stuntman-user", "name": "Synthetic user", "aadObjectId": crate::config::guid(&id)}]))
        }))
}

async fn keep(ctx: &Ctx, dir: &str, conversation: &str, activity: &Value, status: i64, bot: &str) {
    let (d, c, a, b) = (dir.to_string(), conversation.to_string(), activity.to_string(), bot.to_string());
    let _ = ctx.db.call(move |cn| cn.execute("INSERT INTO id_bot(at, dir, conversation, activity, status, bot) VALUES(?1,?2,?3,?4,?5,?6)", rusqlite::params![util::now_ms(), d, c, a, status, b]).map(|_| ())).await;
    ctx.live.send("identity.bot", conversation, "row");
}

fn authed(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()).map(|v| v.starts_with("Bearer ") && v.len() > 20).unwrap_or(false)
}

async fn create(State(ctx): State<Ctx>, h: HeaderMap, Json(b): Json<Value>) -> Response {
    if !authed(&h) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": {"code": "Unauthorized", "message": "Authorization has been denied for this request."}}))).into_response();
    }
    let id = format!("a:{}", util::uuid().replace('-', ""));
    if let Some(a) = b.get("activity") {
        keep(&ctx, "from bot", &id, a, 201, b["bot"]["id"].as_str().unwrap_or("")).await;
    }
    (StatusCode::CREATED, Json(json!({"id": id, "activityId": format!("1:{}", util::now_ms())}))).into_response()
}

async fn activity(State(ctx): State<Ctx>, h: HeaderMap, Path(id): Path<String>, Json(a): Json<Value>) -> Response {
    if !authed(&h) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": {"code": "Unauthorized", "message": "Authorization has been denied for this request."}}))).into_response();
    }
    keep(&ctx, "from bot", &id, &a, 201, a["from"]["id"].as_str().unwrap_or("")).await;
    (StatusCode::CREATED, Json(json!({"id": format!("1:{}", util::now_ms())}))).into_response()
}

async fn activity_reply(State(ctx): State<Ctx>, h: HeaderMap, Path((id, _aid)): Path<(String, String)>, Json(a): Json<Value>) -> Response {
    activity(State(ctx), h, Path(id), Json(a)).await
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("send_bot_activity", "Sends the app's bot an activity as Microsoft Teams would: a message (text) or a card action (value), signed by the Bot Connector (JWT with aud = the bot's app id and the serviceurl claim). twist: bad_signature | expired | wrong_audience | no_token. Returns the bot's answer; its replies land in list_bot_messages.", |ctx, a| async move {
            let endpoint = a.need("endpoint")?;
            let bot = a.need("bot_app_id")?;
            let conv = a.str("conversation").unwrap_or_else(|| format!("a:stuntman-{}", &crate::config::guid(&bot)[..8]));
            let service_url = a.str("service_url").unwrap_or_else(|| format!("{}/connector/", base(&ctx)));
            let from = a.str("from").unwrap_or("29:stuntman-user".into());
            let mut act = a.get("activity").cloned().unwrap_or(json!({}));
            if act.get("type").is_none() {
                act["type"] = json!("message");
            }
            if let Some(t) = a.str("text") {
                act["text"] = json!(t);
            }
            if let Some(v) = a.get("value") {
                act["value"] = v.clone();
            }
            let now = token::now_s();
            for (k, v) in [("id", json!(format!("1:{}", util::now_ms()))), ("timestamp", json!(stuntman_apis::expr::iso(util::now_ms(), None))), ("serviceUrl", json!(service_url)), ("channelId", json!("msteams")),
                ("from", json!({"id": from, "name": a.str("from_name").unwrap_or("Synthetic user".into()), "aadObjectId": crate::config::guid(&from)})), ("conversation", json!({"id": conv, "conversationType": "personal", "tenantId": a.str("tenant_id").unwrap_or_default()})),
                ("recipient", json!({"id": format!("28:{bot}"), "name": "bot"}))] {
                if act.get(k).is_none() {
                    act[k] = v;
                }
            }
            let twist = a.str("twist").unwrap_or_default();
            let claims = json!({"iss": ISSUER, "aud": if twist == "wrong_audience" { "00000000-0000-0000-0000-000000000000".to_string() } else { bot.clone() }, "serviceurl": service_url,
                "nbf": now - 300, "iat": now - 300, "exp": if twist == "expired" { now - 60 } else { now + 3600 }});
            let pem = if twist == "bad_signature" { stuntman_keyvault::crypto::generate(2048).map_err(ApiError::internal)? } else { key(&ctx).await? };
            let jwt = token::sign(&pem, KID, &claims)?;
            let mut req = ctx.http.post(&endpoint).json(&act);
            if twist != "no_token" {
                req = req.header("authorization", format!("Bearer {jwt}"));
            }
            let t0 = std::time::Instant::now();
            let r = req.send().await;
            let (status, body) = match r {
                Ok(x) => (x.status().as_u16() as i64, x.text().await.unwrap_or_default()),
                Err(e) => (0, if e.is_connect() { "connection refused".into() } else { e.to_string() }),
            };
            keep(&ctx, "to bot", &conv, &act, status, &bot).await;
            Ok(json!({"status": status, "body": body, "ms": t0.elapsed().as_millis() as i64, "activity": act, "twist": twist}))
        })
        .req("endpoint", "the bot's messaging endpoint, e.g. http://localhost:5000/api/v1/teams/bot/messages").req("bot_app_id", "the bot's Microsoft app id (the JWT audience)")
        .param("text", "a message").param("value", "a card action's data").param("activity", "the whole activity").param("conversation", "conversation id").param("from", "the user id").param("from_name", "shown name")
        .param("tenant_id", "tid").param("service_url", "where the bot replies (default: Stuntman's connector)").param("twist", "bad_signature | expired | wrong_audience | no_token")
        .route("POST", "/identity/bot/activities"),
        Tool::new("list_bot_messages", "What went between Teams (Stuntman) and the app's bot: activities sent to the bot and the bot's replies and cards to the connector.", |ctx, a| async move {
            let c = a.str("conversation");
            let rows = ctx.db.call(move |cn| {
                let mut st = cn.prepare("SELECT at, dir, conversation, activity, status, bot FROM id_bot WHERE (?1 IS NULL OR conversation=?1) ORDER BY seq DESC LIMIT 200")?;
                let v: Vec<Value> = st.query_map([c], |r| Ok(json!({"at": r.get::<_, i64>(0)?, "dir": r.get::<_, String>(1)?, "conversation": r.get::<_, String>(2)?,
                    "activity": serde_json::from_str::<Value>(&r.get::<_, String>(3)?).unwrap_or(Value::Null), "status": r.get::<_, i64>(4)?, "bot": r.get::<_, String>(5)?})))?.filter_map(|x| x.ok()).collect();
                Ok(v)
            }).await?;
            Ok(json!({"messages": rows, "connector": format!("{}/connector/", base(&ctx)), "openid": format!("{}/v1/.well-known/openidconfiguration", base(&ctx))}))
        })
        .param("conversation", "only one conversation")
        .route("GET", "/identity/bot/messages"),
    ]
}
