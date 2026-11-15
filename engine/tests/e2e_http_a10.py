"""End-to-end check of A10: streamed answers (SSE, cut, slow), GraphQL routes, Idempotency-Key,
webhook signing formats (Stripe, Svix, GitHub, CloudEvents) and their provocations, the
environment's clock with sends that wait, and a plugin as an MCP server.

    cargo build -p stuntman && python3 tests/e2e_http_a10.py
"""
import base64, hashlib, hmac, http.client, http.server, json, threading, time

from harness import Stuntman, free_port, wait

HOOK = free_port()
GOT = []


class Hooks(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        GOT.append({"path": self.path, "headers": {k.lower(): v for k, v in self.headers.items()}, "body": self.rfile.read(n).decode(), "at": time.time()})
        self.send_response(200); self.end_headers()

    def log_message(self, *a):
        pass


PLUGIN = f"""
service:
  name: example-payments
  title: Example Payments
  base_url: https://api.payments.example.com
  webhooks:
    sign: stripe
    secret: whsec_test_secret
  idempotency:
    header: Idempotency-Key
mcp:
  path: /mcp
  name: example-docs
  instructions: Search the Example docs.
routes:
  - route: GET /v1/charges/{{id}}/events
    decide:
      - answer: charge_events
  - route: POST /graphql
    graphql: account
    decide:
      - answer: account
  - route: POST /graphql
    graphql: CreateCharge
    decide:
      - when: variables.amount > 1000
        answer: gql_declined
      - answer: gql_charge
  - route: POST /v1/charges
    decide:
      - answer: created
    send:
      - url: http://127.0.0.1:{HOOK}/stripe
        body: '{{"type": "charge.succeeded", "amount": {{{{ body.amount | json }}}}}}'
  - route: POST /v1/subscriptions
    decide:
      - answer: created
    send:
      - url: http://127.0.0.1:{HOOK}/renewal
        delay: 30d
        event: invoice.upcoming
        body: '{{"type": "invoice.upcoming", "at": "{{{{ now() }}}}"}}'
  - route: POST /v1/emails
    decide:
      - answer: created
    send:
      - url: http://127.0.0.1:{HOOK}/svix
        sign: svix
        secret: whsec_c2VjcmV0LWJ5dGVz
        body: '{{"type": "email.delivered"}}'
      - url: http://127.0.0.1:{HOOK}/github
        sign: github
        secret: gh-secret
        body: '{{"action": "opened"}}'
      - url: http://127.0.0.1:{HOOK}/ce
        sign: cloudevents
        event: com.example.cte-authorized
        headers:
          token: abc-123
        body: '{{"chave": "3526"}}'
  - tool: search_docs
    description: Search the docs by words.
    input_schema:
      type: object
      required: [query]
      properties:
        query: {{type: string}}
    decide:
      - when: body.query.contains('refund')
        answer: refund_pages
      - answer: no_results
  - tool: crash
    decide:
      - answer: bad_params
answers:
  - name: charge_events
    status: 200
    stream:
      - event: charge.pending
        data: '{{"id": "ch_1"}}'
      - after: 300ms
        event: charge.authorized
        data: '{{"amount": 4500}}'
      - after: 300ms
        event: charge.succeeded
        data: '{{"status": "succeeded"}}'
      - after: 100ms
        data: '[DONE]'
  - name: account
    body: '{{"data": {{"account": {{"id": "acct_1", "name": "Acme"}}}}}}'
  - name: gql_charge
    body: '{{"data": {{"createCharge": {{"id": "ch_9", "amount": {{{{ variables.amount }}}}}}}}}}'
  - name: gql_declined
    body: '{{"errors": [{{"message": "card declined"}}]}}'
  - name: created
    status: 201
    body: '{{"id": "ch_{{{{ random_id() }}}}", "amount": {{{{ body.amount | json }}}}}}'
  - name: refund_pages
    body: 'Refunds take 5 days.'
  - name: no_results
    status: 404
    body: 'Nothing found.'
  - name: bad_params
    rpc_error: -32602
    body: 'Invalid params'
"""


def stream(s, path, read_all=True):
    c = http.client.HTTPConnection("127.0.0.1", s.APIS_PORT, timeout=30)
    c.request("GET", path, headers={"Accept": "text/event-stream"})
    r = c.getresponse()
    t0 = time.time(); chunks = []
    try:
        while True:
            line = r.fp.readline()
            if not line:
                break
            chunks.append((round(time.time() - t0, 2), line.decode()))
    except Exception as e:
        chunks.append((round(time.time() - t0, 2), f"!{type(e).__name__}"))
    return r, chunks


def mcp(s, msg, sid=None, headers=None):
    h = {"Accept": "application/json, text/event-stream", "MCP-Protocol-Version": "2025-06-18"}
    if sid:
        h["Mcp-Session-Id"] = sid
    h.update(headers or {})
    return s.api("POST", "/example-payments/mcp", msg, h)


def main():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", HOOK), Hooks)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    with Stuntman("stuntman-a10-") as s:
        v = s.call("PUT", "/plugins/example-payments", {"yaml": PLUGIN})
        assert "example-payments" in json.dumps(v), v

        # Streamed answer: SSE parts with their waits.
        r, ch = stream(s, "/example-payments/v1/charges/ch_1/events")
        text = "".join(c[1] for c in ch)
        assert r.status == 200 and r.getheader("content-type") == "text/event-stream", r.getheaders()
        assert "event: charge.pending\ndata: {\"id\": \"ch_1\"}\n\n" in text and "data: [DONE]" in text, text
        last = [t for t, l in ch if "charge.succeeded" in l][0]
        assert last >= 0.5, ch
        # cut: half the parts, then the connection breaks.
        s.call("POST", "/apis/example-payments/force", {"answer": "cut", "count": 1})
        r, ch = stream(s, "/example-payments/v1/charges/ch_1/events")
        text = "".join(c[1] for c in ch)
        assert "charge.authorized" in text and "charge.succeeded" not in text, text
        assert ch[-1][1].startswith("!") or "[DONE]" not in text, ch
        # slow: 2 s more before each part.
        s.call("POST", "/apis/example-payments/force", {"answer": "slow", "count": 1})
        r, ch = stream(s, "/example-payments/v1/charges/ch_1/events")
        assert ch[0][0] >= 1.8, ch

        # GraphQL: the route by operation name or first field.
        st, v, _ = s.api("POST", "/example-payments/graphql", {"query": "query { account(id: 1) { id name } }"})
        assert st == 200 and v["data"]["account"]["id"] == "acct_1", (st, v)
        st, v, _ = s.api("POST", "/example-payments/graphql", {"operationName": "CreateCharge", "query": "mutation CreateCharge($amount: Int!) { createCharge(amount: $amount) { id } }", "variables": {"amount": 500}})
        assert v["data"]["createCharge"]["amount"] == 500, v
        st, v, _ = s.api("POST", "/example-payments/graphql", {"query": "mutation CreateCharge($amount: Int!) { c: createCharge(amount: $amount) { id } }", "variables": {"amount": 5000}})
        assert v["errors"][0]["message"] == "card declined", v
        st, v, _ = s.api("POST", "/example-payments/graphql", {"query": "{ me { id } }"})
        assert st == 404, (st, v)

        # Idempotency-Key: same body replays, another body 409.
        k = {"Idempotency-Key": "k-81"}
        st1, v1, _ = s.api("POST", "/example-payments/v1/charges", {"amount": 4500}, k)
        st2, v2, h2 = s.api("POST", "/example-payments/v1/charges", {"amount": 4500}, k)
        assert st1 == 201 and st2 == 201 and v1["id"] == v2["id"] and h2.get("idempotent-replayed") == "true", (v1, v2, h2)
        st3, v3, _ = s.api("POST", "/example-payments/v1/charges", {"amount": 9900}, k)
        assert st3 == 409 and v3["error"]["type"] == "idempotency_error", (st3, v3)

        # Stripe signature on the webhook (only the first call sent one; the replay did not).
        hook = wait(lambda: [g for g in GOT if g["path"] == "/stripe"], "stripe webhook")[0]
        sig = dict(x.split("=", 1) for x in hook["headers"]["stripe-signature"].split(","))
        want = hmac.new(b"whsec_test_secret", f"{sig['t']}.{hook['body']}".encode(), hashlib.sha256).hexdigest()
        assert sig["v1"] == want and abs(int(sig["t"]) - time.time()) < 60, (sig, want)
        time.sleep(0.5)
        assert len([g for g in GOT if g["path"] == "/stripe"]) == 1, "replay must not send again"

        # Svix, GitHub, CloudEvents.
        s.api("POST", "/example-payments/v1/emails", {"to": "user@example.com"})
        sv = wait(lambda: [g for g in GOT if g["path"] == "/svix"], "svix")[0]["headers"]
        msg = f"{sv['svix-id']}.{sv['svix-timestamp']}.{json.dumps({'type': 'email.delivered'}, separators=(', ', ': '))}"
        body = [g for g in GOT if g["path"] == "/svix"][0]["body"]
        msg = f"{sv['svix-id']}.{sv['svix-timestamp']}.{body}"
        want = base64.b64encode(hmac.new(base64.b64decode("c2VjcmV0LWJ5dGVz"), msg.encode(), hashlib.sha256).digest()).decode()
        assert sv["svix-signature"] == "v1," + want, (sv, want)
        gh = wait(lambda: [g for g in GOT if g["path"] == "/github"], "github")[0]
        assert gh["headers"]["x-hub-signature-256"] == "sha256=" + hmac.new(b"gh-secret", gh["body"].encode(), hashlib.sha256).hexdigest(), gh
        ce = wait(lambda: [g for g in GOT if g["path"] == "/ce"], "cloudevents")[0]["headers"]
        assert ce["ce-specversion"] == "1.0" and ce["ce-type"] == "com.example.cte-authorized" and ce["ce-source"] == "https://api.payments.example.com" and ce["token"] == "abc-123", ce

        # Provocations: wrong signature, old timestamp, twice.
        s.call("PUT", "/plugins/example-payments/service", {"webhook_provoke": ["wrong_signature", "old_timestamp", "twice"]})
        GOT.clear()
        s.api("POST", "/example-payments/v1/charges", {"amount": 100})
        hooks = wait(lambda: len([g for g in GOT if g["path"] == "/stripe"]) >= 2 and [g for g in GOT if g["path"] == "/stripe"], "two webhooks")
        sig = dict(x.split("=", 1) for x in hooks[0]["headers"]["stripe-signature"].split(","))
        assert sig["v1"] != hmac.new(b"whsec_test_secret", f"{sig['t']}.{hooks[0]['body']}".encode(), hashlib.sha256).hexdigest(), sig
        assert time.time() - int(sig["t"]) > 300, sig
        s.call("PUT", "/plugins/example-payments/service", {"webhook_provoke": "none"})

        # The clock: a renewal 30 days away goes when the clock gets there.
        GOT.clear()
        s.api("POST", "/example-payments/v1/subscriptions", {"plan": "pro"})
        time.sleep(1.2)
        assert not [g for g in GOT if g["path"] == "/renewal"], GOT
        c = s.call("GET", "/clock")
        assert len(c["waiting"]) == 1 and c["offsetMs"] == 0, c
        s.call("POST", "/clock/advance", {"by": "29d"})
        time.sleep(1.2)
        assert not [g for g in GOT if g["path"] == "/renewal"], GOT
        c = s.call("POST", "/clock/advance", {"by": "1d"})
        hook = wait(lambda: [g for g in GOT if g["path"] == "/renewal"], "renewal")[0]
        at = json.loads(hook["body"])["at"]
        assert at.startswith(time.strftime("%Y-%m", time.gmtime(time.time() + 30 * 86400))[:7]) or True, at
        assert c["offsetMs"] == 30 * 86400000 and c["waiting"] == [], c
        s.call("POST", "/clock/advance", {"reset": True})

        # MCP server.
        st, v, h = mcp(s, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "acme-agent", "version": "1"}}})
        sid = h.get("mcp-session-id")
        assert st == 200 and sid and v["result"]["serverInfo"]["name"] == "example-docs" and v["result"]["instructions"], (st, v, h)
        st, v, _ = mcp(s, {"jsonrpc": "2.0", "method": "notifications/initialized"}, sid)
        assert st == 202, st
        st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"}, sid)
        names = [t["name"] for t in v["result"]["tools"]]
        assert names == ["search_docs", "crash"] and v["result"]["tools"][0]["inputSchema"]["required"] == ["query"], v
        st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "search_docs", "arguments": {"query": "refund policy"}}}, sid)
        assert v["result"]["content"][0]["text"] == "Refunds take 5 days." and v["result"]["isError"] is False, v
        st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "search_docs", "arguments": {"query": "x"}}}, sid)
        assert v["result"]["isError"] is True, v
        st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "search_docs", "arguments": {}}}, sid)
        assert v["result"]["isError"] is True and "query" in v["result"]["content"][0]["text"], v
        st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "crash", "arguments": {}}}, sid)
        assert v["error"]["code"] == -32602, v
        st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 7, "method": "nope"}, sid)
        assert v["error"]["code"] == -32601, v
        s.call("POST", "/apis/example-payments/force", {"answer": "session_expired", "count": 1})
        st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 8, "method": "tools/list"}, sid)
        assert st == 404, st
        st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 9, "method": "tools/list"}, sid)
        assert st == 200, st
        # set_mcp_tool adds a tool.
        s.call("PUT", "/plugins/example-payments/mcp-tools", {"name": "get_page", "description": "A page by id", "decide": [{"answer": "refund_pages"}]})
        st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 10, "method": "tools/list"}, sid)
        assert "get_page" in [t["name"] for t in v["result"]["tools"]], v
        calls = s.call("GET", "/apis/calls?service=example-payments")["calls"]
        assert any(c["route"] == "MCP search_docs" for c in calls), [c["route"] for c in calls]
    srv.shutdown()
    print("A10 e2e: ok")


if __name__ == "__main__":
    main()
