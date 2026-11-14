"""End-to-end check of environments (N1–N3): a pipeline makes one per pull request, the app talks to
it by host name or /e/<name>/, it is paused, extended, reset, cloned with state, made from a folder,
expires and is deleted. Every provider door is checked for isolation and for the 503 of a pause.

    cargo build -p stuntman && python3 tests/e2e_environments.py
"""
import hashlib, http.client, http.server, json, os, shutil, smtplib, socket, ssl, subprocess, tempfile, threading, time, urllib.error, urllib.request
from email.message import EmailMessage

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "stuntman")


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


HTTP, HOOKS, SMTP, OTLP, OTLP_G, RECV = (free_port() for _ in range(6))
GOT = []


class Recv(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        GOT.append((self.path, json.loads(self.rfile.read(int(self.headers["Content-Length"])) or b"null")))
        self.send_response(200); self.end_headers()

    def log_message(self, *a):
        pass


def call(method, path, body=None, env=None, host=None, base=None):
    url = (base or f"http://127.0.0.1:{HTTP}") + (f"/e/{env}" if env else "") + path
    h = {"Content-Type": "application/json", "X-Stuntman-Client": "qa-suite"}
    if host:
        h["Host"] = host
    req = urllib.request.Request(url, method=method, data=None if body is None else json.dumps(body).encode(), headers=h)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            t = r.read()
            return r.status, json.loads(t) if t and t[:1] in b"{[" else t.decode()
    except urllib.error.HTTPError as e:
        t = e.read()
        try:
            return e.code, json.loads(t)
        except ValueError:
            return e.code, t.decode()


def api(method, path, body=None, env=None, expect=200):
    st, v = call(method, "/api" + path, body, env)
    assert st == expect, (method, path, st, v)
    return v


def wait(cond, what, t=10):
    end = time.time() + t
    while time.time() < end:
        v = cond()
        if v:
            return v
        time.sleep(0.2)
    raise AssertionError(f"timed out waiting for {what}")


def smtp(port, to, subject):
    m = EmailMessage()
    m["From"] = "Acme <no-reply@acme.example.com>"; m["To"] = to; m["Subject"] = subject
    m.set_content("Your code is 482913")
    with smtplib.SMTP("127.0.0.1", port, timeout=10) as s:
        s.ehlo(); s.starttls(context=ssl._create_unverified_context()); s.ehlo(); s.login("api-staging", "x"); s.send_message(m)


def raw_post(port, path, body, host="127.0.0.1", ctype="application/json"):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    c.request("POST", path, json.dumps(body).encode(), {"Content-Type": ctype, "Host": host, "Authorization": "Bearer SG.anything"})
    r = c.getresponse(); r.read(); return r.status


def trace(tid):
    now = time.time_ns()
    return {"resourceSpans": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "acme-api"}}]},
            "scopeSpans": [{"spans": [{"traceId": tid, "spanId": "b000000000000001", "name": "GET /health", "kind": 2,
                                       "startTimeUnixNano": str(now - 4_000_000), "endTimeUnixNano": str(now)}]}]}]}


def mail(to, subject):
    return {"personalizations": [{"to": [{"email": to}]}], "from": {"email": "no-reply@acme.example.com"}, "subject": subject,
            "content": [{"type": "text/plain", "value": "hello"}]}


def messages(env, to):
    return api("GET", f"/messages?to={to}", env=env)["messages"]


def run(data):
    # N1 · one environment; the only one can't be deleted
    v = api("GET", "/environments")
    assert [e["name"] for e in v["environments"]] == ["stg-a"] and v["default"] == "stg-a", v
    api("DELETE", "/environments/stg-a", expect=409)

    # Configure STG-A: a kept webhook inbox, a kept e-mail inbox, a secret, the default callback target
    api("POST", "/inboxes", {"name": "orders", "kind": "webhook", "keep": True})
    api("POST", "/inboxes", {"address": "kept@example.com", "keep": True})
    with open(os.path.join(data, "environments/stg-a/secrets.yaml"), "w") as f:
        f.write("apm-license-key: test-only\n")
    smtp(SMTP, "olivia.brooks@example.com", "STG-A message")
    wait(lambda: messages("stg-a", "olivia.brooks@example.com"), "stg-a message")

    # N2 · a pipeline makes pr-5462 from STG-A, expiring in 24 h
    v = api("POST", "/environments", {"name": "pr-5462", "from": "stg-a", "expires": "24h", "callbacks": {"default": f"http://127.0.0.1:{RECV}/hooks"}})
    assert v["name"] == "pr-5462" and v["lifetime"] == "expires" and v["clonedFrom"] == "stg-a" and v["createdBy"] == "api:qa-suite", v
    assert abs(v["expiresAt"] - (time.time() * 1000 + 86_400_000)) < 60_000, v
    assert v["addresses"]["services"] == "*.pr-5462.stuntman.local" and v["addresses"]["local"].endswith("/e/pr-5462"), v["addresses"]
    moved = v["smtpPortsMoved"]
    assert moved and moved[0]["from"] == SMTP and moved[0]["to"] != SMTP, moved
    pr_smtp = moved[0]["to"]
    assert set(v["copied"]) == {"services", "scenarios", "inboxes", "secrets"}, v["copied"]
    assert os.path.exists(os.path.join(data, "environments/pr-5462/secrets.yaml"))
    st, e = call("POST", "/api/environments", {"name": "pr-5462", "from": "stg-a"})
    assert st == 409 and "already exists" in e["error"], e
    st, e = call("POST", "/api/environments", {"name": "PR 1"})
    assert st == 400, e

    # Copied: services (own port, own env name), inboxes; not state
    svc = api("GET", "/services", env="pr-5462")["services"][0]["service"]
    assert svc["env"] == "pr-5462" and svc["intake"]["smtp"]["port"] == pr_smtp and svc["addresses"]["domain"].startswith("pr-5462."), svc
    assert any(i["inbox"]["name"] == "orders" for i in api("GET", "/webhooks/inboxes", env="pr-5462")["inboxes"])
    assert any(i["address"] == "kept@example.com" for i in api("GET", "/inboxes", env="pr-5462")["inboxes"])
    assert messages("pr-5462", "olivia.brooks@example.com") == []

    # Isolation, e-mail: SMTP on the copy's port, SendGrid by prefix and by host name
    smtp(pr_smtp, "liam.foster@example.com", "PR message")
    wait(lambda: messages("pr-5462", "liam.foster@example.com"), "pr-5462 SMTP message")
    assert messages("stg-a", "liam.foster@example.com") == []
    assert raw_post(HTTP, "/e/pr-5462/sendgrid/v3/mail/send", mail("ava.reed@example.com", "by prefix")) == 202
    assert raw_post(HTTP, "/sendgrid/v3/mail/send", mail("noah.price@example.com", "by host"), host=f"sendgrid.pr-5462.stuntman.local:{HTTP}") == 202
    wait(lambda: messages("pr-5462", "ava.reed@example.com") and messages("pr-5462", "noah.price@example.com"), "pr-5462 SendGrid messages")
    assert messages("stg-a", "noah.price@example.com") == []
    # The default callback target gets the service's events (the service has no URL of its own)
    wait(lambda: [g for g in GOT if g[0] == "/hooks" and any(x.get("email") == "liam.foster@example.com" for x in g[1])], "events at the default callback target")

    # Isolation, webhooks: by prefix and by host name; an unknown *.stuntman.local host finds nothing
    assert raw_post(HOOKS, "/e/pr-5462/in/orders", {"type": "order.paid", "id": "evt_1"}) < 500
    assert raw_post(HOOKS, "/in/orders", {"type": "order.paid", "id": "evt_2"}, host="hooks.pr-5462.stuntman.local") < 500
    pr_d = api("GET", "/webhooks/inboxes/orders/deliveries", env="pr-5462")["deliveries"]
    stg_d = api("GET", "/webhooks/inboxes/orders/deliveries")["deliveries"]
    assert len(pr_d) == 2 and len(stg_d) == 0, (pr_d, stg_d)
    assert raw_post(HOOKS, "/in/orders", {"id": "x"}, host="hooks.nope.stuntman.local") == 404
    st, _ = call("GET", "/api/env", host="nope.stuntman.local")
    assert st == 404

    # Isolation, telemetry: OTLP/HTTP by prefix
    assert raw_post(OTLP, "/e/pr-5462/v1/traces", trace("a" * 32)) == 200
    assert raw_post(OTLP, "/v1/traces", trace("b" * 32), host="otlp.pr-5462.stuntman.local") == 200
    pr_t = {t["traceId"] for t in api("GET", "/telemetry/traces?since=1h", env="pr-5462")["traces"]}
    stg_t = {t["traceId"] for t in api("GET", "/telemetry/traces?since=1h")["traces"]}
    assert pr_t == {"a" * 32, "b" * 32} and not (stg_t & pr_t), (pr_t, stg_t)

    # N1 · paused: every service answers 503 (SMTP 421), the control API keeps working
    v = api("POST", "/environments/pr-5462/pause", {"paused": True})
    assert v["paused"] and v["state"] == "paused" and v["pausedSince"], v
    assert raw_post(HTTP, "/e/pr-5462/sendgrid/v3/mail/send", mail("ava.reed@example.com", "paused")) == 503
    assert raw_post(HOOKS, "/e/pr-5462/in/orders", {"id": "evt_3"}) == 503
    assert raw_post(OTLP, "/e/pr-5462/v1/traces", trace("c" * 32)) == 503
    try:
        smtp(pr_smtp, "liam.foster@example.com", "paused")
        raise AssertionError("SMTP accepted a message while paused")
    except smtplib.SMTPConnectError as e:
        assert e.smtp_code == 421, e
    assert raw_post(HTTP, "/sendgrid/v3/mail/send", mail("emma.carter@example.com", "stg-a still runs")) == 202
    assert api("GET", "/env", env="pr-5462")["paused"] is True
    api("POST", "/environments/pr-5462/pause", {"paused": False})
    assert raw_post(HOOKS, "/e/pr-5462/in/orders", {"id": "evt_4"}) < 500

    # N3 · settings: stale version → 409; base_domain taken → 409; hosts lines; callbacks
    g = api("GET", "/environments/pr-5462")
    assert "pr-5462" in g["file"] and g["version"], g
    api("PUT", "/environments/pr-5462", {"callbacks": {"default": "http://host.docker.internal:5000"}, "version": g["version"]})
    st, e = call("PUT", "/api/environments/pr-5462", {"lifetime": "kept", "version": g["version"]})
    assert st == 409 and "changed elsewhere" in e["error"] and "host.docker.internal" in e["detail"]["file"], e
    st, e = call("PUT", "/api/environments/pr-5462", {"base_domain": "stg-a.stuntman.local"})
    assert st == 409, e
    st, e = call("PUT", "/api/environments/pr-5462", {"callbacks": {"default": "ftp://x"}})
    assert st == 400, e
    h = api("GET", "/environments/pr-5462/hosts")["hosts"]
    assert "127.0.0.1  smtp.pr-5462.stuntman.local" in h and "127.0.0.1  hooks.pr-5462.stuntman.local" in h, h

    # Extend 24 h
    before = api("GET", "/environments/pr-5462")["expiresAt"]
    after = api("POST", "/environments/pr-5462/extend", {"by": "24h"})["expiresAt"]
    assert after - before == 86_400_000, (before, after)

    # Reset to seeds: traffic goes, kept inboxes and files stay
    v = api("POST", "/environments/pr-5462/reset", {})
    assert messages("pr-5462", "liam.foster@example.com") == []
    assert api("GET", "/webhooks/inboxes/orders/deliveries", env="pr-5462")["deliveries"] == []
    assert api("GET", "/telemetry/traces?since=1h", env="pr-5462")["traces"] == []
    assert any(i["address"] == "kept@example.com" for i in api("GET", "/inboxes", env="pr-5462")["inboxes"])
    assert api("GET", "/services", env="pr-5462")["services"]
    assert messages("stg-a", "olivia.brooks@example.com"), "stg-a kept its state"

    # A copy with state; an empty one; one from a folder with a broken file
    v = api("POST", "/environments", {"name": "stg-b", "from": "stg-a", "copy": ["services", "state"]})
    assert messages("stg-b", "olivia.brooks@example.com"), "state copied"
    v = api("POST", "/environments", {"name": "empty-1", "from": "empty"})
    assert api("GET", "/services", env="empty-1")["services"] == [] and v["lifetime"] == "kept", v
    repo = tempfile.mkdtemp(prefix="stuntman-repo-")
    os.makedirs(os.path.join(repo, "webhooks"))
    with open(os.path.join(repo, "webhooks/inboxes.yaml"), "w") as f:
        f.write("inboxes:\n  - name: billing\n    mode: [unclosed\n")
    v = api("POST", "/environments", {"name": "from-file", "from": repo})
    assert v["problems"] and v["problems"][0]["file"] == "webhooks/inboxes.yaml" and v["problems"][0]["line"], v["problems"]
    shutil.rmtree(repo, ignore_errors=True)

    # Expiry: an environment set to expire is deleted by the sweep (every 30 s)
    api("POST", "/environments", {"name": "short", "from": "empty", "expires": "1s"})
    wait(lambda: "short" not in [e["name"] for e in api("GET", "/environments")["environments"]], "expired environment deleted", 45)
    assert not os.path.exists(os.path.join(data, "environments/short"))

    # MCP per environment: /e/<name>/mcp acts on that environment
    def mcp(env, body, sid=None):
        req = urllib.request.Request(f"http://127.0.0.1:{HTTP}" + (f"/e/{env}" if env else "") + "/mcp", method="POST", data=json.dumps(body).encode(),
                                     headers={"Content-Type": "application/json", **({"Mcp-Session-Id": sid} if sid else {})})
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.headers.get("mcp-session-id"), json.loads(r.read() or b"null")
    sid, init = mcp("pr-5462", {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "claude-code"}}})
    assert "pr-5462" in init["result"]["instructions"], init

    def tool(env, name, args):
        _, r = mcp(env, {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": name, "arguments": args}}, sid)
        return r["result"]["isError"], json.loads(r["result"]["content"][0]["text"])
    err, v = tool("pr-5462", "list_environments", {})
    assert not err and {e["name"] for e in v["environments"]} == {"stg-a", "pr-5462", "stg-b", "empty-1", "from-file"}, v
    err, v = tool(None, "create_environment", {"name": "pr-5463", "from": "STG-A", "expires": "24h"})
    assert not err and v["createdBy"] == "mcp:claude-code" and v["clonedFrom"] == "stg-a", v
    err, v = tool(None, "pause_environment", {"name": "pr-5463"})
    assert not err and v["paused"], v
    err, v = tool(None, "delete_environment", {"name": "pr-5463"})
    assert not err and v["deleted"] == "pr-5463", v

    # N1 · a pipeline cleans up after the merge: the preview's services and history are gone
    api("DELETE", "/environments/pr-5462")
    st, _ = call("GET", "/api/env", env="pr-5462")
    assert st == 404
    assert not os.path.exists(os.path.join(data, "environments/pr-5462"))
    time.sleep(0.5)
    try:
        socket.create_connection(("127.0.0.1", pr_smtp), 1).close()
        raise AssertionError("the deleted environment's SMTP port still listens")
    except OSError:
        pass
    assert messages("stg-a", "olivia.brooks@example.com"), "stg-a untouched"


def main():
    data = tempfile.mkdtemp(prefix="stuntman-env-")
    recv = http.server.ThreadingHTTPServer(("127.0.0.1", RECV), Recv)
    threading.Thread(target=recv.serve_forever, daemon=True).start()
    env = dict(os.environ, STUNTMAN_DATA=data, STUNTMAN_PORT=str(HTTP), STUNTMAN_HOOKS_PORT=str(HOOKS), STUNTMAN_SMTP_PORT=str(SMTP),
               STUNTMAN_OTLP_HTTP_PORT=str(OTLP), STUNTMAN_OTLP_GRPC_PORT=str(OTLP_G), STUNTMAN_WEB="/nonexistent")
    srv = subprocess.Popen([BIN], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    try:
        def up():
            try:
                socket.create_connection(("127.0.0.1", HTTP), 0.2).close()
                return call("GET", "/api/env")[0] == 200
            except OSError:
                return False
        wait(up, "server")
        run(data)
        print("e2e environments: all checks passed")
    finally:
        srv.terminate(); recv.shutdown(); shutil.rmtree(data, ignore_errors=True)


if __name__ == "__main__":
    main()
