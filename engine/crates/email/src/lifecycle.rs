//! What happens to a message after it arrives: the inbox rule (accept, bounce, defer, hold, drop),
//! then the steps that play the recipient (delivered, opened, click…), each sending the provider's
//! event to the app. ponytail: timers live in memory; a restart forgets pending steps.

use std::time::Duration;

use stuntman_core::{util, ApiError, Ctx};
use rand::Rng;
use serde_json::{json, Value};

use crate::{config, events, inbox, message};

/// Statuses after which nothing more happens.
fn is_final(s: &str) -> bool {
    matches!(s, "bounced" | "dropped")
}

async fn current(ctx: &Ctx, id: &str) -> Option<message::Message> {
    message::get(ctx, id, false).await.ok()
}

async fn sleep_until(t: i64) {
    let d = t - util::now_ms();
    if d > 0 {
        tokio::time::sleep(Duration::from_millis(d as u64)).await;
    }
}

pub fn start(ctx: Ctx, svc: config::Service, m: message::Message, ib: Option<inbox::Inbox>) {
    tokio::spawn(async move {
        if let Err(e) = run(&ctx, &svc, m, ib).await {
            eprintln!("email lifecycle: {}", e.message);
        }
    });
}

async fn run(ctx: &Ctx, svc: &config::Service, m: message::Message, ib: Option<inbox::Inbox>) -> Result<(), ApiError> {
    let t0 = m.at;
    let rule = ib.as_ref().map(|i| i.on_arrival.clone()).unwrap_or("accept".into());
    // The rule's name travels with the trip, so telemetry can say which rule made a metric jump.
    if let Some(i) = ib.as_ref().filter(|i| i.on_arrival != "accept") {
        let name = if i.label.trim().is_empty() || i.label.starts_with("rule:") { format!("{} · {}", i.address, i.on_arrival) } else { i.label.trim().to_string() };
        stuntman_core::trips::set_rule(ctx, &m.id, &name).await?;
    }
    match rule.as_str() {
        "bounce" => {
            tokio::time::sleep(Duration::from_millis(800)).await;
            let soft = ib.as_ref().map(|i| i.bounce_kind == "soft").unwrap_or(false);
            return bounce(ctx, svc, &m, soft, "rule").await.map(|_| ());
        }
        "drop" => {
            message::set_status(ctx, &m.id, "dropped", "rule", Some(true), Some("Dropped by the inbox rule".into())).await?;
            events::emit(ctx, svc, &m, "dropped", json!({"reason": "Dropped by the inbox rule", "detail": "by the inbox rule"}), "rule").await?;
            return Ok(());
        }
        "defer" => {
            let n = ib.as_ref().map(|i| i.defer_times).unwrap_or(3);
            defer(ctx, svc, &m, n, "rule").await?;
        }
        "hold" => {
            let hold = ib.as_ref().map(|i| i.hold_for_ms).unwrap_or(600_000) as i64;
            let until = util::now_ms() + hold;
            let id = m.id.clone();
            ctx.db.call(move |c| c.execute("UPDATE email_messages SET status='held', held_until=?2 WHERE id=?1", rusqlite::params![id, until])).await?;
            ctx.live.send("email.message", &m.id, "updated");
            loop {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let Some(cur) = current(ctx, &m.id).await else { return Ok(()) };
                match cur.status.as_str() {
                    "held" if util::now_ms() >= until => break,
                    "held" | "deferred" => continue,
                    s if is_final(s) => return Ok(()),
                    // Decided by hand: delivered now; the remaining steps continue from here.
                    _ => return steps(ctx, svc, &cur, ib.as_ref(), t0, true).await,
                }
            }
        }
        _ => {}
    }
    let Some(cur) = current(ctx, &m.id).await else { return Ok(()) };
    if is_final(&cur.status) {
        return Ok(());
    }
    steps(ctx, svc, &cur, ib.as_ref(), t0, false).await
}

async fn applies(ctx: &Ctx, m: &message::Message, ib: Option<&inbox::Inbox>) -> bool {
    let Some(ib) = ib else { return true };
    let (k, v) = ib.apply_to.split_once(':').unwrap_or((ib.apply_to.as_str(), ""));
    let nth = || {
        let (to, seq) = (m.to.clone(), m.seq);
        async move {
            ctx.db
                .call(move |c| c.query_row("SELECT COUNT(*) FROM email_messages WHERE rcpt = ?1 COLLATE NOCASE AND seq < ?2", rusqlite::params![to, seq], |r| r.get::<_, u64>(0)))
                .await
                .unwrap_or(0)
        }
    };
    match k {
        "first" => nth().await == 0,
        "first_n" | "first-n" | "firstN" => nth().await < v.parse().unwrap_or(1),
        "subject" => regex::Regex::new(v).map(|r| r.is_match(&m.subject)).unwrap_or(false),
        "kind" => m.kind == v,
        "has" => m.extracted.get(v).is_some(),
        "from" => m.from.eq_ignore_ascii_case(v),
        _ => true,
    }
}

async fn steps(ctx: &Ctx, svc: &config::Service, m: &message::Message, ib: Option<&inbox::Inbox>, t0: i64, already_delivered: bool) -> Result<(), ApiError> {
    let list: Vec<inbox::Step> = match ib.and_then(|i| i.steps.clone()) {
        Some(s) => s,
        None => svc.events.default_steps.iter().filter_map(|s| inbox::Step::parse(s).ok()).collect(),
    };
    let list: Vec<inbox::Step> = if applies(ctx, m, ib).await { list } else { vec![] };
    let has_delivery = list.iter().any(|s| s.on && s.action == "delivered");
    if !has_delivery && !already_delivered {
        deliver(ctx, svc, m, "step").await?;
    }
    let mut done: Vec<i64> = vec![];
    let mut prev = util::now_ms().max(t0);
    let mut delivered_at = if already_delivered || !has_delivery { Some(util::now_ms()) } else { None };
    for (i, s) in list.iter().enumerate() {
        if !s.on || (already_delivered && s.action == "delivered") {
            done.push(prev);
            continue;
        }
        let base = match s.from.as_str() {
            "arrival" => t0,
            "delivery" => delivered_at.unwrap_or(prev),
            f if f.starts_with("step:") => f[5..].parse::<usize>().ok().and_then(|n| done.get(n - 1).copied()).unwrap_or(prev),
            _ => prev,
        };
        let vary = if s.vary_ms > 0 { rand::thread_rng().gen_range(-(s.vary_ms as i64)..=s.vary_ms as i64) } else { 0 };
        let t = (base + s.wait_ms as i64 + vary).max(base).max(prev);
        sleep_until(t).await;
        let Some(cur) = current(ctx, &m.id).await else { return Ok(()) };
        if is_final(&cur.status) {
            return Ok(());
        }
        act(ctx, svc, &cur, &s.action, s.link.as_deref(), "step").await?;
        if s.action == "delivered" {
            delivered_at = Some(util::now_ms());
        }
        prev = util::now_ms();
        done.push(prev);
        let _ = i;
    }
    Ok(())
}

pub async fn deliver(ctx: &Ctx, svc: &config::Service, m: &message::Message, by: &str) -> Result<Value, ApiError> {
    message::set_status(ctx, &m.id, "delivered", by, None, None).await?;
    events::emit(ctx, svc, m, "delivered", json!({"response": "250 OK", "detail": by_text(by)}), by).await
}

pub async fn bounce(ctx: &Ctx, svc: &config::Service, m: &message::Message, soft: bool, by: &str) -> Result<Value, ApiError> {
    let (status, reason) = if soft { ("4.2.2", "452 4.2.2 mailbox full") } else { ("5.1.1", "550 5.1.1 mailbox unavailable") };
    message::set_status(ctx, &m.id, "bounced", by, Some(true), Some(reason.into())).await?;
    events::emit(ctx, svc, m, "bounce", json!({"type": "bounce", "status": status, "reason": reason, "detail": reason}), by).await
}

pub async fn drop_it(ctx: &Ctx, svc: &config::Service, m: &message::Message, reason: &str, by: &str) -> Result<Value, ApiError> {
    message::set_status(ctx, &m.id, "dropped", by, Some(true), Some(reason.into())).await?;
    events::emit(ctx, svc, m, "dropped", json!({"reason": reason, "detail": reason}), by).await
}

/// Sends `n` deferred events one second apart (test speed), status "deferred" while it lasts.
pub async fn defer(ctx: &Ctx, svc: &config::Service, m: &message::Message, n: u32, by: &str) -> Result<Value, ApiError> {
    let mut last = Value::Null;
    for attempt in 1..=n {
        message::set_status(ctx, &m.id, "deferred", by, Some(true), None).await?;
        last = events::emit(ctx, svc, m, "deferred", json!({"attempt": attempt.to_string(), "response": "451 4.3.0 try again later", "detail": format!("attempt {attempt} of {n}")}), by).await?;
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
    Ok(last)
}

fn by_text(by: &str) -> String {
    match by {
        "step" => "by the inbox's steps".into(),
        "rule" => "by the inbox rule".into(),
        "you" => "by you".into(),
        o => format!("by {o}"),
    }
}

/// One recipient action, from a step, the screen or an agent. Returns the event and the app's reply.
pub async fn act(ctx: &Ctx, svc: &config::Service, m: &message::Message, action: &str, link: Option<&str>, by: &str) -> Result<Value, ApiError> {
    match action {
        "delivered" => deliver(ctx, svc, m, by).await,
        "opened" | "open" => {
            if !matches!(m.status.as_str(), "clicked") {
                message::set_status(ctx, &m.id, "opened", by, None, None).await?;
            }
            events::emit(ctx, svc, m, "open", json!({"useragent": "Mozilla/5.0 (Stuntman)", "ip": "127.0.0.1", "detail": by_text(by)}), by).await
        }
        "click" => {
            let Some(url) = pick_link(m, link.unwrap_or("first")) else {
                let why = format!("click skipped: no link “{}” in this e-mail", link.unwrap_or("first"));
                message::add_event(ctx, &m.id, "skipped", by, &why, None).await?;
                return Ok(json!({"skipped": why}));
            };
            message::set_status(ctx, &m.id, "clicked", by, None, None).await?;
            // The click also opens the link once, from the simulator, as a recipient's browser would.
            let visit = ctx.http.get(&url).timeout(Duration::from_secs(5)).send().await.map(|r| r.status().as_u16()).ok();
            let mut out = events::emit(ctx, svc, m, "click", json!({"url": url, "useragent": "Mozilla/5.0 (Stuntman)", "ip": "127.0.0.1", "detail": url}), by).await?;
            out["visited"] = json!({"url": url, "status": visit});
            Ok(out)
        }
        "spamreport" => {
            message::set_status(ctx, &m.id, "spamreport", by, None, None).await?;
            events::emit(ctx, svc, m, "spamreport", json!({"detail": by_text(by)}), by).await
        }
        "unsubscribe" => {
            let has = m.links.as_array().map(|a| a.iter().any(|l| l["url"].as_str().unwrap_or("").to_lowercase().contains("unsub"))).unwrap_or(false);
            if !has {
                return Err(ApiError::bad("Unavailable: this e-mail has no unsubscribe link"));
            }
            message::set_status(ctx, &m.id, "unsubscribed", by, None, None).await?;
            events::emit(ctx, svc, m, "unsubscribe", json!({"detail": by_text(by)}), by).await
        }
        o => Err(ApiError::bad(format!("action: open | click | spamreport | unsubscribe (got “{o}”)"))),
    }
}

/// "signLink" (an extractor), "any", "first", a link index, or "re:<regex>".
pub fn pick_link(m: &message::Message, which: &str) -> Option<String> {
    let links: Vec<String> = m.links.as_array()?.iter().filter_map(|l| l["finalUrl"].as_str().or(l["finalUrl"].as_str()).or(l["url"].as_str()).map(String::from)).collect();
    if let Some(v) = m.extracted.get(which).and_then(|e| e["value"].as_str()) {
        if v.starts_with("http") {
            return Some(v.to_string());
        }
    }
    match which {
        "first" | "" => links.first().cloned(),
        "any" => {
            if links.is_empty() {
                None
            } else {
                Some(links[rand::thread_rng().gen_range(0..links.len())].clone())
            }
        }
        w if w.starts_with("re:") => regex::Regex::new(&w[3..]).ok().and_then(|r| links.iter().find(|l| r.is_match(l)).cloned()),
        w if w.starts_with("http") => Some(w.to_string()),
        w => w.parse::<usize>().ok().and_then(|i| links.get(i).cloned()),
    }
}

/// decide_held: deliver | defer | bounce | drop, by hand or by an agent.
pub async fn decide(ctx: &Ctx, svc: &config::Service, m: &message::Message, action: &str, kind: Option<&str>, by: &str) -> Result<Value, ApiError> {
    if !matches!(m.status.as_str(), "held" | "deferred") {
        return Err(ApiError::new(409, format!("{} is not held (it is {})", m.id, m.status)));
    }
    match action {
        "deliver" => deliver(ctx, svc, m, by).await,
        "bounce" => bounce(ctx, svc, m, kind == Some("soft"), by).await,
        "drop" => drop_it(ctx, svc, m, "Dropped by hand", by).await,
        "defer" => {
            // "1" | "3" | "until": until keeps it deferred until someone delivers it.
            let k = kind.unwrap_or("1");
            if k == "until" {
                message::set_status(ctx, &m.id, "deferred", by, Some(true), None).await?;
                return events::emit(ctx, svc, m, "deferred", json!({"attempt": "1", "response": "451 4.3.0 try again later", "detail": "until delivered by hand"}), by).await;
            }
            let n: u32 = k.parse().map_err(|_| ApiError::bad("defer kind: 1, 3 or until"))?;
            let (ctx2, svc2, m2, by2) = (ctx.clone(), svc.clone(), m.clone(), by.to_string());
            tokio::spawn(async move {
                if defer(&ctx2, &svc2, &m2, n, &by2).await.is_ok() {
                    let _ = deliver(&ctx2, &svc2, &m2, &by2).await;
                }
            });
            Ok(json!({"deferring": n}))
        }
        o => Err(ApiError::bad(format!("action: deliver | defer | bounce | drop (got “{o}”)"))),
    }
}
