//! T4 · Metrics math: turns OTLP points (cumulative or delta) into one value per step and per
//! breakdown value, so an agent asks "p95 before and after" instead of reading a chart.

use std::collections::BTreeMap;

use stuntman_core::{util, ApiError, Ctx};
use serde_json::{json, Value};

#[derive(Clone, Debug)]
pub struct Point {
    pub kind: String,
    pub unit: String,
    pub service: String,
    pub attrs: Value,
    pub series: String,
    pub start_ns: i64,
    pub time_ns: i64,
    pub cumulative: bool,
    pub value: Option<f64>,
    pub count: Option<i64>,
    pub sum: Option<f64>,
    pub bounds: Vec<f64>,
    pub buckets: Vec<f64>,
    pub exemplars: Value,
}

/// What one point adds to its step: counts, sum, bucket counts, or a gauge value.
#[derive(Clone, Debug, Default)]
pub struct Delta {
    pub count: f64,
    pub sum: f64,
    pub bounds: Vec<f64>,
    pub buckets: Vec<f64>,
    pub value: Option<f64>,
    pub errors: f64,
}

fn stream(p: &Point) -> String {
    format!("{}|{}", p.series, p.start_ns)
}

pub async fn points(ctx: &Ctx, name: &str, from_ms: i64, to_ms: i64) -> Result<Vec<Point>, ApiError> {
    let (n, a, b) = (name.to_string(), (from_ms - 3_600_000) * 1_000_000, to_ms * 1_000_000);
    crate::db(ctx)
        .call(move |c| {
            let mut st = c.prepare("SELECT kind, unit, service, attrs, series, start_ns, time_ns, cumulative, value, count, sum, bounds, buckets, exemplars FROM tel_points WHERE name = ?1 AND time_ns BETWEEN ?2 AND ?3 ORDER BY series, time_ns")?;
            let v = st
                .query_map(rusqlite::params![n, a, b], |r| {
                    let jv = |i: usize| -> rusqlite::Result<Vec<f64>> { Ok(r.get::<_, Option<String>>(i)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()) };
                    Ok(Point {
                        kind: r.get(0)?,
                        unit: r.get(1)?,
                        service: r.get(2)?,
                        attrs: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(json!({})),
                        series: r.get(4)?,
                        start_ns: r.get(5)?,
                        time_ns: r.get(6)?,
                        cumulative: r.get(7)?,
                        value: r.get(8)?,
                        count: r.get(9)?,
                        sum: r.get(10)?,
                        bounds: jv(11)?,
                        buckets: jv(12)?,
                        exemplars: serde_json::from_str(&r.get::<_, String>(13)?).unwrap_or(json!([])),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>();
            v
        })
        .await
}

pub fn is_error(a: &Value) -> bool {
    a.get("error.type").is_some()
        || a.get("http.response.status_code").or(a.get("http.status_code")).and_then(|v| v.as_i64().or(v.as_str().and_then(|s| s.parse().ok()))).map(|c| c >= 500).unwrap_or(false)
}

/// Cumulative series become deltas. Each start time is its own stream (one per process or replica,
/// as OTLP defines it), so two instances reporting the same series never mix.
pub fn deltas(pts: &[Point]) -> Vec<(Point, Delta)> {
    let mut out = vec![];
    let mut prev: BTreeMap<String, Point> = BTreeMap::new();
    for p in pts {
        let err = if is_error(&p.attrs) { 1.0 } else { 0.0 };
        let d = match p.kind.as_str() {
            "gauge" => Delta { value: p.value, ..Default::default() },
            "histogram" => {
                let (mut c, mut s, mut b) = (p.count.unwrap_or(0) as f64, p.sum.unwrap_or(0.0), p.buckets.clone());
                if p.cumulative {
                    if let Some(q) = prev.get(&stream(p)).filter(|q| q.count.unwrap_or(0) <= p.count.unwrap_or(0)) {
                        c -= q.count.unwrap_or(0) as f64;
                        s -= q.sum.unwrap_or(0.0);
                        if q.buckets.len() == b.len() {
                            for (i, x) in b.iter_mut().enumerate() {
                                *x -= q.buckets[i];
                            }
                        }
                    }
                }
                Delta { count: c, sum: s, bounds: p.bounds.clone(), buckets: b, value: None, errors: err * c }
            }
            _ => {
                let mut v = p.value.unwrap_or(0.0);
                if p.cumulative {
                    if let Some(q) = prev.get(&stream(p)).filter(|q| q.value.unwrap_or(0.0) <= v) {
                        v -= q.value.unwrap_or(0.0);
                    }
                }
                Delta { count: v, sum: v, value: p.value, errors: err * v, ..Default::default() }
            }
        };
        prev.insert(stream(p), p.clone());
        out.push((p.clone(), d));
    }
    out
}

/// Percentile from explicit-bucket counts, interpolated inside the bucket.
pub fn percentile(bounds: &[f64], buckets: &[f64], q: f64) -> Option<f64> {
    let total: f64 = buckets.iter().sum();
    if total <= 0.0 || buckets.is_empty() {
        return None;
    }
    let rank = q * total;
    let mut acc = 0.0;
    for (i, c) in buckets.iter().enumerate() {
        if *c > 0.0 && acc + c >= rank {
            let lo = if i == 0 { 0.0 } else { bounds.get(i - 1).copied().unwrap_or(0.0) };
            let hi = bounds.get(i).copied().unwrap_or(lo * 2.0_f64.max(1.0));
            return Some(lo + (hi - lo) * ((rank - acc) / c).clamp(0.0, 1.0));
        }
        acc += c;
    }
    bounds.last().copied()
}

fn merge(a: &mut Delta, b: &Delta) {
    a.count += b.count;
    a.sum += b.sum;
    a.errors += b.errors;
    if a.bounds.is_empty() {
        a.bounds = b.bounds.clone();
        a.buckets = b.buckets.clone();
    } else if a.bounds == b.bounds {
        for (i, x) in b.buckets.iter().enumerate() {
            if let Some(y) = a.buckets.get_mut(i) {
                *y += x;
            }
        }
    }
    if b.value.is_some() {
        a.value = b.value;
    }
}

pub fn stat_of(d: &Delta, stat: &str, kind: &str, step_ms: i64) -> Option<f64> {
    match stat {
        "count" => Some(d.count),
        "rate" => Some(d.count / (step_ms as f64 / 60_000.0)),
        "sum" => Some(d.sum),
        "avg" => if kind == "gauge" { d.value } else if d.count > 0.0 { Some(d.sum / d.count) } else { None },
        "last" => d.value.or(Some(d.sum)),
        "errors" => Some(d.errors),
        "p50" => percentile(&d.bounds, &d.buckets, 0.50),
        "p95" => percentile(&d.bounds, &d.buckets, 0.95),
        "p99" => percentile(&d.bounds, &d.buckets, 0.99),
        _ => None,
    }
}

pub fn key_of(p: &Point, by: Option<&str>) -> String {
    match by {
        None | Some("") => "all".into(),
        Some("service.name") => p.service.clone(),
        Some(a) => p.attrs.get(a).map(|v| v.as_str().map(String::from).unwrap_or(v.to_string())).unwrap_or("(none)".into()),
    }
}

/// One value per step and per breakdown value, plus window totals per breakdown value.
pub fn series(pts: &[Point], stat: &str, by: Option<&str>, from_ms: i64, to_ms: i64, step_ms: i64) -> Value {
    let kind = pts.first().map(|p| p.kind.clone()).unwrap_or_default();
    let mut steps: BTreeMap<String, BTreeMap<i64, Delta>> = BTreeMap::new();
    let mut totals: BTreeMap<String, Delta> = BTreeMap::new();
    for (p, d) in deltas(pts) {
        let t = p.time_ns / 1_000_000;
        if t < from_ms || t > to_ms {
            continue;
        }
        let k = key_of(&p, by);
        let bucket = from_ms + ((t - from_ms) / step_ms) * step_ms;
        merge(steps.entry(k.clone()).or_default().entry(bucket).or_default(), &d);
        merge(totals.entry(k).or_default(), &d);
    }
    let mut out = vec![];
    for (k, s) in &steps {
        let mut pts_out = vec![];
        let mut t = from_ms;
        while t <= to_ms {
            let v = s.get(&t).and_then(|d| stat_of(d, stat, &kind, step_ms));
            pts_out.push(json!({"t": t, "v": v}));
            t += step_ms;
        }
        let tot = &totals[k];
        out.push(json!({"key": k, "points": pts_out, "value": stat_of(tot, stat, &kind, to_ms - from_ms), "count": tot.count, "errors": tot.errors}));
    }
    out.sort_by(|a, b| b["count"].as_f64().partial_cmp(&a["count"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    json!({"kind": kind, "unit": pts.first().map(|p| p.unit.clone()).unwrap_or_default(), "stat": stat, "by": by, "stepMs": step_ms, "from": from_ms, "to": to_ms, "series": out})
}

/// "30m-15m" (relative), "a-b" (epoch ms) → (from, to).
pub fn window(s: &str) -> Result<(i64, i64), ApiError> {
    let (a, b) = s.split_once("..").or_else(|| s.split_once('-')).ok_or_else(|| ApiError::bad(format!("window “{s}”: use 30m-15m (ago) or epoch-ms..epoch-ms")))?;
    let p = |x: &str| -> Option<i64> {
        let x = x.trim();
        if x.len() >= 12 && x.chars().all(|c| c.is_ascii_digit()) {
            x.parse().ok()
        } else if x == "now" || x == "0" {
            Some(util::now_ms())
        } else {
            util::parse_ms(x).map(|d| util::now_ms() - d as i64)
        }
    };
    let (a, b) = (p(a), p(b));
    match (a, b) {
        (Some(a), Some(b)) => Ok((a.min(b), a.max(b))),
        _ => Err(ApiError::bad(format!("window “{s}”: use 30m-15m (ago) or epoch-ms..epoch-ms"))),
    }
}

pub fn category(name: &str) -> &'static str {
    if name.starts_with("http.client.") || name.starts_with("rpc.client.") {
        "Provider calls"
    } else if ["http.server.", "messaging.", "db.", "rpc.server."].iter().any(|p| name.starts_with(p)) {
        "The app"
    } else if ["process.", "runtime.", "jvm.", "dotnet.", "system.", "go.", "nodejs.", "v8js."].iter().any(|p| name.starts_with(p)) {
        "Runtime"
    } else {
        "Custom"
    }
}

#[cfg(test)]
mod t {
    use super::*;

    fn hist(series: &str, t: i64, count: i64, buckets: Vec<f64>) -> Point {
        Point {
            kind: "histogram".into(), unit: "s".into(), service: "acme-api".into(), attrs: json!({"server.address": series}), series: series.into(),
            start_ns: 1, time_ns: t * 1_000_000, cumulative: true, value: None, count: Some(count), sum: Some(count as f64 * 0.1),
            bounds: vec![0.05, 0.1, 0.5, 1.0, 2.0], buckets, exemplars: json!([]),
        }
    }

    #[test]
    fn cumulative_histograms_become_deltas_and_p95() {
        let p = vec![hist("blob", 60_000, 10, vec![2.0, 6.0, 2.0, 0.0, 0.0, 0.0]), hist("blob", 120_000, 30, vec![2.0, 6.0, 2.0, 10.0, 10.0, 0.0])];
        let d = deltas(&p);
        assert_eq!(d[1].1.count, 20.0);
        assert_eq!(d[1].1.buckets, vec![0.0, 0.0, 0.0, 10.0, 10.0, 0.0]);
        let v = series(&p, "p95", Some("server.address"), 60_000, 120_000, 60_000);
        let pts = v["series"][0]["points"].as_array().unwrap();
        let first = pts[0]["v"].as_f64().unwrap();
        let second = pts[1]["v"].as_f64().unwrap();
        assert!(first < 0.5 && second > 1.0, "{first} {second}");
    }

    #[test]
    fn two_instances_do_not_mix() {
        let mut a1 = hist("blob", 60_000, 10, vec![0.0, 10.0, 0.0, 0.0, 0.0, 0.0]);
        let mut b1 = a1.clone();
        b1.start_ns = 2;
        let mut a2 = hist("blob", 120_000, 30, vec![0.0, 30.0, 0.0, 0.0, 0.0, 0.0]);
        let mut b2 = a2.clone();
        b2.start_ns = 2;
        a1.time_ns = 60_000_000_000;
        b1.time_ns = 61_000_000_000;
        a2.time_ns = 120_000_000_000;
        b2.time_ns = 121_000_000_000;
        let total: f64 = deltas(&[a1, b1, a2, b2]).iter().map(|x| x.1.count).sum();
        assert_eq!(total, 60.0);
    }

    #[test]
    fn windows_and_categories() {
        let (a, b) = window("30m-15m").unwrap();
        assert!(b - a == 900_000);
        assert_eq!(category("http.client.request.duration"), "Provider calls");
        assert_eq!(category("orders.created"), "Custom");
    }
}
