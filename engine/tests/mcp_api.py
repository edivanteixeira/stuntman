"""Exercises every tool through MCP and through HTTP against a running Stuntman, the way an agent and a
test suite would. Cleans up what it creates.

    python3 tests/mcp_api.py [http://localhost:8025] [smtp port 2587] [hooks port 8027] [otlp http port 4318]
"""
import hashlib, hmac, http.client as hc, json, smtplib, ssl, sys, time, urllib.error, urllib.request
from email.message import EmailMessage

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8025"
SMTP = int(sys.argv[2]) if len(sys.argv) > 2 else 2587
HOOKS = int(sys.argv[3]) if len(sys.argv) > 3 else 8027
OTLP = int(sys.argv[4]) if len(sys.argv) > 4 else 4318
RESULTS = []


def http(method, path, body=None, raw=False):
    req = urllib.request.Request(BASE + path, method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "X-Stuntman-Client": "mcp-api-check"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            data = r.read(); return r.status, (data if raw else json.loads(data or b"null"))
    except urllib.error.HTTPError as e:
        data = e.read(); return e.code, (data if raw else json.loads(data or b"null"))


class Mcp:
    def __init__(self):
        self.sid, self.n = None, 0

    def rpc(self, method, params=None):
        self.n += 1
        h = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream"}
        if self.sid:
            h["Mcp-Session-Id"] = self.sid
        body = {"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}}
        r = urllib.request.urlopen(urllib.request.Request(BASE + "/mcp", method="POST", data=json.dumps(body).encode(), headers=h), timeout=60)
        self.sid = r.headers.get("Mcp-Session-Id") or self.sid
        return json.loads(r.read())

    def call(self, _tool, **args):
        r = self.rpc("tools/call", {"name": _tool, "arguments": args})["result"]
        return r["isError"], json.loads(r["content"][0]["text"])


def check(label, ok, detail=""):
    RESULTS.append((label, bool(ok), detail))
    print(("  ok   " if ok else "  FAIL ") + label + (f"  ·  {detail}" if detail and not ok else ""))


def smtp_send(to, subject, text):
    m = EmailMessage(); m["From"] = "Acme <no-reply@acme.example.com>"; m["To"] = to; m["Subject"] = subject; m.set_content(text)
    with smtplib.SMTP("127.0.0.1", SMTP, timeout=10) as s:
        s.starttls(context=ssl._create_unverified_context()); s.login("mcp-check", "x"); s.send_message(m)


def main():
    a = Mcp()
    print("MCP protocol")
    init = a.rpc("initialize", {"protocolVersion": "2025-03-26", "clientInfo": {"name": "claude-code"}, "capabilities": {}})
    check("initialize returns serverInfo and a session", init["result"]["serverInfo"]["name"] == "stuntman" and a.sid)
    req = urllib.request.Request(BASE + "/mcp", method="POST", data=json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}).encode(),
                                 headers={"Content-Type": "application/json", "Mcp-Session-Id": a.sid})
    check("notifications/initialized → 202", urllib.request.urlopen(req).status == 202)
    check("ping", a.rpc("ping")["result"] == {})
    tools = a.rpc("tools/list")["result"]["tools"]
    names = {t["name"] for t in tools}
    _, catalog = http("GET", "/api/tools")
    check(f"tools/list has {len(tools)} tools, same as GET /api/tools", names == {t["name"] for t in catalog}, str(names ^ {t["name"] for t in catalog}))
    check("every tool has a description and an input schema", all(t["description"] and t["inputSchema"]["type"] == "object" for t in tools))
    check("unknown method → -32601", a.rpc("nope")["error"]["code"] == -32601)
    check("unknown tool → -32602", a.rpc("tools/call", {"name": "nope"})["error"]["code"] == -32602)

    print("\nServices (E3)")
    err, s = a.call("list_services", channel="email")
    check("list_services", not err and s["services"], s)
    name = s["services"][0]["service"]["name"]
    st, h = http("GET", f"/api/services/{name}")
    err, m = a.call("get_service", name=name)
    check("get_service: MCP == HTTP", not err and st == 200 and m["service"] == h["service"])
    err, c = a.call("create_service", channel="email", name="mcp-check", copy_from=name)
    check("create_service (copy)", not err and c["service"]["intake"]["smtp"]["port"] != h["service"]["intake"]["smtp"]["port"], c)
    err, u = a.call("update_service", name="mcp-check", patch={"limits": {"keep": "2d"}, "events": {"to": "http://127.0.0.1:8199/hooks/sendgrid"}})
    check("update_service returns the diff", not err and {d["field"] for d in u["diff"]} >= {"limits.keep"}, u.get("diff"))
    st, y = http("GET", "/api/services/mcp-check/yaml", raw=True)
    check("GET …/yaml is the file", st == 200 and b"keep: 2d" in y)
    st, p = http("PUT", "/api/services/mcp-check", {"yaml": y.decode().replace("keep: 2d", "keep: 1d")})
    check("PUT whole YAML (Edit as YAML)", st == 200 and p["service"]["limits"]["keep"] == "1d", p)
    st, bad = http("PUT", "/api/services/mcp-check", {"yaml": "kind: [broken"})
    check("broken YAML → 400 with the parser's reason", st == 400 and "YAML" in bad["error"], bad)
    err, k = a.call("create_key", name="mcp-check", intake="sendgridApi", label="qa-suite")
    check("create_key shows the secret once", not err and k["secret"].startswith("SG."), k)
    err, acc = a.call("set_access", name="mcp-check", intake="sendgridApi", mode="refuse", how="503")
    check("set_access is attributed to the agent", not err and acc["service"]["intake"]["sendgridApi"]["setBy"] == "mcp:claude-code", acc["service"]["intake"]["sendgridApi"])
    st, _ = http("POST", "/sendgrid/mcp-check/v3/mail/send", {"personalizations": [{"to": [{"email": "x@example.com"}]}], "from": {"email": "a@example.com"}, "subject": "s", "content": [{"type": "text/plain", "value": "t"}]})
    check("provider down: SendGrid answers 503", st == 503, st)
    a.call("set_access", name="mcp-check", intake="sendgridApi", mode="validate")
    req = urllib.request.Request(BASE + "/sendgrid/mcp-check/v3/mail/send", method="POST", headers={"Content-Type": "application/json", "Authorization": "Bearer wrong"},
                                 data=json.dumps({"personalizations": [{"to": [{"email": "x@example.com"}]}], "from": {"email": "a@example.com"}, "subject": "s", "content": [{"type": "text/plain", "value": "t"}]}).encode())
    try:
        urllib.request.urlopen(req); code = 202
    except urllib.error.HTTPError as e:
        code = e.code
    check("validate keys: wrong key → 401", code == 401, code)
    err, r = a.call("revoke_key", name="mcp-check", intake="sendgridApi", key_id=k["keyId"])
    check("revoke_key (mode falls back to any when no key is left)", not err and r["service"]["intake"]["sendgridApi"]["mode"] == "any", r)
    err, t = a.call("send_test_event", name="mcp-check", type="bounce")
    check("send_test_event reports the app's reply", not err and "reply" in t, t)
    err, e = a.call("set_access", name="mcp-check", intake="fax", mode="any")
    check("bad intake → error explains", err and "sendgridApi | smtp" in e["error"], e)

    print("\nInboxes (E2)")
    err, ib = a.call("create_inbox", prefix="mcp", ttl="10m", steps=["delivered +1s", "opened +500ms", "click:first +500ms"])
    addr = ib["address"]
    check("create_inbox (agent) with steps", not err and ib["inbox"]["createdBy"] == "mcp:claude-code" and len(ib["inbox"]["steps"]) == 3, ib)
    err, bad = a.call("update_inbox", address=addr, steps=["delivered +1s", {"action": "opened", "waitMs": 1000, "from": "step:3"}])
    check("order rule refused (a step counts from a later one)", err and "never run before" in bad["error"], bad)
    err, bad = a.call("update_inbox", address=addr, steps=["click:nosuch +1s"])
    check("unknown extractor in a click step refused", err and "no extractor" in bad["error"], bad)
    st, li = http("GET", "/api/inboxes?kind=temporary")
    check("GET /api/inboxes?kind=temporary lists it", st == 200 and any(i["address"] == addr for i in li["inboxes"]))
    err, ex = a.call("extend_inbox", address=addr, by="30m")
    check("extend_inbox", not err and ex["expiresAt"] > ib["expiresAt"], ex)

    print("\nMessages (E1) · SMTP in, agent waits")
    smtp_send(addr, "Your verification code", "Use this code to confirm: 604 218. Help: https://acme.example.com/help")
    err, w = a.call("wait_for_message", to=addr, extract="code", timeout="5s")
    check("wait_for_message(extract=code) over MCP", not err and w["value"] == "604218", w)
    mid = w["messageId"]
    st, wh = http("GET", f"/api/messages/wait?to={addr}&extract=code&timeout=5s")
    check("same wait over HTTP", st == 200 and wh["value"] == "604218", wh)
    for _ in range(40):  # steps run on timers: poll instead of guessing a sleep
        err, g = a.call("get_message", id=mid)
        if [e["type"] for e in g.get("events", [])][:4] == ["processed", "delivered", "open", "click"]:
            break
        time.sleep(0.2)
    check("steps ran: delivered → open → click", not err and [e["type"] for e in g["events"]][:4] == ["processed", "delivered", "open", "click"], [e["type"] for e in g["events"]])
    err, l = a.call("list_messages", to=addr, status="clicked")
    st, lh = http("GET", f"/api/messages?to={addr}&status=clicked")
    check("list_messages filters: MCP == HTTP", not err and l["total"] == 1 and lh["total"] == 1, (l.get("total"), lh.get("total")))
    err, gk = a.call("list_messages", group="kind", since="1h")
    check("list_messages group=kind", not err and any(gp["kind"] == "Your verification code" for gp in gk["groups"]))
    err, sr = a.call("simulate_recipient", id=mid, action="spamreport")
    check("simulate_recipient → event and the app's reply", not err and sr["event"]["event"] == "spamreport" and sr["reply"]["status"] == 200, sr)
    err, bad = a.call("simulate_recipient", id=mid, action="unsubscribe")
    check("unsubscribe without a link is refused", err and "no unsubscribe link" in bad["error"], bad)
    st, raw = http("GET", f"/api/messages/{mid}/raw", raw=True)
    check("GET …/raw is the .eml", st == 200 and b"Subject: Your verification code" in raw)
    err, ne = a.call("expect_no_message", to=addr, within="1s")
    check("expect_no_message passes when nothing new arrives", not err and ne["ok"], ne)
    st, to = http("GET", f"/api/messages/wait?to=nobody-{int(time.time())}@example.com&timeout=1s")
    check("wait timeout → 408 with what arrived", st == 408 and "arrived" in to["detail"], to)

    print("\nHeld message, decided by the agent")
    a.call("update_inbox", address=addr, on_arrival="hold", hold_for="5m")
    smtp_send(addr, "Action required: NDA · Globex Inc", "Sign here")
    held = None
    for _ in range(20):
        err, l = a.call("list_messages", to=addr, status="held")
        if l["messages"]:
            held = l["messages"][0]["id"]; break
        time.sleep(0.2)
    check("arrival rule hold → status held", held)
    err, d = a.call("decide_held", message_id=held, action="bounce", kind="hard")
    check("decide_held(bounce) sends the bounce event", not err and d["event"]["event"] == "bounce" and d["event"]["status"] == "5.1.1", d)
    err, again = a.call("decide_held", message_id=held, action="deliver")
    check("deciding twice → 409", err and again["status"] == 409, again)

    print("\nExtractors (E4)")
    err, t = a.call("test_extractor", channel="email", **{"from": "link", "rule": r"https://acme\.example\.com/(\w+)", "last": 20})
    check("test_extractor over the last messages, nothing saved", not err and t["matched"] >= 1, t)
    err, t = a.call("test_extractor", channel="email", **{"from": "text", "rule": "code: (\\d{3} \\d{3}", "message_id": mid})
    check("broken rule explains itself", not err and "nclosed" in t.get("error", ""), t)
    err, c = a.call("create_extractor", channel="email", name="helpPath", **{"from": "link", "rule": r"https://acme\.example\.com/(\w+)"})
    check("create_extractor", not err and c["name"] == "helpPath", c)
    st, _ = http("POST", "/api/channels/email/extractors", {"name": "helpPath", "from": "text", "rule": "x"})
    check("duplicate name → 409", st == 409)
    err, u = a.call("update_extractor", channel="email", name="helpPath", strip=True)
    check("update_extractor", not err and u["strip"] is True, u)
    st, y = http("GET", "/api/channels/email/extractors.yaml", raw=True)
    check("extractors.yaml has it", st == 200 and b"helpPath" in y)
    err, cp = a.call("copy_extractor", channel="email", name="helpPath", to_channel="sms")
    check("copy to SMS refused honestly (not installed)", err and "not installed" in cp["error"], cp)
    err, tb = a.call("tune_builtin", channel="email", min_digits=5, max_digits=4)
    check("tune_builtin validates digits", err, tb)
    err, li = a.call("list_extractors", channel="email")
    check("list_extractors with stats", not err and any(y["extractor"]["name"] == "helpPath" for y in li["yours"]) and li["builtin"][0]["name"] == "code")
    err, dl = a.call("delete_extractor", channel="email", name="helpPath")
    check("delete_extractor", not err and dl["ok"], dl)

    print("\nHTTP-only shapes")
    st, e = http("GET", "/api/messages/msg_9999")
    check("unknown message → 404 JSON", st == 404 and "No message" in e["error"])
    st, e = http("PATCH", "/api/inboxes/" + addr, None)
    st2, e2 = http("POST", "/api/inboxes", {"keep": "yes", "address": "not an address"})
    check("bad input → 400 with a reason", st2 == 400 and e2["error"], e2)
    st, cl = http("GET", "/api/clients")
    check("Connected list shows the agent (MCP) and this script (API)", any(c["name"] == "claude-code" and c["via"] == "MCP" for c in cl["clients"]) and any(c["name"] == "mcp-api-check" for c in cl["clients"]), cl)

    print("\nWebhook inboxes (H1–H2) · design names, MCP and HTTP")
    wname = f"mcp-wh-{int(time.time()) % 100000}"
    err, w = a.call("create_inbox", name=wname, expires="30m", fail=2, then=200, secret="whsec_mcp")
    check("create_inbox(name, expires, fail, then) makes a webhook inbox", not err and w["inbox"]["answer"]["mode"] == "fail_then" and w["url"].endswith(f"/in/{wname}"), w)
    st, wh = http("GET", f"/api/inboxes/{wname}")
    err, wm = a.call("get_inbox", name=wname)
    check("get_inbox(name): MCP == HTTP", not err and st == 200 and wm["inbox"] == wh["inbox"] and wm["summary"] == "fails 2 times, then 200", wm)
    def hook(eid, secret="whsec_mcp"):
        body = json.dumps({"type": "order.completed", "id": eid, "data": {"order": 58213}}).encode(); t = str(int(time.time()))
        sig = hmac.new(secret.encode(), f"{t}.".encode() + body, hashlib.sha256).hexdigest()
        c = hc.HTTPConnection("127.0.0.1", HOOKS, timeout=5)
        c.request("POST", f"/in/{wname}", body, {"Content-Type": "application/json", "X-Acme-Signature": f"t={t},v1={sig}", "X-Acme-Event-Id": eid, "User-Agent": "acme-api/2.50"})
        return c.getresponse().status
    codes = [hook("evt_m1") for _ in range(3)]
    check("the inbox answers 500, 500, 200 to the app's retries", codes == [500, 500, 200], codes)
    err, wd = a.call("wait_for_delivery", inbox=wname, event="order.completed", timeout="5s")
    check("wait_for_delivery returns the event with its attempts", not err and len(wd.get("attempts", [])) >= 1 and wd["signature"]["result"] == "valid", wd)
    err, ld = a.call("list_deliveries", inbox=wname)
    st, lh = http("GET", f"/api/inboxes/{wname}/deliveries")
    check("list_deliveries: MCP == HTTP (/inboxes/{inbox}/deliveries)", not err and ld["total"] == 3 and lh["total"] == 3, (ld.get("total"), lh.get("total")))
    did = ld["deliveries"][0]["id"]
    err, gd = a.call("get_delivery", id=did)
    st, gh = http("GET", f"/api/inboxes/deliveries/{did}")
    check("get_delivery: MCP == HTTP, 3 attempts of one event", not err and st == 200 and len(gd["attempts"]) == 3 and gd["id"] == gh["id"], gd.get("attempts"))
    err, si = a.call("set_inbox", name=wname, mode="error", status=503, reject_bad=True)
    check("set_inbox (agent) → always 503, strict signatures", not err and si["summary"].startswith("always answers 503") and "strict" in si["summary"], si)
    check("bad signature → 401 with strict signatures", hook("evt_m2", secret="wrong") == 401)
    st, pu = http("PUT", f"/api/inboxes/{wname}", {"mode": "always_200", "rejectBad": False})
    check("PUT /inboxes/{name} (HTTP, the sheet's path)", st == 200 and pu["summary"] == "answers 200", pu)
    err, bad = a.call("set_inbox", name=wname, mode="teleport")
    check("unknown mode refused with the list of modes", err and "mode:" in bad["error"], bad)
    err, li = a.call("list_webhook_inboxes")
    check("list_webhook_inboxes includes it", not err and any(i["inbox"]["name"] == wname for i in li["inboxes"]))
    st, yy = http("PUT", "/api/webhooks/inboxes.yaml", {"yaml": "- name: [broken"})
    check("broken webhooks YAML → 400", st == 400, yy)
    err, rm = a.call("remove_inbox", name=wname)
    check("remove_inbox", not err and rm["ok"], rm)
    check("after removal the app gets 410 Gone", hook("evt_m3") == 410)

    print("\nTelemetry (T1–T4) · MCP and HTTP")
    import secrets as _s
    tid = _s.token_hex(16)
    run = _s.token_hex(3)
    svc, word, metric = f"mcp-check-{run}", f"zebrafish{run}", f"mcpcheck{run}.orders.created"
    now = time.time_ns()
    def otlp(path, body):
        c = hc.HTTPConnection("127.0.0.1", OTLP, timeout=5)
        c.request("POST", f"/v1/{path}", json.dumps(body), {"Content-Type": "application/json"})
        return c.getresponse().status
    res = lambda svc: {"attributes": [{"key": "service.name", "value": {"stringValue": svc}}]}
    check("an empty OTLP/JSON batch is accepted", otlp("traces", {}) == 200)
    st = otlp("traces", {"resourceSpans": [{"resource": res(svc), "scopeSpans": [{"spans": [
        {"traceId": tid, "spanId": "c000000000000001", "name": "POST /mcp-check", "kind": 2, "startTimeUnixNano": str(now - 900_000_000), "endTimeUnixNano": str(now),
         "attributes": [{"key": "http.response.status_code", "value": {"intValue": "500"}}], "status": {"code": 2}},
        {"traceId": tid, "spanId": "c000000000000002", "parentSpanId": "c000000000000001", "name": "SendGrid POST /v3/mail/send", "kind": 3,
         "startTimeUnixNano": str(now - 500_000_000), "endTimeUnixNano": str(now - 400_000_000),
         "attributes": [{"key": "url.full", "value": {"stringValue": f"{BASE}/sendgrid/v3/mail/send"}}]}]}]}]})
    check("OTLP/JSON trace accepted", st == 200, st)
    otlp("logs", {"resourceLogs": [{"resource": res(svc), "scopeLogs": [{"logRecords": [
        {"timeUnixNano": str(now - 300_000_000), "severityNumber": 17, "severityText": "ERROR", "traceId": tid, "spanId": "c000000000000001", "body": {"stringValue": f"mcp check {word} failed"}}]}]}]})
    otlp("metrics", {"resourceMetrics": [{"resource": res(svc), "scopeMetrics": [{"metrics": [{"name": metric, "unit": "1",
        "sum": {"aggregationTemporality": 1, "isMonotonic": True, "dataPoints": [{"timeUnixNano": str(now), "asInt": "7", "attributes": [{"key": "tenant", "value": {"stringValue": "globex"}}]}]}}]}]}]})
    err, lt = a.call("list_traces", service=svc, status="error", since="5m")
    st, lh = http("GET", f"/api/telemetry/traces?service={svc}&status=error&since=5m")
    check("list_traces: MCP == HTTP", not err and [t["traceId"] for t in lt["traces"]] == [tid] == [t["traceId"] for t in lh["traces"]], lt)
    check("the client span to Stuntman counts as touching a provider", lt["traces"] and lt["traces"][0]["touchesProvider"], lt)
    err, gt = a.call("get_trace", trace_id=tid)
    check("get_trace: span tree with depth", not err and [s["depth"] for s in gt["spans"]] == [0, 1], gt.get("spans"))
    err, wt = a.call("wait_for_trace", route="POST /mcp-check", timeout="3s", since="5m")
    check("wait_for_trace", not err and wt["traceId"] == tid, wt)
    err, fs = a.call("find_slow_spans", trace_id=tid, over_ms=50)
    check("find_slow_spans puts provider spans first", not err and fs["spans"][0]["reachedStuntman"], fs)
    err, ll = a.call("list_logs", text=word, since="5m")
    check("list_logs full-text search", not err and ll["total"] == 1 and ll["logs"][0]["traceId"] == tid, ll)
    err, wl = a.call("wait_for_log", text=word, timeout="3s", since="5m")
    check("wait_for_log", not err and wl["level"] == "ERROR", wl)
    err, lo = a.call("logs_of_trace", trace_id=tid)
    st, loh = http("GET", f"/api/telemetry/traces/{tid}/logs")
    check("logs_of_trace: MCP == HTTP", not err and len(lo["logs"]) == 1 and lo == loh, lo)
    err, ar = a.call("lines_around", log_id=ll["logs"][0]["id"], before=2, after=2)
    check("lines_around", not err and ar["line"]["id"] == ll["logs"][0]["id"], ar)
    err, cl = a.call("count_logs", level="error", service=svc, since="5m")
    check("count_logs", not err and cl["count"] == 1, cl)
    err, lm = a.call("list_metrics", text=f"mcpcheck{run}")
    check("list_metrics", not err and lm["metrics"][0]["kind"] == "counter" and lm["metrics"][0]["category"] == "Custom", lm)
    err, qm = a.call("query_metric", name=metric, stat="count", by="tenant", since="5m", step="5m")
    check("query_metric by an attribute", not err and qm["series"][0]["key"] == "globex" and qm["series"][0]["count"] == 7, qm)
    err, cw = a.call("compare_windows", name=metric, before="10m-5m", after="5m-0", stat="count")
    check("compare_windows", not err and cw["changes"] == [] or any(c["after"] == 7 for c in cw.get("changes", [])), cw)
    err, ts = a.call("top_series", name=metric, attribute="tenant", since="5m")
    check("top_series", not err and ts["distinct"] == 1, ts)
    err, me = a.call("metric_exemplars", name=metric, at=int(time.time() * 1000))
    check("metric_exemplars (none attached is an empty list)", not err and me["exemplars"] == [], me)
    err, wj = a.call("why_it_jumped", name=metric, stat="count", since="5m")
    st, wjh = http("GET", f"/api/telemetry/metrics/{metric}/why?stat=count&since=5m")
    check("why_it_jumped: MCP == HTTP (one point is not a jump)", not err and st == 200 and wj["jump"] is None and wjh["jump"] is None, wj)
    err, gs = a.call("get_telemetry_settings")
    st, gsh = http("GET", "/api/telemetry/settings")
    check("get_telemetry_settings: MCP == HTTP", not err and gs["settings"] == gsh["settings"], gs.get("settings"))
    err, ss = a.call("set_signal", signal="metrics", keep="2d")
    check("set_signal keep", not err and ss["settings"]["signals"]["metrics"]["keep"] == "2d", ss.get("settings"))
    a.call("set_signal", signal="metrics", keep=gs["settings"]["signals"]["metrics"]["keep"])
    err, pc = a.call("provoke_collector", answer="429", only_for=svc, **{"for": "2m"})
    check("provoke_collector, attributed to the agent", not err and pc["settings"]["collector"]["setBy"] == "mcp:claude-code", pc.get("settings"))
    check("the provoked service gets 429", otlp("traces", {"resourceSpans": [{"resource": res(svc), "scopeSpans": []}]}) == 429)
    check("another service is not affected", otlp("traces", {"resourceSpans": [{"resource": res("someone-else"), "scopeSpans": []}]}) == 200)
    err, sp = a.call("stop_provoking")
    check("stop_provoking", not err and sp["settings"]["collector"]["answer"] == "normal", sp.get("settings"))
    err, bad = a.call("set_forwarding", on=True, endpoint="https://otlp.example-apm.com:4318", header_secret="NRAK-raw")
    check("set_forwarding refuses a raw key", err and "never the key" in bad["error"], bad)
    err, bad = a.call("clear_telemetry")
    check("clear_telemetry needs confirm=true", err, bad)
    err, tw = a.call("telemetry_overview")
    check("telemetry_overview", not err and svc in tw["services"], tw.get("services"))

    print("\nCleanup")
    err, d = a.call("delete_inbox", address=addr)
    check("delete_inbox (with its messages)", not err and d["messagesDeleted"] >= 2, d)
    err, r = a.call("remove_service", name="mcp-check")
    check("remove_service", not err and r["ok"], r)

    bad = [r for r in RESULTS if not r[1]]
    print(f"\n{len(RESULTS) - len(bad)}/{len(RESULTS)} checks passed")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
