//! The e-mail tools (E1–E4 "API & MCP" sheets). Each is an MCP tool and an HTTP route.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use stuntman_core::{extract, tools::Args, util, ApiError, Ctx, Tool, ToolResult};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::{config, events, inbox, lifecycle, message, smtp};

/// An e-mail inbox is named by its address; HTTP paths call it {name} (shared with webhook inboxes).
fn addr(a: &Args) -> Result<String, ApiError> {
    a.str("address").or(a.str("name")).ok_or_else(|| ApiError::bad("“address” is required"))
}

fn by(a: &Args) -> String {
    a.str("_by").unwrap_or("you".into())
}

/// "1h" (ago), "today", or epoch ms.
fn since(a: &Args, k: &str) -> Result<Option<i64>, ApiError> {
    match a.str(k).as_deref() {
        None => Ok(None),
        Some("today") => {
            let now = util::now_ms();
            Ok(Some(now - now % 86_400_000))
        }
        Some(s) if s.len() >= 12 && s.chars().all(|c| c.is_ascii_digit()) => Ok(s.parse().ok()),
        Some(s) => util::parse_ms(s).map(|d| Some(util::now_ms() - d as i64)).ok_or_else(|| ApiError::bad(format!("“{k}”: use 15m, 1h, 7d, today or epoch ms"))),
    }
}

fn filter(a: &Args) -> Result<message::Filter, ApiError> {
    Ok(message::Filter {
        to: a.str("to"),
        subject: a.str("subject"),
        kind: a.str("kind"),
        inbox: a.list("inbox"),
        status: a.list("status"),
        lang: a.list("lang"),
        has: a.list("has"),
        since_ms: since(a, "since")?,
        before_ms: since(a, "before")?,
        q: a.str("q"),
        service: a.str("service"),
        limit: a.u64("limit").unwrap_or(100) as u32,
        after_seq: a.u64("after_seq").map(|x| x as i64),
    })
}

fn check_channel(a: &Args) -> Result<(), ApiError> {
    match a.str("channel").as_deref() {
        None | Some("email") | Some("all") => Ok(()),
        Some(c) => Err(ApiError::bad(format!("Channel “{c}” is not installed in this build; only email"))),
    }
}

async fn svc_of(ctx: &Ctx, m: &message::Message) -> Result<config::Service, ApiError> {
    config::get(ctx, &m.service).or_else(|_| config::default_service(ctx))
}

pub fn all() -> Vec<Tool> {
    let mut v = messages();
    v.extend(inboxes());
    v.extend(services());
    v.extend(extractors());
    v
}

// ---------------------------------------------------------------- messages (E1)

fn messages() -> Vec<Tool> {
    vec![
        Tool::new("list_messages", "The list and filters of the Notifications screen. group=\"kind\" also returns the kinds with counts.", |ctx, a| async move {
            check_channel(&a)?;
            let f = filter(&a)?;
            let (msgs, total) = message::list(&ctx, f.clone()).await?;
            let mut out = json!({"messages": msgs, "total": total});
            if a.str("group").as_deref() == Some("kind") {
                out["groups"] = json!(message::kinds(&ctx, f).await?);
            }
            Ok(out)
        })
        .param("channel", "email (only channel in this build)").param("to", "recipient address").param("subject", "part of the subject")
        .param("kind", "message kind").param("inbox", "kept | temporary | catch-all (list)").param("status", "delivered, opened, clicked, bounced, deferred, dropped, held, spamreport (list)")
        .param("lang", "pt-BR, en, es (list)").param("has", "extractor names that must have a value (list)").param("since", "15m, 1h, 7d, today").param("before", "same as since")
        .param("q", "search address, subject and extracted values").param("limit", "default 100").param("group", "kind")
        .route("GET", "/messages"),
        Tool::new("get_message", "The message pane: headers, text, HTML, extracted values, links, attachments, events.", |ctx, a| async move {
            Ok(json!(message::get(&ctx, &a.need("id")?, true).await?))
        })
        .req("id", "message id, e.g. msg_0412").route("GET", "/messages/{id}"),
        Tool::new("wait_for_message", "Blocks until a matching message arrives; with extract, returns the value. Fails on timeout with what did arrive. Looks back 15 s by default (since).", |ctx, a| async move {
            check_channel(&a)?;
            let to = a.need("to")?;
            let timeout = a.ms("timeout")?.unwrap_or(30_000).min(300_000);
            let started = util::now_ms();
            let from = since(&a, "since")?.unwrap_or(started - 15_000);
            let extract_name = a.str("extract");
            let mut sub = ctx.live.subscribe();
            loop {
                let f = message::Filter { to: Some(to.clone()), subject: a.str("subject"), since_ms: Some(from), limit: 20, ..Default::default() };
                let (msgs, _) = message::list(&ctx, f).await?;
                for m in &msgs {
                    match &extract_name {
                        Some(x) => {
                            if let Some(v) = m.extracted.get(x).map(|e| e["value"].clone()) {
                                return Ok(json!({"value": v, "messageId": m.id, "message": m}));
                            }
                        }
                        None => return Ok(json!({"messageId": m.id, "message": m})),
                    }
                }
                let left = started + timeout as i64 - util::now_ms();
                if left <= 0 {
                    let f = message::Filter { since_ms: Some(from), limit: 10, ..Default::default() };
                    let (arrived, _) = message::list(&ctx, f).await?;
                    let seen: Vec<Value> = arrived.iter().map(|m| json!({"id": m.id, "to": m.to, "subject": m.subject, "extracted": m.extracted})).collect();
                    let why = match (&extract_name, msgs.is_empty()) {
                        (Some(x), false) => format!("A message to {to} arrived, but extractor “{x}” found nothing in it"),
                        _ => format!("No e-mail to {to} in {}", util::fmt_ms(timeout)),
                    };
                    return Err(ApiError::new(408, why).with(json!({"arrived": seen})));
                }
                let _ = tokio::time::timeout(Duration::from_millis(left.min(1000) as u64), sub.recv()).await;
            }
        })
        .req("to", "recipient address").param("subject", "part of the subject").param("channel", "email").param("extract", "extractor name: returns its value")
        .param("timeout", "default 30s").param("since", "how far back counts as arrived: default 15s").route("GET", "/messages/wait"),
        Tool::new("expect_no_message", "Passes if nothing arrives in time; fails with the message that did. For “blocked address gets no e-mail”.", |ctx, a| async move {
            let to = a.need("to")?;
            let within = a.ms("within")?.unwrap_or(10_000).min(300_000);
            let started = util::now_ms();
            tokio::time::sleep(Duration::from_millis(within)).await;
            let f = message::Filter { to: Some(to.clone()), subject: a.str("subject"), since_ms: Some(started), limit: 1, ..Default::default() };
            let (msgs, _) = message::list(&ctx, f).await?;
            match msgs.first() {
                None => Ok(json!({"ok": true, "waited": util::fmt_ms(within)})),
                Some(m) => Err(ApiError::new(409, format!("{} got “{}”", to, m.subject)).with(json!(m))),
            }
        })
        .req("to", "recipient address").param("within", "default 10s").param("subject", "part of the subject").route("GET", "/messages/expect-none"),
        Tool::new("simulate_recipient", "action: open | click | spamreport | unsubscribe. Sends the provider's event webhook to your app; returns the event and your app's reply.", |ctx, a| async move {
            let m = message::get(&ctx, &a.need("id")?, false).await?;
            let svc = svc_of(&ctx, &m).await?;
            let action = a.str("action").or(a.str("type")).ok_or_else(|| ApiError::bad("“action” is required"))?;
            let action = match action.as_str() {
                "open" | "opened" => "opened",
                "click" | "clicked" => "click",
                o => o,
            }
            .to_string();
            if matches!(m.status.as_str(), "bounced" | "dropped" | "held") {
                return Err(ApiError::new(409, format!("The message was {}; nobody can open it", m.status)));
            }
            lifecycle::act(&ctx, &svc, &m, &action, a.str("link").as_deref(), &by(&a)).await
        })
        .req("id", "message id").req("action", "open | click | spamreport | unsubscribe").param("link", "for click: extractor name, first, any, a link index or re:<regex>")
        .route("POST", "/messages/{id}/events"),
        Tool::new("get_attachment", "The file, as base64 and a download URL.", |ctx, a| async move {
            let id = a.need("id")?;
            let which = a.str("name").or(a.str("index")).unwrap_or("0".into());
            let at = message::attachment(&ctx, &id, &which).await?;
            Ok(json!({"name": at.name, "contentType": at.content_type, "size": at.size, "sha256": at.sha256,
                      "base64": B64.encode(&at.data), "url": format!("/api/messages/{id}/attachments/{}", at.index)}))
        })
        .req("id", "message id").param("name", "file name").param("index", "0-based index").route("GET", "/messages/{id}/attachments/{index}/info"),
        Tool::new("delete_messages", "Clears messages of this environment, e.g. before a test. Same filters as list_messages.", |ctx, a| async move {
            let f = filter(&a)?;
            Ok(json!({"count": message::delete(&ctx, f, None).await?}))
        })
        .param("to", "recipient").param("before", "15m, 1h… (older than)").param("since", "newer than").param("status", "list").param("inbox", "list")
        .route("DELETE", "/messages"),
        Tool::new("delete_message", "Deletes one message.", |ctx, a| async move {
            let n = message::delete(&ctx, Default::default(), Some(a.need("id")?)).await?;
            if n == 0 {
                return Err(ApiError::not_found("No such message"));
            }
            Ok(json!({"count": n}))
        })
        .req("id", "message id").route("DELETE", "/messages/{id}"),
        Tool::new("decide_held", "For a held message: deliver | defer | bounce | drop. kind: hard|soft for bounce, 1|3|until for defer. Returns the event your app received and its reply.", |ctx, a| async move {
            let id = a.str("message_id").or(a.str("id")).ok_or_else(|| ApiError::bad("“message_id” is required"))?;
            let m = message::get(&ctx, &id, false).await?;
            let svc = svc_of(&ctx, &m).await?;
            lifecycle::decide(&ctx, &svc, &m, &a.need("action")?, a.str("kind").as_deref(), &by(&a)).await
        })
        .req("message_id", "message id").req("action", "deliver | defer | bounce | drop").param("kind", "hard|soft, or 1|3|until")
        .route("POST", "/messages/{id}/decide"),
    ]
}

// ---------------------------------------------------------------- inboxes (E2)

async fn apply_inbox_fields(ctx: &Ctx, i: &mut inbox::Inbox, a: &Args) -> Result<(), ApiError> {
    if let Some(v) = a.str("on_arrival") {
        i.on_arrival = v;
    }
    if let Some(v) = a.u64("defer_times") {
        i.defer_times = v.clamp(1, 20) as u32;
    }
    if let Some(v) = a.ms("hold_for")? {
        i.hold_for_ms = v;
    }
    if let Some(v) = a.str("bounce_kind") {
        i.bounce_kind = v;
    }
    if let Some(v) = a.str("apply_to") {
        i.apply_to = v;
    }
    if let Some(v) = a.str("label") {
        i.label = v;
    }
    match a.get("steps") {
        Some(Value::Array(arr)) => {
            let steps = arr.iter().map(inbox::Step::from_value).collect::<Result<Vec<_>, _>>().map_err(ApiError::bad)?;
            for s in &steps {
                if s.action == "click" {
                    let l = s.link.clone().unwrap_or("first".into());
                    let known = ["first", "any"].contains(&l.as_str()) || l.starts_with("re:") || config::extractors(ctx)?.extractors.iter().any(|e| e.name == l);
                    if !known {
                        return Err(ApiError::bad(format!("Click: no extractor “{l}”. Use first, any, re:<regex> or an extractor name")));
                    }
                }
            }
            i.steps = Some(steps);
        }
        Some(Value::String(s)) if s == "default" => i.steps = None,
        Some(Value::String(s)) if s == "none" => i.steps = Some(vec![]),
        Some(_) => return Err(ApiError::bad("steps: a list like [\"delivered +1s\", \"opened +30s ±5s\"], \"default\" or \"none\"")),
        None => {}
    }
    Ok(())
}

fn inboxes() -> Vec<Tool> {
    vec![
        Tool::new("create_inbox", "A new inbox. Temporary by default (expires by itself); keep=true keeps it with the environment.", |ctx, a| async move {
            let svc = config::default_service(&ctx)?;
            let keep = a.bool("keep").unwrap_or(false);
            let address = match a.str("address") {
                Some(x) => x.to_lowercase(),
                None => match a.str("local_part") {
                    Some(lp) => format!("{}@{}", lp.to_lowercase(), svc.addresses.domain),
                    None => inbox::random_address(&a.str("prefix").unwrap_or("inbox".into()), &svc.addresses.domain),
                },
            };
            if !address.contains('@') || address.contains(' ') {
                return Err(ApiError::bad("Not an e-mail address"));
            }
            if inbox::get(&ctx, &address).await?.is_some() {
                return Err(ApiError::new(409, format!("{address} already exists")));
            }
            let ttl = a.ms("ttl")?.unwrap_or_else(|| util::parse_ms(&svc.addresses.temporary_ttl).unwrap_or(1_800_000));
            let now = util::now_ms();
            let mut i = inbox::Inbox {
                address: address.clone(),
                kind: if keep { "kept".into() } else { "temporary".into() },
                created_by: by(&a),
                created_at: now,
                expires_at: if keep { None } else { Some(now + ttl as i64) },
                on_arrival: "accept".into(),
                defer_times: 3,
                hold_for_ms: 600_000,
                bounce_kind: "hard".into(),
                steps: None,
                apply_to: "every".into(),
                label: String::new(),
                messages: 0,
            };
            apply_inbox_fields(&ctx, &mut i, &a).await?;
            inbox::save(&ctx, &i).await?;
            ctx.live.send("email.inbox", &address, "created");
            Ok(json!({"address": address, "expiresAt": i.expires_at, "inbox": i}))
        })
        .param("ttl", "default from the service, e.g. 30m").param("keep", "true: never expires").param("local_part", "the part before @").param("prefix", "random address prefix")
        .param("address", "full address (any domain)").param("on_arrival", "accept | bounce | defer | hold | drop").param("defer_times", "for defer").param("hold_for", "for hold, e.g. 10m")
        .param("bounce_kind", "hard | soft").param("steps", "[\"delivered +1s\", \"opened +30s ±5s\", \"click:signLink +2s\"], \"default\" or \"none\"").param("apply_to", "every | first | first_n:N | subject:<re> | kind:<k> | has:<x> | from:<addr>").param("label", "free text")
        .route("POST", "/inboxes"),
        Tool::new("list_inboxes", "Inboxes with rule, steps and message count.", |ctx, a| async move {
            Ok(json!({"inboxes": inbox::list(&ctx, a.str("kind"), a.str("q")).await?, "domain": config::default_service(&ctx).map(|s| s.addresses.domain).unwrap_or_default()}))
        })
        .param("kind", "kept | temporary").param("q", "search").route("GET", "/inboxes"),
        Tool::new("get_inbox", "The inbox, plus its last 20 messages.", |ctx, a| async move {
            let i = inbox::need(&ctx, &addr(&a)?).await?;
            let (msgs, _) = message::list(&ctx, message::Filter { to: Some(i.address.clone()), limit: 20, ..Default::default() }).await?;
            Ok(json!({"inbox": i, "messages": msgs}))
        })
        .req("address", "address").route("GET", "/inboxes/{name}"),
        Tool::new("update_inbox", "Change what happens on arrival, the steps or Apply to.", |ctx, a| async move {
            let mut i = inbox::need(&ctx, &addr(&a)?).await?;
            apply_inbox_fields(&ctx, &mut i, &a).await?;
            inbox::save(&ctx, &i).await?;
            ctx.live.send("email.inbox", &i.address, "updated");
            Ok(json!(inbox::need(&ctx, &i.address).await?))
        })
        .req("address", "address").param("on_arrival", "accept | bounce | defer | hold | drop").param("defer_times", "n").param("hold_for", "10m")
        .param("bounce_kind", "hard | soft").param("steps", "list, \"default\" or \"none\"").param("apply_to", "see create_inbox").param("label", "text")
        .route("PATCH", "/inboxes/{name}"),
        Tool::new("extend_inbox", "Adds time to a temporary inbox (also revives an expired one).", |ctx, a| async move {
            let mut i = inbox::need(&ctx, &addr(&a)?).await?;
            if i.kind == "kept" {
                return Err(ApiError::bad("Kept inboxes never expire"));
            }
            let by_ms = a.ms("by")?.unwrap_or(1_800_000) as i64;
            i.expires_at = Some(i.expires_at.unwrap_or(0).max(util::now_ms()) + by_ms);
            inbox::save(&ctx, &i).await?;
            ctx.live.send("email.inbox", &i.address, "updated");
            Ok(json!(i))
        })
        .req("address", "address").param("by", "default 30m").route("POST", "/inboxes/{name}/extend"),
        Tool::new("keep_inbox", "Temporary becomes kept: never expires, survives clones.", |ctx, a| async move {
            let mut i = inbox::need(&ctx, &addr(&a)?).await?;
            i.kind = "kept".into();
            i.expires_at = None;
            inbox::save(&ctx, &i).await?;
            ctx.live.send("email.inbox", &i.address, "updated");
            Ok(json!(i))
        })
        .req("address", "address").route("POST", "/inboxes/{name}/keep"),
        Tool::new("delete_inbox", "Deletes the inbox (and its messages unless with_messages=false). Mail sent there afterwards lands in the catch-all.", |ctx, a| async move {
            let address = addr(&a)?;
            inbox::need(&ctx, &address).await?;
            let n = inbox::delete(&ctx, &address, a.bool("with_messages").unwrap_or(true)).await?;
            ctx.live.send("email.inbox", &address, "deleted");
            Ok(json!({"ok": true, "messagesDeleted": n}))
        })
        .req("address", "address").param("with_messages", "default true").route("DELETE", "/inboxes/{name}"),
    ]
}

// ---------------------------------------------------------------- services (E3)

/// JSON keys from HTTP/MCP patches → the camelCase of the YAML file.
fn camel(v: Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| {
                    let mut s = String::new();
                    let mut up = false;
                    for c in k.chars() {
                        if c == '_' {
                            up = true;
                        } else if up {
                            s.push(c.to_ascii_uppercase());
                            up = false;
                        } else {
                            s.push(c);
                        }
                    }
                    (s, camel(v))
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(camel).collect()),
        o => o,
    }
}

fn merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                merge(b.entry(k).or_insert(Value::Null), v);
            }
        }
        (b, p) => *b = p,
    }
}

fn diff(a: &Value, b: &Value, path: &str, out: &mut Vec<Value>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                diff(x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null), &format!("{path}.{k}"), out);
            }
        }
        _ if a != b => out.push(json!({"field": path.trim_start_matches('.'), "from": a, "to": b})),
        _ => {}
    }
}

async fn store_service(ctx: &Ctx, old_name: &str, mut s: config::Service) -> ToolResult {
    s.env = ctx.env.to_string();
    if s.default {
        for mut o in config::list(ctx)? {
            if o.name != s.name && o.default {
                o.default = false;
                config::save(ctx, &o)?;
            }
        }
    }
    config::save(ctx, &s)?;
    if old_name != s.name {
        let _ = std::fs::remove_file(config::file(ctx, old_name));
    }
    smtp::reconcile(ctx).await;
    ctx.live.send("email.service", &s.name, "updated");
    service_view(ctx, &s)
}

fn service_view(ctx: &Ctx, s: &config::Service) -> ToolResult {
    let yaml = serde_yaml::to_string(s).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(json!({
        "service": s,
        "yaml": yaml,
        "file": format!("email/services/{}.yaml", s.name),
        "sendgridUrl": format!("{}/sendgrid/v3/mail/send", ctx.app_url(stuntman_core::main_port())),
        "smtpAddress": format!("{}:{}", ctx.app_host().0, s.intake.smtp.port),
        // On a port several environments share, the SMTP user names the environment.
        "smtpUser": crate::smtp::sharers(s.intake.smtp.port).len().gt(&1).then(|| ctx.env.to_string()),
        "smtpSharedWith": crate::smtp::sharers(s.intake.smtp.port),
        "smtpError": smtp::bind_errors().lock().unwrap().get(&smtp::key(ctx, &s.name)).cloned(),
        "publicKey": ctx.signer.public_key_b64(),
        "eventTypes": config::EVENT_TYPES,
    }))
}

fn intake_of<'a>(s: &'a mut config::Service, intake: &str) -> Result<&'a mut config::Access, ApiError> {
    match intake {
        "sendgridApi" | "sendgrid_api" | "sendgrid" => Ok(&mut s.intake.sendgrid_api),
        "smtp" => Ok(&mut s.intake.smtp.access),
        o => Err(ApiError::bad(format!("intake: sendgridApi | smtp (got “{o}”)"))),
    }
}

fn services() -> Vec<Tool> {
    vec![
        Tool::new("list_services", "E-mail services with intake, access and event target.", |ctx, a| async move {
            check_channel(&a)?;
            let list = config::list(&ctx)?;
            Ok(json!({"services": list.iter().map(|s| service_view(&ctx, s)).collect::<Result<Vec<_>, _>>()?}))
        })
        .param("channel", "email").route("GET", "/services"),
        Tool::new("get_service", "The YAML, parsed, plus addresses to copy and the events public key.", |ctx, a| async move {
            service_view(&ctx, &config::get(&ctx, &a.need("name")?)?)
        })
        .req("name", "service name").route("GET", "/services/{name}"),
        Tool::new("update_service", "Any field of the YAML (camelCase or snake_case); returns the diff applied. name renames, default=true makes it the default.", |ctx, a| async move {
            let name = a.need("name")?;
            let s = config::get(&ctx, &name)?;
            let patch = match a.get("patch") {
                Some(p) => p.clone(),
                None => Value::Object(a.0.iter().filter(|(k, _)| !k.starts_with('_') && *k != "name").map(|(k, v)| (k.clone(), v.clone())).collect::<Map<_, _>>()),
            };
            let mut patch = camel(patch);
            if let Some(n) = patch.get("newName").cloned() {
                patch.as_object_mut().unwrap().remove("newName");
                patch["name"] = n;
            }
            let before = serde_json::to_value(&s).unwrap();
            let mut after = before.clone();
            merge(&mut after, patch);
            let new: config::Service = serde_json::from_value(after.clone()).map_err(|e| ApiError::bad(format!("Not a valid service: {e}")))?;
            if new.name != name && config::get(&ctx, &new.name).is_ok() {
                return Err(ApiError::new(409, format!("A service “{}” already exists", new.name)));
            }
            let mut d = vec![];
            diff(&before, &after, "", &mut d);
            let mut out = store_service(&ctx, &name, new).await?;
            out["diff"] = json!(d);
            Ok(out)
        })
        .req("name", "service name").param("patch", "fields to change, e.g. {\"limits\": {\"keep\": \"2d\"}}").param("new_name", "rename")
        .route("PATCH", "/services/{name}"),
        Tool::new("put_service_yaml", "Replaces the whole service file (Edit as YAML).", |ctx, a| async move {
            let name = a.need("name")?;
            let y = a.need("yaml")?;
            let s: config::Service = serde_yaml::from_str(&y).map_err(|e| ApiError::bad(format!("YAML: {e}")))?;
            store_service(&ctx, &name, s).await
        })
        .req("name", "service name").req("yaml", "the whole file").route("PUT", "/services/{name}"),
        Tool::new("set_access", "How a test makes the provider unreachable or strict. intake: sendgridApi | smtp · mode: any | validate | refuse · how: timeout | refused | tls | 503 | 429. For smtp also starttls=false and refuse_recipients=550|451|none.", |ctx, a| async move {
            let name = a.need("name")?;
            let mut s = config::get(&ctx, &name)?;
            let intake = a.need("intake")?;
            if let Some(v) = a.bool("starttls") {
                s.intake.smtp.starttls = v;
            }
            if let Some(v) = a.str("refuse_recipients") {
                if !["none", "550", "451"].contains(&v.as_str()) {
                    return Err(ApiError::bad("refuse_recipients: none | 550 | 451"));
                }
                s.intake.smtp.refuse_recipients = v;
            }
            let who = by(&a);
            let acc = intake_of(&mut s, &intake)?;
            if let Some(on) = a.bool("on") {
                acc.on = on;
            }
            if let Some(mode) = a.str("mode") {
                let mode = match mode.as_str() {
                    "reject" => "refuse".to_string(),
                    m => m.to_string(),
                };
                if !["any", "validate", "refuse"].contains(&mode.as_str()) {
                    return Err(ApiError::bad("mode: any | validate | refuse"));
                }
                if mode == "validate" && acc.keys.is_empty() {
                    return Err(ApiError::bad("Generate a key first: with no keys, every send would get 401"));
                }
                acc.how = if mode == "refuse" { Some(a.str("how").unwrap_or("timeout".into())) } else { None };
                if let Some(h) = &acc.how {
                    if !["timeout", "refused", "tls", "503", "429"].contains(&h.as_str()) {
                        return Err(ApiError::bad("how: timeout | refused | tls | 503 | 429"));
                    }
                }
                acc.mode = mode;
                acc.set_by = Some(who);
                acc.set_at = Some(util::now_ms());
            }
            store_service(&ctx, &name, s).await
        })
        .req("name", "service").req("intake", "sendgridApi | smtp").param("mode", "any | validate | refuse").param("how", "timeout | refused | tls | 503 | 429")
        .param("on", "intake on/off").param("starttls", "smtp: offer STARTTLS").param("refuse_recipients", "smtp: none | 550 | 451")
        .route("PUT", "/services/{name}/intake/{intake}/access"),
        Tool::new("create_key", "A key the app must use when the intake validates keys. The secret is shown once.", |ctx, a| async move {
            let name = a.need("name")?;
            let mut s = config::get(&ctx, &name)?;
            let intake = a.need("intake")?;
            let secret = format!("SG.{}.{:032x}", ctx.env.replace('-', ""), rand::random::<u128>());
            let k = config::Key {
                id: util::id("key"),
                label: a.str("label").unwrap_or("key".into()),
                sha256: format!("{:x}", Sha256::digest(secret.as_bytes())),
                hint: secret[secret.len() - 4..].to_string(),
                created: util::now_ms(),
            };
            intake_of(&mut s, &intake)?.keys.push(k.clone());
            store_service(&ctx, &name, s).await?;
            Ok(json!({"keyId": k.id, "secret": secret, "label": k.label}))
        })
        .req("name", "service").req("intake", "sendgridApi | smtp").param("label", "who uses it").route("POST", "/services/{name}/intake/{intake}/keys"),
        Tool::new("revoke_key", "Revokes a key; sends with it get 401 (or 535 on SMTP) while keys are validated.", |ctx, a| async move {
            let name = a.need("name")?;
            let mut s = config::get(&ctx, &name)?;
            let id = a.need("key_id")?;
            let acc = intake_of(&mut s, &a.need("intake")?)?;
            let n = acc.keys.len();
            acc.keys.retain(|k| k.id != id);
            if acc.keys.len() == n {
                return Err(ApiError::not_found(format!("No key {id}")));
            }
            if acc.keys.is_empty() && acc.mode == "validate" {
                acc.mode = "any".into();
            }
            store_service(&ctx, &name, s).await
        })
        .req("name", "service").req("intake", "sendgridApi | smtp").req("key_id", "key id").route("DELETE", "/services/{name}/intake/{intake}/keys/{key_id}"),
        Tool::new("send_test_event", "Sends one event to the app now; returns the body sent and your app's status, time and body.", |ctx, a| async move {
            let mut s = config::get(&ctx, &a.need("name")?)?;
            let r = events::test(&ctx, &mut s, &a.str("type").unwrap_or("delivered".into())).await?;
            ctx.live.send("email.service", &s.name, "updated");
            Ok(r)
        })
        .req("name", "service").param("type", "processed, delivered, bounce…").route("POST", "/services/{name}/events/test"),
        Tool::new("create_service", "A new e-mail service, blank or copied from another.", |ctx, a| async move {
            check_channel(&a)?;
            let name = a.need("name")?;
            config::valid_name(&name)?;
            if config::get(&ctx, &name).is_ok() {
                return Err(ApiError::new(409, format!("A service “{name}” already exists")));
            }
            let mut s = match a.str("copy_from") {
                Some(c) => {
                    let mut s = config::get(&ctx, &c)?;
                    s.intake.sendgrid_api.keys.clear();
                    s.intake.smtp.access.keys.clear();
                    s
                }
                None => config::Service::new(&ctx, &name),
            };
            s.name = name.clone();
            s.default = config::list(&ctx)?.is_empty();
            // Next free SMTP port: not used by a service here, and not held by anything else
            // (another environment's service, or another program on this machine).
            let used: Vec<u16> = config::list(&ctx)?.iter().map(|x| x.intake.smtp.port).collect();
            while used.contains(&s.intake.smtp.port) || std::net::TcpListener::bind(("0.0.0.0", s.intake.smtp.port)).is_err() {
                s.intake.smtp.port += 1;
            }
            store_service(&ctx, &name, s).await
        })
        .req("name", "service name").param("channel", "email").param("copy_from", "service to copy").route("POST", "/services"),
        Tool::new("remove_service", "Removes the service; its messages stay until their retention ends.", |ctx, a| async move {
            let name = a.need("name")?;
            let s = config::get(&ctx, &name)?;
            std::fs::remove_file(config::file(&ctx, &name)).map_err(|e| ApiError::internal(e.to_string()))?;
            if s.default {
                if let Some(mut o) = config::list(&ctx)?.into_iter().next() {
                    o.default = true;
                    config::save(&ctx, &o)?;
                }
            }
            smtp::reconcile(&ctx).await;
            ctx.live.send("email.service", &name, "deleted");
            Ok(json!({"ok": true}))
        })
        .req("name", "service").route("DELETE", "/services/{name}"),
    ]
}

// ---------------------------------------------------------------- extractors (E4)

fn ex_from_args(a: &Args, base: Option<extract::Extractor>) -> Result<extract::Extractor, ApiError> {
    let mut e = base.unwrap_or_default();
    let src = match a.get("patch") {
        Some(p) => Args::from(p.clone()),
        None => a.clone(),
    };
    if let Some(n) = src.str("name").or(src.str("new_name")) {
        e.name = n;
    }
    if let Some(f) = src.str("from") {
        e.from = serde_json::from_value(json!(f.to_lowercase())).map_err(|_| ApiError::bad("from: subject | text | html | link | header | attachment"))?;
    }
    if let Some(r) = src.str("rule") {
        e.rule = r;
    }
    match src.get("only_on") {
        Some(Value::Object(o)) => {
            e.only_on = extract::OnlyOn {
                when: o.get("when").and_then(|v| v.as_str()).unwrap_or("any").into(),
                value: o.get("value").and_then(|v| v.as_str()).unwrap_or("").into(),
            }
        }
        Some(Value::String(s)) => {
            let (w, v) = s.split_once(':').unwrap_or((s.as_str(), ""));
            e.only_on = extract::OnlyOn { when: w.into(), value: v.into() };
        }
        _ => {}
    }
    for (k, f) in [("follow_redirects", &mut e.follow_redirects), ("all_matches", &mut e.all_matches), ("strip", &mut e.strip)] {
        if let Some(b) = src.bool(k) {
            *f = b;
        }
    }
    Ok(e)
}

/// Rebuilds what extractors see from a stored message.
async fn doc_of(ctx: &Ctx, id: &str, follow: bool) -> Result<(message::Message, extract::Doc), ApiError> {
    let m = message::get(ctx, id, true).await?;
    let mut links: Vec<extract::Link> = m
        .links
        .as_array()
        .map(|a| a.iter().map(|l| extract::Link { url: l["url"].as_str().unwrap_or("").into(), final_url: l["finalUrl"].as_str().map(String::from), redirects: l["redirects"].as_u64().unwrap_or(0) as u32 }).collect())
        .unwrap_or_default();
    if follow {
        for l in links.iter_mut().take(10) {
            if l.final_url.is_none() {
                extract::follow(&ctx.http, l).await;
            }
        }
    }
    let headers: Vec<(String, String)> = m
        .headers
        .as_ref()
        .and_then(|h| h.as_array())
        .map(|a| a.iter().map(|p| (p[0].as_str().unwrap_or("").to_string(), p[1].as_str().unwrap_or("").to_string())).collect())
        .unwrap_or_default();
    let doc = extract::Doc {
        subject: m.subject.clone(),
        text: m.text.clone().unwrap_or_default(),
        html: m.html.clone().unwrap_or_default(),
        links,
        headers,
        attachments: m.attachments.as_ref().map(|a| a.iter().filter(|x| !x.inline).map(|x| x.name.clone()).collect()).unwrap_or_default(),
        kind: m.kind.clone(),
        from: m.from.clone(),
        to: m.to.clone(),
    };
    Ok((m, doc))
}

async fn used_by(ctx: &Ctx, name: &str) -> Result<Vec<String>, ApiError> {
    let mut out = vec![];
    for i in inbox::list(ctx, None, None).await? {
        let n = i.steps.as_ref().map(|s| s.iter().filter(|s| s.link.as_deref() == Some(name)).count()).unwrap_or(0);
        if n > 0 {
            out.push(format!("{} (step)", i.address));
        }
        if i.apply_to == format!("has:{name}") {
            out.push(format!("{} (apply to)", i.address));
        }
    }
    Ok(out)
}

fn check_ex_channel(a: &Args) -> Result<(), ApiError> {
    match a.str("channel").as_deref() {
        None | Some("email") => Ok(()),
        Some(c) => Err(ApiError::bad(format!("Channel “{c}” is not installed in this build"))),
    }
}

fn extractors() -> Vec<Tool> {
    vec![
        Tool::new("list_extractors", "Built-ins and yours, with matches today and the last match.", |ctx, a| async move {
            check_ex_channel(&a)?;
            let cfg = config::extractors(&ctx)?;
            let stats = message::hit_stats(&ctx).await?;
            let st = |n: &str| stats.get(n).cloned().unwrap_or((0, None));
            let mut yours = vec![];
            for e in &cfg.extractors {
                let (today, last) = st(&e.name);
                yours.push(json!({"extractor": e, "matchedToday": today, "lastMatch": last, "usedBy": used_by(&ctx, &e.name).await?}));
            }
            let b = |n: &str, d: &str| {
                let (today, last) = st(n);
                json!({"name": n, "description": d, "matchedToday": today, "lastMatch": last})
            };
            Ok(json!({
                "builtin": [b("code", &format!("{}–{} digits near “{}”", cfg.code.min_digits, cfg.code.max_digits, cfg.code.words.iter().take(3).cloned().collect::<Vec<_>>().join("”, “"))),
                            b("links", "every link, short links followed"), b("attachments", "name, type, size, hash")],
                "code": cfg.code,
                "yours": yours,
                "file": "email/extractors.yaml",
            }))
        })
        .param("channel", "email").route("GET", "/channels/{channel}/extractors"),
        Tool::new("get_extractor", "The rule, where it is used, and its health.", |ctx, a| async move {
            check_ex_channel(&a)?;
            let name = a.need("name")?;
            let cfg = config::extractors(&ctx)?;
            let e = cfg.extractors.into_iter().find(|e| e.name == name).ok_or_else(|| ApiError::not_found(format!("No extractor “{name}”")))?;
            let (today, last) = message::hit_stats(&ctx).await?.get(&name).cloned().unwrap_or((0, None));
            Ok(json!({"extractor": e, "matchedToday": today, "lastMatch": last, "usedBy": used_by(&ctx, &name).await?}))
        })
        .req("channel", "email").req("name", "extractor").route("GET", "/channels/{channel}/extractors/{name}"),
        Tool::new("test_extractor", "Runs a rule on one message (message_id) or the last N (last=50). Per message: value or why not. Nothing is saved.", |ctx, a| async move {
            check_ex_channel(&a)?;
            let e = ex_from_args(&a, None)?;
            let e = extract::Extractor { name: if e.name.is_empty() { "test".into() } else { e.name }, ..e };
            if let Err(err) = extract::validate(&e) {
                return Ok(json!({"error": err}));
            }
            let ids: Vec<String> = match a.str("message_id") {
                Some(id) => vec![id],
                None => message::list(&ctx, message::Filter { limit: a.u64("last").unwrap_or(50) as u32, ..Default::default() }).await?.0.into_iter().map(|m| m.id).collect(),
            };
            let mut results = vec![];
            let (mut matched, mut skipped, mut missed) = (0, 0, 0);
            for id in &ids {
                let (m, d) = doc_of(&ctx, id, e.follow_redirects && ids.len() == 1).await?;
                let o = extract::run(&e, &d);
                if o.skipped {
                    skipped += 1
                } else if o.value.is_some() {
                    matched += 1
                } else {
                    missed += 1
                }
                results.push(json!({"id": m.id, "seq": m.seq, "subject": m.subject, "lang": m.lang, "outcome": o,
                    "links": if ids.len() == 1 { json!(d.links) } else { Value::Null }}));
            }
            Ok(json!({"matched": matched, "skipped": skipped, "missed": missed, "results": results}))
        })
        .param("channel", "email").req("from", "subject | text | html | link | header | attachment").req("rule", "regex, CSS selector or header name")
        .param("only_on", "{when: any|subject|kind|from|to, value} or \"subject:Action required\"").param("follow_redirects", "link only").param("all_matches", "list")
        .param("strip", "remove spaces and dashes").param("message_id", "one message").param("last", "default 50")
        .route("POST", "/channels/{channel}/extractors/test"),
        Tool::new("create_extractor", "Saves a new extractor in email/extractors.yaml.", |ctx, a| async move {
            check_ex_channel(&a)?;
            let e = ex_from_args(&a, None)?;
            extract::validate(&e).map_err(ApiError::bad)?;
            if ["code", "links", "attachments"].contains(&e.name.as_str()) {
                return Err(ApiError::bad(format!("“{}” is a built-in name", e.name)));
            }
            let mut cfg = config::extractors(&ctx)?;
            if cfg.extractors.iter().any(|x| x.name == e.name) {
                return Err(ApiError::new(409, format!("An extractor “{}” already exists", e.name)));
            }
            cfg.extractors.push(e.clone());
            config::save_extractors(&ctx, &cfg)?;
            ctx.live.send("email.extractor", &e.name, "created");
            Ok(json!(e))
        })
        .param("channel", "email").req("name", "e.g. resetLink").req("from", "source").req("rule", "rule").param("only_on", "see test_extractor")
        .param("follow_redirects", "bool").param("all_matches", "bool").param("strip", "bool").route("POST", "/channels/{channel}/extractors"),
        Tool::new("update_extractor", "Changes an extractor; returns it.", |ctx, a| async move {
            check_ex_channel(&a)?;
            let name = a.need("name")?;
            let mut cfg = config::extractors(&ctx)?;
            let i = cfg.extractors.iter().position(|e| e.name == name).ok_or_else(|| ApiError::not_found(format!("No extractor “{name}”")))?;
            let mut src = a.clone();
            src.0.remove("name");
            if let Some(n) = a.str("new_name") {
                src.0.insert("name".into(), json!(n));
            }
            let e = ex_from_args(&src, Some(cfg.extractors[i].clone()))?;
            extract::validate(&e).map_err(ApiError::bad)?;
            cfg.extractors[i] = e.clone();
            config::save_extractors(&ctx, &cfg)?;
            ctx.live.send("email.extractor", &e.name, "updated");
            Ok(json!(e))
        })
        .param("channel", "email").req("name", "extractor").param("new_name", "rename").param("from", "source").param("rule", "rule").param("only_on", "condition")
        .param("follow_redirects", "bool").param("all_matches", "bool").param("strip", "bool").route("PATCH", "/channels/{channel}/extractors/{name}"),
        Tool::new("copy_extractor", "Copies to another channel; sources that do not exist there (HTML, header) are refused.", |_ctx, a| async move {
            let to = a.need("to_channel").or(a.need("to"))?;
            Err(ApiError::bad(format!("Channel “{to}” is not installed in this build")))
        })
        .param("channel", "email").req("name", "extractor").req("to_channel", "sms | whatsapp").route("POST", "/channels/{channel}/extractors/{name}/copy"),
        Tool::new("delete_extractor", "Deletes it; tells you who used it.", |ctx, a| async move {
            check_ex_channel(&a)?;
            let name = a.need("name")?;
            let mut cfg = config::extractors(&ctx)?;
            let n = cfg.extractors.len();
            cfg.extractors.retain(|e| e.name != name);
            if cfg.extractors.len() == n {
                return Err(ApiError::not_found(format!("No extractor “{name}”")));
            }
            config::save_extractors(&ctx, &cfg)?;
            ctx.live.send("email.extractor", &name, "deleted");
            Ok(json!({"ok": true, "wasUsedBy": used_by(&ctx, &name).await?}))
        })
        .param("channel", "email").req("name", "extractor").route("DELETE", "/channels/{channel}/extractors/{name}"),
        Tool::new("tune_builtin", "Settings of the built-in “code” extractor.", |ctx, a| async move {
            check_ex_channel(&a)?;
            let mut cfg = config::extractors(&ctx)?;
            let c = &mut cfg.code;
            if let Some(v) = a.bool("on") {
                c.on = v;
            }
            if let Some(v) = a.u64("min_digits") {
                c.min_digits = v as usize;
            }
            if let Some(v) = a.u64("max_digits") {
                c.max_digits = v as usize;
            }
            if a.get("words").is_some() {
                c.words = a.list("words");
            }
            if let Some(v) = a.bool("prefer_prominent") {
                c.prefer_prominent = v;
            }
            if c.min_digits < 3 || c.max_digits > 12 || c.min_digits > c.max_digits {
                return Err(ApiError::bad("Digits: from 3 to 12, and “from” not above “to”"));
            }
            let out = json!(c);
            config::save_extractors(&ctx, &cfg)?;
            ctx.live.send("email.extractor", "code", "updated");
            Ok(out)
        })
        .param("channel", "email").param("on", "bool").param("min_digits", "4").param("max_digits", "8").param("words", "list").param("prefer_prominent", "bool")
        .route("PATCH", "/channels/{channel}/extractors/builtin/code"),
        Tool::new("put_extractors_yaml", "Replaces email/extractors.yaml.", |ctx, a| async move {
            check_ex_channel(&a)?;
            let cfg: config::Extractors = serde_yaml::from_str(&a.need("yaml")?).map_err(|e| ApiError::bad(format!("YAML: {e}")))?;
            for e in &cfg.extractors {
                extract::validate(e).map_err(|m| ApiError::bad(format!("{}: {m}", e.name)))?;
            }
            config::save_extractors(&ctx, &cfg)?;
            ctx.live.send("email.extractor", "*", "updated");
            Ok(json!({"ok": true}))
        })
        .param("channel", "email").req("yaml", "the file").route("PUT", "/channels/{channel}/extractors.yaml"),
    ]
}
