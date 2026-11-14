//! Sends a realistic trace, logs and a metric like an app's OTLP exporter would, over OTLP/HTTP
//! protobuf or OTLP/gRPC. Used by tests/e2e_telemetry.py and handy to fill the screens.
//!
//!     cargo run -p stuntman-telemetry --example otlp_send -- http http://localhost:4318 <trace-hex32> [stuntman-http-port]
//!     cargo run -p stuntman-telemetry --example otlp_send -- grpc http://localhost:4317 <trace-hex32>

use opentelemetry_proto::tonic::{
    collector::{
        logs::v1::{logs_service_client::LogsServiceClient, ExportLogsServiceRequest},
        metrics::v1::{metrics_service_client::MetricsServiceClient, ExportMetricsServiceRequest},
        trace::v1::{trace_service_client::TraceServiceClient, ExportTraceServiceRequest},
    },
    common::v1::{any_value::Value, AnyValue, KeyValue},
    logs::v1::{LogRecord, ResourceLogs, ScopeLogs},
    metrics::v1::{metric::Data, Exemplar, Histogram, HistogramDataPoint, Metric, ResourceMetrics, ScopeMetrics},
    resource::v1::Resource,
    trace::v1::{span::Event, status::StatusCode, ResourceSpans, ScopeSpans, Span, Status},
};
use prost::Message;

fn kv(k: &str, v: Value) -> KeyValue {
    KeyValue { key: k.into(), value: Some(AnyValue { value: Some(v) }) }
}
fn s(v: &str) -> Value {
    Value::StringValue(v.into())
}
fn res(service: &str) -> Option<Resource> {
    Some(Resource { attributes: vec![kv("service.name", s(service)), kv("host.name", s("acme-api-7c9f"))], ..Default::default() })
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (mode, ep, trace) = (args[1].clone(), args[2].clone(), hex::decode(&args[3]).unwrap());
    let stuntman_port = args.get(4).cloned().unwrap_or("8025".into());
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
    let t0 = now - 900_000_000;
    let ms = 1_000_000u64;
    let sid = |n: u8| vec![0xa0, 0, 0, 0, 0, 0, 0, n];
    let span = |n: u8, parent: Option<u8>, name: &str, kind: i32, a: u64, b: u64, attrs: Vec<KeyValue>, err: bool, events: Vec<Event>| Span {
        trace_id: trace.clone(), span_id: sid(n), parent_span_id: parent.map(sid).unwrap_or_default(), name: name.into(), kind,
        start_time_unix_nano: t0 + a * ms, end_time_unix_nano: t0 + b * ms, attributes: attrs, events,
        status: Some(Status { code: if err { StatusCode::Error as i32 } else { StatusCode::Unset as i32 }, message: String::new() }), ..Default::default()
    };
    let exc = Event { time_unix_nano: t0 + 840 * ms, name: "exception".into(), attributes: vec![
        kv("exception.type", s("TimeoutException")), kv("exception.message", s("upload to blob storage timed out after 2 attempts")),
        kv("exception.stacktrace", s("TimeoutException: upload to blob storage timed out after 2 attempts\n  at Acme.Orders.ContractStore.SaveAsync()\n  at Acme.Orders.CreateOrderHandler.Handle()"))], ..Default::default() };
    let spans = vec![
        span(1, None, "POST /orders", 2, 0, 842, vec![kv("http.request.method", s("POST")), kv("http.route", s("/orders")), kv("http.response.status_code", Value::IntValue(500))], true, vec![exc]),
        span(2, Some(1), "INSERT orders", 3, 10, 22, vec![kv("db.system", s("postgresql"))], false, vec![]),
        span(3, Some(1), "Azure Blob PUT contracts/58213.pdf", 3, 30, 330, vec![kv("server.address", s("acmefiles.blob.stuntman.local")), kv("http.response.status_code", Value::IntValue(503))], true, vec![]),
        span(4, Some(1), "Azure Blob PUT contracts/58213.pdf", 3, 540, 790, vec![kv("server.address", s("acmefiles.blob.stuntman.local")), kv("http.response.status_code", Value::IntValue(503))], true, vec![]),
        span(5, Some(1), "SendGrid POST /v3/mail/send", 3, 795, 830, vec![kv("url.full", s(&format!("http://localhost:{stuntman_port}/sendgrid/v3/mail/send"))), kv("http.response.status_code", Value::IntValue(202))], false, vec![]),
        span(6, Some(1), "respond 500", 1, 832, 842, vec![], true, vec![]),
    ];
    let traces = ExportTraceServiceRequest { resource_spans: vec![ResourceSpans { resource: res("acme-api"), scope_spans: vec![ScopeSpans { spans, ..Default::default() }], ..Default::default() }] };
    let log = |t: u64, sev: i32, text: &str, body: &str, span_n: u8| LogRecord {
        time_unix_nano: t0 + t * ms, severity_number: sev, severity_text: text.into(), body: Some(AnyValue { value: Some(s(body)) }),
        trace_id: trace.clone(), span_id: sid(span_n), attributes: vec![kv("order.id", Value::IntValue(58213)), kv("tenant", s("globex"))], ..Default::default()
    };
    let logs = ExportLogsServiceRequest { resource_logs: vec![ResourceLogs { resource: res("acme-api"), scope_logs: vec![ScopeLogs { log_records: vec![
        log(5, 9, "INFO", "Creating order 58213 for Globex Inc", 1),
        log(331, 13, "WARN", "Blob upload got 503, retrying in 200 ms (attempt 1)", 3),
        log(791, 13, "WARN", "Blob upload got 503 again (attempt 2); giving up", 4),
        log(840, 17, "ERROR", "Order 58213 failed: contract upload timed out after 2 attempts", 6),
    ], ..Default::default() }], ..Default::default() }] };
    let point = |host: &str, count: u64, buckets: Vec<u64>, t: u64| HistogramDataPoint {
        attributes: vec![kv("server.address", s(host)), kv("http.request.method", s("PUT"))], start_time_unix_nano: t0 - 3_600_000 * ms, time_unix_nano: t,
        count, sum: Some(count as f64 * 0.3), bucket_counts: buckets, explicit_bounds: vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5],
        exemplars: vec![Exemplar { time_unix_nano: t, trace_id: trace.clone(), span_id: sid(4), value: Some(opentelemetry_proto::tonic::metrics::v1::exemplar::Value::AsDouble(1.21)), ..Default::default() }],
        ..Default::default()
    };
    let metrics = ExportMetricsServiceRequest { resource_metrics: vec![ResourceMetrics { resource: res("acme-api"), scope_metrics: vec![ScopeMetrics { metrics: vec![Metric {
        name: "http.client.request.duration".into(), unit: "s".into(), description: "Duration of HTTP client requests".into(),
        data: Some(Data::Histogram(Histogram { aggregation_temporality: 2, data_points: vec![
            point("acmefiles.blob.stuntman.local", 10, vec![0, 2, 6, 2, 0, 0, 0], now - 120_000 * ms),
            point("acmefiles.blob.stuntman.local", 30, vec![0, 2, 6, 2, 4, 16, 0], now - 10_000 * ms),
            point("maps.example-geo.com", 12, vec![0, 0, 2, 10, 0, 0, 0], now - 10_000 * ms),
        ] })), ..Default::default() }], ..Default::default() }], ..Default::default() }] };

    if mode == "grpc" {
        TraceServiceClient::connect(ep.clone()).await.unwrap().export(traces).await.unwrap();
        LogsServiceClient::connect(ep.clone()).await.unwrap().export(logs).await.unwrap();
        MetricsServiceClient::connect(ep).await.unwrap().export(metrics).await.unwrap();
    } else {
        let c = reqwest::Client::new();
        for (path, body) in [("traces", traces.encode_to_vec()), ("logs", logs.encode_to_vec()), ("metrics", metrics.encode_to_vec())] {
            let r = c.post(format!("{ep}/v1/{path}")).header("content-type", "application/x-protobuf").header("api-key", "NRAK-anything").body(body).send().await.unwrap();
            assert!(r.status().is_success(), "{path}: {}", r.status());
        }
    }
    println!("sent {mode}");
}
