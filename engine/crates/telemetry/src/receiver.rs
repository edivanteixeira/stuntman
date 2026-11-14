//! The OTLP receiver: OTLP/HTTP (protobuf or JSON, gzip) and OTLP/gRPC, like any collector. It
//! honours T3: signals turned off are answered 200 and dropped; a provoked collector answers 403,
//! 401, 429, 503, slowly, drops half, or refuses connections (its listeners really go down).

use std::{
    collections::BTreeMap,
    io::Read,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use stuntman_core::{env::Envs, util, Ctx};
use opentelemetry_proto::tonic::collector::{
    logs::v1::{logs_service_server::{LogsService, LogsServiceServer}, ExportLogsServiceRequest, ExportLogsServiceResponse},
    metrics::v1::{metrics_service_server::{MetricsService, MetricsServiceServer}, ExportMetricsServiceRequest, ExportMetricsServiceResponse},
    trace::v1::{trace_service_server::{TraceService, TraceServiceServer}, ExportTraceServiceRequest, ExportTraceServiceResponse},
};
use prost::Message;
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use crate::{config, store};

/// Counters for the screens: batches kept, dropped because a signal is off, refused on purpose.
#[derive(Default)]
pub struct Stats {
    pub kept: BTreeMap<String, u64>,
    pub dropped_off: BTreeMap<String, u64>,
    pub refused: u64,
    pub dropped_half: u64,
    pub last_forward: Option<Value>,
    pub drop_events: Vec<(i64, String, u64)>,
}

pub fn stats() -> &'static Mutex<Stats> {
    static S: OnceLock<Mutex<Stats>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Items dropped in the last `ms` for one signal: spans, log lines or metric data points
/// ("1,830 data points dropped in the last 15 min").
pub fn dropped_since(signal: &str, ms: i64) -> u64 {
    let cut = util::now_ms() - ms;
    stats().lock().unwrap().drop_events.iter().filter(|(t, s, _)| *t >= cut && s == signal).map(|e| e.2).sum()
}

#[derive(Clone, Copy, PartialEq)]
pub enum Sig {
    Traces,
    Logs,
    Metrics,
}

impl Sig {
    fn name(&self) -> &'static str {
        match self {
            Sig::Traces => "traces",
            Sig::Logs => "logs",
            Sig::Metrics => "metrics",
        }
    }
}

/// The verdict before storing: Ok(true) keep, Ok(false) answer 200 and drop, Err(code) refuse.
async fn verdict(ctx: &Ctx, sig: Sig, services: &[String], headers: Option<&HeaderMap>, items: u64) -> Result<bool, u16> {
    if ctx.state.paused() {
        return Err(503);
    }
    let s = config::get(ctx).map_err(|_| 500u16)?;
    if !s.receive.any_api_key {
        if let Some(h) = headers {
            if !h.contains_key("api-key") && !h.contains_key("authorization") && !h.contains_key("x-api-key") {
                return Err(401);
            }
        }
    }
    let svc = services.first().map(String::as_str).unwrap_or("");
    if let Some(a) = s.provoked_for(svc) {
        match a {
            "403" | "401" | "429" | "503" => {
                stats().lock().unwrap().refused += 1;
                return Err(a.parse().unwrap());
            }
            "refused" => {
                stats().lock().unwrap().refused += 1;
                return Err(503);
            }
            "slow" => tokio::time::sleep(Duration::from_secs(5)).await,
            "drop_half" if rand::random::<bool>() => {
                stats().lock().unwrap().dropped_half += 1;
                return Ok(false);
            }
            _ => {}
        }
    }
    let on = match sig {
        Sig::Traces => s.signals.traces.on,
        Sig::Logs => s.signals.logs.on,
        Sig::Metrics => s.signals.metrics.on,
    };
    if !on {
        let mut st = stats().lock().unwrap();
        *st.dropped_off.entry(sig.name().into()).or_default() += 1;
        st.drop_events.push((util::now_ms(), sig.name().into(), items));
        let cut = util::now_ms() - 3_600_000;
        st.drop_events.retain(|(t, _, _)| *t >= cut);
        return Ok(false);
    }
    Ok(true)
}

fn items_traces(r: &ExportTraceServiceRequest) -> u64 {
    r.resource_spans.iter().flat_map(|x| &x.scope_spans).map(|s| s.spans.len() as u64).sum()
}
fn items_logs(r: &ExportLogsServiceRequest) -> u64 {
    r.resource_logs.iter().flat_map(|x| &x.scope_logs).map(|s| s.log_records.len() as u64).sum()
}
fn items_metrics(r: &ExportMetricsServiceRequest) -> u64 {
    use opentelemetry_proto::tonic::metrics::v1::metric::Data;
    r.resource_metrics.iter().flat_map(|x| &x.scope_metrics).flat_map(|s| &s.metrics).map(|m| match &m.data {
        Some(Data::Gauge(g)) => g.data_points.len(),
        Some(Data::Sum(x)) => x.data_points.len(),
        Some(Data::Histogram(h)) => h.data_points.len(),
        Some(Data::ExponentialHistogram(h)) => h.data_points.len(),
        Some(Data::Summary(x)) => x.data_points.len(),
        None => 0,
    } as u64).sum()
}

fn services_of_traces(r: &ExportTraceServiceRequest) -> Vec<String> {
    r.resource_spans.iter().map(|x| svc(&x.resource)).collect()
}
fn services_of_logs(r: &ExportLogsServiceRequest) -> Vec<String> {
    r.resource_logs.iter().map(|x| svc(&x.resource)).collect()
}
fn services_of_metrics(r: &ExportMetricsServiceRequest) -> Vec<String> {
    r.resource_metrics.iter().map(|x| svc(&x.resource)).collect()
}
fn svc(r: &Option<opentelemetry_proto::tonic::resource::v1::Resource>) -> String {
    store::attrs(&r.as_ref().map(|r| r.attributes.clone()).unwrap_or_default())["service.name"].as_str().unwrap_or("unknown_service").to_string()
}

/// Copies the raw batch to a real APM when forwarding is on. A failure never reaches the app.
fn forward(ctx: &Ctx, sig: Sig, body: Vec<u8>, content_type: &str) {
    let Ok(s) = config::get(ctx) else { return };
    if !s.forward.enabled || s.forward.endpoint.is_empty() {
        return;
    }
    let url = format!("{}/v1/{}", s.forward.endpoint.trim_end_matches('/'), sig.name());
    let headers: Vec<(String, String)> = s.forward.headers.iter().map(|(k, v)| (k.clone(), util::resolve_secret(v, &ctx.data_dir))).collect();
    let (http, ct) = (ctx.http.clone(), content_type.to_string());
    tokio::spawn(async move {
        let mut req = http.post(&url).header("content-type", ct).timeout(Duration::from_secs(10)).body(body);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let r = match req.send().await {
            Ok(r) => json!({"at": util::now_ms(), "status": r.status().as_u16(), "ok": r.status().is_success(), "host": url}),
            Err(e) => json!({"at": util::now_ms(), "error": e.to_string(), "ok": false, "host": url}),
        };
        stats().lock().unwrap().last_forward = Some(r);
    });
}

async fn keep(ctx: &Ctx, sig: Sig, traces: Option<ExportTraceServiceRequest>, logs: Option<ExportLogsServiceRequest>, metrics: Option<ExportMetricsServiceRequest>) {
    let r = match sig {
        Sig::Traces => store::add_traces(ctx, traces.unwrap()).await,
        Sig::Logs => store::add_logs(ctx, logs.unwrap()).await,
        Sig::Metrics => store::add_metrics(ctx, metrics.unwrap()).await,
    };
    match r {
        Ok((services, _)) => {
            *stats().lock().unwrap().kept.entry(sig.name().into()).or_default() += 1;
            for s in services {
                ctx.live.seen(&s, "OTLP");
            }
        }
        Err(e) => eprintln!("telemetry: {}", e.message),
    }
}

// ---------------------------------------------------------------- OTLP/HTTP

/// OTLP/JSON lets senders omit empty fields; "{}" is a valid, empty batch.
fn otlp_json<T: serde::de::DeserializeOwned>(raw: &[u8], key: &str) -> Result<T, String> {
    let mut v: Value = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
    if let Value::Object(o) = &mut v {
        o.entry(key).or_insert(json!([]));
    }
    // OTLP/JSON sends 64-bit integers as strings; these fields of the generated types want numbers.
    // (A failure inside a flattened oneof would otherwise turn into a silently empty metric.)
    fn ints(v: &mut Value) {
        let num = |x: &mut Value| {
            if let Some(n) = x.as_str().and_then(|s| s.parse::<i64>().ok()) {
                *x = json!(n);
            }
        };
        match v {
            Value::Object(o) => {
                for (k, x) in o.iter_mut() {
                    match k.as_str() {
                        "asInt" | "count" | "zeroCount" => num(x),
                        "bucketCounts" => {
                            if let Value::Array(a) = x {
                                a.iter_mut().for_each(num);
                            }
                        }
                        _ => ints(x),
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(ints),
            _ => {}
        }
    }
    ints(&mut v);
    serde_json::from_value(v).map_err(|e| e.to_string())
}

async fn http_export(ctx: Ctx, sig: Sig, h: HeaderMap, body: Bytes) -> Response {
    let mut raw = body.to_vec();
    if h.get("content-encoding").and_then(|v| v.to_str().ok()).map(|v| v.contains("gzip")).unwrap_or(false) {
        let mut out = vec![];
        if flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut out).is_err() {
            return (StatusCode::BAD_REQUEST, "bad gzip body").into_response();
        }
        raw = out;
    }
    let json_body = h.get("content-type").and_then(|v| v.to_str().ok()).map(|v| v.contains("json")).unwrap_or(false);
    let bad = |e: String| (StatusCode::BAD_REQUEST, e).into_response();
    let (t, l, m, services, items) = match sig {
        Sig::Traces => {
            let r: Result<ExportTraceServiceRequest, String> = if json_body { otlp_json(&raw, "resourceSpans") } else { ExportTraceServiceRequest::decode(&raw[..]).map_err(|e| e.to_string()) };
            match r { Ok(r) => { let s = services_of_traces(&r); let n = items_traces(&r); (Some(r), None, None, s, n) } Err(e) => return bad(e) }
        }
        Sig::Logs => {
            let r: Result<ExportLogsServiceRequest, String> = if json_body { otlp_json(&raw, "resourceLogs") } else { ExportLogsServiceRequest::decode(&raw[..]).map_err(|e| e.to_string()) };
            match r { Ok(r) => { let s = services_of_logs(&r); let n = items_logs(&r); (None, Some(r), None, s, n) } Err(e) => return bad(e) }
        }
        Sig::Metrics => {
            let r: Result<ExportMetricsServiceRequest, String> = if json_body { otlp_json(&raw, "resourceMetrics") } else { ExportMetricsServiceRequest::decode(&raw[..]).map_err(|e| e.to_string()) };
            match r { Ok(r) => { let s = services_of_metrics(&r); let n = items_metrics(&r); (None, None, Some(r), s, n) } Err(e) => return bad(e) }
        }
    };
    match verdict(&ctx, sig, &services, Some(&h), items).await {
        Err(code) => {
            let mut resp = (StatusCode::from_u16(code).unwrap_or(StatusCode::SERVICE_UNAVAILABLE), "provoked by Stuntman (Exporter settings)").into_response();
            if code == 429 || code == 503 {
                resp.headers_mut().insert("retry-after", "5".parse().unwrap());
            }
            return resp;
        }
        Ok(true) => {
            forward(&ctx, sig, body.to_vec(), h.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/x-protobuf"));
            keep(&ctx, sig, t, l, m).await;
        }
        Ok(false) => {}
    }
    if json_body {
        ([("content-type", "application/json")], "{}").into_response()
    } else {
        ([("content-type", "application/x-protobuf")], Vec::<u8>::new()).into_response()
    }
}

/// /v1/<signal> goes to the host's environment; /e/<name>/v1/<signal> to that one.
fn http_router(envs: Envs) -> Router {
    async fn by_host(State(e): State<Envs>, Path(sig): Path<String>, h: HeaderMap, b: Bytes) -> Response {
        let c = e.by_host(h.get("host").and_then(|v| v.to_str().ok()));
        route(c, &sig, h, b).await
    }
    async fn by_name(State(e): State<Envs>, Path((name, sig)): Path<(String, String)>, h: HeaderMap, b: Bytes) -> Response {
        route(e.get(&name), &sig, h, b).await
    }
    async fn route(c: Option<Ctx>, sig: &str, h: HeaderMap, b: Bytes) -> Response {
        let Some(c) = c else { return (StatusCode::NOT_FOUND, "No Stuntman environment answers for this host").into_response() };
        let s = match sig {
            "traces" => Sig::Traces,
            "logs" => Sig::Logs,
            "metrics" => Sig::Metrics,
            _ => return StatusCode::NOT_FOUND.into_response(),
        };
        http_export(c, s, h, b).await
    }
    Router::new().route("/v1/{sig}", post(by_host)).route("/e/{name}/v1/{sig}", post(by_name)).with_state(envs)
}

// ---------------------------------------------------------------- OTLP/gRPC

/// gRPC has no path to carry a prefix: the x-stuntman-env metadata (OTEL_EXPORTER_OTLP_HEADERS)
/// names the environment, else the default one.
struct Grpc(Envs);

impl Grpc {
    fn ctx<T>(&self, r: &tonic::Request<T>) -> Result<Ctx, tonic::Status> {
        match r.metadata().get("x-stuntman-env").or(r.metadata().get("x-duble-env")).and_then(|v| v.to_str().ok()) {
            Some(n) => self.0.get(n).ok_or_else(|| tonic::Status::not_found(format!("No Stuntman environment {n}"))),
            None => self.0.default_env().ok_or_else(|| tonic::Status::unavailable("no environment")),
        }
    }
}

fn status(code: u16) -> tonic::Status {
    match code {
        401 => tonic::Status::unauthenticated("provoked by Stuntman"),
        403 => tonic::Status::permission_denied("provoked by Stuntman"),
        429 => tonic::Status::resource_exhausted("provoked by Stuntman"),
        _ => tonic::Status::unavailable("provoked by Stuntman"),
    }
}

#[tonic::async_trait]
impl TraceService for Grpc {
    async fn export(&self, r: tonic::Request<ExportTraceServiceRequest>) -> Result<tonic::Response<ExportTraceServiceResponse>, tonic::Status> {
        let ctx = self.ctx(&r)?;
        let r = r.into_inner();
        match verdict(&ctx, Sig::Traces, &services_of_traces(&r), None, items_traces(&r)).await {
            Err(c) => return Err(status(c)),
            Ok(true) => {
                forward(&ctx, Sig::Traces, r.encode_to_vec(), "application/x-protobuf");
                keep(&ctx, Sig::Traces, Some(r), None, None).await
            }
            Ok(false) => {}
        }
        Ok(tonic::Response::new(ExportTraceServiceResponse { partial_success: None }))
    }
}

#[tonic::async_trait]
impl LogsService for Grpc {
    async fn export(&self, r: tonic::Request<ExportLogsServiceRequest>) -> Result<tonic::Response<ExportLogsServiceResponse>, tonic::Status> {
        let ctx = self.ctx(&r)?;
        let r = r.into_inner();
        match verdict(&ctx, Sig::Logs, &services_of_logs(&r), None, items_logs(&r)).await {
            Err(c) => return Err(status(c)),
            Ok(true) => {
                forward(&ctx, Sig::Logs, r.encode_to_vec(), "application/x-protobuf");
                keep(&ctx, Sig::Logs, None, Some(r), None).await
            }
            Ok(false) => {}
        }
        Ok(tonic::Response::new(ExportLogsServiceResponse { partial_success: None }))
    }
}

#[tonic::async_trait]
impl MetricsService for Grpc {
    async fn export(&self, r: tonic::Request<ExportMetricsServiceRequest>) -> Result<tonic::Response<ExportMetricsServiceResponse>, tonic::Status> {
        let ctx = self.ctx(&r)?;
        let r = r.into_inner();
        match verdict(&ctx, Sig::Metrics, &services_of_metrics(&r), None, items_metrics(&r)).await {
            Err(c) => return Err(status(c)),
            Ok(true) => {
                forward(&ctx, Sig::Metrics, r.encode_to_vec(), "application/x-protobuf");
                keep(&ctx, Sig::Metrics, None, None, Some(r)).await
            }
            Ok(false) => {}
        }
        Ok(tonic::Response::new(ExportMetricsServiceResponse { partial_success: None }))
    }
}

// ---------------------------------------------------------------- listeners

fn handles() -> &'static Mutex<Option<(u16, u16, JoinHandle<()>, JoinHandle<()>)>> {
    static H: OnceLock<Mutex<Option<(u16, u16, JoinHandle<()>, JoinHandle<()>)>>> = OnceLock::new();
    H.get_or_init(Default::default)
}

pub fn bind_error() -> &'static Mutex<Option<String>> {
    static E: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    E.get_or_init(Default::default)
}

/// Starts, moves or stops the listeners to match the settings. "Connection refused" for all
/// services takes them down for real.
pub async fn reconcile(envs: &Envs) {
    let Some(ctx) = envs.default_env() else { return };
    let Ok(s) = config::get(&ctx) else { return };
    let refuse = s.provoked_for("").is_some() && s.collector.answer == "refused" && s.collector.only_for == "all";
    let want = (!refuse).then_some((s.receive.http, s.receive.grpc));
    let mut h = handles().lock().unwrap();
    let current = h.as_ref().map(|x| (x.0, x.1));
    if current == want {
        return;
    }
    if let Some((_, _, a, b)) = h.take() {
        a.abort();
        b.abort();
    }
    let Some((hp, gp)) = want else { return };
    let std_http = match std::net::TcpListener::bind(("0.0.0.0", hp)) {
        Ok(l) => l,
        Err(e) => {
            *bind_error().lock().unwrap() = Some(format!("OTLP/HTTP port {hp}: {e}"));
            return;
        }
    };
    let std_grpc = match std::net::TcpListener::bind(("0.0.0.0", gp)) {
        Ok(l) => l,
        Err(e) => {
            *bind_error().lock().unwrap() = Some(format!("OTLP/gRPC port {gp}: {e}"));
            return;
        }
    };
    *bind_error().lock().unwrap() = None;
    std_http.set_nonblocking(true).unwrap();
    std_grpc.set_nonblocking(true).unwrap();
    let app = http_router(envs.clone());
    let a = tokio::spawn(async move {
        let l = tokio::net::TcpListener::from_std(std_http).unwrap();
        let _ = axum::serve(l, app).await;
    });
    let c = envs.clone();
    let b = tokio::spawn(async move {
        let l = tokio::net::TcpListener::from_std(std_grpc).unwrap();
        let incoming = tokio_stream_shim::listener(l);
        let _ = tonic::transport::Server::builder()
            .add_service(TraceServiceServer::new(Grpc(c.clone())))
            .add_service(LogsServiceServer::new(Grpc(c.clone())))
            .add_service(MetricsServiceServer::new(Grpc(c)))
            .serve_with_incoming(incoming)
            .await;
    });
    *h = Some((hp, gp, a, b));
}

/// tonic wants a stream of connections; this turns a listener into one.
mod tokio_stream_shim {
    pub fn listener(l: tokio::net::TcpListener) -> impl futures::Stream<Item = std::io::Result<tokio::net::TcpStream>> {
        futures::stream::unfold(l, |l| async move {
            let r = l.accept().await.map(|(s, _)| s);
            Some((r, l))
        })
    }
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn otlp_json_metrics_as_the_spec_writes_them() {
        let body = br#"{"resourceMetrics":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"acme-api"}}]},"scopeMetrics":[{"metrics":[
          {"name":"h","unit":"s","histogram":{"aggregationTemporality":1,"dataPoints":[{"timeUnixNano":"2","startTimeUnixNano":"1","count":"3","sum":0.6,"bucketCounts":["0","3"],"explicitBounds":[0.1]}]}},
          {"name":"c","sum":{"aggregationTemporality":2,"isMonotonic":true,"dataPoints":[{"timeUnixNano":"2","asInt":"7"}]}},
          {"name":"g","gauge":{"dataPoints":[{"timeUnixNano":"2","asDouble":0.5}]}}]}]}]}"#;
        let r: ExportMetricsServiceRequest = otlp_json(body, "resourceMetrics").unwrap();
        let ms = &r.resource_metrics[0].scope_metrics[0].metrics;
        assert!(ms.iter().all(|m| m.data.is_some()), "every metric keeps its data");
        let empty: ExportTraceServiceRequest = otlp_json(b"{}", "resourceSpans").unwrap();
        assert!(empty.resource_spans.is_empty());
    }
}
