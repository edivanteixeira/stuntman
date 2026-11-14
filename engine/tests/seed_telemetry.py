"""Synthetic telemetry like the T1–T4 screens: acme-api's failing POST /orders (with the e-mail it sent
to Stuntman in the same trace), a few other traces from acme-worker and acme-front, their logs, metrics.

    cargo build -p stuntman-telemetry --example otlp_send && python3 tests/seed_telemetry.py [http://localhost:8025] [otlp http 4318]
"""
import json, os, secrets, subprocess, sys, time, urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8025"
OTLP = sys.argv[2] if len(sys.argv) > 2 else "4318"
PORT = BASE.rsplit(":", 1)[1]
SENDER = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "target", "debug", "examples", "otlp_send")


def post(path, body):
    urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{OTLP}/v1/{path}", method="POST", data=json.dumps(body).encode(), headers={"Content-Type": "application/json"}))


def span(tid, sid, parent, name, ms_ago, dur, kind=2, status=200, attrs=()):
    now = time.time_ns() - ms_ago * 1_000_000
    a = [{"key": "http.response.status_code", "value": {"intValue": str(status)}}] + [{"key": k, "value": {"stringValue": v}} for k, v in attrs]
    return {"traceId": tid, "spanId": sid, "parentSpanId": parent, "name": name, "kind": kind, "startTimeUnixNano": str(now - dur * 1_000_000), "endTimeUnixNano": str(now), "attributes": a,
            "status": {"code": 2 if status >= 500 else 0}}


def res(service, spans):
    return {"resourceSpans": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": service}}]}, "scopeSpans": [{"spans": spans}]}]}


t1 = secrets.token_hex(16)
req = urllib.request.Request(f"{BASE}/sendgrid/v3/mail/send", method="POST", headers={"Authorization": "Bearer SG.demo", "Content-Type": "application/json", "User-Agent": "acme-api/2.50",
                             "traceparent": f"00-{t1}-a000000000000005-01"},
                             data=json.dumps({"personalizations": [{"to": [{"email": "emma.carter@example.com"}]}], "from": {"email": "no-reply@acme.example.com", "name": "Acme"},
                                              "subject": "We could not create order 58213", "content": [{"type": "text/plain", "value": "Your order could not be completed. We are on it."}]}).encode())
urllib.request.urlopen(req)
subprocess.run([SENDER, "http", f"http://127.0.0.1:{OTLP}", t1, PORT], check=True, capture_output=True)

for i, (svc, name, dur, status, extra) in enumerate([
    ("acme-api", "GET /orders/58213/invoice", 96, 200, []), ("acme-worker", "orders-created process", 1310, 200, []),
    ("acme-api", "POST /sign/58190", 1640, 200, []), ("acme-api", "GET /health", 4, 200, []), ("acme-front", "GET /", 120, 200, []),
    ("acme-api", "POST /webhooks/blob", 84, 200, [])]):
    tid = secrets.token_hex(16)
    spans = [span(tid, f"{i + 1:016x}", "", name, 2000 * (i + 1), dur, status=status)]
    if name.startswith("POST /sign"):
        spans.append(span(tid, f"c{i:015x}", f"{i + 1:016x}", "Azure Blob GET contracts/58190.pdf", 2000 * (i + 1) + 100, 1200, kind=3, attrs=[("server.address", "acmefiles.blob.stuntman.local")]))
    post("traces", res(svc, spans))

now = time.time_ns()
post("logs", {"resourceLogs": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "acme-worker"}}]}, "scopeLogs": [{"logRecords": [
    {"timeUnixNano": str(now - 5_000_000_000), "severityNumber": 9, "severityText": "INFO", "body": {"stringValue": "Message #1042 completed by acme-worker-1"}},
    {"timeUnixNano": str(now - 7_000_000_000), "severityNumber": 13, "severityText": "WARN", "body": {"stringValue": "Lock lost on #1043, the message will be delivered again"}},
    {"timeUnixNano": str(now - 9_000_000_000), "severityNumber": 9, "severityText": "INFO", "body": {"stringValue": "{\"event\": \"order.created\", \"order\": {\"id\": 58214, \"total\": 1290.0}}"}}]}]}]})
print("seeded telemetry", BASE, OTLP, "trace", t1)

# Fourteen minutes of metrics with a jump in the last three: blob uploads slow and failing (answered by
# Stuntman), a real geo host steady; requests to acme-api with 5xx while it lasted.
def hist_point(host, t_ns, count, buckets, errors=0):
    return {"timeUnixNano": str(t_ns), "startTimeUnixNano": str(t_ns - 60_000_000_000), "count": str(count), "sum": count * 0.2,
            "bucketCounts": [str(b) for b in buckets], "explicitBounds": [0.05, 0.1, 0.25, 0.5, 1.0, 2.5],
            "attributes": [{"key": "server.address", "value": {"stringValue": host}}] + ([{"key": "error.type", "value": {"stringValue": "503"}}] if errors else [])}

now = time.time_ns()
client, server = [], []
for m in range(14, 0, -1):
    t = now - m * 60_000_000_000 + 30_000_000_000
    jump = m <= 3
    client.append(hist_point("acmefiles.blob.stuntman.local", t, 10, [0, 6, 4, 0, 0, 0, 0] if not jump else [0, 0, 0, 0, 2, 8, 0]))
    if jump:
        client.append(hist_point("acmefiles.blob.stuntman.local", t, 3, [0, 0, 0, 0, 0, 3, 0], errors=1))
    client.append(hist_point("maps.example-geo.com", t, 1, [0, 0, 0, 1, 0, 0, 0]))
    for code, n in ([("200", 52), ("500", 0)] if not jump else [("200", 40), ("500", 9)]):
        if n:
            server.append({"timeUnixNano": str(t), "startTimeUnixNano": str(t - 60_000_000_000), "count": str(n), "sum": n * 0.1, "bucketCounts": [str(n), "0", "0", "0", "0", "0", "0"],
                           "explicitBounds": [0.05, 0.1, 0.25, 0.5, 1.0, 2.5], "attributes": [{"key": "http.response.status_code", "value": {"intValue": code}}]})
post("metrics", {"resourceMetrics": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "acme-api"}}]}, "scopeMetrics": [{"metrics": [
    {"name": "http.client.request.duration", "unit": "s", "histogram": {"aggregationTemporality": 1, "dataPoints": client}},
    {"name": "http.server.request.duration", "unit": "s", "histogram": {"aggregationTemporality": 1, "dataPoints": server}}]}]}]})

# What Stuntman answered during the jump: the bounces@ inbox rejects mail by rule.
urllib.request.urlopen(urllib.request.Request(f"{BASE}/sendgrid/v3/mail/send", method="POST", headers={"Authorization": "Bearer SG.demo", "Content-Type": "application/json", "User-Agent": "acme-api/2.50"},
    data=json.dumps({"personalizations": [{"to": [{"email": "bounces@acme.example.com"}]}], "from": {"email": "no-reply@acme.example.com"},
                     "subject": "Action required: Lease · Northwind", "content": [{"type": "text/plain", "value": "Please sign."}]}).encode()))
print("seeded metrics with a jump")
