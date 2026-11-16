//! SM1–SM3 as tools: each is an MCP tool and an HTTP route under /api/sms.

use std::time::Duration;

use serde_json::{json, Value};
use stuntman_core::{tools::Args, util, ApiError, Tool};

use crate::{config, flow, store};

fn since(a: &Args, d: &str) -> Result<i64, ApiError> {
    let s = a.str("since").unwrap_or(d.into());
    let ms = util::parse_ms(&s).ok_or_else(|| ApiError::bad("since: 15m, 1h, 1d"))?;
    Ok(util::now_ms() - ms as i64)
}

fn by(a: &Args) -> String {
    a.str("by").or(a.str("_by")).unwrap_or("you".into())
}

/// What a test reads: the code, from the template fields (token, code, otp) or the first 4–8 digits.
fn extracted(m: &Value) -> Value {
    let f = &m["body"]["template"]["fields"];
    if let Some(o) = f.as_object() {
        if let Some((k, v)) = o.iter().find(|(k, _)| ["token", "code", "otp", "codigo"].contains(&k.to_lowercase().as_str())) {
            return json!({"otp": v, "from": format!("template field “{k}”")});
        }
    }
    // Digits inside links (a port, a short code) are not codes.
    let urls = regex::Regex::new(r"https?://\S+").unwrap();
    let text = urls.replace_all(m["text"].as_str().unwrap_or(""), " ");
    let re = regex::Regex::new(r"\b(\d{4,8})\b").unwrap();
    re.captures(&text).map(|c| json!({"otp": &c[1], "from": "the first 4–8 digits"})).unwrap_or(Value::Null)
}

#[cfg(test)]
mod t {
    #[test]
    fn digits_in_links_are_not_codes() {
        let m = serde_json::json!({"text": "[template d5d33983] link: http://localhost:5051/YMTdBDkQ, participantName: Jack"});
        assert!(super::extracted(&m).is_null());
        assert_eq!(super::extracted(&serde_json::json!({"text": "Acme: your code is 482913"}))["otp"], "482913");
    }
}

fn view(m: Value) -> Value {
    let mut m = m;
    m["extracted"] = extracted(&m);
    m
}

fn check_number(n: &str) -> Result<(), ApiError> {
    if n == "*" || (n.starts_with('+') && config::digits(n).len() >= 4) { Ok(()) } else { Err(ApiError::bad(format!("Use +{} (with country code), a prefix ending in *, or * for everyone else", config::digits(n)))) }
}

pub fn all() -> Vec<Tool> {
    vec![
        // ---------------------------------------------------------------- SM1 messages
        Tool::new("list_sms", "SMS both ways (the app's sends, refusals and the person's replies), newest first: text, encoding, segments, status history, the code found.", |ctx, a| async move {
            let f = store::Filter { number: a.str("number"), status: a.str("status"), dir: a.str("dir"), text: a.str("text"), q: a.str("q"), since_ms: Some(since(&a, "1d")?), limit: a.u64("limit").unwrap_or(200) as u32 };
            Ok(json!({"messages": store::list(&ctx, f).await?.into_iter().map(view).collect::<Vec<_>>()}))
        }).param("number", "+5511999999999").param("status", "accepted, SENT, DELIVERED, NOT_DELIVERED, refused, received").param("dir", "out | in").param("q", "number, text, id")
          .param("since", "15m, 1h, 1d (default)").param("limit", "default 200").route("GET", "/sms/messages"),
        Tool::new("get_sms", "One SMS with its status history (and what the app answered to each callback).", |ctx, a| async move {
            Ok(json!({"message": view(store::get(&ctx, &a.need("id")?).await?)}))
        }).req("id", "message id").route("GET", "/sms/messages/{id}"),
        Tool::new("wait_for_sms", "Waits for the app to send an SMS to a number (optionally with text containing…); returns it with the code found (extracted.otp).", |ctx, a| async move {
            let number = a.need("number")?;
            let timeout = a.ms("timeout")?.unwrap_or(30_000);
            let from = since(&a, "60s")?;
            let started = util::now_ms();
            loop {
                let f = store::Filter { number: Some(number.clone()), dir: Some("out".into()), text: a.str("text_contains").or(a.str("text")), since_ms: Some(from), limit: 50, ..Default::default() };
                if let Some(m) = store::list(&ctx, f).await?.into_iter().find(|m| m["status"] != "refused") {
                    return Ok(json!({"message": view(m.clone()), "id": m["id"], "extracted": extracted(&m)}));
                }
                if util::now_ms() - started > timeout as i64 {
                    return Err(ApiError::new(408, format!("No SMS to {number} in {}", util::fmt_ms(timeout))));
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }).req("number", "+5511999999999").param("text_contains", "part of the text").param("timeout", "default 30s").param("since", "how far back counts (default 60s)")
          .route("GET", "/sms/wait"),
        Tool::new("reply_sms_as_user", "The person texts back (STOP, HELP or anything): Zenvia's MESSAGE webhook goes to the app. STOP-like keywords opt the number out; START opts it back in.", |ctx, a| async move {
            let m = flow::reply_from(&ctx, &a.need("number")?, &a.need("text")?, &a.str("to").unwrap_or_default(), &by(&a)).await?;
            Ok(json!({"message": m}))
        }).req("number", "+5511999999999").req("text", "what the person texts").param("to", "the sender it answers (default the first)").param("by", "who did it")
          .route("POST", "/sms/numbers/{number}/reply"),
        Tool::new("set_sms_status", "Moves an SMS to SENT, DELIVERED or NOT_DELIVERED (reason: invalid, carrier_rejected, phone_off, blocked, not_covered); the MESSAGE_STATUS callback goes to the app.", |ctx, a| async move {
            let st = a.need("status")?.to_uppercase();
            if !["SENT", "DELIVERED", "NOT_DELIVERED"].contains(&st.as_str()) { return Err(ApiError::bad("status: SENT, DELIVERED or NOT_DELIVERED")); }
            let reason = if st == "NOT_DELIVERED" { a.str("reason").unwrap_or("invalid".into()) } else { String::new() };
            Ok(json!({"message": view(flow::status(&ctx, &a.need("id")?, &st, &reason, &by(&a)).await?)}))
        }).req("id", "message id").req("status", "SENT | DELIVERED | NOT_DELIVERED").param("reason", "invalid | carrier_rejected | phone_off | blocked | not_covered").param("by", "who")
          .route("POST", "/sms/messages/{id}/status"),
        Tool::new("resend_sms_status", "Sends the same MESSAGE_STATUS callback again (a duplicate), to test that the app ignores it.", |ctx, a| async move {
            let id = a.need("id")?;
            let m = store::get(&ctx, &id).await?;
            let st = a.str("status").unwrap_or(m["status"].as_str().unwrap_or("DELIVERED").to_string());
            flow::deliver(&ctx, "MESSAGE_STATUS", &id, &st, flow::status_body(&m, &st, m["reason"].as_str().unwrap_or("")));
            Ok(json!({"resent": st, "id": id}))
        }).req("id", "message id").param("status", "default the current one").route("POST", "/sms/messages/{id}/resend"),
        Tool::new("list_sms_callbacks", "The callbacks Stuntman sent to the app (MESSAGE_STATUS, MESSAGE) with the app's answers.", |ctx, a| async move {
            Ok(json!({"callbacks": store::hooks(&ctx, a.u64("limit").unwrap_or(100) as u32).await?}))
        }).param("limit", "default 100").route("GET", "/sms/callbacks"),
        Tool::new("retry_sms_callbacks", "Sends again the callbacks the app did not take (non-2xx or no answer) in the last hour.", |ctx, _a| async move {
            let since = util::now_ms() - 3_600_000;
            let rows: Vec<(String, String, String)> = ctx.db.call(move |c| c.prepare("SELECT kind, message_id, body FROM sms_hooks WHERE at>=?1 AND (status IS NULL OR status<200 OR status>=300) ORDER BY id DESC")?
                .query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect()).await?;
            let mut seen = std::collections::HashSet::new();
            let mut n = 0;
            for (kind, mid, body) in rows {
                if !seen.insert(format!("{kind}|{mid}|{}", body.len())) { continue; }
                let b: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let st = b["messageStatus"]["code"].as_str().unwrap_or("received").to_string();
                flow::deliver(&ctx, &kind, &mid, &st, b);
                n += 1;
            }
            Ok(json!({"retried": n}))
        }).route("POST", "/sms/callbacks/retry"),
        Tool::new("list_sms_opt_outs", "Numbers that texted STOP (or were opted out): new sends to them are refused until START.", |ctx, _a| async move {
            Ok(json!({"optOuts": store::optouts(&ctx).await?}))
        }).route("GET", "/sms/opt-outs"),
        Tool::new("sms_opt_out", "Adds a number to the opt-out list without texting STOP.", |ctx, a| async move {
            let n = config::digits(&a.need("number")?);
            store::opt_out(&ctx, &n, &by(&a)).await?;
            Ok(json!({"number": n, "optedOut": true}))
        }).req("number", "+5511999999999").route("POST", "/sms/opt-outs"),
        Tool::new("sms_opt_in", "Removes a number from the opt-out list.", |ctx, a| async move {
            let n = config::digits(&a.need("number")?);
            Ok(json!({"number": n, "removed": store::opt_in(&ctx, &n).await?}))
        }).req("number", "+5511999999999").route("DELETE", "/sms/opt-outs/{number}"),
        Tool::new("force_sms_failure", "The next sends (count, default 1) fail as Zenvia does: 500 (INTERNAL_ERROR), 400 (VALIDATION_ERROR body), or the connection: reset, empty, timeout. normal stops it. Each one is listed as refused.", |ctx, a| async move {
            let f = stuntman_core::force::set(&ctx, "sms", &a.need("answer")?, a.u64("count").unwrap_or(1), &by(&a)).await?;
            Ok(json!({"forced": f}))
        }).req("answer", "500 | 400 | reset | empty | timeout | normal").param("count", "how many sends (default 1)").param("by", "who").route("POST", "/sms/force"),
        // ---------------------------------------------------------------- SM2 recipients
        Tool::new("list_sms_recipients", "The simulated people behind numbers, in match order (first match wins; * is everyone else), with their steps.", |ctx, _a| async move {
            Ok(json!({"recipients": config::recipients(&ctx)?, "yaml": config::text(&ctx, "recipients.yaml")}))
        }).route("GET", "/sms/recipients"),
        Tool::new("set_sms_recipient", "Creates or replaces a recipient (sms/recipients.yaml): carrier and steps (delivered_after, not_delivered: reason, never_delivered: 48h, on_question / on_nth / on_text: {reply, after}).", |ctx, a| async move {
            let number = a.need("number")?;
            check_number(&number)?;
            let steps: Vec<config::Step> = serde_json::from_value(a.get("steps").cloned().unwrap_or(json!([]))).map_err(|e| ApiError::bad(format!("steps: {e}")))?;
            let r = config::Recipient { number: number.clone(), carrier: a.str("carrier").unwrap_or_default(), steps };
            let mut all = config::recipients(&ctx)?;
            match all.iter().position(|x| x.number == number) {
                Some(i) => all[i] = r,
                // New ones go before "*" so they match first.
                None => { let i = all.iter().position(|x| x.number == "*").unwrap_or(all.len()); all.insert(i, r); }
            }
            config::save_recipients(&ctx, &all)?;
            ctx.live.send("sms.recipients", &number, "saved");
            Ok(json!({"recipients": all}))
        }).req("number", "+5511999990001, a prefix +55119999* or *").param("carrier", "Vivo, Claro, TIM…").param("steps", "[{delivered_after: 3s}, …]").route("PUT", "/sms/recipients"),
        Tool::new("remove_sms_recipient", "Removes a recipient; the number falls back to the next match or “Everyone else”.", |ctx, a| async move {
            let number = a.need("number")?;
            if number == "*" { return Err(ApiError::bad("“Everyone else” stays; change its steps instead")); }
            let mut all = config::recipients(&ctx)?;
            all.retain(|x| x.number != number);
            config::save_recipients(&ctx, &all)?;
            ctx.live.send("sms.recipients", &number, "removed");
            Ok(json!({"recipients": all}))
        }).req("number", "the recipient's number or pattern").route("DELETE", "/sms/recipients"),
        // ---------------------------------------------------------------- SM3 service
        Tool::new("get_sms_service", "The service block (sms/service.yaml): API shape, access, senders, callbacks, opt-out keywords, limits, declared templates; plus the base URL for the app.", |ctx, _a| async move {
            let s = config::service(&ctx)?;
            let base = format!("{}/zenvia", ctx.app_url(stuntman_core::main_port()));
            Ok(json!({"service": s, "yaml": config::text(&ctx, "service.yaml"), "baseUrl": base, "apiUrl": format!("{base}/v2/channels/{{0}}/messages"),
                "callbackDefault": ctx.state.callback_default(), "optOuts": store::optouts(&ctx).await?, "forced": stuntman_core::force::get(&ctx, "sms").await?}))
        }).route("GET", "/sms/service"),
        Tool::new("set_sms_service", "Changes the service block: any_token, strict_senders, senders, callbacks {status, inbound}, opt_out {keywords, back_in}, limits {throughput, max_segments}, templates [{id, name, text}], strict_templates.", |ctx, a| async move {
            let mut s = config::service(&ctx)?;
            if let Some(x) = a.bool("any_token") { s.access.any_token = x; }
            if let Some(x) = a.bool("strict_senders") { s.strict_senders = x; }
            if let Some(x) = a.bool("strict_templates") { s.strict_templates = x; }
            let take = |k: &str, f: &mut dyn FnMut(Value) -> Result<(), String>| -> Result<(), ApiError> { if let Some(v) = a.get(k).cloned() { f(v).map_err(|e| ApiError::bad(format!("{k}: {e}")))?; } Ok(()) };
            take("senders", &mut |v| { s.senders = serde_json::from_value(v).map_err(|e| e.to_string())?; Ok(()) })?;
            take("callbacks", &mut |v| { s.callbacks = serde_json::from_value(v).map_err(|e| e.to_string())?; Ok(()) })?;
            take("opt_out", &mut |v| { s.opt_out = serde_json::from_value(v).map_err(|e| e.to_string())?; Ok(()) })?;
            take("limits", &mut |v| { s.limits = serde_json::from_value(v).map_err(|e| e.to_string())?; Ok(()) })?;
            take("templates", &mut |v| { s.templates = serde_json::from_value(v).map_err(|e| e.to_string())?; Ok(()) })?;
            config::save_service(&ctx, &s)?;
            ctx.live.send("sms.service", "", "saved");
            Ok(json!({"service": s}))
        }).param("any_token", "true: any X-API-TOKEN works").param("strict_senders", "unknown from → 400").param("senders", "[{id, type}]").param("callbacks", "{status, inbound}")
          .param("opt_out", "{keywords, back_in}").param("limits", "{throughput, max_segments}").param("templates", "[{id, name, text with {{field}}}]").param("strict_templates", "unknown templateId → 400")
          .route("PUT", "/sms/service"),
    ]
}
