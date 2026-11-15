"""End-to-end check of WhatsApp (W1–W4): plays the app that talks to Meta's Cloud API, and its
webhook endpoint (which checks X-Hub-Signature-256 like a careful app).

    cargo build -p stuntman && python3 tests/e2e_whatsapp.py
"""
import hashlib, hmac, http.client, http.server, json, os, shutil, socket, subprocess, tempfile, threading, time, urllib.error, urllib.parse, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "stuntman")


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


HTTP, RECV = free_port(), free_port()
BASE = f"http://127.0.0.1:{HTTP}"
GOT = []          # webhooks the app received: (signature ok, body)
APP = {"secret": "", "verify": "", "answer": 200}


class Recv(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass

    def do_GET(self):  # Meta's verification request
        q = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        ok = q.get("hub.verify_token", [""])[0] == APP["verify"]
        b = (q.get("hub.challenge", [""])[0] if ok else "nope").encode()
        self.send_response(200 if ok else 403); self.end_headers(); self.wfile.write(b)

    def do_POST(self):
        b = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        sig = self.headers.get("X-Hub-Signature-256", "")
        good = sig == "sha256=" + hmac.new(APP["secret"].encode(), b, hashlib.sha256).hexdigest()
        GOT.append((good, json.loads(b)))
        code = APP["answer"] if good else 401
        self.send_response(code); self.end_headers()


def call(method, path, body=None, token="any-token", base=BASE + "/api"):
    req = urllib.request.Request(base + path, method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "Authorization": f"Bearer {token}", "User-Agent": "acme-api/2.1",
                                          "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def graph(method, path, body=None, token="any-token"):
    return call(method, path, body, token, base=BASE + "/whatsapp/v21.0")


def statuses(wamid):
    return [v["status"] for ok, b in GOT for e in b.get("entry", []) for c in e["changes"] for v in c["value"].get("statuses", []) if v["id"] == wamid]


def wait(cond, what, t=15):
    end = time.time() + t
    while time.time() < end:
        if cond(): return
        time.sleep(0.1)
    raise AssertionError(f"timeout: {what}")


def tpl(name, to, params, lang="en_US"):
    return {"messaging_product": "whatsapp", "to": to, "type": "template",
            "template": {"name": name, "language": {"code": lang}, "components": [{"type": "body", "parameters": [{"type": "text", "text": p} for p in params]}]}}


def run():
    st, n = call("GET", "/whatsapp/number"); assert st == 200, n
    num = n["number"]
    pnid, waba = num["phone_number_id"], num["waba_id"]
    call("PUT", "/whatsapp/number", {"webhook_url": f"http://127.0.0.1:{RECV}/webhooks/whatsapp"})
    import re
    sec = open(os.path.join(DATA, "environments", "stg-a", "secrets.yaml")).read()
    APP["secret"] = re.search(r"whatsapp-app-secret: *'?([0-9a-f]+)", sec).group(1)
    APP["verify"] = re.search(r"whatsapp-verify-token: *'?([0-9a-f]+)", sec).group(1)

    # W4 verification GET and a forged webhook.
    st, v = call("POST", "/whatsapp/number/webhook/verify"); assert st == 200 and v["ok"], v

    # W2 templates.
    st, _ = call("POST", "/whatsapp/templates", {"name": "order_confirmation", "body": "Hi {{1}}, your order {{2}} is confirmed. Total: {{3}}. Delivery: {{4}}.",
        "header": "Order confirmed", "footer": "Acme Store", "buttons": [{"text": "Track order", "url": "https://shop.example.com/track/{{1}}"}, {"text": "Talk to us", "quick_reply": "TALK_TO_US"}]})
    assert st == 200, st
    call("POST", "/whatsapp/templates", {"name": "back_in_stock", "body": "{{1}} is back", "category": "marketing"})
    call("PUT", "/whatsapp/templates/back_in_stock/status", {"status": "paused"})

    # W3 recipients.
    call("PUT", "/whatsapp/recipients/+14155550142", {"profile_name": "Emma Carter", "steps": [{"delivered_after": "300ms"}, {"read_after": "800ms"}, {"on_buttons": {"tap": "Talk to us", "after": "1s"}}]})
    call("PUT", "/whatsapp/recipients/+13125550110", {"whatsapp": False})
    st, e = call("GET", "/whatsapp/recipients/explain?number=%2B13125550110"); assert "131026" in e["next"], e

    # W1: a template goes sent → delivered → read, signed; the person taps "Talk to us".
    st, r = graph("POST", f"/{pnid}/messages", tpl("order_confirmation", "+14155550142", ["Emma", "58213", "$1,290.00", "Nov 14"])); assert st == 200, r
    wamid = r["messages"][0]["id"]
    assert r["contacts"][0]["wa_id"] == "14155550142"
    wait(lambda: statuses(wamid)[-1:] == ["read"], "read")
    assert statuses(wamid) == ["sent", "delivered", "read"], statuses(wamid)
    assert all(ok for ok, _ in GOT), "every webhook is signed with the app secret"
    wait(lambda: any(m.get("type") == "button" for ok, b in GOT for e in b["entry"] for c in e["changes"] for m in c["value"].get("messages", [])), "button tap")
    btn = [m for ok, b in GOT for e in b["entry"] for c in e["changes"] for m in c["value"].get("messages", []) if m.get("type") == "button"][0]
    assert btn["button"]["payload"] == "TALK_TO_US" and btn["context"]["id"] == wamid, btn
    st, w = call("GET", "/whatsapp/numbers/%2B14155550142/window"); assert w["window"]["open"], w
    st, m = call("GET", f"/whatsapp/messages/{wamid}"); assert m["message"]["text"].startswith("Hi Emma, your order 58213"), m
    assert m["message"]["traceId"] == "4bf92f3577b34da6a3ce929d0e0e4736"
    st, waited = call("GET", "/whatsapp/wait?number=%2B14155550142&template=order_confirmation&timeout=2s"); assert waited["wamid"] == wamid, waited

    # Text inside the window is delivered; outside it fails 131047.
    st, r = graph("POST", f"/{pnid}/messages", {"messaging_product": "whatsapp", "to": "14155550142", "type": "text", "text": {"body": "Sure, reply with the new address."}})
    t1 = r["messages"][0]["id"]; wait(lambda: "delivered" in statuses(t1), "text delivered")
    st, r = graph("POST", f"/{pnid}/messages", {"messaging_product": "whatsapp", "to": "12065550133", "type": "text", "text": {"body": "Thanks"}})
    t2 = r["messages"][0]["id"]; wait(lambda: "failed" in statuses(t2), "131047")
    err = [v for ok, b in GOT for e in b["entry"] for c in e["changes"] for v in c["value"].get("statuses", []) if v["id"] == t2 and v["status"] == "failed"][0]
    assert err["errors"][0]["code"] == 131047, err

    # Not on WhatsApp → 200 then failed 131026.
    st, r = graph("POST", f"/{pnid}/messages", tpl("order_confirmation", "+13125550110", ["Liam", "1", "$1", "Nov 1"])); assert st == 200
    t3 = r["messages"][0]["id"]; wait(lambda: "failed" in statuses(t3), "131026")

    # Synchronous refusals, as Meta answers them.
    st, r = graph("POST", f"/{pnid}/messages", tpl("shipping_update", "+14155550142", ["a", "b", "c"])); assert st == 404 and r["error"]["code"] == 132001, r
    st, r = graph("POST", f"/{pnid}/messages", tpl("order_confirmation", "+14155550142", ["a", "b", "c"])); assert r["error"]["code"] == 132000, r
    st, r = graph("POST", f"/{pnid}/messages", tpl("back_in_stock", "+14155550142", ["Boots"])); assert r["error"]["code"] == 132015, r
    st, u = call("GET", "/whatsapp/templates/undeclared"); assert u["undeclared"][0]["name"] == "shipping_update" and u["undeclared"][0]["params"] == 3, u
    st, r = graph("POST", "/999/messages", tpl("order_confirmation", "+14155550142", ["a", "b", "c", "d"])); assert r["error"]["code"] == 100, r

    # Tokens: expired on purpose, then strict.
    call("POST", "/whatsapp/number/tokens/expire")
    st, r = graph("POST", f"/{pnid}/messages", tpl("order_confirmation", "+14155550142", ["a", "b", "c", "d"])); assert st == 401 and r["error"]["code"] == 190 and r["error"]["error_subcode"] == 463, r
    call("POST", "/whatsapp/number/tokens/restore")
    call("PUT", "/whatsapp/number", {"any_token": False})
    st, r = graph("POST", f"/{pnid}/messages", tpl("order_confirmation", "+14155550142", ["a", "b", "c", "d"])); assert st == 401 and r["error"]["code"] == 190, r
    tok = re.search(r"whatsapp-token: *'?([0-9a-f]+)", sec).group(1)
    st, r = graph("POST", f"/{pnid}/messages", tpl("order_confirmation", "+14155550142", ["a", "b", "c", "d"]), token=tok); assert st == 200, r
    call("PUT", "/whatsapp/number", {"any_token": True})

    # The person sends an image; the app fetches it like from Meta.
    st, m = call("POST", "/whatsapp/numbers/%2B14155550142/reply", {"image": "Here is the receipt"}); assert st == 200, m
    media = m["message"]["body"]["image"]["id"]
    st, info = graph("GET", f"/{media}"); assert st == 200 and info["mime_type"] == "image/png", info
    req = urllib.request.Request(info["url"].replace("localhost", "127.0.0.1"), headers={"Authorization": "Bearer x"})
    assert urllib.request.urlopen(req).read()[:4] == b"\x89PNG"

    # Template management through Meta's API: created PENDING, approved after approve_after.
    call("PUT", "/whatsapp/number", {"approve_after": "1s"})
    n0 = len(GOT)
    st, r = graph("POST", f"/{waba}/message_templates", {"name": "payment_reminder", "language": "en_US", "category": "UTILITY", "components": [{"type": "BODY", "text": "Your invoice {{1}} is due"}]})
    assert r["status"] == "PENDING", r
    wait(lambda: any(c["field"] == "message_template_status_update" and c["value"]["event"] == "APPROVED" for ok, b in GOT[n0:] for e in b["entry"] for c in e["changes"]), "auto approval", 20)
    st, lst = graph("GET", f"/{waba}/message_templates?name=payment_reminder"); assert lst["data"][0]["status"] == "APPROVED", lst
    # Category change and low quality, as Meta sends them.
    call("PATCH", "/whatsapp/templates/payment_reminder", {"category": "marketing"})
    wait(lambda: any(c["field"] == "template_category_update" for ok, b in GOT for e in b["entry"] for c in e["changes"]), "category update")

    # Forged webhooks the app must reject.
    for k in ["wrong_signature", "missing_signature", "old_secret"]:
        st, r = call("POST", "/whatsapp/number/webhook/bad", {"kind": k}); assert r["status"] == 401 and r["rejected"], (k, r)
    # A duplicate status (the app should ignore it) and a failure on purpose.
    call("POST", f"/whatsapp/messages/{wamid}/resend", {"status": "read"})
    wait(lambda: statuses(wamid).count("read") == 2, "resent read")
    st, r = call("POST", f"/whatsapp/messages/{t1}/status", {"status": "failed", "error_code": 131049}); assert r["message"]["status"] == "failed", r
    st, h = call("GET", "/whatsapp/webhooks"); assert h["webhooks"] and h["webhooks"][0]["status"] in (200, 401), h

    # API versions: a range refuses others with 2500, as Meta answers an unknown path.
    call("PUT", "/whatsapp/number", {"api_versions": "v18.0-v20.0"})
    st, r = graph("POST", f"/{pnid}/messages", tpl("order_confirmation", "+14155550142", ["a", "b", "c", "d"])); assert st == 400 and r["error"]["code"] == 2500, r
    call("PUT", "/whatsapp/number", {"api_versions": "any"})

    # The app's webhook fails; "Retry now" sends the failed ones again once it is back.
    APP["answer"] = 500
    st, r = graph("POST", f"/{pnid}/messages", tpl("order_confirmation", "+14155550142", ["a", "b", "c", "d"])); w5 = r["messages"][0]["id"]
    wait(lambda: any(x["wamid"] == w5 and x["status"] == 500 for x in call("GET", "/whatsapp/webhooks")[1]["webhooks"]), "a failed status webhook")
    APP["answer"] = 200
    n0 = len(GOT)
    st, r = call("POST", "/whatsapp/webhooks/retry"); assert r["retried"] >= 1, r
    wait(lambda: any(v["id"] == w5 for ok, b in GOT[n0:] for e in b["entry"] for c in e["changes"] for v in c["value"].get("statuses", [])), "retried webhook")

    # Forced failures at the HTTP level: Meta's 500 and 400 bodies, then the connection itself.
    txt = {"messaging_product": "whatsapp", "to": "14155550142", "type": "text", "text": {"body": "Your code is 4821"}}
    st, f = call("POST", "/whatsapp/force", {"answer": "500", "count": 2}); assert f["forced"]["left"] == 2, f
    for _ in range(2):
        st, r = graph("POST", f"/{pnid}/messages", txt); assert st == 500 and r["error"]["code"] == 131000 and r["error"]["fbtrace_id"], (st, r)
    st, r = graph("POST", f"/{pnid}/messages", txt); assert st == 200, "count 2: the third send goes"
    call("POST", "/whatsapp/force", {"answer": "400"})
    st, r = graph("POST", f"/{pnid}/messages", txt); assert st == 400 and r["error"]["code"] == 100 and r["error"]["type"] == "OAuthException", (st, r)
    for gen, exc in [("reset", (ConnectionResetError, http.client.RemoteDisconnected)), ("empty", (http.client.RemoteDisconnected,)), ("timeout", (socket.timeout, TimeoutError))]:
        call("POST", "/whatsapp/force", {"answer": gen})
        c = http.client.HTTPConnection("127.0.0.1", HTTP, timeout=2)
        try:
            c.request("POST", f"/whatsapp/v21.0/{pnid}/messages", json.dumps(txt), {"Content-Type": "application/json", "Authorization": "Bearer any-token"})
            raise AssertionError(f"{gen}: answered {c.getresponse().status}")
        except exc:
            pass
        finally:
            c.close()
    st, n2 = call("GET", "/whatsapp/number"); assert n2["forced"] is None, n2["forced"]
    st, ms = call("GET", "/whatsapp/messages?number=%2B14155550142&since=5m")
    assert sum(m["status"] == "refused" for m in ms["messages"]) == 6, [m["status"] for m in ms["messages"]]


def main():
    global DATA
    DATA = tempfile.mkdtemp(prefix="duble-wa-")
    recv = http.server.ThreadingHTTPServer(("127.0.0.1", RECV), Recv)
    threading.Thread(target=recv.serve_forever, daemon=True).start()
    env = dict(os.environ, STUNTMAN_ENV=os.environ.get("STUNTMAN_ENV", "stg-a"), STUNTMAN_DATA=DATA, STUNTMAN_PORT=str(HTTP), STUNTMAN_HOOKS_PORT=str(free_port()), STUNTMAN_SMTP_PORT=str(free_port()), STUNTMAN_OTLP_HTTP_PORT=str(free_port()),
               STUNTMAN_OTLP_GRPC_PORT=str(free_port()), STUNTMAN_BLOB_PORT=str(free_port()), STUNTMAN_S3_PORT=str(free_port()), STUNTMAN_APIS_PORT=str(free_port()),
               STUNTMAN_APIS_TLS_PORT=str(free_port()), STUNTMAN_WEB="/nonexistent")
    log = DATA + ".log"
    srv = subprocess.Popen([BIN], env=env, stdout=open(log, "w"), stderr=subprocess.STDOUT)
    try:
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", HTTP), 0.2).close(); break
            except OSError:
                time.sleep(0.1)
        time.sleep(0.5)
        run()
        # A restart while a message waits for "read": the step resumes.
        call("PUT", "/whatsapp/recipients/+15125550100", {"steps": [{"delivered_after": "200ms"}, {"read_after": "4s"}, {"on_buttons": {"tap": "Talk to us", "after": "3s"}}]})
        st, n = call("GET", "/whatsapp/number")
        st, r = graph("POST", f"/{n['number']['phone_number_id']}/messages", tpl("order_confirmation", "+15125550100", ["a", "b", "c", "d"])); w6 = r["messages"][0]["id"]
        wait(lambda: "delivered" in statuses(w6), "delivered before restart")
        srv.terminate(); srv.wait()
        srv2 = subprocess.Popen([BIN], env=env, stdout=open(log, "a"), stderr=subprocess.STDOUT)
        try:
            wait(lambda: "read" in statuses(w6), "read after restart", 20)
            taps = lambda: [m for ok, b in GOT for e in b["entry"] for c in e["changes"] for m in c["value"].get("messages", []) if m.get("type") == "button" and m["context"]["id"] == w6]
            wait(lambda: len(taps()) == 1, "the tap scheduled before the restart", 20)
            time.sleep(1.5)
            assert len(taps()) == 1, "tapped once, not twice"
        finally:
            srv2.terminate()
        print("e2e whatsapp: all checks passed")
    finally:
        srv.terminate(); recv.shutdown(); shutil.rmtree(DATA, ignore_errors=True)


if __name__ == "__main__":
    main()
