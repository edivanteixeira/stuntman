"""Synthetic webhook data like the H1–H2 screens: kept inboxes globex, initech, umbrella and two
ephemeral ones, with signed deliveries from a pretend acme-api.

    python3 tests/seed_webhook.py [http://localhost:8025] [hooks port 8027]
"""
import hashlib, hmac, http.client, json, sys, time, urllib.error, urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8025"
HOOKS = int(sys.argv[2]) if len(sys.argv) > 2 else 8027


def api(method, path, body=None, who="qa-suite"):
    req = urllib.request.Request(BASE + "/api" + path, method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "X-Stuntman-Client": who})
    try:
        return json.loads(urllib.request.urlopen(req).read() or b"null")
    except urllib.error.HTTPError as e:
        return {"error": e.read().decode()}


def send(inbox, event, eid, data, secret="whsec_globex_2026", ts=None, timeout=10):
    body = json.dumps({"type": event, "id": eid, "data": data}).encode()
    t = str(ts or int(time.time()))
    sig = hmac.new(secret.encode(), f"{t}.".encode() + body, hashlib.sha256).hexdigest()
    c = http.client.HTTPConnection("127.0.0.1", HOOKS, timeout=timeout)
    try:
        c.request("POST", f"/in/{inbox}", body, {"Content-Type": "application/json", "X-Acme-Signature": f"t={t},v1={sig}", "X-Acme-Event-Id": eid,
                                                 "User-Agent": "acme-api/2.50 (acme-worker-2)",
                                                 "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"})
        return c.getresponse().status
    except Exception as e:
        return type(e).__name__


api("POST", "/webhooks/inboxes", {"name": "globex", "keep": True, "secret": "secret://globex-webhook-secret", "previousSecret": "secret://globex-webhook-secret-2025", "header": "X-Acme-Signature"})
api("POST", "/webhooks/inboxes", {"name": "initech", "keep": True, "fail": 3, "then": 200, "secret": "secret://initech-webhook-secret"})
api("POST", "/webhooks/inboxes", {"name": "umbrella", "keep": True, "mode": "hang", "hang": "35s"})
api("POST", "/webhooks/inboxes", {"name": "pr-5451-a9f2", "expires": "42m"}, who="qa-suite")
api("POST", "/webhooks/inboxes", {"name": "tmp-7c03", "expires": "6m", "traps": ["redirect_private"]}, who="claude-code")

send("globex", "invoice.paid", "evt_3c11", {"invoice": "INV-2207", "customer": "globex"}, ts=int(time.time()) - 540)
send("globex", "order.cancelled", "evt_51aa", {"order": 58190, "customer": "globex"})
send("globex", "order.cancelled", "evt_51aa", {"order": 58190, "customer": "globex"})
send("globex", "order.completed", "evt_77e0", {"order": 58212, "customer": "globex"}, secret="whsec_globex_2025")
send("globex", "order.created", "evt_9b20", {"order": 58214, "customer": "globex"})
send("globex", "order.completed", "evt_8d41e2b7", {"order": 58213, "customer": "globex", "total": 1290.00})
for _ in range(4):
    send("initech", "order.completed", "evt_c0de", {"order": 58213, "customer": "initech"})
    time.sleep(0.4)
send("umbrella", "order.completed", "evt_u1", {"order": 58213}, timeout=2)
send("tmp-7c03", "order.completed", "evt_t1", {"order": 58215})
print("seeded webhooks", BASE, HOOKS)
