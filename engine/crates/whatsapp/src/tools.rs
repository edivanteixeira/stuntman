//! W1–W4 as tools: each is an MCP tool and an HTTP route under /api/whatsapp (same names as the
//! screens' "what each control calls").

use std::time::Duration;

use stuntman_core::{tools::Args, util, ApiError, Tool};
use serde_json::{json, Value};

use crate::{config, flow, store};

fn since(a: &Args, d: &str) -> Result<i64, ApiError> {
    let s = a.str("since").unwrap_or(d.into());
    let ms = util::parse_ms(&s).ok_or_else(|| ApiError::bad("since: 15m, 1h, 1d"))?;
    Ok(util::now_ms() - ms as i64)
}

fn steps_of(v: &Value) -> Result<Vec<config::Step>, ApiError> {
    serde_json::from_value(v.clone()).map_err(|e| ApiError::bad(format!("steps: {e}")))
}

fn check_number(n: &str) -> Result<(), ApiError> {
    if n == "*" || (n.starts_with('+') && config::digits(n).len() >= 4) { Ok(()) } else { Err(ApiError::bad(format!("Use +{} (with country code), a prefix ending in *, or * for everyone else", config::digits(n)))) }
}

pub fn all() -> Vec<Tool> {
    vec![
        // ---------------------------------------------------------------- W1 conversations
        Tool::new("list_whatsapp_messages", "WhatsApp messages both ways (the app's sends and the person's replies), newest first, with status history and the window.", |ctx, a| async move {
            let f = store::Filter { number: a.str("number"), status: a.str("status"), template: a.str("template"), dir: a.str("dir"), text: a.str("text"), q: a.str("q"),
                since_ms: Some(since(&a, "1d")?), limit: a.u64("limit").unwrap_or(200) as u32 };
            let msgs = store::list(&ctx, f).await?;
            Ok(json!({"messages": msgs}))
        }).param("number", "+14155550142").param("status", "accepted, sent, delivered, read, failed, received").param("template", "template name").param("dir", "out (the app) | in (the person)")
          .param("q", "number, text, wamid").param("since", "15m, 1h, 1d (default)").param("limit", "default 200").route("GET", "/whatsapp/messages"),
        Tool::new("list_whatsapp_conversations", "One row per number: the last message, the 24 h window and the person's name.", |ctx, a| async move {
            Ok(json!({"conversations": store::conversations(&ctx, a.str("q")).await?}))
        }).param("q", "number, text, wamid").route("GET", "/whatsapp/conversations"),
        Tool::new("get_whatsapp_message", "One message with its status history (and what the app's webhook answered to each).", |ctx, a| async move {
            let m = store::get(&ctx, &a.need("wamid")?).await?;
            let w = flow::window_json(store::last_inbound(&ctx, m["number"].as_str().unwrap_or("")).await?);
            Ok(json!({"message": m, "window": w}))
        }).req("wamid", "message id").route("GET", "/whatsapp/messages/{wamid}"),
        Tool::new("wait_for_whatsapp", "Waits for the app to send a WhatsApp message to a number (optionally a template or a text containing…); returns it.", |ctx, a| async move {
            let number = a.need("number")?;
            let timeout = a.ms("timeout")?.unwrap_or(30_000);
            let from = since(&a, "60s")?;
            let started = util::now_ms();
            loop {
                let f = store::Filter { number: Some(number.clone()), dir: Some("out".into()), template: a.str("template"), text: a.str("text"), since_ms: Some(from), limit: 50, ..Default::default() };
                if let Some(m) = store::list(&ctx, f).await?.into_iter().filter(|m| m["status"] != "refused").last() {
                    return Ok(json!({"message": m, "wamid": m["id"]}));
                }
                if util::now_ms() - started > timeout as i64 {
                    return Err(ApiError::new(408, format!("No WhatsApp message to {number} in {}", util::fmt_ms(timeout))));
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }).req("number", "+14155550142").param("template", "template name").param("text", "part of the text").param("timeout", "default 30s").param("since", "how far back counts (default 60s)")
          .route("GET", "/whatsapp/wait"),
        Tool::new("reply_as_user", "The person writes back: a text, a tap on a template button, an image or a location. Stuntman sends Meta's messages webhook to the app; a reply opens the 24 h window.", |ctx, a| async move {
            let number = a.need("number")?;
            let r = if let Some(t) = a.str("text") {
                flow::Reply::Text(t)
            } else if let Some(b) = a.str("button") {
                // The last template to this number that had this button.
                let msgs = store::list(&ctx, store::Filter { number: Some(number.clone()), dir: Some("out".into()), template: None, limit: 50, ..Default::default() }).await?;
                let tpls = config::templates(&ctx)?;
                let found = msgs.iter().find_map(|m| tpls.iter().find(|t| t.name == m["template"].as_str().unwrap_or("")).and_then(|t| t.buttons.iter().find(|x| x.text.eq_ignore_ascii_case(&b) && !x.quick_reply.is_empty())).map(|x| (x.clone(), m["id"].as_str().unwrap_or("").to_string())));
                let (btn, ctxid) = found.ok_or_else(|| ApiError::bad(format!("No template sent to {number} has a quick-reply button “{b}”")))?;
                flow::Reply::Button { text: btn.text, payload: btn.quick_reply, context: ctxid }
            } else if a.get("image").is_some() {
                flow::Reply::Image { caption: a.str("image").filter(|s| s != "true").unwrap_or_default() }
            } else if let Some(l) = a.get("location") {
                let (lat, lng, name) = match l {
                    Value::Object(o) => (o.get("latitude").and_then(|x| x.as_f64()).unwrap_or(40.7484), o.get("longitude").and_then(|x| x.as_f64()).unwrap_or(-73.9857), o.get("name").and_then(|x| x.as_str()).unwrap_or("Home").to_string()),
                    _ => (40.7484, -73.9857, "Home".to_string()),
                };
                flow::Reply::Location { latitude: lat, longitude: lng, name }
            } else {
                return Err(ApiError::bad("Give text, button, image or location"));
            };
            let m = flow::reply(&ctx, &number, r, &a.str("by").unwrap_or("you".into())).await?;
            Ok(json!({"message": m}))
        }).req("number", "+14155550142").param("text", "what the person writes").param("button", "the button's text, e.g. Talk to us").param("image", "caption (or true)")
          .param("location", "{latitude, longitude, name}").param("by", "who did it (shown on screen)").route("POST", "/whatsapp/numbers/{number}/reply"),
        Tool::new("set_message_status", "Moves a message to sent, delivered, read or failed (with a Meta error code); the status webhook goes to the app.", |ctx, a| async move {
            let st = a.need("status")?;
            if !["sent", "delivered", "read", "failed"].contains(&st.as_str()) { return Err(ApiError::bad("status: sent, delivered, read or failed")); }
            let code = if st == "failed" { Some(a.u64("error_code").unwrap_or(131000) as u32) } else { None };
            let m = flow::status(&ctx, &a.need("wamid")?, &st, code, &a.str("by").unwrap_or("you".into())).await?;
            Ok(json!({"message": m}))
        }).req("wamid", "message id").req("status", "sent | delivered | read | failed").param("error_code", "131026, 131047, 131049, 131051, 130429, 131000").param("by", "who")
          .route("POST", "/whatsapp/messages/{wamid}/status"),
        Tool::new("resend_status", "Sends the same status webhook again, to test that the app ignores duplicates.", |ctx, a| async move {
            let id = a.need("wamid")?;
            let m = store::get(&ctx, &id).await?;
            let st = a.str("status").unwrap_or(m["status"].as_str().unwrap_or("delivered").to_string());
            let n = config::number(&ctx)?;
            let code = m["error"].as_u64().map(|c| c as u32).filter(|_| st == "failed");
            flow::deliver(&ctx, "status", &id, &st, flow::status_body(&n, &m, &st, code));
            Ok(json!({"resent": st, "wamid": id}))
        }).req("wamid", "message id").param("status", "default the current one").route("POST", "/whatsapp/messages/{wamid}/resend"),
        Tool::new("get_window", "The 24 h customer service window of a number: open or closed and until when (outside it only templates are delivered; texts fail with 131047).", |ctx, a| async move {
            let n = a.need("number")?;
            Ok(json!({"number": config::digits(&n), "window": flow::window_json(store::last_inbound(&ctx, &n).await?)}))
        }).req("number", "+14155550142").route("GET", "/whatsapp/numbers/{number}/window"),
        Tool::new("list_whatsapp_webhooks", "The webhooks Stuntman sent to the app (statuses, messages, template and quality updates) with the app's answers.", |ctx, a| async move {
            Ok(json!({"webhooks": store::hooks(&ctx, a.u64("limit").unwrap_or(100) as u32).await?}))
        }).param("limit", "default 100").route("GET", "/whatsapp/webhooks"),
        Tool::new("retry_whatsapp_webhooks", "Sends again the webhooks the app did not take (non-2xx or no answer) in the last hour, the newest per message.", |ctx, _a| async move {
            let since = util::now_ms() - 3_600_000;
            let rows: Vec<(String, String, String)> = ctx.db.call(move |c| c.prepare("SELECT kind, wamid, body FROM wa_hooks WHERE at>=?1 AND (status IS NULL OR status<200 OR status>=300) AND kind NOT LIKE 'bad:%' ORDER BY id DESC")?
                .query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect()).await?;
            let mut seen = std::collections::HashSet::new();
            let mut n = 0;
            for (kind, wamid, body) in rows {
                if !seen.insert(format!("{kind}|{wamid}|{}", body.len())) { continue; }
                let st = serde_json::from_str::<Value>(&body).ok().and_then(|b| b["entry"][0]["changes"][0]["value"]["statuses"][0]["status"].as_str().map(String::from)).unwrap_or_default();
                flow::deliver(&ctx, &kind, &wamid, &st, serde_json::from_str(&body).unwrap_or(Value::Null));
                n += 1;
            }
            Ok(json!({"retried": n}))
        }).route("POST", "/whatsapp/webhooks/retry"),
        // ---------------------------------------------------------------- W2 templates
        Tool::new("list_templates", "WhatsApp templates (whatsapp/templates.yaml): languages, status, quality, category and today's use.", |ctx, a| async move {
            let mut out = vec![];
            for t in config::templates(&ctx)? {
                if a.str("status").map(|s| s != t.status).unwrap_or(false) || a.str("category").map(|c| c != t.category).unwrap_or(false) { continue; }
                let u = store::usage(&ctx, &t.name, util::now_ms() - 86_400_000).await?;
                out.push(json!({"template": t, "id": t.id(), "params": t.body_params(), "usage": u}));
            }
            Ok(json!({"templates": out, "undeclared": store::list_undeclared(&ctx).await?}))
        }).param("status", "approved, pending, rejected, paused, disabled").param("category", "utility, marketing, authentication").route("GET", "/whatsapp/templates"),
        Tool::new("set_template_status", "Approved, pending, rejected (with a reason), paused or disabled; Stuntman sends message_template_status_update to the app and sends follow the new status.", |ctx, a| async move {
            let (name, st) = (a.need("name")?, a.need("status")?.to_lowercase());
            if !["approved", "pending", "rejected", "paused", "disabled"].contains(&st.as_str()) { return Err(ApiError::bad("status: approved, pending, rejected, paused or disabled")); }
            let mut all = config::templates(&ctx)?;
            let t = all.iter_mut().find(|t| t.name == name).ok_or_else(|| ApiError::not_found(format!("No template {name}")))?;
            t.status = st.clone();
            t.reason = if st == "rejected" { a.str("reason").unwrap_or("INVALID_FORMAT".into()) } else { String::new() };
            if st == "approved" { t.created_at = None; if t.quality == "low" { t.quality = "medium".into(); } }
            let t2 = t.clone();
            config::save_templates(&ctx, &all)?;
            flow::template_event(&ctx, &t2, &st, &t2.reason);
            ctx.live.send("whatsapp.templates", &name, &st);
            Ok(json!({"template": t2}))
        }).req("name", "template name").req("status", "approved | pending | rejected | paused | disabled").param("reason", "for rejected: INVALID_FORMAT, ABUSIVE_CONTENT, TAG_CONTENT_MISMATCH…")
          .route("PUT", "/whatsapp/templates/{name}/status"),
        Tool::new("update_template", "Changes a template's category (sends template_category_update), quality (low pauses it, as Meta does) or text.", |ctx, a| async move {
            let name = a.need("name")?;
            let mut all = config::templates(&ctx)?;
            let t = all.iter_mut().find(|t| t.name == name).ok_or_else(|| ApiError::not_found(format!("No template {name}")))?;
            let old_cat = t.category.clone();
            if let Some(c) = a.str("category") { t.category = c.to_lowercase(); }
            if let Some(b) = a.str("body") { t.body = b; }
            if let Some(h) = a.str("header") { t.header = h; }
            if let Some(f) = a.str("footer") { t.footer = f; }
            if let Some(v) = a.get("variables") { t.variables = serde_json::from_value(v.clone()).unwrap_or_default(); }
            if let Some(b) = a.get("buttons") { t.buttons = serde_json::from_value(b.clone()).map_err(|e| ApiError::bad(format!("buttons: {e}")))?; }
            if let Some(l) = a.get("languages") { t.languages = serde_json::from_value(l.clone()).map_err(|e| ApiError::bad(format!("languages: {e}")))?; }
            let mut paused = false;
            if let Some(q) = a.str("quality") {
                t.quality = q.to_lowercase();
                if t.quality == "low" && t.status == "approved" { t.status = "paused".into(); paused = true; }
            }
            let t2 = t.clone();
            config::save_templates(&ctx, &all)?;
            if t2.category != old_cat { flow::category_event(&ctx, &t2, &old_cat); }
            if paused { flow::template_event(&ctx, &t2, "paused", "LOW_QUALITY"); }
            ctx.live.send("whatsapp.templates", &name, "updated");
            Ok(json!({"template": t2}))
        }).req("name", "template name").param("category", "utility | marketing | authentication").param("quality", "high | medium | low").param("body", "text with {{1}}…")
          .param("header", "text").param("footer", "text").param("buttons", "[{text, url} | {text, quick_reply}]").param("languages", "[en_US, es_MX]").param("variables", "what each {{n}} means")
          .route("PATCH", "/whatsapp/templates/{name}"),
        Tool::new("add_template", "Declares a template (writes whatsapp/templates.yaml).", |ctx, a| async move {
            let name = a.need("name")?;
            if !regex::Regex::new(r"^[a-z0-9_]{1,512}$").unwrap().is_match(&name) { return Err(ApiError::bad("name: lowercase_with_underscores")); }
            let lang = a.str("language").unwrap_or("en_US".into());
            let mut all = config::templates(&ctx)?;
            if all.iter().any(|t| t.name == name && t.languages.contains(&lang)) { return Err(ApiError::new(409, format!("{name} already exists in {lang}"))); }
            let t = config::Template { name: name.clone(), languages: vec![lang], body: a.need("body")?, category: a.str("category").unwrap_or("utility".into()).to_lowercase(),
                status: a.str("status").unwrap_or("approved".into()).to_lowercase(), header: a.str("header").unwrap_or_default(), footer: a.str("footer").unwrap_or_default(),
                buttons: a.get("buttons").map(|b| serde_json::from_value(b.clone())).transpose().map_err(|e| ApiError::bad(format!("buttons: {e}")))?.unwrap_or_default(), ..Default::default() };
            match all.iter_mut().find(|x| x.name == name) {
                Some(x) => x.languages.extend(t.languages.clone()),
                None => all.push(t.clone()),
            }
            config::save_templates(&ctx, &all)?;
            store::forget_undeclared(&ctx, &name).await?;
            ctx.live.send("whatsapp.templates", &name, "added");
            Ok(json!({"template": t}))
        }).req("name", "lowercase_with_underscores").req("body", "text with {{1}}, {{2}}…").param("language", "default en_US").param("category", "utility (default) | marketing | authentication")
          .param("status", "approved (default) | pending").param("header", "text").param("footer", "text").param("buttons", "[{text, url} | {text, quick_reply}]").route("POST", "/whatsapp/templates"),
        Tool::new("remove_template", "Deletes a template from whatsapp/templates.yaml (sends with it then fail 132001).", |ctx, a| async move {
            let name = a.need("name")?;
            let mut all = config::templates(&ctx)?;
            let n = all.len();
            all.retain(|t| t.name != name);
            if all.len() == n { return Err(ApiError::not_found(format!("No template {name}"))); }
            config::save_templates(&ctx, &all)?;
            ctx.live.send("whatsapp.templates", &name, "removed");
            Ok(json!({"removed": name}))
        }).req("name", "template name").route("DELETE", "/whatsapp/templates/{name}"),
        Tool::new("template_usage", "Sends, failures by error code, reads and buttons tapped for a template, plus the values the app sent last.", |ctx, a| async move {
            let name = a.need("name")?;
            Ok(json!({"name": name, "usage": store::usage(&ctx, &name, since(&a, "1d")?).await?}))
        }).req("name", "template name").param("since", "default 1d").route("GET", "/whatsapp/templates/{name}/usage"),
        Tool::new("undeclared_templates", "Templates the app asked for that are not declared; Stuntman answered them 132001.", |ctx, _a| async move {
            Ok(json!({"undeclared": store::list_undeclared(&ctx).await?}))
        }).route("GET", "/whatsapp/templates/undeclared"),
        // ---------------------------------------------------------------- W3 recipients
        Tool::new("list_recipients", "The simulated people behind numbers, in match order (the first match wins; * is everyone else), with their steps.", |ctx, _a| async move {
            let r = config::recipients(&ctx)?;
            let mut out = vec![];
            for x in r {
                let last = if x.number.contains('*') { vec![] } else { store::list(&ctx, store::Filter { number: Some(x.number.clone()), limit: 5, ..Default::default() }).await? };
                out.push(json!({"recipient": x, "last": last}));
            }
            Ok(json!({"recipients": out}))
        }).route("GET", "/whatsapp/recipients"),
        Tool::new("set_recipient", "Creates or replaces a recipient (whatsapp/recipients.yaml): whether the number has WhatsApp, the profile name and the steps each message goes through.", |ctx, a| async move {
            let number = a.need("number")?;
            check_number(&number)?;
            let mut all = config::recipients(&ctx)?;
            let mut r = all.iter().find(|x| x.number == number).cloned().unwrap_or(config::Recipient { number: number.clone(), ..Default::default() });
            if let Some(w) = a.bool("whatsapp") { r.whatsapp = w; }
            if let Some(p) = a.str("profile_name") { r.profile_name = p; }
            if let Some(s) = a.get("steps") { r.steps = steps_of(s)?; }
            match all.iter().position(|x| x.number == number) {
                Some(i) => all[i] = r.clone(),
                None => {
                    let at = a.u64("position").map(|p| p as usize).unwrap_or_else(|| all.iter().position(|x| x.number == "*").unwrap_or(all.len()));
                    all.insert(at.min(all.len()), r.clone());
                }
            }
            config::save_recipients(&ctx, &all)?;
            ctx.live.send("whatsapp.recipients", &number, "set");
            Ok(json!({"recipient": r}))
        }).req("number", "+14155550142, a prefix like +1312555 01*, or *").param("whatsapp", "false: sends fail with 131026").param("profile_name", "sent as contacts.profile.name")
          .param("steps", "[{delivered_after: 1s}, {read_after: 90s}, {never_read: true}, {stay_at_sent: true}, {fail: 131049}, {on_buttons: {tap, after}}, {on_text: {reply, after}}, {send_image: {caption, after}}, {send_location: {latitude, longitude, name, after}}, {block_after: 2}]")
          .param("position", "where in the match order (default before *)").route("PUT", "/whatsapp/recipients/{number}"),
        Tool::new("remove_recipient", "Removes a recipient; the number falls back to the next match or “Everyone else”.", |ctx, a| async move {
            let number = a.need("number")?;
            if number == "*" { return Err(ApiError::bad("“Everyone else” stays; change its steps instead")); }
            let mut all = config::recipients(&ctx)?;
            let n = all.len();
            all.retain(|x| x.number != number);
            if all.len() == n { return Err(ApiError::not_found(format!("No recipient {number}"))); }
            config::save_recipients(&ctx, &all)?;
            ctx.live.send("whatsapp.recipients", &number, "removed");
            Ok(json!({"removed": number}))
        }).req("number", "as listed").route("DELETE", "/whatsapp/recipients/{number}"),
        Tool::new("explain_number", "Which recipient matches a number, which others also match, and what will happen to the next message.", |ctx, a| async move {
            let number = a.need("number")?;
            let all = config::recipients(&ctx)?;
            let m: Vec<&config::Recipient> = all.iter().filter(|x| x.matches(&number)).collect();
            let r = config::recipient_for(&ctx, &number);
            let next = if !r.whatsapp { "fails with 131026 (no WhatsApp)".to_string() } else if let Some(c) = r.fail_code() { format!("sent, then fails with {c}") } else {
                format!("sent, {}, {}", r.delivered_ms().map(|d| format!("delivered after {}", util::fmt_ms(d))).unwrap_or("stays at sent".into()), r.read_ms().map(|d| format!("read after {}", util::fmt_ms(d))).unwrap_or("never read".into())) };
            Ok(json!({"number": config::digits(&number), "match": r, "alsoMatch": m.iter().skip(1).map(|x| x.number.clone()).collect::<Vec<_>>(), "next": next,
                      "window": flow::window_json(store::last_inbound(&ctx, &number).await?)}))
        }).req("number", "+14155550142").route("GET", "/whatsapp/recipients/explain"),
        // ---------------------------------------------------------------- W4 number settings
        Tool::new("get_whatsapp_number", "The number block (whatsapp/number.yaml): ids, access mode, webhook (URL, last verification), tier, throughput, quality.", |ctx, _a| async move {
            let n = config::number(&ctx)?;
            let v: Value = std::fs::read_to_string(ctx.path("whatsapp/verify.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
            Ok(json!({"number": n, "webhookUrl": flow::url(&ctx, &n), "verification": v, "graphBase": format!("http://localhost:{}{}/whatsapp", std::env::var("STUNTMAN_PORT").unwrap_or("8025".into()), ctx.prefix()),
                      "yaml": config::text(&ctx, "number.yaml"), "forced": stuntman_core::force::get(&ctx, "whatsapp").await?}))
        }).route("GET", "/whatsapp/number"),
        Tool::new("set_whatsapp_number", "Changes the number block: any_token, tier, throughput, quality (sends its webhook), webhook_url, sign, display_name, approve_after, api_versions.", |ctx, a| async move {
            let mut n = config::number(&ctx)?;
            let old_q = n.quality.clone();
            if let Some(v) = a.bool("any_token") { n.access.any_token = v; }
            if let Some(v) = a.str("tier") { n.limits.tier = v; }
            if let Some(v) = a.u64("throughput") { n.limits.throughput = v as u32; }
            if let Some(v) = a.str("quality") { n.quality = v.to_lowercase(); }
            if let Some(v) = a.str("webhook_url") { n.webhook.url = v; }
            if let Some(v) = a.bool("sign") { n.webhook.sign = v; }
            if let Some(v) = a.str("display_name") { n.display.name = v; }
            if let Some(v) = a.str("display_number") { n.display.number = v; }
            if let Some(v) = a.str("approve_after") { n.approve_after = v; }
            if let Some(v) = a.str("api_versions") { n.api_versions = v; }
            if let Some(v) = a.str("phone_number_id") { n.phone_number_id = v; }
            if let Some(v) = a.str("waba_id") { n.waba_id = v; }
            config::save_number(&ctx, &n)?;
            if n.quality != old_q { flow::quality_event(&ctx, &n, &old_q); }
            ctx.live.send("whatsapp.number", "", "changed");
            Ok(json!({"number": n}))
        }).param("any_token", "false: only secret://whatsapp-token is accepted").param("tier", "250 | 1k | 10k | 100k | unlimited").param("throughput", "messages/s").param("quality", "green | yellow | red | flagged | restricted")
          .param("webhook_url", "where Meta's webhooks go").param("sign", "X-Hub-Signature-256 on webhooks").param("display_name", "shown as").param("display_number", "+15550104477")
          .param("approve_after", "templates the app creates stay pending this long (5m)").param("api_versions", "any").param("phone_number_id", "id").param("waba_id", "id").route("PUT", "/whatsapp/number"),
        Tool::new("force_whatsapp_failure", "The next sends (count, default 1) fail as Meta's Graph does: 500 (#131000), 400 (#100 Invalid parameter), or the connection: reset, empty, timeout. normal stops it. Each one is kept as a refused message.", |ctx, a| async move {
            let by = a.str("by").or(a.str("_by")).unwrap_or("you".into());
            Ok(json!({"forced": stuntman_core::force::set(&ctx, "whatsapp", &a.need("answer")?, a.u64("count").unwrap_or(1), &by).await?}))
        }).req("answer", "500 | 400 | reset | empty | timeout | normal").param("count", "how many sends (default 1)").param("by", "who").route("POST", "/whatsapp/force"),
        Tool::new("expire_whatsapp_tokens", "Every Graph call gets 190 “Session has expired” until restored.", |ctx, _a| async move {
            let mut n = config::number(&ctx)?;
            n.tokens_expired_at = Some(util::now_ms());
            config::save_number(&ctx, &n)?;
            ctx.live.send("whatsapp.number", "", "tokens");
            Ok(json!({"expiredAt": n.tokens_expired_at}))
        }).route("POST", "/whatsapp/number/tokens/expire"),
        Tool::new("restore_whatsapp_tokens", "Tokens work again.", |ctx, _a| async move {
            let mut n = config::number(&ctx)?;
            n.tokens_expired_at = None;
            config::save_number(&ctx, &n)?;
            ctx.live.send("whatsapp.number", "", "tokens");
            Ok(json!({"restored": true}))
        }).route("POST", "/whatsapp/number/tokens/restore"),
        Tool::new("verify_whatsapp_webhook", "Sends Meta's verification GET (hub.mode, hub.verify_token, hub.challenge) to the app's callback and checks it echoes the challenge.", |ctx, _a| async move {
            let n = config::number(&ctx)?;
            let u = flow::url(&ctx, &n);
            if u.is_empty() { return Err(ApiError::bad("No callback URL: set it in Number settings")); }
            let challenge = (rand::random::<u32>() % 900_000_000 + 100_000_000).to_string();
            let tok = util::resolve_secret(&n.webhook.verify_token, &ctx.data_dir);
            let t = util::now_ms();
            let r = ctx.http.get(&u).query(&[("hub.mode", "subscribe"), ("hub.verify_token", tok.as_str()), ("hub.challenge", challenge.as_str())]).timeout(Duration::from_secs(10)).send().await;
            let (status, body, err) = match r { Ok(x) => { let s = x.status().as_u16(); (Some(s), x.text().await.unwrap_or_default(), String::new()) } Err(e) => (None, String::new(), e.to_string()) };
            let ok = status == Some(200) && body.trim() == challenge;
            let v = json!({"at": t, "ok": ok, "status": status, "answer": body.chars().take(200).collect::<String>(), "challenge": challenge, "error": err, "url": u});
            let _ = std::fs::write(ctx.path("whatsapp/verify.json"), v.to_string());
            ctx.live.send("whatsapp.number", "", "verified");
            Ok(v)
        }).route("POST", "/whatsapp/number/webhook/verify"),
        Tool::new("send_bad_webhook", "Sends a status webhook the app must reject: wrong_signature, missing_signature, old_secret or replay (the last webhook again). Returns the app's status code.", |ctx, a| async move {
            let kind = a.need("kind")?;
            let n = config::number(&ctx)?;
            let u = flow::url(&ctx, &n);
            if u.is_empty() { return Err(ApiError::bad("No callback URL: set it in Number settings")); }
            let secret = util::resolve_secret(&n.webhook.app_secret, &ctx.data_dir);
            let m = store::list(&ctx, store::Filter { dir: Some("out".into()), limit: 1, ..Default::default() }).await?.into_iter().next()
                .unwrap_or(json!({"id": flow::wamid(), "number": "14155550142", "category": "utility"}));
            let body = flow::status_body(&n, &m, "delivered", None).to_string();
            let (b, sig) = match kind.as_str() {
                "wrong_signature" => (body.clone(), Some(flow::sign("not-the-app-secret", body.as_bytes()))),
                "missing_signature" => (body.clone(), None),
                "old_secret" => (body.clone(), Some(flow::sign(&format!("{secret}-previous"), body.as_bytes()))),
                "replay" => {
                    let last = ctx.db.call(|c| c.query_row("SELECT body, signature FROM wa_hooks WHERE kind='status' ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))).await
                        .map_err(|_| ApiError::bad("No webhook sent yet to replay"))?;
                    (last.0, Some(last.1).filter(|s| !s.is_empty()))
                }
                _ => return Err(ApiError::bad("kind: wrong_signature, missing_signature, old_secret or replay")),
            };
            let (code, ms, err) = flow::post_once(&ctx, &u, &b, sig.as_deref()).await;
            store::hook(&ctx, &format!("bad:{kind}"), "", &u, code, ms, &err, 1, &b, sig.as_deref().unwrap_or("")).await?;
            Ok(json!({"kind": kind, "status": code, "rejected": code.map(|c| c >= 400).unwrap_or(false), "error": err}))
        }).req("kind", "wrong_signature | missing_signature | old_secret | replay").route("POST", "/whatsapp/number/webhook/bad"),
    ]
}
