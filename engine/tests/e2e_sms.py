"""Zenvia SMS as a real app sends it (v2, template contents, X-API-TOKEN), callbacks caught by a local server.
Run with Stuntman up:  python3 tests/e2e_sms.py   (uses the default environment; restores its callbacks)
  STUNTMAN_URL (default http://127.0.0.1:8025)
"""
import http.client, http.server, json, os, socket, threading, time, urllib.error, urllib.parse, urllib.request, uuid

BASE = os.environ.get("STUNTMAN_URL", "http://127.0.0.1:8025")
API = f"{BASE}/api"
ZENVIA = BASE + "/zenvia/v2/channels/{0}/messages"
got = []


class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        got.append(json.loads(self.rfile.read(int(self.headers["content-length"]))))
        self.send_response(200); self.end_headers()

    def log_message(self, *a):
        pass


def call(method, url, body=None, headers=None):
    h = {"content-type": "application/json", **(headers or {})}
    r = urllib.request.Request(url, data=json.dumps(body).encode() if body is not None else None, method=method, headers=h)
    try:
        with urllib.request.urlopen(r) as x:
            return x.status, json.loads(x.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def wait(pred, t=15):
    end = time.time() + t
    while time.time() < end:
        if pred():
            return True
        time.sleep(0.2)
    return False


srv = http.server.HTTPServer(("0.0.0.0", 0), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()
# HOOK_HOST=host.docker.internal when Stuntman runs in Docker
hook = f"http://{os.environ.get('HOOK_HOST', '127.0.0.1')}:{srv.server_port}/zenvia"
_, before = call("GET", f"{API}/sms/service")
call("PUT", f"{API}/sms/service", {"callbacks": {"status": hook, "inbound": hook}, "templates": [{"id": "11111111-2222-4333-8444-555555555555", "name": "signing code", "text": "{{workspaceShortName}}: your signing code is {{token}}."}]})
call("PUT", f"{API}/sms/recipients", {"number": "+5511999990001", "carrier": "Vivo", "steps": [{"delivered_after": "1s"}, {"on_text": {"reply": "STOP", "after": "1s"}}]})
call("PUT", f"{API}/sms/recipients", {"number": "+5511999990002", "carrier": "Claro", "steps": [{"delivered_after": "1s"}, {"not_delivered": "carrier_rejected"}]})
call("DELETE", f"{API}/sms/opt-outs/5511999990001")
try:
    tok = {"X-API-TOKEN": "any"}
    body = {"externalId": str(uuid.uuid4()), "from": "AcmeKey", "to": "5511999990001",
            "contents": [{"type": "template", "templateId": "11111111-2222-4333-8444-555555555555", "fields": {"workspaceShortName": "Acme", "token": "482913"}}]}
    s, r = call("POST", ZENVIA.format("sms"), body, tok)
    assert s == 200 and r["id"] and r["direction"] == "OUT", (s, r)
    s, w = call("GET", f"{API}/sms/wait?number=%2B5511999990001&timeout=5s")
    assert w["extracted"]["otp"] == "482913" and w["message"]["text"] == "Acme: your signing code is 482913.", w
    assert wait(lambda: any(g.get("messageStatus", {}).get("code") == "DELIVERED" and g["messageId"] == r["id"] for g in got)), got
    assert wait(lambda: any(g.get("type") == "MESSAGE" and g["message"]["contents"][0]["text"] == "STOP" for g in got)), got
    s, e = call("POST", ZENVIA.format("sms"), body, tok)
    assert s == 400 and e["details"][0]["code"] == "OPTED_OUT", (s, e)
    s, _ = call("POST", ZENVIA.format("sms"), body)
    assert s == 401
    s, e = call("POST", ZENVIA.format("sms"), {**body, "to": "12"}, tok)
    assert s == 400 and e["details"][0]["path"] == "to", e
    # an undeclared template still goes; its fields are shown
    s, r2 = call("POST", ZENVIA.format("sms"), {**body, "to": "5511999990002", "contents": [{"type": "template", "templateId": "d5d33983-0000-4000-8000-000000000000", "fields": {"link": "https://example.com/s/1"}}]}, tok)
    assert s == 200
    assert wait(lambda: any(g.get("messageStatus", {}).get("code") == "NOT_DELIVERED" and g["messageId"] == r2["id"] for g in got)), got
    nd = [g for g in got if g.get("messageId") == r2["id"] and g["messageStatus"]["code"] == "NOT_DELIVERED"][0]
    assert nd["messageStatus"]["causes"][0]["channelErrorCode"] == "carrier_rejected"
    s, m = call("GET", f"{API}/sms/messages?number=%2B5511999990002&since=5m")
    assert m["messages"][0]["text"].startswith("[template d5d33983]"), m["messages"][0]["text"]
    # Forced failures at the HTTP level: 500, 400 with Zenvia's body, then the connection itself.
    fb = {**body, "to": "5511999990003", "contents": [{"type": "text", "text": "Acme: code 111222"}]}
    s, f = call("POST", f"{API}/sms/force", {"answer": "500", "count": 2})
    assert s == 200 and f["forced"]["answer"] == "500" and f["forced"]["left"] == 2, f
    for _ in range(2):
        s, e = call("POST", ZENVIA.format("sms"), fb, tok)
        assert s == 500 and e["code"] == "INTERNAL_ERROR", (s, e)
    s, _ = call("POST", ZENVIA.format("sms"), fb, tok)
    assert s == 200, "count 2: the third send goes"
    call("POST", f"{API}/sms/force", {"answer": "400"})
    s, e = call("POST", ZENVIA.format("sms"), fb, tok)
    assert s == 400 and e["code"] == "VALIDATION_ERROR" and e["details"][0]["path"] == "to", (s, e)
    u = urllib.parse.urlparse(ZENVIA.format("sms"))
    for gen, exc in [("reset", (ConnectionResetError, http.client.RemoteDisconnected)), ("empty", (http.client.RemoteDisconnected,)), ("timeout", (socket.timeout, TimeoutError))]:
        call("POST", f"{API}/sms/force", {"answer": gen})
        c = http.client.HTTPConnection(u.hostname, u.port, timeout=2)
        try:
            c.request("POST", u.path, json.dumps(fb), {"Content-Type": "application/json", **tok})
            raise AssertionError(f"{gen}: answered {c.getresponse().status}")
        except exc as x:
            pass
        finally:
            c.close()
    call("POST", f"{API}/sms/force", {"answer": "normal"})
    _, m = call("GET", f"{API}/sms/messages?number=%2B5511999990003&status=refused&since=5m")
    reasons = [x["reason"] for x in m["messages"]]
    assert len(reasons) == 6 and sum("500 Internal Server Error (forced" in x for x in reasons) == 2 and any(x.startswith("400 ") for x in reasons) and any("connection reset" in x for x in reasons), reasons
    _, sv = call("GET", f"{API}/sms/service")
    assert sv["forced"] is None, sv["forced"]
    print("sms ok: forced 500 ×2, 400 with Zenvia's body, reset, empty, timeout (all listed as refused), template rendered, otp extracted, SENT/DELIVERED callbacks, STOP reply → opt-out → 400, 401 without token, bad number, undeclared template, NOT_DELIVERED with cause")
finally:
    call("PUT", f"{API}/sms/service", {"callbacks": before["service"]["callbacks"], "templates": before["service"]["templates"]})
    for n in ["+5511999990001", "+5511999990002"]:
        call("DELETE", f"{API}/sms/recipients", {"number": n})
    call("DELETE", f"{API}/sms/opt-outs/5511999990001")
