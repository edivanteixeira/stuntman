//! T1 traces and T2 logs, read back for the screens, tests and agents.

use stuntman_core::{trips, util, ApiError, Ctx};
use rusqlite::{params, OptionalExtension, Row};
use serde_json::{json, Value};

fn ms(ns: i64) -> i64 {
    ns / 1_000_000
}

#[derive(Default, Clone)]
pub struct TraceFilter {
    pub service: Option<String>,
    /// error | ok
    pub status: Option<String>,
    pub slower_than_ms: Option<i64>,
    pub touching: bool,
    pub since_ms: Option<i64>,
    pub route: Option<String>,
    pub limit: u32,
}

/// One row per trace: the root span, duration, span count, errors, and spans that reached Stuntman.
pub async fn traces(ctx: &Ctx, f: TraceFilter) -> Result<Vec<Value>, ApiError> {
    let since_ns = f.since_ms.map(|s| s * 1_000_000).unwrap_or(0);
    let lim = f.limit.clamp(1, 500) as i64 * 4;
    let rows: Vec<Value> = crate::db(ctx)
        .call(move |c| {
            let mut st = c.prepare(
                "SELECT trace_id, MIN(start_ns), MAX(end_ns), COUNT(*), MAX(status = 2), SUM(dub),
                   GROUP_CONCAT(DISTINCT service)
                 FROM tel_spans INDEXED BY tel_spans_start WHERE start_ns >= ?1 GROUP BY trace_id ORDER BY MIN(start_ns) DESC LIMIT ?2",
            )?;
            let v = st
                .query_map(params![since_ns, lim], |r| {
                    Ok(json!({"traceId": r.get::<_, String>(0)?, "startNs": r.get::<_, i64>(1)?, "endNs": r.get::<_, i64>(2)?, "spans": r.get::<_, i64>(3)?,
                              "anyError": r.get::<_, bool>(4)?, "dubSpans": r.get::<_, i64>(5)?, "services": r.get::<_, String>(6)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await?;
    let mut out = vec![];
    for mut t in rows {
        let tid = t["traceId"].as_str().unwrap_or("").to_string();
        let tid2 = tid.clone();
        let root = crate::db(ctx)
        .call(move |c| {
                c.query_row(
                    "SELECT name, service, attrs, status, start_ns, end_ns FROM tel_spans WHERE trace_id = ?1 ORDER BY (parent_id = '' OR parent_id = '0000000000000000') DESC, start_ns LIMIT 1",
                    [tid2],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?)),
                )
                .optional()
            })
            .await?;
        let Some((name, svc, attrs, rst, rs, re)) = root else { continue };
        let a: Value = serde_json::from_str(&attrs).unwrap_or(json!({}));
        let code = a.get("http.response.status_code").or(a.get("http.status_code")).and_then(|v| v.as_i64().or(v.as_str().and_then(|s| s.parse().ok())));
        let trips = trips::of_trace(ctx, &tid).await?;
        let dur = ms(re.max(t["endNs"].as_i64().unwrap_or(0)) - rs);
        let error = rst == 2 || code.map(|c| c >= 500).unwrap_or(false) || t["anyError"] == true && code.is_none();
        t["name"] = json!(name);
        t["service"] = json!(svc);
        t["statusCode"] = json!(code);
        t["error"] = json!(error);
        t["durationMs"] = json!(dur);
        t["at"] = json!(ms(rs));
        t["trips"] = json!(trips.len());
        t["touchesProvider"] = json!(t["dubSpans"].as_i64().unwrap_or(0) > 0 || !trips.is_empty());
        if let Some(s) = &f.service {
            if !t["services"].as_str().unwrap_or("").split(',').any(|x| x == s) {
                continue;
            }
        }
        match f.status.as_deref() {
            Some("error") if !error => continue,
            Some("ok") if error => continue,
            _ => {}
        }
        if f.slower_than_ms.map(|s| dur <= s).unwrap_or(false) || (f.touching && t["touchesProvider"] != true) {
            continue;
        }
        if let Some(r) = &f.route {
            if !name.contains(r.as_str()) {
                continue;
            }
        }
        out.push(t);
        if out.len() >= f.limit as usize {
            break;
        }
    }
    Ok(out)
}

fn span_row(r: &Row) -> rusqlite::Result<Value> {
    let j = |i: usize| -> rusqlite::Result<Value> { Ok(serde_json::from_str(&r.get::<_, String>(i)?).unwrap_or(Value::Null)) };
    Ok(json!({"spanId": r.get::<_, String>(0)?, "parentId": r.get::<_, String>(1)?, "name": r.get::<_, String>(2)?, "service": r.get::<_, String>(3)?,
        "kind": r.get::<_, i64>(4)?, "startNs": r.get::<_, i64>(5)?, "endNs": r.get::<_, i64>(6)?, "status": r.get::<_, i64>(7)?, "statusMessage": r.get::<_, String>(8)?,
        "attributes": j(9)?, "events": j(10)?, "host": r.get::<_, String>(11)?, "reachedStuntman": r.get::<_, bool>(12)?}))
}

/// The span tree, depth-first, with offsets; client spans matched to the trips they caused.
pub async fn trace(ctx: &Ctx, trace_id: &str) -> Result<Value, ApiError> {
    let tid = trace_id.to_lowercase();
    let t2 = tid.clone();
    let mut spans = crate::db(ctx)
        .call(move |c| {
            let mut st = c.prepare("SELECT span_id, parent_id, name, service, kind, start_ns, end_ns, status, status_msg, attrs, events, host, dub FROM tel_spans WHERE trace_id = ?1 ORDER BY start_ns")?;
            let v = st.query_map([t2], span_row)?.collect::<rusqlite::Result<Vec<_>>>();
            v
        })
        .await?;
    if spans.is_empty() {
        let trips = trips::of_trace(ctx, &tid).await?;
        return Err(ApiError::not_found(format!("No spans for trace {tid}")).with(json!({"trips": trips})));
    }
    let start = spans.iter().filter_map(|s| s["startNs"].as_i64()).min().unwrap_or(0);
    let end = spans.iter().filter_map(|s| s["endNs"].as_i64()).max().unwrap_or(start);
    // Trips of this trace go to the client span that was open when the trip arrived (closest one).
    let mut trips_left = trips::of_trace(ctx, &tid).await?;
    for s in spans.iter_mut() {
        s["offsetMs"] = json!((s["startNs"].as_i64().unwrap_or(0) - start) as f64 / 1e6);
        s["durationMs"] = json!((s["endNs"].as_i64().unwrap_or(0) - s["startNs"].as_i64().unwrap_or(0)) as f64 / 1e6);
        let exc = s["events"].as_array().and_then(|e| e.iter().find(|x| x["name"] == "exception").cloned());
        if let Some(e) = exc {
            s["exception"] = e["attributes"].clone();
        }
    }
    // Each trip goes to a client span: one whose name or URL mentions the trip's host first, else
    // one that reached Stuntman; the nearest in time wins (clocks of the app and Stuntman may differ).
    let mut i = 0;
    while i < trips_left.len() {
        let t = &trips_left[i];
        let (at, host) = (t["at"].as_i64().unwrap_or(0), t["host"].as_str().unwrap_or("").to_lowercase());
        let score = |s: &Value| -> Option<(u8, i64)> {
            if !(s["kind"] == 3 || s["kind"] == 4) || s.get("trip").is_some() {
                return None;
            }
            let text = format!("{} {} {}", s["name"], s["host"], s["attributes"]).to_lowercase();
            let rank = if !host.is_empty() && text.contains(&host) { 0 } else if s["reachedStuntman"] == true { 1 } else { return None };
            Some((rank, (s["startNs"].as_i64().unwrap_or(0) / 1_000_000 - at).abs()))
        };
        if let Some(j) = (0..spans.len()).filter_map(|j| score(&spans[j]).map(|sc| (sc, j))).min().map(|x| x.1) {
            let t = trips_left.remove(i);
            spans[j]["trip"] = t;
            spans[j]["reachedStuntman"] = json!(true);
        } else {
            i += 1;
        }
    }
    // Depth from the parent chain, then a depth-first order.
    let ids: Vec<String> = spans.iter().map(|s| s["spanId"].as_str().unwrap_or("").to_string()).collect();
    let mut order: Vec<usize> = vec![];
    fn walk(i: usize, depth: usize, spans: &mut Vec<Value>, ids: &[String], order: &mut Vec<usize>) {
        spans[i]["depth"] = json!(depth);
        order.push(i);
        let me = ids[i].clone();
        let kids: Vec<usize> = (0..spans.len()).filter(|j| spans[*j]["parentId"].as_str() == Some(me.as_str()) && *j != i).collect();
        for k in kids {
            walk(k, depth + 1, spans, ids, order);
        }
    }
    let roots: Vec<usize> = (0..spans.len()).filter(|i| { let p = spans[*i]["parentId"].as_str().unwrap_or(""); p.is_empty() || !ids.iter().any(|x| x == p) }).collect();
    for r in roots {
        walk(r, 0, &mut spans, &ids, &mut order);
    }
    let ordered: Vec<Value> = order.into_iter().map(|i| spans[i].clone()).collect();
    let root = ordered.first().cloned().unwrap_or(Value::Null);
    Ok(json!({"traceId": tid, "name": root["name"], "service": root["service"], "durationMs": (end - start) as f64 / 1e6, "startMs": start / 1_000_000,
              "spans": ordered, "unmatchedTrips": trips_left}))
}

#[derive(Default, Clone)]
pub struct LogFilter {
    pub service: Vec<String>,
    pub level: Vec<String>,
    pub text: Option<String>,
    pub trace_id: Option<String>,
    pub since_ms: Option<i64>,
    pub limit: u32,
    pub before_seq: Option<i64>,
    pub after_seq: Option<i64>,
}

fn log_row(r: &Row) -> rusqlite::Result<Value> {
    let body: String = r.get(5)?;
    Ok(json!({"id": format!("log_{}", r.get::<_, i64>(0)?), "seq": r.get::<_, i64>(0)?, "timeNs": r.get::<_, i64>(1)?, "at": r.get::<_, i64>(1)? / 1_000_000,
        "service": r.get::<_, String>(2)?, "severity": r.get::<_, i64>(3)?, "level": r.get::<_, String>(4)?, "message": body,
        "traceId": r.get::<_, String>(6)?, "spanId": r.get::<_, String>(7)?, "attributes": serde_json::from_str::<Value>(&r.get::<_, String>(8)?).unwrap_or(Value::Null)}))
}

const LOG_COLS: &str = "seq, time_ns, service, severity, level, body, trace_id, span_id, attrs";

fn log_where(f: &LogFilter) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value as V;
    let mut w = vec!["1=1".to_string()];
    let mut p: Vec<V> = vec![];
    let list = |col: &str, vals: &[String], w: &mut Vec<String>, p: &mut Vec<V>| {
        if !vals.is_empty() {
            let mut ors = vec![];
            for v in vals {
                p.push(V::Text(v.clone()));
                ors.push(format!("{col} = ?{}", p.len()));
            }
            w.push(format!("({})", ors.join(" OR ")));
        }
    };
    list("service", &f.service, &mut w, &mut p);
    let lv: Vec<String> = f.level.iter().map(|l| l.to_uppercase().replace("WARNING", "WARN")).collect();
    list("level", &lv, &mut w, &mut p);
    if let Some(t) = &f.text {
        // Words go to the FTS5 index (each a quoted prefix, all required); a hex id matches trace ids.
        let m = t.split_whitespace().map(|w| format!("\"{}\"*", w.replace('"', ""))).collect::<Vec<_>>().join(" ");
        p.push(V::Text(m));
        let n = p.len();
        p.push(V::Text(t.trim().to_lowercase()));
        let n2 = p.len();
        w.push(format!("(seq IN (SELECT rowid FROM tel_logs_fts WHERE tel_logs_fts MATCH ?{n}) OR trace_id LIKE ?{n2} || '%')"));
    }
    if let Some(t) = &f.trace_id {
        p.push(V::Text(t.to_lowercase()));
        w.push(format!("trace_id = ?{}", p.len()));
    }
    if let Some(s) = f.since_ms {
        p.push(V::Integer(s * 1_000_000));
        w.push(format!("time_ns >= ?{}", p.len()));
    }
    if let Some(s) = f.before_seq {
        p.push(V::Integer(s));
        w.push(format!("seq < ?{}", p.len()));
    }
    if let Some(s) = f.after_seq {
        p.push(V::Integer(s));
        w.push(format!("seq > ?{}", p.len()));
    }
    (w.join(" AND "), p)
}

pub async fn logs(ctx: &Ctx, f: LogFilter, oldest_first: bool) -> Result<(Vec<Value>, i64), ApiError> {
    crate::db(ctx)
        .call(move |c| {
            let (w, p) = log_where(&f);
            let total: i64 = c.query_row(&format!("SELECT COUNT(*) FROM tel_logs WHERE {w}"), rusqlite::params_from_iter(p.iter()), |r| r.get(0))?;
            let ord = if oldest_first { "ASC" } else { "DESC" };
            let mut st = c.prepare(&format!("SELECT {LOG_COLS} FROM tel_logs WHERE {w} ORDER BY time_ns {ord}, seq {ord} LIMIT {}", f.limit.clamp(1, 2000)))?;
            let v = st.query_map(rusqlite::params_from_iter(p.iter()), log_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((v, total))
        })
        .await
}

/// Counts per level and per service in the window, for the filter column of T2.
pub async fn log_facets(ctx: &Ctx, since_ms: Option<i64>) -> Result<Value, ApiError> {
    let s = since_ms.unwrap_or(0) * 1_000_000;
    crate::db(ctx)
        .call(move |c| {
            let mut out = json!({"levels": {}, "services": {}});
            let mut st = c.prepare("SELECT level, COUNT(*) FROM tel_logs WHERE time_ns >= ?1 GROUP BY level")?;
            for r in st.query_map([s], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
                let (k, v) = r?;
                out["levels"][k] = json!(v);
            }
            let mut st = c.prepare("SELECT service, COUNT(*) FROM tel_logs WHERE time_ns >= ?1 GROUP BY service ORDER BY COUNT(*) DESC")?;
            for r in st.query_map([s], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
                let (k, v) = r?;
                out["services"][k] = json!(v);
            }
            Ok(out)
        })
        .await
}

pub async fn log(ctx: &Ctx, id: &str) -> Result<Value, ApiError> {
    let seq: i64 = id.trim_start_matches("log_").parse().map_err(|_| ApiError::bad("log id like log_42"))?;
    crate::db(ctx)
        .call(move |c| c.query_row(&format!("SELECT {LOG_COLS} FROM tel_logs WHERE seq = ?1"), [seq], log_row).optional())
        .await?
        .ok_or_else(|| ApiError::not_found(format!("No log line {id}")))
}

/// Lines just before and after one line, by time, across services.
pub async fn around(ctx: &Ctx, id: &str, before: u32, after: u32) -> Result<Value, ApiError> {
    let me = log(ctx, id).await?;
    let t = me["timeNs"].as_i64().unwrap_or(0);
    let seq = me["seq"].as_i64().unwrap_or(0);
    crate::db(ctx)
        .call(move |c| {
            let mut st = c.prepare(&format!("SELECT {LOG_COLS} FROM tel_logs WHERE (time_ns < ?1 OR (time_ns = ?1 AND seq < ?2)) ORDER BY time_ns DESC, seq DESC LIMIT ?3"))?;
            let mut b = st.query_map(params![t, seq, before], log_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            b.reverse();
            let mut st = c.prepare(&format!("SELECT {LOG_COLS} FROM tel_logs WHERE (time_ns > ?1 OR (time_ns = ?1 AND seq > ?2)) ORDER BY time_ns, seq LIMIT ?3"))?;
            let a = st.query_map(params![t, seq, after], log_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({"before": b, "line": me, "after": a}))
        })
        .await
}

/// Services seen per signal and when data last arrived.
pub async fn overview(ctx: &Ctx, since_ms: i64) -> Result<Value, ApiError> {
    let s = since_ms * 1_000_000;
    crate::db(ctx)
        .call(move |c| {
            let mut services = serde_json::Map::new();
            let mut st = c.prepare("SELECT service, COUNT(DISTINCT trace_id) FROM tel_spans INDEXED BY tel_spans_start WHERE start_ns >= ?1 GROUP BY service ORDER BY 2 DESC")?;
            for r in st.query_map([s], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
                let (k, v) = r?;
                services.insert(k, json!(v));
            }
            let last: Option<i64> = c.query_row("SELECT MAX(m) FROM (SELECT (SELECT received FROM tel_spans ORDER BY seq DESC LIMIT 1) m UNION SELECT (SELECT received FROM tel_logs ORDER BY seq DESC LIMIT 1) UNION SELECT (SELECT received FROM tel_points ORDER BY seq DESC LIMIT 1))", [], |r| r.get(0))?;
            let traces: i64 = c.query_row("SELECT COUNT(DISTINCT trace_id) FROM tel_spans INDEXED BY tel_spans_start WHERE start_ns >= ?1", [s], |r| r.get(0))?;
            let logs: i64 = c.query_row("SELECT COUNT(*) FROM tel_logs WHERE time_ns >= ?1", [s], |r| r.get(0))?;
            let series: i64 = c.query_row("SELECT COUNT(DISTINCT series) FROM tel_points INDEXED BY tel_points_time WHERE time_ns >= ?1", [s], |r| r.get(0))?;
            // Services seen in the same window, through the time indexes (the whole tables took seconds).
            let day = s;
            let all_services: i64 = c.query_row("SELECT COUNT(*) FROM (SELECT service FROM tel_spans INDEXED BY tel_spans_start WHERE start_ns >= ?1 UNION SELECT service FROM tel_logs INDEXED BY tel_logs_time WHERE time_ns >= ?1 UNION SELECT service FROM tel_points INDEXED BY tel_points_time WHERE time_ns >= ?1)", [day], |r| r.get(0))?;
            Ok(json!({"services": services, "lastData": last, "traces": traces, "logs": logs, "series": series, "serviceCount": all_services}))
        })
        .await
}

pub fn since(v: Option<String>) -> Result<Option<i64>, ApiError> {
    match v {
        None => Ok(None),
        Some(s) if s.len() >= 12 && s.chars().all(|c| c.is_ascii_digit()) => Ok(s.parse().ok()),
        Some(s) => util::parse_ms(&s).map(|d| Some(util::now_ms() - d as i64)).ok_or_else(|| ApiError::bad("since: 15m, 1h, 24h or epoch ms")),
    }
}

/// The service that owns each trace (its spans' service.name), for rows other modules recorded with
/// only a trace id: a Storage call made by "azsdk-net-Storage.Blobs" belongs to acme-api.
pub async fn services_of(ctx: &Ctx, traces: Vec<String>) -> std::collections::HashMap<String, String> {
    let traces: Vec<String> = traces.into_iter().filter(|t| t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit())).collect();
    if traces.is_empty() { return Default::default(); }
    let sql = format!("SELECT trace_id, service FROM tel_spans WHERE trace_id IN ({}) GROUP BY trace_id",
        traces.iter().map(|t| format!("'{}'", t.to_lowercase())).collect::<Vec<_>>().join(","));
    crate::db(ctx).call(move |c| { let mut st = c.prepare(&sql)?; let v = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<_>>(); v })
        .await.unwrap_or_default()
}
