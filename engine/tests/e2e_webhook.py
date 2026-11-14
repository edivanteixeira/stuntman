"""End-to-end check of webhook inboxes (H1–H2): plays the app that sends webhooks to its customers.

    cargo build -p stuntman && python3 tests/e2e_webhook.py
"""
import hashlib, hmac, http.client, json, os, shutil, socket, subprocess, tempfile, time, urllib.error, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


HTTP, HOOKS = free_port(), free_port()
API = f"http://127.0.0.1:{HTTP}/api"


def call(method, path, body=None):
    req = urllib.request.Request(API + path, method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "X-Stuntman-Client": "qa-suite"})
    try:
        with urllib.request.urlopen(req, timeout=90) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def send(inbox, event, eid, data, secret="whsec_new", timeout=10, ts=None, follow=False):
    """The app's webhook sender: Stripe-style t=…,v1=… signature. Returns (status | error, seconds)."""
    body = json.dumps({"type": event, "id": eid, "data": data}).encode()
    t = str(ts or int(time.time()))
    sig = hmac.new(secret.encode(), f"{t}.".encode() + body, hashlib.sha256).hexdigest()
    c = http.client.HTTPConnection("127.0.0.1", HOOKS, timeout=timeout)
    t0 = time.time()
    try:
        c.request("POST", f"/in/{inbox}", body, {"Content-Type": "application/json", "X-Acme-Signature": f"t={t},v1={sig}", "X-Acme-Event-Id": eid, "User-Agent": "acme-api/2.50"})
        r = c.getresponse()
        if follow and r.status == 302:
            loc = r.getheader("Location"); r.read()
            urllib.request.urlopen(loc, timeout=5).read()
        return r.status, r, time.time() - t0
    except Exception as e:
        return type(e).__name__, None, time.time() - t0


def wait(inbox, event=None):
    q = f"/inboxes/deliveries/wait?inbox={inbox}&timeout=20s" + (f"&event={event}" if event else "")
    st, d = call("GET", q)
    assert st == 200, d
    return d


def main():
    data = tempfile.mkdtemp(prefix="duble-wh-")
    env = dict(os.environ, STUNTMAN_DATA=data, STUNTMAN_PORT=str(HTTP), STUNTMAN_HOOKS_PORT=str(HOOKS), STUNTMAN_SMTP_PORT=str(free_port()), STUNTMAN_WEB="/nonexistent")
    srv = subprocess.Popen([os.path.join(ROOT, "target", "debug", "stuntman")], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    try:
        for _ in range(50):
            try:
                socket.create_connection(("127.0.0.1", HOOKS), 0.2).close(); break
            except OSError:
                time.sleep(0.1)
        run()
        print("e2e webhook: all checks passed")
    finally:
        srv.terminate(); shutil.rmtree(data, ignore_errors=True)


def run():
    # Kept inbox, signatures checked, old secret recognised.
    st, g = call("POST", "/inboxes", {"name": "globex", "keep": True, "secret": "whsec_new", "previousSecret": "whsec_old"})
    assert st == 200 and g["url"].endswith(f":{HOOKS}/in/globex"), g
    assert send("globex", "order.completed", "evt_1", {"order": 58213})[0] == 200
    d = wait("globex", "order.completed")
    assert d["label"] == "#58213" and d["signature"]["result"] == "valid" and d["answer"] == "200" and d["attempt"] == 1, d
    assert send("globex", "order.completed", "evt_2", {"order": 58212}, secret="whsec_old")[0] == 200
    time.sleep(0.3)
    _, l = call("GET", "/inboxes/globex/deliveries")
    assert l["deliveries"][0]["signature"]["result"] == "old_secret", l["deliveries"][0]
    # Replay: same event id again after a 200 → duplicate.
    send("globex", "order.cancelled", "evt_3", {"order": 58190}); send("globex", "order.cancelled", "evt_3", {"order": 58190})
    time.sleep(0.3)
    _, l = call("GET", "/webhooks/deliveries?inbox=globex&event=evt_3")
    assert l["deliveries"][0]["duplicateOf"] and l["deliveries"][0]["attempt"] == 2, l["deliveries"][0]
    # Old timestamp is reported.
    send("globex", "invoice.paid", "evt_4", {"invoice": "INV-2207"}, ts=int(time.time()) - 540)
    time.sleep(0.3)
    _, l = call("GET", "/webhooks/deliveries?inbox=globex&event=invoice.paid")
    assert 530_000 < l["deliveries"][0]["signature"]["timestampAgeMs"] < 560_000 and l["deliveries"][0]["label"] == "INV-2207"

    # Fail 3 times per event, then 200: the app's retries are counted as attempts.
    call("POST", "/inboxes", {"name": "initech", "keep": True, "fail": 3, "then": 200})
    codes = [send("initech", "order.completed", "evt_9", {"order": 1})[0] for _ in range(4)]
    assert codes == [500, 500, 500, 200], codes
    d = wait("initech")
    _, full = call("GET", f"/inboxes/deliveries/{d['id']}")
    assert [a["answer"] for a in full["attempts"]] == ["500", "500", "500", "200"], full["attempts"]
    assert send("initech", "order.completed", "evt_10", {"order": 2})[0] == 500, "a new event fails again (per event id)"

    # Hang: the client gives up at 2 s, Stuntman records when.
    call("POST", "/inboxes", {"name": "umbrella", "keep": True, "mode": "hang", "hang": "35s"})
    st, _, secs = send("umbrella", "order.completed", "evt_20", {"order": 3}, timeout=2)
    assert st in ("TimeoutError", "timeout") and secs < 4, (st, secs)
    time.sleep(0.5)
    _, l = call("GET", "/webhooks/deliveries?inbox=umbrella")
    assert l["deliveries"][0]["answer"] == "no answer" and 1500 < l["deliveries"][0]["gaveUpMs"] < 3500, l["deliveries"][0]

    # Reset and empty reply are real socket behaviour.
    call("PUT", "/inboxes/umbrella", {"mode": "reset"})
    assert send("umbrella", "e", "evt_21", {})[0] in ("ConnectionResetError", "RemoteDisconnected", "BadStatusLine")
    call("PUT", "/inboxes/umbrella", {"mode": "empty"})
    assert send("umbrella", "e", "evt_22", {})[0] in ("RemoteDisconnected", "BadStatusLine", "ConnectionResetError")

    # Strict signatures: 401 to a bad one.
    call("PUT", "/inboxes/globex", {"rejectBad": True})
    assert send("globex", "order.completed", "evt_30", {"order": 4}, secret="nope")[0] == 401

    # Traps: a redirect to a private address (not followed, then followed), a huge and a slow answer.
    call("POST", "/inboxes", {"name": "trap", "expires": "30m", "traps": ["redirect_private"]})
    assert send("trap", "e", "evt_40", {})[0] == 302
    assert send("trap", "e", "evt_41", {}, follow=True)[0] == 302
    time.sleep(0.5)
    _, l = call("GET", "/webhooks/deliveries?inbox=trap")
    assert l["deliveries"][0]["traps"][0]["result"] == "failed" and "SSRF" in l["deliveries"][0]["traps"][0]["detail"], l["deliveries"][0]["traps"]
    call("PUT", "/inboxes/trap", {"traps": ["huge"]})
    st, r, _ = send("trap", "e", "evt_42", {})
    r.read(100_000); r.close()
    time.sleep(1)
    _, l = call("GET", "/webhooks/deliveries?inbox=trap")
    assert l["deliveries"][0]["traps"][0]["result"] == "passed", l["deliveries"][0]["traps"]
    call("PUT", "/inboxes/trap", {"traps": ["slow"]})
    st, r, _ = send("trap", "e", "evt_43", {}, timeout=2)
    try:
        r.read()
    except Exception:
        pass
    r.close()
    time.sleep(2)
    _, l = call("GET", "/webhooks/deliveries?inbox=trap")
    assert l["deliveries"][0]["traps"][0]["result"] == "passed", l["deliveries"][0]

    # The design's 10.0.0.5 target is sent as is, and reported as not observable.
    call("PUT", "/inboxes/trap", {"traps": ["redirect_private"], "redirectTo": "http://10.0.0.5/admin"})
    c = http.client.HTTPConnection("127.0.0.1", HOOKS, timeout=5); c.request("POST", "/in/trap", b"{}"); r = c.getresponse()
    assert r.status == 302 and r.getheader("Location") == "http://10.0.0.5/admin"
    time.sleep(0.3)
    _, l = call("GET", "/inboxes/trap/deliveries")
    assert l["deliveries"][0]["traps"][0]["result"] == "unobserved", l["deliveries"][0]["traps"]

    # Shared names: get_inbox and DELETE /inboxes/{name} reach webhook inboxes; e-mail addresses still work.
    st, gi = call("GET", "/inboxes/globex")
    assert st == 200 and gi["summary"] and gi["url"].endswith("/in/globex"), gi
    st, em = call("POST", "/inboxes", {"address": "dev@acme.example.com", "keep": True})
    assert st == 200 and em["address"] == "dev@acme.example.com", em
    st, ge = call("GET", "/inboxes/dev@acme.example.com")
    assert st == 200 and ge["inbox"]["address"] == "dev@acme.example.com", ge
    assert call("DELETE", "/inboxes/dev@acme.example.com")[0] == 200

    # Removed inbox → 410 Gone, recorded.
    call("DELETE", "/inboxes/trap")
    assert send("trap", "e", "evt_50", {})[0] == 410

    # MCP, with the shortcut from the sheet; YAML holds the kept inboxes only.
    h = {"Content-Type": "application/json"}
    r = urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{HTTP}/mcp", method="POST", headers=h, data=json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "claude-code"}}}).encode()))
    h["Mcp-Session-Id"] = r.headers["Mcp-Session-Id"]
    r = urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{HTTP}/mcp", method="POST", headers=h, data=json.dumps(
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "create_inbox", "arguments": {"expires": "30m", "fail": 2, "then": 200}}}).encode()))
    out = json.loads(json.loads(r.read())["result"]["content"][0]["text"])
    assert out["inbox"]["answer"]["mode"] == "fail_then" and out["inbox"]["createdBy"] == "mcp:claude-code", out
    y = urllib.request.urlopen(f"{API}/webhooks/inboxes.yaml").read().decode()
    assert "name: globex" in y and "name: initech" in y and out["name"] not in y, y
    st, bad = call("PUT", "/inboxes/globex", {"mode": "teleport"})
    assert st == 400 and "mode:" in bad["error"], bad


if __name__ == "__main__":
    main()
