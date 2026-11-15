"""End-to-end check of the e-mail service, the way an app and a test use it.

Starts the built server on spare ports with an empty data folder, plays the app (SMTP with
STARTTLS + AUTH, SendGrid API) and the app's event webhook (verifying SendGrid's ECDSA signature),
and drives everything else through the HTTP API and MCP.

    cargo build -p stuntman && python3 tests/e2e_email.py
"""
import base64, http.server, json, os, shutil, smtplib, socket, ssl, subprocess, tempfile, threading, time, urllib.error, urllib.request
from email.message import EmailMessage

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "stuntman")


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


HTTP, SMTP, HOOK = free_port(), free_port(), free_port()
API = f"http://127.0.0.1:{HTTP}/api"
EVENTS, HOOK_STATUS = [], [200]


class Hook(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers["Content-Length"]))
        EVENTS.append({"body": json.loads(body), "sig": self.headers.get("X-Twilio-Email-Event-Webhook-Signature"),
                       "ts": self.headers.get("X-Twilio-Email-Event-Webhook-Timestamp"), "raw": body})
        self.send_response(HOOK_STATUS[0]); self.end_headers(); self.wfile.write(b"ok")

    def do_GET(self):  # the link a click opens
        self.send_response(200); self.end_headers(); self.wfile.write(b"signed")

    def log_message(self, *a):
        pass


def call(method, path, body=None, expect=None):
    req = urllib.request.Request(API + path, method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "X-Stuntman-Client": "e2e"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            status, data = r.status, r.read()
    except urllib.error.HTTPError as e:
        status, data = e.code, e.read()
    out = json.loads(data) if data and data[:1] in b"{[" else data
    if expect is not None:
        assert status == expect, f"{method} {path}: {status} {out}"
    return status, out


def wait(cond, what, t=10):
    end = time.time() + t
    while time.time() < end:
        v = cond()
        if v:
            return v
        time.sleep(0.2)
    raise AssertionError("timed out waiting for " + what)


def events_for(email, kind):
    return [e for ev in EVENTS for e in ev["body"] if e.get("email") == email and e["event"] == kind]


def smtp_send(to, subject, text, html=None, user="api-staging", password="x", attach=None, headers=None):
    m = EmailMessage()
    m["From"] = "Acme <no-reply@acme.example.com>"; m["To"] = to; m["Subject"] = subject
    for k, v in (headers or {}).items():
        m[k] = v
    m.set_content(text)
    if html:
        m.add_alternative(html, subtype="html")
    if attach:
        m.add_attachment(attach[1], maintype="text", subtype="csv", filename=attach[0])
    with smtplib.SMTP("127.0.0.1", SMTP, timeout=10) as s:
        s.ehlo(); s.starttls(context=ssl._create_unverified_context()); s.ehlo(); s.login(user, password); s.send_message(m)


def main():
    data = tempfile.mkdtemp(prefix="duble-e2e-")
    hook = http.server.ThreadingHTTPServer(("127.0.0.1", HOOK), Hook)
    threading.Thread(target=hook.serve_forever, daemon=True).start()
    env = dict(os.environ, STUNTMAN_ENV=os.environ.get("STUNTMAN_ENV", "stg-a"), STUNTMAN_DATA=data, STUNTMAN_PORT=str(HTTP), STUNTMAN_SMTP_PORT=str(SMTP), STUNTMAN_WEB="/nonexistent")
    srv = subprocess.Popen([BIN], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    try:
        wait(lambda: call("GET", "/env")[0] == 200 if _up() else False, "server")
        run()
        print("e2e e-mail: all checks passed")
    finally:
        srv.terminate(); hook.shutdown(); shutil.rmtree(data, ignore_errors=True)


def _up():
    try:
        socket.create_connection(("127.0.0.1", HTTP), 0.2).close(); return True
    except OSError:
        return False


def run():
    hook_url = f"http://127.0.0.1:{HOOK}/hooks/sendgrid"
    _, svc = call("PATCH", "/services/transactional", {"events": {"to": hook_url, "types": ["processed", "delivered", "deferred", "bounce", "dropped", "open", "click", "spamreport"]}}, 200)
    assert any(d["field"] == "events.to" for d in svc["diff"]), svc["diff"]
    pub = serialization.load_der_public_key(base64.b64decode(svc["publicKey"]))

    # 1. SMTP with STARTTLS + AUTH: OTP lands in the catch-all, built-in "code" finds it, wait returns it.
    smtp_send("emma.carter@example.com", "Your verification code", "Use this code to confirm your signature on NDA · Acme Corp: 482 913. It expires in 10 minutes.",
              html="<p>Use this code</p><div><strong>482 913</strong></div><a href='https://account.acme.example.com/help'>Need help?</a>",
              headers={"traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"})
    # .NET SmtpClient style: no TLS, AUTH LOGIN with the user on the same line (initial response).
    m2 = EmailMessage(); m2["From"] = "Acme Sign <no-reply@example.com>"; m2["To"] = "olivia.brooks@example.com"; m2["Subject"] = "Sign the NDA"; m2.set_content("sign")
    with smtplib.SMTP("127.0.0.1", SMTP, timeout=10) as s2:
        s2.ehlo(); s2.user, s2.password = "no-reply@example.com", "12345"
        code, _ = s2.auth("LOGIN", s2.auth_login, initial_response_ok=True)
        assert code == 235, code
        s2.send_message(m2)
    call("GET", "/messages/wait?to=olivia.brooks@example.com&timeout=5s", expect=200)
    _, w = call("GET", "/messages/wait?to=emma.carter@example.com&extract=code&timeout=5s", expect=200)
    assert w["value"] == "482913", w
    mid = w["messageId"]
    _, m = call("GET", f"/messages/{mid}", expect=200)
    assert m["inbox"] == "catch-all" and m["lang"] == "en" and m["via"] == "smtp" and m["traceId"].startswith("4bf92f35"), m
    wait(lambda: events_for("emma.carter@example.com", "delivered"), "delivered event (default step +1s)")
    ev = next(e for e in EVENTS if any(x["event"] == "delivered" for x in e["body"]))
    pub.verify(base64.b64decode(ev["sig"]), ev["ts"].encode() + ev["raw"], ec.ECDSA(hashes.SHA256()))  # raises if bad

    # 2. Raw .eml download.
    st, raw = call("GET", f"/messages/{mid}/raw")
    assert st == 200 and b"Subject: Your verification code" in raw

    # 3. User extractor on links, tested first, then saved; SendGrid API send with custom args.
    _, t = call("POST", "/channels/email/extractors/test", {"from": "link", "rule": r"/operation/[0-9a-f-]{36}/[^\s\"]+", "last": 50}, 200)
    assert t["matched"] == 0
    call("POST", "/channels/email/extractors", {"name": "signLink", "from": "link", "rule": r"/operation/[0-9a-f-]{36}/[^\s\"]+",
                                                 "onlyOn": {"when": "subject", "value": "Action required|Acción necesaria"}}, 200)
    _, bad = call("POST", "/channels/email/extractors/test", {"from": "link", "rule": "/operation/([0-9a-f-]{36}/"}, 200)
    assert "nclosed" in bad["error"], bad

    _, ib = call("POST", "/inboxes", {"prefix": "signer", "ttl": "30m", "steps": ["delivered +1s", "opened +1s", "click:signLink +500ms"]}, 200)
    addr = ib["address"]
    assert addr.endswith("@stg-a.inbox.stuntman.dev") and ib["expiresAt"]
    link = f"http://127.0.0.1:{HOOK}/operation/3f2c9e1a-77b0-4c1e-9d0a-5b1c2e7f0a11/2026-11-12"
    req = urllib.request.Request(f"http://127.0.0.1:{HTTP}/sendgrid/v3/mail/send", method="POST", headers={"Authorization": "Bearer SG.anything", "Content-Type": "application/json"},
                                 data=json.dumps({"personalizations": [{"to": [{"email": addr}], "custom_args": {"operation_id": "58213"}}],
                                                  "from": {"email": "no-reply@acme.example.com", "name": "Acme"}, "subject": "Action required: NDA · Acme Corp",
                                                  "content": [{"type": "text/html", "value": f"<p>Please sign</p><a href='{link}'>Sign</a> <a href='https://acme.example.com/privacy'>privacy</a>"}],
                                                  "attachments": [{"content": base64.b64encode(b"id,nombre\n31,Ventas\n").decode(), "filename": "grupos.csv", "type": "text/csv"}]}).encode())
    with urllib.request.urlopen(req) as r:
        assert r.status == 202 and r.headers["X-Message-Id"]
    _, w = call("GET", f"/messages/wait?to={addr}&extract=signLink&timeout=5s", expect=200)
    assert w["value"] == link, w
    assert w["message"]["kind"] == "Action required: ‹…› · ‹…›" and w["message"]["inbox"] == "temporary"
    clicks = wait(lambda: events_for(addr, "click"), "click step", 8)
    assert clicks[0]["url"] == link and clicks[0]["operation_id"] == "58213", clicks
    _, m = call("GET", f"/messages/{w['messageId']}", expect=200)
    assert m["status"] == "clicked" and [e["type"] for e in m["events"]][:4] == ["processed", "delivered", "open", "click"], m["events"]
    assert m["attachments"][0]["name"] == "grupos.csv"
    st, csv = call("GET", f"/messages/{w['messageId']}/attachments/0")
    assert st == 200 and csv.startswith(b"id,nombre")

    # 4. Inbox rule: bounce; then hold and decide by hand.
    call("PATCH", f"/inboxes/{addr}", {"onArrival": "bounce"}, 200)
    smtp_send(addr, "Action required: Lease · Northwind", "sign here")
    b = wait(lambda: events_for(addr, "bounce"), "bounce")
    assert b[0]["status"] == "5.1.1"
    call("PATCH", f"/inboxes/{addr}", {"onArrival": "hold", "holdFor": "10m"}, 200)
    smtp_send(addr, "Welcome to Acme", "hello")
    held = wait(lambda: [x for x in call("GET", f"/messages?to={addr}&status=held")[1]["messages"]], "held message")
    _, d = call("POST", f"/messages/{held[0]['id']}/decide", {"action": "bounce", "kind": "soft"}, 200)
    assert d["event"]["event"] == "bounce" and d["reply"]["status"] == 200, d

    # 5. Recipient action by hand and the app's reply.
    HOOK_STATUS[0] = 500
    _, r = call("POST", f"/messages/{mid}/events", {"type": "spamreport"}, 200)
    assert r["reply"]["status"] == 500
    HOOK_STATUS[0] = 200

    # 6. Access: validate keys, then the provider is down.
    _, k = call("POST", "/services/transactional/intake/smtp/keys", {"label": "api-staging"}, 200)
    call("PUT", "/services/transactional/intake/smtp/access", {"mode": "validate"}, 200)
    try:
        smtp_send("x@example.com", "s", "t", password="wrong")
        raise AssertionError("bad password was accepted")
    except smtplib.SMTPAuthenticationError as e:
        assert e.smtp_code == 535
    smtp_send("ok@example.com", "Key works", "t", password=k["secret"])
    call("PUT", "/services/transactional/intake/smtp/access", {"mode": "refuse", "how": "503"}, 200)
    try:
        smtp_send("x@example.com", "s", "t")
        raise AssertionError("refused intake accepted mail")
    except (smtplib.SMTPConnectError, smtplib.SMTPServerDisconnected) as e:
        pass
    call("PUT", "/services/transactional/intake/smtp/access", {"mode": "any", "refuseRecipients": "550"}, 200)
    try:
        smtp_send("x@example.com", "s", "t")
        raise AssertionError("RCPT should be refused")
    except smtplib.SMTPRecipientsRefused:
        pass
    call("PUT", "/services/transactional/intake/smtp/access", {"refuseRecipients": "none"}, 200)

    # 7. expect_no_message and the wait timeout explain themselves.
    st, e = call("GET", "/messages/wait?to=nobody@example.com&timeout=1s")
    assert st == 408 and "No e-mail" in e["error"], e

    # 8. MCP: initialize, list tools, call one; the action is attributed to the agent.
    def mcp(body, sid=None):
        h = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream"}
        if sid:
            h["Mcp-Session-Id"] = sid
        r = urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{HTTP}/mcp", method="POST", data=json.dumps(body).encode(), headers=h))
        return r.headers.get("Mcp-Session-Id"), (json.loads(r.read()) if r.status == 200 else None)
    sid, init = mcp({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26", "clientInfo": {"name": "claude-code"}, "capabilities": {}}})
    assert init["result"]["serverInfo"]["name"] == "stuntman" and sid
    _, tl = mcp({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}, sid)
    names = {t["name"] for t in tl["result"]["tools"]}
    for n in ["list_messages", "wait_for_message", "create_inbox", "decide_held", "set_access", "test_extractor", "simulate_recipient"]:
        assert n in names, n
    _, c = mcp({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "create_inbox", "arguments": {"prefix": "agent"}}}, sid)
    out = json.loads(c["result"]["content"][0]["text"])
    assert out["inbox"]["createdBy"] == "mcp:claude-code", out
    _, clients = call("GET", "/clients", expect=200)
    assert any(c["name"] == "claude-code" and c["via"] == "MCP" for c in clients["clients"]), clients

    # 9. Service YAML round trip, rename guard, delete.
    st, y = call("GET", "/services/transactional/yaml")
    assert st == 200 and b"plugin: email@1.0" in y
    call("POST", "/services", {"channel": "email", "name": "marketing"}, 200)
    call("PATCH", "/services/marketing", {"newName": "transactional"}, 409)
    _, l = call("GET", "/services", expect=200)
    ports = [s["service"]["intake"]["smtp"]["port"] for s in l["services"]]
    assert len(set(ports)) == 2, ports
    call("DELETE", "/services/marketing", expect=200)
    _, n = call("DELETE", f"/messages?to=emma.carter@example.com", expect=200)
    assert n["count"] >= 1


if __name__ == "__main__":
    main()
