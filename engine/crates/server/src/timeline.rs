//! The Timeline (B1): one feed of what reached every service, who did it, and when Stuntman was off.

use serde_json::{json, Value};
use stuntman_core::{trips, util, ApiError, Ctx, Tool};

fn window(a: &stuntman_core::tools::Args) -> Result<(i64, i64), ApiError> {
    // A zoom: an explicit window.
    if let (Some(f), Some(t)) = (a.u64("from"), a.u64("to")) {
        if t > f { return Ok((f as i64, t as i64)); }
    }
    let span = match a.str("range").as_deref().unwrap_or("1h") {
        "15m" => 15 * 60_000,
        "1h" => 3_600_000,
        "24h" => 86_400_000,
        r => return Err(ApiError::bad(format!("range is 15m, 1h or 24h, not {r}"))),
    };
    let to = a.u64("to").map(|x| x as i64).unwrap_or_else(util::now_ms);
    Ok((to - span, to))
}

fn filter(a: &stuntman_core::tools::Args, from_ms: i64, to_ms: i64) -> trips::Filter {
    trips::Filter { from_ms, to_ms, module: a.str("service"), actor: a.str("who"), outcome: a.str("show").filter(|s| s != "all"), text: a.str("q"), limit: a.u64("limit").unwrap_or(500) as u32 }
}

/// A mutating tool call by an agent, a test or you: one row in the service it acted on.
pub async fn act(ctx: &Ctx, module: &str, tool: &str, by: &str, args: &Value, failed: Option<&str>) {
    if by.is_empty() || ["wait_for_timeline"].contains(&tool) { return; }
    let who = by.trim_start_matches("mcp:").trim_start_matches("api:");
    let mut brief: Vec<String> = args.as_object().into_iter().flatten().filter(|(k, _)| !k.starts_with('_'))
        .map(|(k, v)| {
            // Never on screen: secret values an agent passed (the Timeline is shared with everyone on the environment).
            let hide = ["secret", "password", "token", "key", "value", "pfx", "pem", "certificate"].iter().any(|s| k.to_lowercase().contains(s));
            format!("{k}={}", if hide { "•••".to_string() } else { v.as_str().map(String::from).unwrap_or_else(|| v.to_string()) })
        }).collect();
    brief.sort();
    let mut detail = brief.join("  ·  ");
    if detail.len() > 160 { detail = format!("{}…", detail.chars().take(160).collect::<String>()); }
    if let Some(e) = failed { detail = format!("{e}  ·  {detail}"); }
    let label = format!("{who} called {}", tool.replace('_', " "));
    let outcome = if failed.is_some() { "failed" } else if tool.contains("provoke") || tool.contains("force") || tool.contains("scenario") || tool.contains("fail") { "provoked" } else { "ok" };
    let _ = trips::record(ctx, trips::Trip { id: &util::id("act"), module, label: &label, status: if failed.is_some() { "error" } else { "ok" }, trace_id: None,
        link: "", host: "", actor: by, outcome, detail: &detail }).await;
}

/// Rows whose caller is a library (a user agent such as azsdk-net-Storage.Blobs) take the name of the
/// app that owns their trace in Telemetry; the library stays in `client` and in the second line.
/// Done on read: the app's spans often arrive a few seconds after the call.
async fn attribute(ctx: &Ctx, rows: &mut [Value]) {
    let own = |a: &str| a == "you" || a.starts_with("mcp:") || a.starts_with("api:");
    let traces: Vec<String> = rows.iter().filter(|r| !own(r["actor"].as_str().unwrap_or(""))).filter_map(|r| r["traceId"].as_str().map(String::from)).collect();
    let owners = stuntman_telemetry::query::services_of(ctx, traces).await;
    for r in rows.iter_mut() {
        let (Some(t), Some(a)) = (r["traceId"].as_str(), r["actor"].as_str()) else { continue };
        let Some(app) = owners.get(t) else { continue };
        if own(a) || app == a { continue; }
        let (a, app) = (a.to_string(), app.clone());
        if a.is_empty() { r["actor"] = json!(app); r["client"] = json!(""); continue; }
        if let Some(rest) = r["label"].as_str().and_then(|l| l.strip_prefix(a.as_str())).map(String::from) { r["label"] = json!(format!("{app}{rest}")); }
        r["detail"] = json!(format!("via {a}  ·  {}", r["detail"].as_str().unwrap_or("")));
        r["client"] = json!(a);
        r["actor"] = json!(app);
    }
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("timeline", "Everything that reached this environment's services in a window: rows (newest first), counts per service and per actor, activity lanes, and offline gaps.", |ctx, a| async move {
            let (from, to) = window(&a)?;
            // A selected period inside the window: the rows and counts follow it, the lanes keep the window.
            let (rf, rt) = match (a.u64("sel_from"), a.u64("sel_to")) {
                (Some(f), Some(t)) if t > f => (f as i64, t as i64),
                _ => (from, to),
            };
            let total: i64 = trips::counts(&ctx, from, to).await?["modules"].as_object().map(|o| o.values().filter_map(|v| v.as_i64()).sum()).unwrap_or(0);
            let mut rows = trips::list(&ctx, filter(&a, rf, rt)).await?;
            attribute(&ctx, &mut rows).await;
            // Who counts follow the attribution: a library's calls count for the app that made them.
            let mut counts = trips::counts(&ctx, rf, rt).await?;
            for r in rows.iter().filter(|r| r["client"].is_string()) {
                let (c, app) = (r["client"].as_str().unwrap_or(""), r["actor"].as_str().unwrap_or(""));
                let a = &mut counts["actors"];
                let left = a[c].as_i64().unwrap_or(0) - 1;
                if left > 0 { a[c] = json!(left); } else if let Some(o) = a.as_object_mut() { o.remove(c); }
                a[app] = json!(a[app].as_i64().unwrap_or(0) + 1);
            }
            Ok(json!({"from": from, "to": to, "selFrom": rf, "selTo": rt, "windowTotal": total, "rows": rows, "counts": counts,
                "lanes": trips::lanes(&ctx, from, to, a.u64("buckets").unwrap_or(60) as i64).await?, "gaps": trips::gaps(&ctx, from, to).await?}))
        })
        .param("range", "15m, 1h (default) or 24h")
        .param("to", "end of the window, epoch ms (default: now); with from, an explicit window (zoom)")
        .param("from", "start of an explicit window, epoch ms (with to)")
        .param("sel_from", "a selected period inside the window, epoch ms: rows and counts follow it")
        .param("sel_to", "end of the selected period, epoch ms")
        .param("service", "email, whatsapp, storage, keyvault, identity, apis, webhook…")
        .param("who", "app, you, mcp, api, or an exact actor such as mcp:claude-code")
        .param("show", "all (default), failed or provoked")
        .param("q", "text in the sentence, detail, actor or trace id")
        .param("limit", "rows, default 500")
        .param("buckets", "lane buckets, default 60")
        .route("GET", "/timeline"),
        Tool::new("get_timeline_row", "One Timeline row and the rest of its journey (rows with the same trace id).", |ctx, a| async move {
            let id = a.need("id")?;
            let row = trips::one(&ctx, &id).await?.ok_or_else(|| ApiError::not_found(format!("No Timeline row {id}")))?;
            let mut journey = match row["traceId"].as_str() { Some(t) => trips::same_trace(&ctx, t).await?, None => vec![] };
            let mut one = [row];
            attribute(&ctx, &mut one).await;
            attribute(&ctx, &mut journey).await;
            let [row] = one;
            Ok(json!({"row": row, "journey": journey}))
        })
        .req("id", "row id")
        .route("GET", "/timeline/{id}"),
        Tool::new("wait_for_timeline", "Waits until a row that matches appears (after `since`, default now) and returns it; 408 after the timeout.", |ctx, a| async move {
            let since = a.u64("since").map(|x| x as i64).unwrap_or_else(util::now_ms);
            let limit = a.ms("timeout")?.unwrap_or(10_000);
            let end = std::time::Instant::now() + std::time::Duration::from_millis(limit);
            loop {
                let rows = trips::list(&ctx, trips::Filter { limit: 1, ..filter(&a, since, i64::MAX) }).await?;
                if let Some(r) = rows.into_iter().next() { return Ok(r); }
                if std::time::Instant::now() >= end { return Err(ApiError::new(408, format!("No matching Timeline row within {limit} ms"))); }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        })
        .param("service", "as in timeline").param("who", "as in timeline").param("show", "as in timeline").param("q", "as in timeline")
        .param("since", "epoch ms, default now").param("timeout", "e.g. 10s (default)")
        .route("POST", "/timeline/wait"),
    ]
}
