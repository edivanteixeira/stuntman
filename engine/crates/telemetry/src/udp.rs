//! A13: the older ways apps ship logs and metrics. GELF (Graylog) over UDP — plain, gzip or zlib,
//! chunked — and over HTTP POST /gelf; StatsD and DogStatsD over UDP. Both land in the same logs and
//! metrics as OTLP. UDP has no host name: datagrams go to the default environment, or to the one a
//! GELF `_stuntman_env` field or a StatsD `stuntman_env` tag names.

use std::{collections::HashMap, io::Read};

use opentelemetry_proto::tonic::{
    collector::{logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest},
    common::v1::{any_value::Value as AV, AnyValue, KeyValue},
    logs::v1::{LogRecord, ResourceLogs, ScopeLogs},
    metrics::v1::{metric::Data, number_data_point, Gauge, Histogram, HistogramDataPoint, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum},
    resource::v1::Resource,
};
use serde_json::Value;
use stuntman_core::{env::Envs, util, Ctx};

fn kv(k: &str, v: AV) -> KeyValue {
    KeyValue { key: k.into(), value: Some(AnyValue { value: Some(v) }) }
}

fn resource(service: &str, via: &str) -> Option<Resource> {
    Some(Resource { attributes: vec![kv("service.name", AV::StringValue(service.into())), kv("telemetry.via", AV::StringValue(via.into()))], ..Default::default() })
}

fn unzip(b: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    if b.starts_with(&[0x1f, 0x8b]) && flate2::read::GzDecoder::new(b).read_to_end(&mut out).is_ok() {
        return out;
    }
    if b.first() == Some(&0x78) && flate2::read::ZlibDecoder::new(b).read_to_end(&mut out).is_ok() {
        return out;
    }
    b.to_vec()
}

/// One GELF message → an OTLP log request (and the environment it names, if any).
pub fn gelf(v: &Value) -> (ExportLogsServiceRequest, Option<String>) {
    let s = |k: &str| v.get(k).map(|x| match x { Value::String(s) => s.clone(), o => o.to_string() }).unwrap_or_default();
    let service = ["_service", "_app", "_application", "_service_name", "facility", "host"].iter().map(|k| s(k)).find(|x| !x.is_empty()).unwrap_or("gelf".into());
    let level = v.get("level").and_then(|l| l.as_i64()).unwrap_or(6);
    let (sev, text) = match level { 0..=2 => (21, "FATAL"), 3 => (17, "ERROR"), 4 => (13, "WARN"), 5 | 6 => (9, "INFO"), _ => (5, "DEBUG") };
    let ts = v.get("timestamp").and_then(|t| t.as_f64().or_else(|| t.as_str().and_then(|x| x.parse().ok()))).map(|t| (t * 1e9) as u64).unwrap_or(util::now_ms() as u64 * 1_000_000);
    let mut attrs = vec![];
    let mut env = None;
    let mut trace = vec![];
    for (k, x) in v.as_object().cloned().unwrap_or_default() {
        if let Some(name) = k.strip_prefix('_') {
            if name == "stuntman_env" {
                env = x.as_str().map(String::from);
                continue;
            }
            if name == "trace_id" || name == "traceId" {
                trace = hex::decode(x.as_str().unwrap_or("")).unwrap_or_default();
            }
            attrs.push(kv(name, match x { Value::String(t) => AV::StringValue(t), Value::Number(n) if n.is_i64() => AV::IntValue(n.as_i64().unwrap()), Value::Number(n) => AV::DoubleValue(n.as_f64().unwrap_or(0.0)), Value::Bool(b) => AV::BoolValue(b), o => AV::StringValue(o.to_string()) }));
        }
    }
    if !s("host").is_empty() {
        attrs.push(kv("host.name", AV::StringValue(s("host"))));
    }
    let full = s("full_message");
    if !full.is_empty() {
        attrs.push(kv("full_message", AV::StringValue(full)));
    }
    let rec = LogRecord { time_unix_nano: ts, observed_time_unix_nano: util::now_ms() as u64 * 1_000_000, severity_number: sev, severity_text: text.into(), body: Some(AnyValue { value: Some(AV::StringValue(s("short_message"))) }), attributes: attrs, trace_id: trace, ..Default::default() };
    (ExportLogsServiceRequest { resource_logs: vec![ResourceLogs { resource: resource(&service, "gelf"), scope_logs: vec![ScopeLogs { log_records: vec![rec], ..Default::default() }], ..Default::default() }] }, env)
}

/// StatsD lines ("name:1|c|@0.5|#env:stg,route:/x") → an OTLP metrics request.
pub fn statsd(text: &str) -> (ExportMetricsServiceRequest, Option<String>) {
    let now = util::now_ms() as u64 * 1_000_000;
    let mut by_service: HashMap<String, Vec<Metric>> = HashMap::new();
    let mut env = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("_e{") && !l.starts_with("_sc|")) {
        let Some((name, rest)) = line.split_once(':') else { continue };
        let mut parts = rest.split('|');
        let value: f64 = parts.next().and_then(|v| v.trim_start_matches('+').parse().ok()).unwrap_or(0.0);
        let kind = parts.next().unwrap_or("c").to_string();
        let mut rate = 1.0;
        let mut attrs = vec![];
        let mut service = "statsd".to_string();
        for p in parts {
            if let Some(r) = p.strip_prefix('@') {
                rate = r.parse().unwrap_or(1.0);
            } else if let Some(tags) = p.strip_prefix('#') {
                for t in tags.split(',') {
                    let (k, v) = t.split_once(':').unwrap_or((t, ""));
                    match k {
                        "stuntman_env" => env = Some(v.to_string()),
                        "service" => service = v.to_string(),
                        _ => attrs.push(kv(k, AV::StringValue(v.into()))),
                    }
                }
            }
        }
        let point = |v: f64| NumberDataPoint { attributes: attrs.clone(), time_unix_nano: now, start_time_unix_nano: now, value: Some(number_data_point::Value::AsDouble(v)), ..Default::default() };
        let data = match kind.as_str() {
            "c" => Data::Sum(Sum { data_points: vec![point(value / rate)], aggregation_temporality: 1, is_monotonic: true }),
            "ms" | "h" | "d" => Data::Histogram(Histogram { data_points: vec![HistogramDataPoint { attributes: attrs.clone(), time_unix_nano: now, start_time_unix_nano: now, count: 1, sum: Some(value), bucket_counts: vec![1], explicit_bounds: vec![], min: Some(value), max: Some(value), ..Default::default() }], aggregation_temporality: 1 }),
            _ => Data::Gauge(Gauge { data_points: vec![point(value)] }),
        };
        by_service.entry(service).or_default().push(Metric { name: name.to_string(), unit: if ["ms", "h", "d"].contains(&kind.as_str()) { "ms".into() } else { String::new() }, data: Some(data), ..Default::default() });
    }
    let req = ExportMetricsServiceRequest { resource_metrics: by_service.into_iter().map(|(s, m)| ResourceMetrics { resource: resource(&s, "statsd"), scope_metrics: vec![ScopeMetrics { metrics: m, ..Default::default() }], ..Default::default() }).collect() };
    (req, env)
}

fn pick(envs: &Envs, name: Option<String>) -> Option<Ctx> {
    name.and_then(|n| envs.get(&n)).or_else(|| envs.default_env())
}

pub async fn gelf_in(ctx: &Ctx, raw: &[u8]) -> Result<usize, String> {
    let b = unzip(raw);
    let v: Value = serde_json::from_slice(&b).map_err(|e| format!("not GELF JSON: {e}"))?;
    let list = match v { Value::Array(a) => a, o => vec![o] };
    let mut n = 0;
    for m in list {
        let (req, _) = gelf(&m);
        n += crate::store::add_logs(ctx, req).await.map(|x| x.1).unwrap_or(0);
    }
    Ok(n)
}

pub fn serve(envs: Envs, gelf_port: u16, statsd_port: u16) {
    let e1 = envs.clone();
    tokio::spawn(async move {
        let Ok(sock) = tokio::net::UdpSocket::bind(("0.0.0.0", gelf_port)).await else { eprintln!("GELF: port {gelf_port} is taken"); return };
        let mut buf = vec![0u8; 65536];
        let mut chunks: HashMap<[u8; 8], (u8, HashMap<u8, Vec<u8>>, i64)> = HashMap::new();
        loop {
            let Ok((n, _)) = sock.recv_from(&mut buf).await else { continue };
            let d = &buf[..n];
            let whole: Option<Vec<u8>> = if d.len() > 12 && d[0] == 0x1e && d[1] == 0x0f {
                let mut id = [0u8; 8];
                id.copy_from_slice(&d[2..10]);
                let (seq, count) = (d[10], d[11]);
                let e = chunks.entry(id).or_insert((count, HashMap::new(), util::now_ms()));
                e.1.insert(seq, d[12..].to_vec());
                if e.1.len() as u8 == e.0 {
                    let (c, parts, _) = chunks.remove(&id).unwrap();
                    Some((0..c).flat_map(|i| parts.get(&i).cloned().unwrap_or_default()).collect())
                } else {
                    None
                }
            } else {
                Some(d.to_vec())
            };
            chunks.retain(|_, v| util::now_ms() - v.2 < 5000);
            if let Some(w) = whole {
                let b = unzip(&w);
                if let Ok(v) = serde_json::from_slice::<Value>(&b) {
                    let (req, env) = gelf(&v);
                    if let Some(ctx) = pick(&e1, env) {
                        let _ = crate::store::add_logs(&ctx, req).await;
                    }
                }
            }
        }
    });
    tokio::spawn(async move {
        let Ok(sock) = tokio::net::UdpSocket::bind(("0.0.0.0", statsd_port)).await else { eprintln!("StatsD: port {statsd_port} is taken"); return };
        let mut buf = vec![0u8; 65536];
        loop {
            let Ok((n, _)) = sock.recv_from(&mut buf).await else { continue };
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let (req, env) = statsd(&text);
            if let Some(ctx) = pick(&envs, env) {
                let _ = crate::store::add_metrics(&ctx, req).await;
            }
        }
    });
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn gelf_and_statsd() {
        let (r, env) = gelf(&serde_json::json!({"version": "1.1", "host": "web-1", "short_message": "boom", "level": 3, "_app": "acme-tms", "_stuntman_env": "stg-b", "_order": 42}));
        assert_eq!(env.as_deref(), Some("stg-b"));
        let l = &r.resource_logs[0].scope_logs[0].log_records[0];
        assert_eq!((l.severity_text.as_str(), l.attributes.len()), ("ERROR", 3));
        let (m, _) = statsd("orders.created:2|c|@0.5|#service:acme-tms,region:br\nlatency:120|ms\nqueue.depth:7|g");
        let names: Vec<String> = m.resource_metrics.iter().flat_map(|r| r.scope_metrics[0].metrics.iter().map(|x| x.name.clone())).collect();
        assert_eq!(names.len(), 3);
    }
}
