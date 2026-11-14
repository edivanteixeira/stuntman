//! Spans, log lines and metric points, as they arrive by OTLP. Kept in telemetry's own SQLite file
//! (data/telemetry.sqlite, its own connection, so high log volume never slows the other services),
//! for the time each signal's "keep" allows and within the storage limit (oldest dropped first).
//! Log text is indexed with FTS5.

use stuntman_core::{util, ApiError, Ctx};
use opentelemetry_proto::tonic::{
    collector::{logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest, trace::v1::ExportTraceServiceRequest},
    common::v1::{any_value::Value as AV, AnyValue, KeyValue},
    metrics::v1::{metric::Data, number_data_point, Exemplar},
};
use rusqlite::params;
use serde_json::{json, Map, Value};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS tel_spans(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, trace_id TEXT NOT NULL, span_id TEXT NOT NULL, parent_id TEXT NOT NULL, name TEXT NOT NULL,
  service TEXT NOT NULL, kind INTEGER NOT NULL, start_ns INTEGER NOT NULL, end_ns INTEGER NOT NULL, status INTEGER NOT NULL,
  status_msg TEXT NOT NULL, attrs TEXT NOT NULL, events TEXT NOT NULL, host TEXT NOT NULL DEFAULT '', dub INTEGER NOT NULL DEFAULT 0,
  received INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS tel_spans_trace ON tel_spans(trace_id);
CREATE INDEX IF NOT EXISTS tel_spans_start ON tel_spans(start_ns);
CREATE TABLE IF NOT EXISTS tel_logs(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, time_ns INTEGER NOT NULL, service TEXT NOT NULL, severity INTEGER NOT NULL,
  level TEXT NOT NULL, body TEXT NOT NULL, trace_id TEXT NOT NULL, span_id TEXT NOT NULL, attrs TEXT NOT NULL, received INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS tel_logs_time ON tel_logs(time_ns);
CREATE INDEX IF NOT EXISTS tel_logs_trace ON tel_logs(trace_id);
CREATE VIRTUAL TABLE IF NOT EXISTS tel_logs_fts USING fts5(body, attrs, content='tel_logs', content_rowid='seq');
CREATE TRIGGER IF NOT EXISTS tel_logs_ai AFTER INSERT ON tel_logs BEGIN
  INSERT INTO tel_logs_fts(rowid, body, attrs) VALUES (new.seq, new.body, new.attrs); END;
CREATE TRIGGER IF NOT EXISTS tel_logs_ad AFTER DELETE ON tel_logs BEGIN
  INSERT INTO tel_logs_fts(tel_logs_fts, rowid, body, attrs) VALUES('delete', old.seq, old.body, old.attrs); END;
CREATE TABLE IF NOT EXISTS tel_points(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, kind TEXT NOT NULL, unit TEXT NOT NULL, description TEXT NOT NULL,
  service TEXT NOT NULL, attrs TEXT NOT NULL, series TEXT NOT NULL, start_ns INTEGER NOT NULL, time_ns INTEGER NOT NULL,
  cumulative INTEGER NOT NULL, value REAL, count INTEGER, sum REAL, bounds TEXT, buckets TEXT, exemplars TEXT NOT NULL, received INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS tel_points_name ON tel_points(name, time_ns);
CREATE INDEX IF NOT EXISTS tel_points_time ON tel_points(time_ns);
";

pub fn any(v: &Option<AnyValue>) -> Value {
    match v.as_ref().and_then(|a| a.value.as_ref()) {
        Some(AV::StringValue(s)) => json!(s),
        Some(AV::BoolValue(b)) => json!(b),
        Some(AV::IntValue(i)) => json!(i),
        Some(AV::DoubleValue(d)) => json!(d),
        Some(AV::ArrayValue(a)) => json!(a.values.iter().map(|x| any(&Some(x.clone()))).collect::<Vec<_>>()),
        Some(AV::KvlistValue(kv)) => attrs(&kv.values),
        Some(AV::BytesValue(b)) => json!(hex::encode(b)),
        None => Value::Null,
    }
}

pub fn attrs(kv: &[KeyValue]) -> Value {
    let mut m = Map::new();
    for k in kv {
        m.insert(k.key.clone(), any(&k.value));
    }
    Value::Object(m)
}

fn service_of(res: &Option<opentelemetry_proto::tonic::resource::v1::Resource>) -> (String, Value) {
    let a = res.as_ref().map(|r| attrs(&r.attributes)).unwrap_or(json!({}));
    let s = a.get("service.name").and_then(|v| v.as_str()).unwrap_or("unknown_service").to_string();
    (s, a)
}

/// The host a client span called, and whether that host is Stuntman.
pub fn host_of(a: &Value) -> (String, bool) {
    let get = |k: &str| a.get(k).map(|v| v.as_str().map(String::from).unwrap_or(v.to_string()));
    let url = get("url.full").or_else(|| get("http.url"));
    let mut host = get("server.address").or_else(|| get("net.peer.name")).or_else(|| get("http.host")).unwrap_or_default();
    let mut port = get("server.port").or_else(|| get("net.peer.port")).unwrap_or_default();
    if let Some(u) = &url {
        if let Some(rest) = u.split("://").nth(1) {
            let hp = rest.split('/').next().unwrap_or("");
            let (h, p) = hp.rsplit_once(':').map(|(h, p)| (h.to_string(), p.to_string())).unwrap_or((hp.to_string(), String::new()));
            if host.is_empty() {
                host = h;
            }
            if port.is_empty() {
                port = p;
            }
        }
    }
    if host.is_empty() {
        return (String::new(), false);
    }
    let ports: Vec<String> = ["STUNTMAN_PORT", "STUNTMAN_HOOKS_PORT", "STUNTMAN_SMTP_PORT"].iter().filter_map(|k| std::env::var(k).ok()).chain(["8025".into(), "8027".into(), "2525".into()]).collect();
    let local = ["localhost", "127.0.0.1", "::1", "0.0.0.0"].contains(&host.as_str());
    let dub = (host.contains("stuntman") || host.contains("duble")) || (local && ports.contains(&port));
    (if port.is_empty() { host } else { format!("{host}:{port}") }, dub)
}

fn id(b: &[u8]) -> String {
    hex::encode(b)
}

/// Returns (service names seen, spans kept).
pub async fn add_traces(ctx: &Ctx, req: ExportTraceServiceRequest) -> Result<(Vec<String>, usize), ApiError> {
    let now = util::now_ms();
    let mut rows = vec![];
    let mut services = vec![];
    for rs in req.resource_spans {
        let (svc, _) = service_of(&rs.resource);
        services.push(svc.clone());
        for ss in rs.scope_spans {
            for sp in ss.spans {
                let a = attrs(&sp.attributes);
                let (host, dub) = if sp.kind == 3 || sp.kind == 4 { host_of(&a) } else { (String::new(), false) };
                let events = json!(sp.events.iter().map(|e| json!({"name": e.name, "timeNs": e.time_unix_nano, "attributes": attrs(&e.attributes)})).collect::<Vec<_>>());
                let (st, msg) = sp.status.as_ref().map(|s| (s.code, s.message.clone())).unwrap_or((0, String::new()));
                rows.push((id(&sp.trace_id), id(&sp.span_id), id(&sp.parent_span_id), sp.name, svc.clone(), sp.kind, sp.start_time_unix_nano as i64, sp.end_time_unix_nano as i64, st, msg, a.to_string(), events.to_string(), host, dub));
            }
        }
    }
    let n = rows.len();
    crate::db(ctx)
        .call(move |c| {
            let tx = c.transaction()?;
            {
                let mut st = tx.prepare("INSERT INTO tel_spans(trace_id, span_id, parent_id, name, service, kind, start_ns, end_ns, status, status_msg, attrs, events, host, dub, received) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)")?;
                for r in rows {
                    st.execute(params![r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9, r.10, r.11, r.12, r.13, now])?;
                }
            }
            tx.commit()
        })
        .await?;
    services.dedup();
    if n > 0 {
        ctx.live.send("telemetry.traces", "*", "created");
    }
    Ok((services, n))
}

pub async fn add_logs(ctx: &Ctx, req: ExportLogsServiceRequest) -> Result<(Vec<String>, usize), ApiError> {
    let now = util::now_ms();
    let mut rows = vec![];
    let mut services = vec![];
    for rl in req.resource_logs {
        let (svc, _) = service_of(&rl.resource);
        services.push(svc.clone());
        for sl in rl.scope_logs {
            for l in sl.log_records {
                let body = match any(&l.body) {
                    Value::String(s) => s,
                    Value::Null => String::new(),
                    o => o.to_string(),
                };
                let t = if l.time_unix_nano > 0 { l.time_unix_nano } else { l.observed_time_unix_nano.max(now as u64 * 1_000_000) };
                let level = if !l.severity_text.is_empty() { l.severity_text.to_uppercase() } else { level_of(l.severity_number).to_string() };
                rows.push((t as i64, svc.clone(), l.severity_number, normalize_level(&level), body, id(&l.trace_id), id(&l.span_id), attrs(&l.attributes).to_string()));
            }
        }
    }
    let n = rows.len();
    crate::db(ctx)
        .call(move |c| {
            let tx = c.transaction()?;
            {
                let mut st = tx.prepare("INSERT INTO tel_logs(time_ns, service, severity, level, body, trace_id, span_id, attrs, received) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)")?;
                for r in rows {
                    st.execute(params![r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, now])?;
                }
            }
            tx.commit()
        })
        .await?;
    services.dedup();
    if n > 0 {
        ctx.live.send("telemetry.logs", "*", "created");
    }
    Ok((services, n))
}

pub fn level_of(n: i32) -> &'static str {
    match n {
        1..=4 => "TRACE",
        5..=8 => "DEBUG",
        9..=12 => "INFO",
        13..=16 => "WARN",
        17..=20 => "ERROR",
        21..=24 => "FATAL",
        _ => "INFO",
    }
}

fn normalize_level(l: &str) -> String {
    match l {
        "WARNING" => "WARN".into(),
        "ERR" => "ERROR".into(),
        "CRITICAL" => "FATAL".into(),
        "INFORMATION" => "INFO".into(),
        o => o.to_string(),
    }
}

fn exemplars(e: &[Exemplar]) -> String {
    json!(e
        .iter()
        .map(|x| json!({"traceId": id(&x.trace_id), "spanId": id(&x.span_id), "timeNs": x.time_unix_nano,
            "value": match x.value { Some(opentelemetry_proto::tonic::metrics::v1::exemplar::Value::AsDouble(d)) => json!(d), Some(opentelemetry_proto::tonic::metrics::v1::exemplar::Value::AsInt(i)) => json!(i), None => Value::Null }}))
        .collect::<Vec<_>>())
    .to_string()
}

pub async fn add_metrics(ctx: &Ctx, req: ExportMetricsServiceRequest) -> Result<(Vec<String>, usize), ApiError> {
    let now = util::now_ms();
    type Row = (String, &'static str, String, String, String, String, String, i64, i64, bool, Option<f64>, Option<i64>, Option<f64>, Option<String>, Option<String>, String);
    let mut rows: Vec<Row> = vec![];
    let mut services = vec![];
    for rm in req.resource_metrics {
        let (svc, _) = service_of(&rm.resource);
        services.push(svc.clone());
        for sm in rm.scope_metrics {
            for m in sm.metrics {
                let series = |a: &Value| format!("{}|{}|{}", m.name, svc, a);
                match &m.data {
                    Some(Data::Gauge(g)) => {
                        for p in &g.data_points {
                            let a = attrs(&p.attributes);
                            rows.push((m.name.clone(), "gauge", m.unit.clone(), m.description.clone(), svc.clone(), a.to_string(), series(&a), p.start_time_unix_nano as i64, p.time_unix_nano as i64, false, num(&p.value), None, None, None, None, exemplars(&p.exemplars)));
                        }
                    }
                    Some(Data::Sum(s)) => {
                        for p in &s.data_points {
                            let a = attrs(&p.attributes);
                            let kind = if s.is_monotonic { "counter" } else { "updown" };
                            rows.push((m.name.clone(), kind, m.unit.clone(), m.description.clone(), svc.clone(), a.to_string(), series(&a), p.start_time_unix_nano as i64, p.time_unix_nano as i64, s.aggregation_temporality == 2, num(&p.value), None, None, None, None, exemplars(&p.exemplars)));
                        }
                    }
                    Some(Data::Histogram(h)) => {
                        for p in &h.data_points {
                            let a = attrs(&p.attributes);
                            rows.push((m.name.clone(), "histogram", m.unit.clone(), m.description.clone(), svc.clone(), a.to_string(), series(&a), p.start_time_unix_nano as i64, p.time_unix_nano as i64, h.aggregation_temporality == 2, None,
                                Some(p.count as i64), p.sum, Some(json!(p.explicit_bounds).to_string()), Some(json!(p.bucket_counts).to_string()), exemplars(&p.exemplars)));
                        }
                    }
                    // Exponential histograms and summaries are kept as their count and sum only.
                    Some(Data::ExponentialHistogram(h)) => {
                        for p in &h.data_points {
                            let a = attrs(&p.attributes);
                            rows.push((m.name.clone(), "histogram", m.unit.clone(), m.description.clone(), svc.clone(), a.to_string(), series(&a), p.start_time_unix_nano as i64, p.time_unix_nano as i64, h.aggregation_temporality == 2, None, Some(p.count as i64), p.sum, None, None, exemplars(&p.exemplars)));
                        }
                    }
                    Some(Data::Summary(sm)) => {
                        for p in &sm.data_points {
                            let a = attrs(&p.attributes);
                            rows.push((m.name.clone(), "histogram", m.unit.clone(), m.description.clone(), svc.clone(), a.to_string(), series(&a), p.start_time_unix_nano as i64, p.time_unix_nano as i64, true, None, Some(p.count as i64), Some(p.sum), None, None, "[]".into()));
                        }
                    }
                    None => {}
                }
            }
        }
    }
    let n = rows.len();
    crate::db(ctx)
        .call(move |c| {
            let tx = c.transaction()?;
            {
                let mut st = tx.prepare("INSERT INTO tel_points(name, kind, unit, description, service, attrs, series, start_ns, time_ns, cumulative, value, count, sum, bounds, buckets, exemplars, received) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)")?;
                for r in rows {
                    st.execute(params![r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9, r.10, r.11, r.12, r.13, r.14, r.15, now])?;
                }
            }
            tx.commit()
        })
        .await?;
    services.dedup();
    if n > 0 {
        ctx.live.send("telemetry.metrics", "*", "created");
    }
    Ok((services, n))
}

fn num(v: &Option<number_data_point::Value>) -> Option<f64> {
    match v {
        Some(number_data_point::Value::AsDouble(d)) => Some(*d),
        Some(number_data_point::Value::AsInt(i)) => Some(*i as f64),
        None => None,
    }
}

/// Approximate bytes used by telemetry rows.
/// Bytes the data takes on disk (pages in use), read in constant time. Summing row lengths read
/// the whole file on every overview and made every telemetry screen wait behind it.
pub async fn used_bytes(ctx: &Ctx) -> Result<i64, ApiError> {
    crate::db(ctx)
        .call(|c| {
            let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0))?;
            let free: i64 = c.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
            let size: i64 = c.query_row("PRAGMA page_size", [], |r| r.get(0))?;
            Ok((pages - free).max(0) * size)
        })
        .await
}

pub async fn sweep(ctx: &Ctx, s: &crate::config::Settings) -> Result<(), ApiError> {
    let keep = |k: &str| util::parse_ms(k).unwrap_or(86_400_000) as i64;
    let now_ns = util::now_ms() * 1_000_000;
    let (tk, lk, mk) = (keep(&s.signals.traces.keep), keep(&s.signals.logs.keep), keep(&s.signals.metrics.keep));
    crate::db(ctx)
        .call(move |c| {
            c.execute("DELETE FROM tel_spans WHERE start_ns < ?1", [now_ns - tk * 1_000_000])?;
            c.execute("DELETE FROM tel_logs WHERE time_ns < ?1", [now_ns - lk * 1_000_000])?;
            c.execute("DELETE FROM tel_points WHERE time_ns < ?1", [now_ns - mk * 1_000_000])?;
            Ok(())
        })
        .await?;
    let max = s.storage_bytes() as i64;
    for _ in 0..20 {
        if used_bytes(ctx).await? <= max {
            break;
        }
        crate::db(ctx)
            .call(|c| {
                c.execute("DELETE FROM tel_spans WHERE seq IN (SELECT seq FROM tel_spans ORDER BY seq LIMIT 2000)", [])?;
                c.execute("DELETE FROM tel_logs WHERE seq IN (SELECT seq FROM tel_logs ORDER BY seq LIMIT 5000)", [])?;
                c.execute("DELETE FROM tel_points WHERE seq IN (SELECT seq FROM tel_points ORDER BY seq LIMIT 2000)", [])
            })
            .await?;
    }
    Ok(())
}

pub async fn clear(ctx: &Ctx) -> Result<Value, ApiError> {
    let v = crate::db(ctx)
        .call(|c| {
            let t: i64 = c.query_row("SELECT COUNT(DISTINCT trace_id) FROM tel_spans", [], |r| r.get(0))?;
            let l: i64 = c.query_row("SELECT COUNT(*) FROM tel_logs", [], |r| r.get(0))?;
            let m: i64 = c.query_row("SELECT COUNT(DISTINCT series) FROM tel_points", [], |r| r.get(0))?;
            c.execute_batch("DELETE FROM tel_spans; DELETE FROM tel_logs; DELETE FROM tel_points;")?;
            Ok(json!({"traces": t, "logs": l, "series": m}))
        })
        .await?;
    ctx.live.send("telemetry.traces", "*", "deleted");
    Ok(v)
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn hosts() {
        assert_eq!(host_of(&json!({"url.full": "http://localhost:8025/sendgrid/v3/mail/send"})), ("localhost:8025".into(), true));
        assert_eq!(host_of(&json!({"server.address": "acme-files.blob.stuntman.local", "server.port": 443})), ("acme-files.blob.stuntman.local:443".into(), true));
        assert_eq!(host_of(&json!({"url.full": "https://maps.example-geo.com/v1/geocode"})), ("maps.example-geo.com".into(), false));
    }
}
