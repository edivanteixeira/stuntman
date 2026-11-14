//! Telemetry tools (T1–T4 "API & MCP" sheets).

use std::time::Duration;

use stuntman_core::{tools::Args, trips, util, ApiError, Ctx, Tool};
use serde_json::{json, Value};

use crate::{config, metrics, query, receiver, store};

fn since(a: &Args, default: &str) -> Result<i64, ApiError> {
    Ok(query::since(a.str("since").or(Some(default.into())))?.unwrap_or(0))
}

pub fn endpoints(ctx: &Ctx) -> Value {
    let host = ctx.host.split(':').next().unwrap_or("localhost").to_string();
    let (h, g) = crate::ports();
    let p = ctx.prefix();
    let grpc_header = if p.is_empty() { String::new() } else { format!("\n# gRPC: OTEL_EXPORTER_OTLP_HEADERS=x-stuntman-env={}", ctx.env) };
    json!({"http": format!("http://{host}:{h}{p}"), "grpc": format!("{host}:{g}"),
           "env": format!("OTEL_EXPORTER_OTLP_ENDPOINT=http://{host}:{h}{p}\nOTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf\nOTEL_SERVICE_NAME=acme-api{grpc_header}")})
}

pub fn all() -> Vec<Tool> {
    let mut v = traces();
    v.extend(logs());
    v.extend(settings());
    v.extend(metrics_tools());
    v
}

fn traces() -> Vec<Tool> {
    vec![
        Tool::new("telemetry_overview", "What the receiver got: services, counts, last data, dropped and refused batches, the provoked answer, endpoints.", |ctx, a| async move {
            let s = since(&a, "15m")?;
            let mut o = query::overview(&ctx, s).await?;
            o["dropped"] = json!({"traces": receiver::dropped_since("traces", 900_000), "logs": receiver::dropped_since("logs", 900_000), "metrics": receiver::dropped_since("metrics", 900_000)});
            let (refused, half, fwd) = { let st = receiver::stats().lock().unwrap(); (st.refused, st.dropped_half, st.last_forward.clone()) };
            o["refused"] = json!(refused);
            o["droppedHalf"] = json!(half);
            o["lastForward"] = json!(fwd);
            o["endpoints"] = endpoints(&ctx);
            o["listening"] = json!(receiver::bind_error().lock().unwrap().clone());
            o["settings"] = json!(config::get(&ctx)?);
            o["usedBytes"] = json!(store::used_bytes(&ctx).await?);
            Ok(o)
        })
        .param("since", "15m, 1h, 24h").route("GET", "/telemetry/overview"),
        Tool::new("list_traces", "Traces with duration, span count, status and how many spans reached Stuntman, newest first.", |ctx, a| async move {
            let f = query::TraceFilter {
                service: a.str("service"),
                status: a.str("status"),
                slower_than_ms: a.ms("slower_than")?.map(|x| x as i64),
                touching: a.bool("touching_provider").unwrap_or(false),
                since_ms: Some(since(&a, "15m")?),
                route: a.str("route"),
                limit: a.u64("limit").unwrap_or(100) as u32,
            };
            Ok(json!({"traces": query::traces(&ctx, f).await?}))
        })
        .param("service", "service.name").param("status", "error | ok").param("slower_than", "e.g. 1s").param("touching_provider", "true: only traces that reached Stuntman")
        .param("since", "default 15m").param("route", "part of the root span name").param("limit", "default 100").route("GET", "/telemetry/traces"),
        Tool::new("get_trace", "The span tree; provider spans carry the trip, what answered it, and a link.", |ctx, a| async move {
            query::trace(&ctx, &a.need("trace_id")?).await
        })
        .req("trace_id", "32 hex characters").route("GET", "/telemetry/traces/{trace_id}"),
        Tool::new("wait_for_trace", "The first matching trace: what an agent waits for after acting in the app. Looks back 15 s by default.", |ctx, a| async move {
            let route = a.str("route").or(a.str("span_name")).ok_or_else(|| ApiError::bad("route or span_name is required"))?;
            let timeout = a.ms("timeout")?.unwrap_or(30_000).min(300_000) as i64;
            let started = util::now_ms();
            let from = query::since(a.str("since"))?.unwrap_or(started - 15_000);
            let mut sub = ctx.live.subscribe();
            loop {
                let f = query::TraceFilter { route: Some(route.clone()), status: a.str("status"), since_ms: Some(from), limit: 5, ..Default::default() };
                if let Some(t) = query::traces(&ctx, f).await?.into_iter().next() {
                    return query::trace(&ctx, t["traceId"].as_str().unwrap_or("")).await;
                }
                let left = started + timeout - util::now_ms();
                if left <= 0 {
                    return Err(ApiError::new(408, format!("No trace “{route}” in {}", util::fmt_ms(timeout as u64))));
                }
                let _ = tokio::time::timeout(Duration::from_millis(left.min(1000) as u64), sub.recv()).await;
            }
        })
        .param("route", "e.g. POST /orders").param("span_name", "any span name").param("status", "error | ok").param("timeout", "default 30s").param("since", "default 15s")
        .route("GET", "/telemetry/traces/wait"),
        Tool::new("trace_of", "From a trip, message or delivery back to the app trace that caused it.", |ctx, a| async move {
            let id = a.str("trip_id").or(a.str("message_id")).or(a.str("id")).ok_or_else(|| ApiError::bad("trip_id or message_id is required"))?;
            let t = trips::get(&ctx, &id).await?.ok_or_else(|| ApiError::not_found(format!("No trip {id}")))?;
            let tid = t["traceId"].as_str().ok_or_else(|| ApiError::not_found(format!("{id} came without a trace id: the app does not propagate trace context there")))?.to_string();
            let mut tr = query::trace(&ctx, &tid).await.unwrap_or_else(|_| json!({"traceId": tid, "spans": [], "note": "the trace id is known but no spans arrived: is the app's trace exporter on?"}));
            tr["trip"] = t;
            Ok(tr)
        })
        .param("trip_id", "msg_0412, dlv_0042…").param("message_id", "same").route("GET", "/telemetry/trips/{trip_id}/trace"),
        Tool::new("find_slow_spans", "Where the time went: spans over the threshold, provider spans first.", |ctx, a| async move {
            let over = a.ms("over_ms")?.or(a.u64("over_ms")).unwrap_or(500) as f64;
            let t = query::trace(&ctx, &a.need("trace_id")?).await?;
            let mut s: Vec<Value> = t["spans"].as_array().cloned().unwrap_or_default().into_iter().filter(|s| s["durationMs"].as_f64().unwrap_or(0.0) >= over || s["reachedStuntman"] == true).collect();
            s.sort_by(|a, b| (b["reachedStuntman"] == true).cmp(&(a["reachedStuntman"] == true)).then(b["durationMs"].as_f64().partial_cmp(&a["durationMs"].as_f64()).unwrap_or(std::cmp::Ordering::Equal)));
            Ok(json!({"traceId": t["traceId"], "spans": s}))
        })
        .req("trace_id", "trace").param("over_ms", "default 500").route("GET", "/telemetry/traces/{trace_id}/slow"),
    ]
}

fn log_filter(a: &Args, default_since: &str) -> Result<query::LogFilter, ApiError> {
    Ok(query::LogFilter {
        service: a.list("service"),
        level: a.list("level"),
        text: a.str("text"),
        trace_id: a.str("trace_id"),
        since_ms: Some(since(a, default_since)?),
        limit: a.u64("limit").unwrap_or(100) as u32,
        before_seq: a.u64("before_seq").map(|x| x as i64),
        after_seq: a.u64("after_seq").map(|x| x as i64),
    })
}

fn logs() -> Vec<Tool> {
    vec![
        Tool::new("list_logs", "Newest first, with level, service, message, trace id and attributes; facets give counts per level and service.", |ctx, a| async move {
            let f = log_filter(&a, "15m")?;
            let s = f.since_ms;
            let (v, total) = query::logs(&ctx, f, false).await?;
            Ok(json!({"logs": v, "total": total, "facets": query::log_facets(&ctx, s).await?}))
        })
        .param("service", "list").param("level", "error, warn, info, debug (list)").param("text", "words in the message or attributes, or a trace id")
        .param("trace_id", "trace").param("since", "default 15m").param("limit", "default 100").param("before_seq", "page back").route("GET", "/telemetry/logs"),
        Tool::new("get_log", "One log line.", |ctx, a| async move { query::log(&ctx, &a.need("log_id")?).await }).req("log_id", "log_42").route("GET", "/telemetry/logs/{log_id}"),
        Tool::new("wait_for_log", "The first matching line: what an agent waits for after acting in the app. Looks back 15 s by default.", |ctx, a| async move {
            if a.str("text").is_none() && a.str("level").is_none() {
                return Err(ApiError::bad("text or level is required"));
            }
            let timeout = a.ms("timeout")?.unwrap_or(30_000).min(300_000) as i64;
            let started = util::now_ms();
            let mut f = log_filter(&a, "15s")?;
            f.since_ms = Some(query::since(a.str("since"))?.unwrap_or(started - 15_000));
            f.limit = 1;
            let mut sub = ctx.live.subscribe();
            loop {
                if let Some(l) = query::logs(&ctx, f.clone(), true).await?.0.into_iter().next() {
                    return Ok(l);
                }
                let left = started + timeout - util::now_ms();
                if left <= 0 {
                    return Err(ApiError::new(408, format!("No matching log line in {}", util::fmt_ms(timeout as u64))));
                }
                let _ = tokio::time::timeout(Duration::from_millis(left.min(1000) as u64), sub.recv()).await;
            }
        })
        .param("text", "words").param("level", "error…").param("service", "service.name").param("timeout", "default 30s").param("since", "default 15s").route("GET", "/telemetry/logs/wait"),
        Tool::new("logs_of_trace", "Every line the app wrote while serving one trace, in order.", |ctx, a| async move {
            let f = query::LogFilter { trace_id: Some(a.need("trace_id")?), limit: 2000, ..Default::default() };
            Ok(json!({"logs": query::logs(&ctx, f, true).await?.0}))
        })
        .req("trace_id", "trace").route("GET", "/telemetry/traces/{trace_id}/logs"),
        Tool::new("lines_around", "The lines just before and after one line, across services.", |ctx, a| async move {
            query::around(&ctx, &a.need("log_id")?, a.u64("before").unwrap_or(20) as u32, a.u64("after").unwrap_or(20) as u32).await
        })
        .req("log_id", "log_42").param("before", "default 20").param("after", "default 20").route("GET", "/telemetry/logs/{log_id}/around"),
        Tool::new("count_logs", "Quick check that an error stopped happening after a fix.", |ctx, a| async move {
            let f = log_filter(&a, "15m")?;
            Ok(json!({"count": query::logs(&ctx, f, false).await?.1}))
        })
        .param("level", "list").param("service", "list").param("text", "words").param("since", "default 15m").route("GET", "/telemetry/logs/count"),
    ]
}

async fn settings_view(ctx: &Ctx) -> Result<Value, ApiError> {
    let s = config::get(ctx)?;
    let ov = query::overview(ctx, util::now_ms() - 900_000).await?;
    Ok(json!({"settings": s, "yaml": serde_yaml::to_string(&s).unwrap_or_default(), "file": format!("telemetry/{}.yaml", ctx.env),
        "endpoints": endpoints(ctx), "usedBytes": store::used_bytes(ctx).await?, "maxBytes": s.storage_bytes(), "last15min": ov,
        "lastForward": receiver::stats().lock().unwrap().last_forward.clone(), "answers": config::ANSWERS,
        "lastChange": config::last_change().lock().unwrap().clone().map(|(by, at)| json!({"by": by, "at": at}))}))
}

async fn store_settings(ctx: &Ctx, s: &config::Settings, by: &str) -> Result<Value, ApiError> {
    config::save_by(ctx, s, by)?;
    receiver::reconcile(&ctx.envs).await;
    settings_view(ctx).await
}

fn settings() -> Vec<Tool> {
    vec![
        Tool::new("get_telemetry_settings", "Endpoints, signals with keep, collector answer, forwarding, storage used.", |ctx, _a| async move { settings_view(&ctx).await })
            .route("GET", "/telemetry/settings"),
        Tool::new("put_telemetry_settings", "Replaces the settings (Save on T3): a settings object, or yaml with the whole file.", |ctx, a| async move {
            let s: config::Settings = match (a.get("settings"), a.str("yaml")) {
                (Some(v), _) => serde_json::from_value(v.clone()).map_err(|e| ApiError::bad(format!("settings: {e}")))?,
                (None, Some(y)) => serde_yaml::from_str(&y).map_err(|e| ApiError::bad(format!("YAML: {e}")))?,
                _ => return Err(ApiError::bad("settings or yaml is required")),
            };
            store_settings(&ctx, &s, &a.str("_by").unwrap_or("you".into())).await
        })
        .param("settings", "the whole object").param("yaml", "or the whole file").route("PUT", "/telemetry/settings"),
        Tool::new("set_signal", "Turn traces, logs or metrics on or off; off means answered 200 and dropped.", |ctx, a| async move {
            let mut s = config::get(&ctx)?;
            let sig = match a.need("signal")?.as_str() {
                "traces" => &mut s.signals.traces,
                "logs" => &mut s.signals.logs,
                "metrics" => &mut s.signals.metrics,
                o => return Err(ApiError::bad(format!("signal: traces | logs | metrics (got “{o}”)"))),
            };
            if let Some(on) = a.bool("on") {
                sig.on = on;
            }
            if let Some(k) = a.str("keep") {
                sig.keep = k;
            }
            store_settings(&ctx, &s, &a.str("_by").unwrap_or("you".into())).await
        })
        .req("signal", "traces | logs | metrics").param("on", "bool").param("keep", "24h, 7d").route("PUT", "/telemetry/signals/{signal}"),
        Tool::new("provoke_collector", "403, 401, 429, 503, refused, slow or drop_half; ends by itself.", |ctx, a| async move {
            let mut s = config::get(&ctx)?;
            let answer = a.need("answer")?;
            let ms = a.ms("for")?.unwrap_or(600_000);
            s.collector.answer = answer;
            s.collector.only_for = a.str("only_for").unwrap_or("all".into());
            s.collector.since = Some(util::now_ms());
            s.collector.until = Some(util::now_ms() + ms as i64);
            s.collector.set_by = a.str("_by").unwrap_or("you".into());
            receiver::stats().lock().unwrap().refused = 0;
            let v = store_settings(&ctx, &s, &a.str("_by").unwrap_or("you".into())).await?;
            let c = ctx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                if let Ok(mut s) = config::get(&c) {
                    if s.collector.until.map(|u| u <= util::now_ms()).unwrap_or(false) {
                        s.collector.answer = "normal".into();
                        s.collector.until = None;
                        s.collector.since = None;
                        let _ = config::save(&c, &s);
                        receiver::reconcile(&c.envs).await;
                    }
                }
            });
            Ok(v)
        })
        .req("answer", "403 | 401 | 429 | 503 | refused | slow | drop_half").param("only_for", "all or a service.name").param("for", "default 10m")
        .route("POST", "/telemetry/collector/provoke"),
        Tool::new("stop_provoking", "The collector answers normally again; the strip goes away.", |ctx, a| async move {
            let mut s = config::get(&ctx)?;
            s.collector.answer = "normal".into();
            s.collector.only_for = "all".into();
            s.collector.since = None;
            s.collector.until = None;
            store_settings(&ctx, &s, &a.str("_by").unwrap_or("you".into())).await
        })
        .route("DELETE", "/telemetry/collector/provoke"),
        Tool::new("set_forwarding", "Copy everything to a real APM; the key is a Secrets reference, never a value.", |ctx, a| async move {
            let mut s = config::get(&ctx)?;
            if let Some(on) = a.bool("on") {
                s.forward.enabled = on;
            }
            if let Some(e) = a.str("endpoint") {
                s.forward.endpoint = e;
            }
            if let Some(h) = a.str("header_secret") {
                if !h.starts_with("secret://") && !h.starts_with("env:") {
                    return Err(ApiError::bad("header_secret: a reference like secret://apm-license-key, never the key itself"));
                }
                let name = a.str("header").unwrap_or("api-key".into());
                s.forward.headers.insert(name, h);
            }
            store_settings(&ctx, &s, &a.str("_by").unwrap_or("you".into())).await
        })
        .param("on", "bool").param("endpoint", "https://host:4318").param("header_secret", "secret://apm-license-key").param("header", "default api-key")
        .route("PUT", "/telemetry/forwarding"),
        Tool::new("clear_telemetry", "Deletes traces, logs and metrics of the environment; settings stay.", |ctx, a| async move {
            if a.bool("confirm") != Some(true) {
                return Err(ApiError::bad("Pass confirm=true: this cannot be undone"));
            }
            store::clear(&ctx).await
        })
        .req("confirm", "true").route("DELETE", "/telemetry/data"),
    ]
}

fn metrics_tools() -> Vec<Tool> {
    vec![
        Tool::new("list_metrics", "Name, type, unit, category, services and number of series.", |ctx, a| async move {
            let (svc, text) = (a.str("service"), a.str("text"));
            let rows = crate::db(&ctx)
                .call(move |c| {
                    let mut st = c.prepare("SELECT name, kind, unit, MAX(description), GROUP_CONCAT(DISTINCT service), COUNT(DISTINCT series), MAX(time_ns) FROM tel_points
                        WHERE (?1 IS NULL OR service = ?1) AND (?2 IS NULL OR name LIKE '%' || ?2 || '%' OR attrs LIKE '%' || ?2 || '%') GROUP BY name ORDER BY name")?;
                    let v = st
                        .query_map(rusqlite::params![svc, text], |r| {
                            let name: String = r.get(0)?;
                            Ok(json!({"name": name, "category": metrics::category(&name), "kind": r.get::<_, String>(1)?, "unit": r.get::<_, String>(2)?, "description": r.get::<_, String>(3)?,
                                "services": r.get::<_, String>(4)?.split(',').collect::<Vec<_>>(), "series": r.get::<_, i64>(5)?, "last": r.get::<_, i64>(6)? / 1_000_000}))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>();
                    v
                })
                .await?;
            Ok(json!({"metrics": rows}))
        })
        .param("service", "service.name").param("text", "metric name or attribute").route("GET", "/telemetry/metrics"),
        Tool::new("query_metric", "One value per step, per breakdown value. stat: count, rate, sum, avg, p50, p95, p99, last, errors.", |ctx, a| async move {
            let name = a.need("name")?;
            let to = util::now_ms();
            let from = query::since(a.str("since").or(Some("15m".into())))?.unwrap_or(to - 900_000);
            let step = a.ms("step")?.unwrap_or(60_000).max(1000) as i64;
            let pts = metrics::points(&ctx, &name, from, to).await?;
            if pts.is_empty() {
                return Err(ApiError::not_found(format!("No points for {name} in this window")));
            }
            let mut v = metrics::series(&pts, &a.str("stat").unwrap_or("p95".into()), a.str("by").as_deref(), from, to, step);
            v["name"] = json!(name);
            let mut keys: Vec<String> = pts.iter().flat_map(|p| p.attrs.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default()).collect();
            keys.sort();
            keys.dedup();
            v["attributes"] = json!(keys);
            v["hosts"] = json!(v["series"].as_array().map(|s| s.iter().map(|x| { let k = x["key"].as_str().unwrap_or(""); json!({"key": k, "stuntman": store::host_of(&json!({"server.address": k})).1}) }).collect::<Vec<_>>()));
            Ok(v)
        })
        .req("name", "metric name").param("stat", "p95 (default), count, rate, sum, avg, p50, p99, last, errors").param("by", "an attribute, e.g. server.address")
        .param("since", "default 15m").param("step", "default 1m").route("GET", "/telemetry/metrics/{name}"),
        Tool::new("compare_windows", "The two values and the change, to say whether a fix or a rule made a difference. Windows: 30m-15m (ago) or epoch-ms..epoch-ms.", |ctx, a| async move {
            let name = a.need("name")?;
            let stat = a.str("stat").unwrap_or("p95".into());
            let (b0, b1) = metrics::window(&a.need("before")?)?;
            let (a0, a1) = metrics::window(&a.need("after")?)?;
            let by = a.str("by");
            let one = |f: i64, t: i64| {
                let (ctx, name, stat, by) = (ctx.clone(), name.clone(), stat.clone(), by.clone());
                async move {
                    let p = metrics::points(&ctx, &name, f, t).await?;
                    Ok::<Value, ApiError>(metrics::series(&p, &stat, by.as_deref(), f, t, (t - f).max(1000)))
                }
            };
            let (before, after) = (one(b0, b1).await?, one(a0, a1).await?);
            let mut changes = vec![];
            for s in before["series"].as_array().cloned().unwrap_or_default() {
                let k = s["key"].clone();
                let after_v = after["series"].as_array().and_then(|x| x.iter().find(|y| y["key"] == k).map(|y| y["value"].clone())).unwrap_or(Value::Null);
                let (bv, av) = (s["value"].as_f64(), after_v.as_f64());
                changes.push(json!({"key": k, "before": bv, "after": av, "change": match (bv, av) { (Some(b), Some(a)) if b != 0.0 => json!((a - b) / b), _ => Value::Null }}));
            }
            Ok(json!({"name": name, "stat": stat, "by": by, "changes": changes}))
        })
        .req("name", "metric").req("before", "e.g. 30m-15m").req("after", "e.g. 15m-0").param("stat", "default p95").param("by", "attribute")
        .route("GET", "/telemetry/metrics/{name}/compare"),
        Tool::new("metric_exemplars", "The trace ids the app attached to points near a time; open them with get_trace.", |ctx, a| async move {
            let name = a.need("name")?;
            let at = a.u64("at").map(|x| x as i64).unwrap_or(util::now_ms());
            let step = a.ms("step")?.unwrap_or(60_000) as i64;
            let pts = metrics::points(&ctx, &name, at - step, at + step).await?;
            let by = a.str("by");
            let want = a.str("by_value");
            let mut ex: Vec<Value> = vec![];
            for p in pts.iter().filter(|p| (p.time_ns / 1_000_000 - at).abs() <= step) {
                if let Some(w) = &want {
                    if &metrics::key_of(p, by.as_deref()) != w {
                        continue;
                    }
                }
                for e in p.exemplars.as_array().cloned().unwrap_or_default() {
                    ex.push(json!({"traceId": e["traceId"], "spanId": e["spanId"], "value": e["value"], "at": e["timeNs"].as_i64().unwrap_or(0) / 1_000_000, "attributes": p.attrs}));
                }
            }
            ex.sort_by(|a, b| b["value"].as_f64().partial_cmp(&a["value"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
            Ok(json!({"exemplars": ex}))
        })
        .req("name", "metric").param("at", "epoch ms").param("by", "attribute").param("by_value", "only this breakdown value").param("step", "default 1m")
        .route("GET", "/telemetry/metrics/{name}/exemplars"),
        Tool::new("top_series", "How many values an attribute has: finds the attribute that will cost money in a real APM.", |ctx, a| async move {
            let (name, attr) = (a.need("name")?, a.need("attribute")?);
            let s = query::since(a.str("since").or(Some("1h".into())))?.unwrap_or(0) * 1_000_000;
            let path = format!("$.\"{}\"", attr.replace('"', ""));
            let v = crate::db(&ctx)
                .call(move |c| {
                    let distinct: i64 = c.query_row("SELECT COUNT(DISTINCT json_extract(attrs, ?2)) FROM tel_points WHERE name = ?1 AND time_ns >= ?3", rusqlite::params![name, path, s], |r| r.get(0))?;
                    let mut st = c.prepare("SELECT json_extract(attrs, ?2) v, COUNT(*) FROM tel_points WHERE name = ?1 AND time_ns >= ?3 GROUP BY v ORDER BY 2 DESC LIMIT 20")?;
                    let top = st.query_map(rusqlite::params![name, path, s], |r| Ok(json!({"value": r.get::<_, Option<String>>(0)?, "points": r.get::<_, i64>(1)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(json!({"distinct": distinct, "top": top}))
                })
                .await?;
            Ok(v)
        })
        .req("name", "metric").req("attribute", "e.g. user.id").param("since", "default 1h").route("GET", "/telemetry/metrics/{name}/series"),
        Tool::new("why_it_jumped", "Finds steps where a metric jumped (over twice its median) and says which breakdown value drove it, its errors, and what Stuntman answered then.", |ctx, a| async move {
            let name = a.need("name")?;
            let to = util::now_ms();
            let from = query::since(a.str("since").or(Some("15m".into())))?.unwrap_or(to - 900_000);
            let step = a.ms("step")?.unwrap_or(60_000).max(1000) as i64;
            let stat = a.str("stat").unwrap_or("p95".into());
            let by = a.str("by").unwrap_or("server.address".into());
            let pts = metrics::points(&ctx, &name, from, to).await?;
            let all = metrics::series(&pts, &stat, None, from, to, step);
            let vals: Vec<(i64, f64)> = all["series"][0]["points"].as_array().cloned().unwrap_or_default().iter().filter_map(|p| Some((p["t"].as_i64()?, p["v"].as_f64()?))).collect();
            if vals.len() < 3 {
                return Ok(json!({"jump": null, "note": "not enough points to tell a jump from noise"}));
            }
            let mut sorted: Vec<f64> = vals.iter().map(|v| v.1).collect();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let median = sorted[sorted.len() / 2];
            let high: Vec<i64> = vals.iter().filter(|v| v.1 > median * 2.0 && v.1 > 0.0).map(|v| v.0).collect();
            let (Some(a0), Some(a1)) = (high.first().copied(), high.last().copied()) else {
                return Ok(json!({"jump": null, "median": median}));
            };
            let (j0, j1) = (a0, a1 + step);
            let inside = metrics::series(&pts, &stat, Some(&by), j0, j1, (j1 - j0).max(1000));
            let outside = metrics::series(&pts, &stat, Some(&by), from, j0.max(from + 1000), (j0 - from).max(1000));
            let mut drivers = vec![];
            for s in inside["series"].as_array().cloned().unwrap_or_default() {
                let k = s["key"].clone();
                let before = outside["series"].as_array().and_then(|x| x.iter().find(|y| y["key"] == k).and_then(|y| y["value"].as_f64()));
                drivers.push(json!({"key": k, "during": s["value"], "before": before, "calls": s["count"], "errors": s["errors"],
                    "stuntman": store::host_of(&json!({"server.address": k})).1}));
            }
            drivers.sort_by(|a, b| b["during"].as_f64().partial_cmp(&a["during"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
            let provoked: Vec<Value> = trips::between(&ctx, j0 - step, j1 + step).await?.into_iter()
                .filter(|t| { let st = t["status"].as_str().unwrap_or(""); ["bounced", "dropped", "deferred"].contains(&st) || st.starts_with('4') || st.starts_with('5') || ["reset", "empty", "no answer"].contains(&st) })
                .collect();
            // Name each rule the way Stuntman's own screens do: the inbox (or host) that answered and how.
            let mut rules: Vec<Value> = vec![];
            for t in &provoked {
                let module = t["module"].as_str().unwrap_or("");
                let target = match module {
                    "email" => t["label"].as_str().and_then(|l| l.rsplit("→ ").next()).unwrap_or("").trim().to_string(),
                    "webhook" => t["link"].as_str().and_then(|l| l.rsplit('/').next()).unwrap_or("").to_string(),
                    _ => t["host"].as_str().unwrap_or("").to_string(),
                };
                let name = t["rule"].as_str().filter(|r| !r.is_empty()).map(String::from).unwrap_or_else(|| format!("{target} · {}", t["status"].as_str().unwrap_or("")));
                match rules.iter_mut().find(|r| r["name"] == name.as_str()) {
                    Some(r) => r["count"] = json!(r["count"].as_i64().unwrap_or(0) + 1),
                    None => {
                        let link = match module {
                            "email" => format!("/inboxes/{target}"),
                            "webhook" => format!("/inboxes/webhooks/{target}/behaviour"),
                            _ => t["link"].as_str().unwrap_or("").to_string(),
                        };
                        rules.push(json!({"name": name, "module": module, "count": 1, "link": link, "status": t["status"]}))
                    }
                }
            }
            let collector = config::get(&ctx)?.collector;
            Ok(json!({"jump": {"from": j0, "to": j1, "median": median, "peak": vals.iter().map(|v| v.1).fold(0.0_f64, f64::max)}, "by": by, "drivers": drivers, "provoked": provoked, "rules": rules,
                      "ruleOn": !provoked.is_empty(), "collector": collector}))
        })
        .req("name", "metric").param("stat", "default p95").param("by", "default server.address").param("since", "default 15m").param("step", "default 1m")
        .route("GET", "/telemetry/metrics/{name}/why"),
    ]
}
