"""End-to-end check of telemetry (T1–T4): an app exporting OTLP (JSON, protobuf over HTTP, gRPC),
joined to the trips it made to Stuntman, plus the collector's provoked failures and forwarding.

    cargo build -p stuntman && cargo build -p stuntman-telemetry --example otlp_send && python3 tests/e2e_telemetry.py
"""
import http.server, json, os, secrets, shutil, socket, subprocess, tempfile, threading, time, urllib.error, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SENDER = os.path.join(ROOT, "target", "debug", "examples", "otlp_send")


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


HTTP, OTLP, GRPC, APM = free_port(), free_port(), free_port(), free_port()
API = f"http://127.0.0.1:{HTTP}/api"
FORWARDED = []


class Apm(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers["Content-Length"])); FORWARDED.append((self.path, self.headers.get("api-key")))
        self.send_response(200); self.end_headers()

    def log_message(self, *a):
        pass


def call(method, path, body=None):
    req = urllib.request.Request(API + path, method=method, data=None if body is None else json.dumps(body).encode(), headers={"Content-Type": "application/json", "X-Stuntman-Client": "qa-suite"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def otlp_json(path, body):
    req = urllib.request.Request(f"http://127.0.0.1:{OTLP}/v1/{path}", method="POST", data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code
    except (urllib.error.URLError, ConnectionError) as e:
        return type(getattr(e, "reason", e)).__name__


def json_trace(tid, name="GET /health", service="acme-front", ms=4):
    now = time.time_ns()
    return {"resourceSpans": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": service}}]},
            "scopeSpans": [{"spans": [{"traceId": tid, "spanId": "b000000000000001", "name": name, "kind": 2,
                                       "startTimeUnixNano": str(now - ms * 1_000_000), "endTimeUnixNano": str(now),
                                       "attributes": [{"key": "http.response.status_code", "value": {"intValue": "200"}}]}]}]}]}


def json_logs(n, service="acme-worker"):
    now = time.time_ns()
    return {"resourceLogs": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": service}}]},
            "scopeLogs": [{"logRecords": [{"timeUnixNano": str(now + i), "severityNumber": 9, "severityText": "INFO", "body": {"stringValue": f"Message #{1040 + i} completed"}} for i in range(n)]}]}]}


def main():
    data = tempfile.mkdtemp(prefix="stuntman-tel-")
    apm = http.server.ThreadingHTTPServer(("127.0.0.1", APM), Apm)
    threading.Thread(target=apm.serve_forever, daemon=True).start()
    os.makedirs(os.path.join(data, "environments/stg-a"))  # secrets belong to an environment
    with open(os.path.join(data, "environments/stg-a/secrets.yaml"), "w") as f:
        f.write("apm-license-key: NRAK-test-key\n")
    env = dict(os.environ, STUNTMAN_DATA=data, STUNTMAN_PORT=str(HTTP), STUNTMAN_OTLP_HTTP_PORT=str(OTLP), STUNTMAN_OTLP_GRPC_PORT=str(GRPC),
               STUNTMAN_HOOKS_PORT=str(free_port()), STUNTMAN_SMTP_PORT=str(free_port()), STUNTMAN_WEB="/nonexistent")
    srv = subprocess.Popen([os.path.join(ROOT, "target", "debug", "stuntman")], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    try:
        for _ in range(60):
            try:
                socket.create_connection(("127.0.0.1", OTLP), 0.2).close(); socket.create_connection(("127.0.0.1", HTTP), 0.2).close(); break
            except OSError:
                time.sleep(0.1)
        run()
        print("e2e telemetry: all checks passed")
    finally:
        srv.terminate(); apm.shutdown(); shutil.rmtree(data, ignore_errors=True)


def run():
    # T1 empty state, then JSON over HTTP.
    _, tr = call("GET", "/telemetry/traces?since=1h")
    assert tr["traces"] == [], tr
    t_health = secrets.token_hex(16)
    assert otlp_json("traces", json_trace(t_health)) == 200
    _, tr = call("GET", "/telemetry/traces?since=1h")
    assert tr["traces"][0]["traceId"] == t_health and tr["traces"][0]["touchesProvider"] is False

    # The app's trace (protobuf over HTTP), and the e-mail it sent to Stuntman with the same trace context.
    t1 = secrets.token_hex(16)
    req = urllib.request.Request(f"http://127.0.0.1:{HTTP}/sendgrid/v3/mail/send", method="POST", headers={
        "Authorization": "Bearer SG.x", "Content-Type": "application/json", "traceparent": f"00-{t1}-a000000000000005-01"},
        data=json.dumps({"personalizations": [{"to": [{"email": "emma.carter@example.com"}]}], "from": {"email": "no-reply@acme.example.com"},
                         "subject": "Order 58213 failed", "content": [{"type": "text/plain", "value": "We could not store your contract."}]}).encode())
    assert urllib.request.urlopen(req).status == 202
    out = subprocess.run([SENDER, "http", f"http://127.0.0.1:{OTLP}", t1, str(HTTP)], capture_output=True, text=True, timeout=60)
    assert out.returncode == 0, out.stderr
    st, t = call("GET", f"/telemetry/traces/{t1}")
    assert st == 200 and t["name"] == "POST /orders" and len(t["spans"]) == 6, t
    by = {s["spanId"][-2:]: s for s in t["spans"]}
    assert by["05"]["reachedStuntman"] and by["05"]["trip"]["module"] == "email", by["05"]
    assert by["03"]["reachedStuntman"] and by["04"]["reachedStuntman"], "blob spans point at a stuntman host"
    assert by["01"]["exception"]["exception.type"] == "TimeoutException" and by["01"]["depth"] == 0 and by["02"]["depth"] == 1
    _, lt = call("GET", "/telemetry/traces?status=error&touching_provider=true&since=1h")
    assert [x["traceId"] for x in lt["traces"]] == [t1] and lt["traces"][0]["statusCode"] == 500, lt
    _, sl = call("GET", f"/telemetry/traces/{t1}/slow?over_ms=200")
    assert sl["spans"][0]["reachedStuntman"], sl
    _, back = call("GET", f"/telemetry/trips/{by['05']['trip']['id']}/trace")
    assert back["traceId"] == t1, back

    # gRPC, a second service.
    t2 = secrets.token_hex(16)
    out = subprocess.run([SENDER, "grpc", f"http://127.0.0.1:{GRPC}", t2], capture_output=True, text=True, timeout=60)
    assert out.returncode == 0, out.stderr
    assert call("GET", f"/telemetry/traces/{t2}")[0] == 200

    # T2 logs: full-text search, by trace, around a line, counts, wait.
    _, lg = call("GET", "/telemetry/logs?text=contract%20upload&since=1h")
    assert lg["total"] == 2 and lg["logs"][0]["level"] == "ERROR", lg  # one per trace (http and grpc)
    _, lot = call("GET", f"/telemetry/traces/{t1}/logs")
    assert [l["level"] for l in lot["logs"]] == ["INFO", "WARN", "WARN", "ERROR"], lot
    _, ar = call("GET", f"/telemetry/logs/{lot['logs'][2]['id']}/around?before=1&after=1")
    # Neighbours by time, across services and traces.
    assert ar["before"][0]["timeNs"] <= ar["line"]["timeNs"] <= ar["after"][0]["timeNs"] and ar["line"]["id"] == lot["logs"][2]["id"], ar
    _, cnt = call("GET", "/telemetry/logs/count?level=error&since=1h")
    assert cnt["count"] == 2
    _, w = call("GET", "/telemetry/logs/wait?level=error&timeout=2s&since=1h")
    assert w["level"] == "ERROR"
    assert lg["facets"]["levels"]["ERROR"] == 2 and "acme-api" in lg["facets"]["services"], lg["facets"]

    # T4 metrics: list, p95 by host, Stuntman vs real host, exemplars, compare, cardinality.
    _, lm = call("GET", "/telemetry/metrics")
    assert lm["metrics"][0]["name"] == "http.client.request.duration" and lm["metrics"][0]["category"] == "Provider calls", lm
    _, q = call("GET", "/telemetry/metrics/http.client.request.duration?stat=p95&by=server.address&since=15m&step=5m")
    blob = next(s for s in q["series"] if s["key"].startswith("acmefiles"))
    # Two senders (two processes, two start times) report the same series: 30 each, never mixed.
    assert blob["value"] > 1.0 and blob["count"] == 60, blob
    hosts = {h["key"]: h["stuntman"] for h in q["hosts"]}
    assert hosts["acmefiles.blob.stuntman.local"] is True and hosts["maps.example-geo.com"] is False, hosts
    _, ex = call("GET", f"/telemetry/metrics/http.client.request.duration/exemplars?at={int(time.time() * 1000) - 10_000}&step=1m")
    assert any(e["traceId"] in (t1, t2) for e in ex["exemplars"]), ex
    _, cmp = call("GET", "/telemetry/metrics/http.client.request.duration/compare?before=15m-1m&after=1m-0&stat=count&by=server.address")
    assert any(c["key"].startswith("acmefiles") for c in cmp["changes"]), cmp
    # A jump: 12 quiet minutes, then 3 slow ones on a Stuntman host; the explanation names it.
    now = time.time_ns()
    pts = [{"timeUnixNano": str(now - m * 60_000_000_000 + 30_000_000_000), "startTimeUnixNano": str(now - (m + 1) * 60_000_000_000), "count": "10", "sum": 1.0,
            "bucketCounts": (["0", "10", "0", "0"] if m > 3 else ["0", "0", "0", "10"]), "explicitBounds": [0.05, 0.1, 1.0],
            "attributes": [{"key": "server.address", "value": {"stringValue": "sb.stuntman.local"}}]} for m in range(15, 0, -1)]
    assert otlp_json("metrics", {"resourceMetrics": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "acme-worker"}}]},
        "scopeMetrics": [{"metrics": [{"name": "messaging.client.operation.duration", "unit": "s", "histogram": {"aggregationTemporality": 1, "dataPoints": pts}}]}]}]}) == 200
    _, wj = call("GET", "/telemetry/metrics/messaging.client.operation.duration/why?since=20m&step=1m")
    assert wj["jump"] and wj["drivers"][0]["key"] == "sb.stuntman.local" and wj["drivers"][0]["stuntman"] is True and wj["drivers"][0]["during"] > wj["drivers"][0]["before"], wj
    _, ts = call("GET", "/telemetry/metrics/http.client.request.duration/series?attribute=server.address")
    assert ts["distinct"] == 2, ts

    # T3: a signal off is answered 200 and dropped.
    call("PUT", "/telemetry/signals/logs", {"on": False})
    _, before = call("GET", "/telemetry/logs/count?since=1h")
    assert otlp_json("logs", json_logs(5)) == 200
    _, after = call("GET", "/telemetry/logs/count?since=1h")
    _, ov = call("GET", "/telemetry/overview")
    assert after["count"] == before["count"] and ov["dropped"]["logs"] == 5, (before, after, ov["dropped"])  # 5 log lines dropped
    call("PUT", "/telemetry/signals/logs", {"on": True})
    assert otlp_json("logs", json_logs(3)) == 200

    # Provoked collector: 503 over HTTP; gRPC unavailable; refused takes the listeners down; stop.
    _, p = call("POST", "/telemetry/collector/provoke", {"answer": "503", "for": "5m"})
    assert p["settings"]["collector"]["answer"] == "503" and p["settings"]["collector"]["setBy"] == "api:qa-suite"
    assert otlp_json("traces", json_trace(secrets.token_hex(16))) == 503
    out = subprocess.run([SENDER, "grpc", f"http://127.0.0.1:{GRPC}", secrets.token_hex(16)], capture_output=True, text=True, timeout=60)
    assert out.returncode != 0 and "Unavailable" in (out.stderr + out.stdout), out.stderr[-300:]
    call("POST", "/telemetry/collector/provoke", {"answer": "refused", "for": "5m"})
    time.sleep(0.3)
    assert otlp_json("traces", json_trace(secrets.token_hex(16))) in ("ConnectionRefusedError", "RemoteDisconnected", "ConnectionResetError"), "listener should be down"
    st, _ = call("DELETE", "/telemetry/collector/provoke")
    assert st == 200
    for _ in range(30):
        if otlp_json("traces", json_trace(secrets.token_hex(16))) == 200:
            break
        time.sleep(0.2)
    else:
        raise AssertionError("receiver did not come back after stop_provoking")
    call("POST", "/telemetry/collector/provoke", {"answer": "403", "onlyFor": "acme-worker", "for": "5m"})
    assert otlp_json("logs", json_logs(1, "acme-worker")) == 403 and otlp_json("logs", json_logs(1, "acme-front")) == 200, "only_for one service"
    call("DELETE", "/telemetry/collector/provoke")

    # Forwarding: the key is a Secrets reference; the real APM gets the batch with the resolved key.
    st, bad = call("PUT", "/telemetry/forwarding", {"on": True, "endpoint": "otlp.example-apm.com"})
    assert st == 400 and "https://" in bad["error"], bad
    st, bad = call("PUT", "/telemetry/forwarding", {"on": True, "endpoint": f"http://127.0.0.1:{APM}", "headerSecret": "NRAK-raw-key"})
    assert st == 400 and "never the key" in bad["error"], bad
    st, _ = call("PUT", "/telemetry/forwarding", {"on": True, "endpoint": f"http://127.0.0.1:{APM}", "headerSecret": "secret://apm-license-key"})
    assert st == 200
    assert otlp_json("traces", json_trace(secrets.token_hex(16))) == 200
    for _ in range(30):
        if FORWARDED:
            break
        time.sleep(0.1)
    assert FORWARDED and FORWARDED[0] == ("/v1/traces", "NRAK-test-key"), FORWARDED
    call("PUT", "/telemetry/forwarding", {"on": False})

    # Settings file round trip; MCP sees the same tools; clear.
    st, s = call("GET", "/telemetry/settings")
    assert st == 200 and "kind: telemetry" in s["yaml"]
    y = s["yaml"].replace("keep: 7d", "keep: 2d")
    st, s2 = call("PUT", "/telemetry/settings", {"yaml": y})
    assert st == 200 and s2["settings"]["signals"]["metrics"]["keep"] == "2d", s2
    h = {"Content-Type": "application/json"}
    r = urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{HTTP}/mcp", method="POST", headers=h, data=json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "find_slow_spans", "arguments": {"trace_id": t1, "over_ms": 200}}}).encode()))
    res = json.loads(json.loads(r.read())["result"]["content"][0]["text"])
    assert res["spans"][0]["reachedStuntman"], res
    st, bad = call("DELETE", "/telemetry/data")
    assert st == 400
    st, c = call("DELETE", "/telemetry/data", {"confirm": True})
    assert st == 200 and c["traces"] >= 3, c
    assert call("GET", "/telemetry/traces?since=1h")[1]["traces"] == []


if __name__ == "__main__":
    main()
