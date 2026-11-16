//! Webhook inbox tools (H1–H2 "API & MCP" sheets), with the sheets' names. create_inbox and
//! get_inbox are shared with e-mail inboxes: the server sends a call here when it names a webhook
//! inbox (no "@") or carries webhook keys; create_webhook_inbox and get_webhook_inbox stay as explicit names.

use std::time::Duration;

use stuntman_core::{tools::Args, util, ApiError, Ctx, Tool, ToolResult};
use serde_json::{json, Value};

use crate::{config, store};

pub fn url(ctx: &Ctx, name: &str) -> String {
    // Where the app posts: the environment's own domain (no prefix), else localhost and /e/<name>.
    let (host, prefix) = ctx.app_host();
    format!("http://{host}:{}{prefix}/in/{name}", crate::port())
}

fn by(a: &Args) -> String {
    a.str("_by").unwrap_or("you".into())
}

fn since(a: &Args) -> Result<Option<i64>, ApiError> {
    match a.str("since") {
        None => Ok(None),
        Some(s) => util::parse_ms(&s).map(|d| Some(util::now_ms() - d as i64)).ok_or_else(|| ApiError::bad("since: 15m, 1h, 1d")),
    }
}

async fn view(ctx: &Ctx, i: &config::Inbox) -> Value {
    let hour = util::now_ms() - 3_600_000;
    let n = i.name.clone();
    let recent = ctx
        .db
        .call(move |c| c.query_row("SELECT COUNT(*) FROM webhook_deliveries WHERE inbox = ?1 AND at >= ?2", rusqlite::params![n, hour], |r| r.get::<_, i64>(0)))
        .await
        .unwrap_or(0);
    json!({"inbox": i, "url": url(ctx, &i.name), "summary": i.summary(), "kind": if i.kept { "kept" } else { "ephemeral" }, "deliveriesLastHour": recent, "expired": i.expired()})
}

/// Applies behaviour keys from MCP/HTTP (flat, as in the sheets) or a whole `inbox` object.
fn apply(i: &mut config::Inbox, a: &Args) -> Result<(), ApiError> {
    if let Some(v) = a.get("inbox") {
        let keep_name = i.name.clone();
        *i = serde_json::from_value(v.clone()).map_err(|e| ApiError::bad(format!("inbox: {e}")))?;
        if i.name.is_empty() {
            i.name = keep_name;
        }
        return Ok(());
    }
    let an = &mut i.answer;
    if let Some(v) = a.str("mode") {
        an.mode = match v.as_str() {
            "200" | "ok" | "always" => "always_200".into(),
            "202" => "accept_202".into(),
            "fail" => "fail_then".into(),
            m => m.into(),
        };
    }
    if let Some(v) = a.u64("status").or(a.u64("fail")) {
        an.status = v as u16;
    }
    // create_inbox(fail=3, then=200) from the sheet: fail is a count there.
    if let (Some(n), true) = (a.u64("fail"), a.get("then").is_some()) {
        an.mode = "fail_then".into();
        an.times = n as u32;
        an.status = a.u64("status").unwrap_or(500) as u16;
    }
    if let Some(v) = a.u64("times") {
        an.times = v as u32;
    }
    if let Some(v) = a.str("per") {
        an.per = v;
    }
    if let Some(v) = a.u64("share") {
        an.share = v as u32;
    }
    if let Some(v) = a.u64("then") {
        an.then = v as u16;
    }
    if let Some(v) = a.str("delay") {
        i.timing.delay = v;
    }
    if let Some(v) = a.str("hang") {
        i.timing.hang = v;
    }
    if a.get("traps").is_some() {
        i.traps = a.list("traps");
    }
    if let Some(v) = a.get("redirect_to") {
        i.redirect_to = v.as_str().unwrap_or("").to_string();
    }
    let sg = &mut i.signature;
    if let Some(v) = a.str("secret") {
        sg.secret = v;
    }
    if let Some(v) = a.str("previous_secret") {
        sg.previous_secret = v;
    }
    if let Some(v) = a.str("scheme") {
        sg.scheme = v;
    }
    if let Some(v) = a.str("header") {
        sg.header = v;
    }
    if let Some(v) = a.str("tolerance") {
        sg.tolerance = v;
    }
    if let Some(v) = a.bool("reject_bad") {
        sg.reject_bad = v;
    }
    Ok(())
}

fn lifetime(i: &mut config::Inbox, a: &Args) -> Result<(), ApiError> {
    if a.bool("keep") == Some(true) || a.str("expires").as_deref() == Some("never") || a.str("expires").as_deref() == Some("kept") {
        i.kept = true;
        i.expires_at = None;
    } else if let Some(e) = a.str("expires") {
        let ms = util::parse_ms(&e).ok_or_else(|| ApiError::bad("expires: 1h, 1d, or never"))?;
        i.kept = false;
        i.expires_at = Some(util::now_ms() + ms as i64);
    }
    Ok(())
}

pub fn all() -> Vec<Tool> {
    vec![
        Tool::new("list_webhook_inboxes", "Kept and ephemeral webhook inboxes, with how each answers and its URL.", |ctx, _a| async move {
            let mut out = vec![];
            for i in config::all(&ctx).await? {
                out.push(view(&ctx, &i).await);
            }
            Ok(json!({"inboxes": out, "listening": crate::BIND_ERROR.lock().unwrap().clone().map_or(json!(true), |e| json!(e))}))
        })
        .route("GET", "/webhooks/inboxes"),
        Tool::new("get_webhook_inbox", "Behaviour, signature settings, lifetime, URL, and the attempts of its last event.", |ctx, a| async move {
            let i = config::need(&ctx, &a.need("name")?).await?;
            let mut v = view(&ctx, &i).await;
            let (last, _) = store::list(&ctx, Some(i.name.clone()), None, None, 1).await?;
            if let Some(d) = last.first() {
                v["lastEvent"] = store::get(&ctx, d["id"].as_str().unwrap_or("")).await?;
            }
            v["yaml"] = json!(if i.kept { serde_yaml::to_string(&vec![config::Inbox { created_by: String::new(), created_at: None, ..i.clone() }]).unwrap_or_default() } else { serde_yaml::to_string(&i).unwrap_or_default() });
            Ok(v)
        })
        .req("name", "inbox name").route("GET", "/webhooks/inboxes/{name}"),
        Tool::new("create_webhook_inbox", "An inbox the app can send webhooks to; ephemeral by default (expires=\"1h\"), keep=true keeps it with the environment. Behaviour keys as in set_inbox; fail=3, then=200 is a shortcut.", |ctx, a| async move {
            let name = a.str("name").unwrap_or_else(|| format!("tmp-{:04x}", rand::random::<u16>()));
            config::valid_name(&name).map_err(ApiError::bad)?;
            if config::get(&ctx, &name).await?.is_some() {
                return Err(ApiError::new(409, format!("An inbox “{name}” already exists")));
            }
            let mut i = config::Inbox::new(&name);
            i.created_by = by(&a);
            i.created_at = Some(util::now_ms());
            i.expires_at = Some(util::now_ms() + 3_600_000);
            lifetime(&mut i, &a)?;
            apply(&mut i, &a)?;
            i.name = name.clone();
            config::put(&ctx, &i, None).await?;
            let _ = ctx.db.call({ let n = name.clone(); move |c| c.execute("DELETE FROM webhook_gone WHERE name = ?1", [n]) }).await;
            Ok(json!({"name": name, "url": url(&ctx, &name), "expiresAt": i.expires_at, "inbox": i}))
        })
        .param("name", "lowercase, part of the URL; random when left out").param("expires", "1h (default), 1d, or never").param("keep", "true: kept with the environment")
        .param("mode", "always_200 | accept_202 | error | fail_then | fail_share | hang | reset | empty").param("status", "error status, default 500")
        .param("fail", "shortcut: fail this many times (with then=)").param("then", "success status after failing")
        .param("traps", "redirect_private, huge, slow").param("secret", "shared secret to check signatures").route("POST", "/webhooks/inboxes"),
        Tool::new("set_inbox", "How the inbox answers: mode, status, times, per, share, then, delay, hang, traps, secret, previous_secret, scheme, header, tolerance, reject_bad, expires; or a whole inbox object.", |ctx, a| async move {
            let name = a.need("name")?;
            let mut i = config::need(&ctx, &name).await?;
            let before = i.clone();
            apply(&mut i, &a)?;
            lifetime(&mut i, &a)?;
            if let Some(n) = a.str("new_name") {
                i.name = n;
            }
            if i.name != name && config::get(&ctx, &i.name).await?.is_some() {
                return Err(ApiError::new(409, format!("An inbox “{}” already exists", i.name)));
            }
            if i.kept && i.created_at.is_none() {
                i.created_at = before.created_at;
            }
            config::put(&ctx, &i, Some(&name)).await?;
            if i.answer != before.answer {
                store::reset_counts(&ctx, &i.name).await?;
            }
            Ok(view(&ctx, &i).await)
        })
        .req("name", "inbox").param("mode", "see create_webhook_inbox").param("status", "error status").param("times", "fail_then count").param("per", "event_id | inbox")
        .param("share", "fail_share percent").param("then", "success status").param("delay", "none or 2s").param("hang", "never or 35s")
        .param("traps", "list: redirect_private, huge, slow").param("redirect_to", "redirect trap target; empty = observable loopback, or e.g. http://10.0.0.5/admin").param("secret", "shared secret, or env:NAME").param("previous_secret", "secret before rotation")
        .param("scheme", "t-v1-hmac-sha256 | hmac-sha256-hex | hmac-sha256-base64 | none").param("header", "signature header name").param("tolerance", "5m")
        .param("reject_bad", "answer 401 to bad signatures").param("expires", "1h, 1d, never").param("new_name", "rename").param("inbox", "whole object instead of keys")
        .route("PUT", "/webhooks/inboxes/{name}").route("PUT", "/inboxes/{name}"),
        Tool::new("remove_inbox", "Removes the inbox; later deliveries get 410 Gone. Its history is kept for 24 h.", |ctx, a| async move {
            let name = a.need("name")?;
            if !config::remove(&ctx, &name).await? {
                return Err(ApiError::not_found(format!("No webhook inbox “{name}”")));
            }
            Ok(json!({"ok": true}))
        })
        .req("name", "inbox").route("DELETE", "/webhooks/inboxes/{name}"),
        Tool::new("list_deliveries", "Event, attempt, signature result, answer and trap results, newest first.", |ctx, a| async move {
            let (v, total) = store::list(&ctx, a.str("inbox").or(a.str("name")), a.str("event"), since(&a)?, a.u64("limit").unwrap_or(200) as u32).await?;
            Ok(json!({"deliveries": v, "total": total}))
        })
        .param("inbox", "inbox name").param("event", "event type or id").param("since", "15m, 1h, 1d").param("limit", "default 200")
        .route("GET", "/webhooks/deliveries").route("GET", "/webhooks/inboxes/{name}/deliveries").route("GET", "/inboxes/{name}/deliveries"),
        Tool::new("get_delivery", "Headers, body, checks, and all attempts of the same event.", |ctx, a| async move {
            store::get(&ctx, &a.need("id")?).await
        })
        .req("id", "delivery id, e.g. dlv_0042").route("GET", "/webhooks/deliveries/{id}").route("GET", "/inboxes/deliveries/{id}"),
        Tool::new("wait_for_delivery", "Blocks until the inbox has answered a delivery (of this event type), then returns it with all its attempts. Looks back 15 s by default.", |ctx, a| async move {
            let inbox = a.need("inbox")?;
            let timeout = a.ms("timeout")?.unwrap_or(60_000).min(600_000);
            let started = util::now_ms();
            let from = since(&a)?.unwrap_or(started - 15_000);
            let mut sub = ctx.live.subscribe();
            loop {
                let (v, _) = store::list(&ctx, Some(inbox.clone()), a.str("event"), Some(from), 50).await?;
                if let Some(d) = v.iter().find(|d| d["done"] == true) {
                    return wait_more(&ctx, d["id"].as_str().unwrap_or("")).await;
                }
                let left = started + timeout as i64 - util::now_ms();
                if left <= 0 {
                    return Err(ApiError::new(408, format!("No delivery to {inbox} in {}", util::fmt_ms(timeout))).with(json!({"pending": v})));
                }
                let _ = tokio::time::timeout(Duration::from_millis(left.min(1000) as u64), sub.recv()).await;
            }
        })
        .req("inbox", "inbox name").param("event", "event type or id").param("timeout", "default 60s").param("since", "how far back counts, default 15s")
        .route("GET", "/webhooks/deliveries/wait").route("GET", "/inboxes/deliveries/wait"),
        Tool::new("put_webhooks_yaml", "Replaces webhooks/inboxes.yaml (the kept inboxes).", |ctx, a| async move {
            let v: Vec<config::Inbox> = serde_yaml::from_str(&a.need("yaml")?).map_err(|e| ApiError::bad(format!("YAML: {e}")))?;
            for i in &v {
                i.validate().map_err(|e| ApiError::bad(format!("{}: {e}", i.name)))?;
            }
            config::save_kept(&ctx, &v)?;
            ctx.live.send("webhook.inbox", "*", "updated");
            Ok(json!({"ok": true, "inboxes": v.len()}))
        })
        .req("yaml", "the whole file").route("PUT", "/webhooks/inboxes.yaml"),
    ]
}

/// A delivery whose trap is still pending (redirect not followed yet) is returned as it is.
async fn wait_more(ctx: &Ctx, id: &str) -> ToolResult {
    store::get(ctx, id).await
}
