"""End-to-end check of services from an OpenAPI spec (A8) and of recording the real provider (A9).

A8: a spec read from a URL (with a $ref to another file) makes routes, answers from examples and
schemas, bodies checked against the schemas, an OAuth token route, the spec served back, and a
re-import that keeps the steps.
A9: calls go through Stuntman to a fake sandbox and back; what is kept is masked; the review groups
paths, names answers and suggests the rule; make_plugin writes a plugin that answers alone, binary
answers included. Everything over the HTTP API (the same tools MCP has).

    cargo build -p stuntman && python3 tests/e2e_spec_record.py
"""
import http.client, http.server, json, os, re, shutil, socket, subprocess, tempfile, threading, time, urllib.error, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(os.environ.get("CARGO_TARGET_DIR", os.path.join(ROOT, "target")), "debug", "stuntman")
SPECS = os.path.join(ROOT, "tests", "openapi")


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


HTTP, HOOKS, SMTP, OTLP, OTLP_G, BLOB, S3, APIS, TLS, KV, AMQP, AMQPS, FILES, SANDBOX = (free_port() for _ in range(14))
PDF = b"%PDF-1.4\n" + bytes(range(256)) * 8
SEEN = []


class Files(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **k):
        super().__init__(*a, directory=SPECS, **k)

    def log_message(self, *a):
        pass


class Sandbox(http.server.BaseHTTPRequestHandler):
    """A fake shipping provider's sandbox (Globex Shipping, synthetic)."""
    shipments = {}

    def reply(self, status, body, ctype="application/json"):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status); self.send_header("Content-Type", ctype); self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(n) or b"{}")
        SEEN.append({"path": self.path, "auth": self.headers.get("Authorization"), "body": body})
        if self.path == "/v2/shipments":
            if not body.get("destination", {}).get("postal_code"):
                return self.reply(422, {"error": "postal_code invalid"})
            sid = f"SHP-{88229 + len(self.shipments)}"
            self.shipments[sid] = body
            return self.reply(201, {"id": sid, "status": "created", "destination": body["destination"], "contact": body.get("contact"), "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())})
        if self.path.endswith("/labels"):
            return self.reply(200, PDF, "application/pdf")
        self.reply(404, {"error": "no route"})

    def do_GET(self):
        SEEN.append({"path": self.path, "auth": self.headers.get("Authorization")})
        m = re.match(r"^/v2/shipments/([^/?]+)$", self.path)
        if m:
            s = self.shipments.get(m.group(1))
            return self.reply(200, {"id": m.group(1), "status": "in_transit"}) if s else self.reply(404, {"error": "not found"})
        self.reply(404, {"error": "no route"})

    def log_message(self, *a):
        pass


def call(method, path, body=None, expect=200):
    req = urllib.request.Request(f"http://127.0.0.1:{HTTP}/api{path}", method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "X-Stuntman-Client": "qa-suite"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            st, v = r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        st, v = e.code, json.loads(e.read() or b"null")
    assert expect is None or st == expect, (method, path, st, v)
    return v


def api(method, path, body=None, headers=None):
    c = http.client.HTTPConnection("127.0.0.1", APIS, timeout=30)
    h = {"User-Agent": "acme-shop/2.0"}
    h.update(headers or {})
    data = None
    if body is not None:
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        h.setdefault("Content-Type", "application/json")
    c.request(method, path, data, h)
    r = c.getresponse()
    raw = r.read()
    try:
        v = json.loads(raw)
    except Exception:
        v = raw
    return r.status, v, {k.lower(): v for k, v in r.getheaders()}


def wait(f, what, t=15):
    end = time.time() + t
    while time.time() < end:
        v = f()
        if v:
            return v
        time.sleep(0.1)
    raise AssertionError("timed out waiting for " + what)


def openapi():
    url = f"http://127.0.0.1:{FILES}/payments.yaml"
    im = call("POST", "/plugins/spec", {"service": "example-payments", "source": url})
    assert im["kind"] == "openapi" and im["spec"]["version"] == "3.1.0" and im["warnings"] == [], im
    routes = {r["route"]: r for r in im["routes"]}
    assert routes["POST /v1/charges"]["schema"] == "ChargeRequest" and routes["POST /v1/charges"]["answer"] == "create_charge", routes
    assert routes["POST /oauth/token"]["token"] is True, routes
    assert im["webhooks"] == ["charge.succeeded"], im
    names = {a["name"]: a["status"] for a in im["answers"]}
    assert names["payment_required"] == 402 and names["invalid"] == 422 and names["not_found"] == 404 and names["conflict"] == 409, names
    call("POST", "/plugins/spec", {"service": "example-payments", "source": url}, expect=409)

    # The spec served back, with the file its $ref points to.
    st, body, h = api("GET", "/example-payments/payments.yaml")
    assert st == 200 and b"openapi: 3.1.0" in body and "yaml" in h["content-type"], (st, h)
    st, body, _ = api("GET", "/example-payments/common/errors.yaml")
    assert st == 200 and b"Error:" in body, st

    # A token, then charges: the success answer echoes the amount; the schema refuses wrong bodies.
    st, tok, _ = api("POST", "/example-payments/oauth/token", "grant_type=client_credentials&client_id=acme-shop&client_secret=x".encode(), {"Content-Type": "application/x-www-form-urlencoded"})
    assert st == 200 and tok["access_token"], tok
    auth = {"Authorization": "Bearer " + tok["access_token"]}
    st, v, _ = api("POST", "/example-payments/v1/charges", {"amount": 4500, "currency": "usd", "card": {"number": "4242424242424242"}})
    assert st == 401, (st, v)
    st, v, h = api("POST", "/example-payments/v1/charges", {"amount": 4500, "currency": "usd", "card": {"number": "4242424242424242"}}, auth)
    assert st == 201 and v["amount"] == 4500 and v["currency"] == "usd" and v["id"] == "ch_3Nf0" and h["request-id"] == "req_8Kq2", (st, v, h)
    assert re.match(r"\d{4}-\d\d-\d\dT", v["created"]), v
    st, v, _ = api("POST", "/example-payments/v1/charges", {"amount": "45.00", "currency": "usd", "card": {"number": "1"}}, auth)
    assert st == 422 and v["error"]["code"] == "invalid_request", (st, v)
    calls = call("GET", "/apis/calls?service=example-payments")["calls"]
    assert "amount: expected integer" in json.dumps(calls[0]["steps"]), calls[0]
    st, v, _ = api("POST", "/example-payments/v1/charges", {"amount": 10, "currency": "usd", "card": {"number": "1"}, "color": "red"}, auth)
    assert st == 422 and "color is not a field" in json.dumps(call("GET", "/apis/calls?service=example-payments")["calls"][0]["steps"]), (st, v)
    st, v, _ = api("GET", "/example-payments/v1/customers", headers=auth)
    assert st == 200 and v["data"][0]["email"] == "user@example.com", v

    # Named answers from the spec: forced like any other.
    call("POST", "/apis/example-payments/force", {"service": "example-payments", "answer": "payment_required", "count": 1})
    st, v, _ = api("POST", "/example-payments/v1/charges", {"amount": 4500, "currency": "eur", "card": {"number": "4000000000000002"}}, auth)
    assert st == 402 and v["error"]["code"] == "card_declined", (st, v)

    # A rule, then a re-import of a spec with one more path: the rule stays, the path is added.
    call("PUT", "/plugins/example-payments/routes", {"service": "example-payments", "method": "POST", "path": "/v1/charges",
         "steps": {"receive": {"auth": "bearer", "schema": "ChargeRequest", "invalid": "invalid"},
                   "decide": [{"when": "body.card.number.endsWith('0002')", "answer": "payment_required"}, {"answer": "create_charge"}]}})
    st, v, _ = api("POST", "/example-payments/v1/charges", {"amount": 4500, "currency": "eur", "card": {"number": "4000000000000002"}}, auth)
    assert st == 402, (st, v)
    spec2 = open(os.path.join(SPECS, "payments.yaml")).read().replace("  /oauth/token:", "  /v1/payouts:\n    post:\n      operationId: createPayout\n      responses:\n        \"201\": {description: ok, content: {application/json: {example: {id: po_1}}}}\n  /oauth/token:")
    call("PUT", "/plugins/example-payments/routes", {"service": "example-payments", "method": "GET", "path": "/v1/balance", "steps": {"decide": [{"answer": "get_charge"}]}})
    re_ = call("POST", "/plugins/example-payments/spec", {"service": "example-payments", "spec": spec2, "source": url})
    assert re_["added"] == ["POST /v1/payouts"] and re_["notInSpec"] == ["GET /v1/balance"], re_
    st, v, _ = api("GET", "/example-payments/v1/balance", headers=auth)
    assert st == 200, "a route made by hand survives the re-import"
    st, v, _ = api("POST", "/example-payments/v1/charges", {"amount": 4500, "currency": "eur", "card": {"number": "4000000000000002"}}, auth)
    assert st == 402, "the rule survived the re-import"
    st, v, _ = api("POST", "/example-payments/v1/payouts", {}, auth)
    assert st == 201 and v["id"] == "po_1", (st, v)

    # A WSDL through the same tool is a SOAP service.
    w = call("POST", "/plugins/spec", {"service": "example-insurance", "source": os.path.join(ROOT, "tests", "soap", "PolicyService.wsdl")})
    assert w["kind"] == "wsdl" and len(w["operations"]) == 4, w


def record():
    up = f"http://127.0.0.1:{SANDBOX}"
    st0 = call("POST", "/plugins/globex-shipping/recording", {"service": "globex-shipping", "upstream": up, "title": "Globex Shipping", "mask": ["reference"]})
    assert st0["state"] == "recording" and st0["mode"] == "every", st0
    key = {"Authorization": "Bearer sk_sandbox_123"}
    st, v, _ = api("POST", "/globex-shipping/v2/shipments", {"destination": {"city": "Porto", "postal_code": "4000-001"}, "contact": "ana.souza@mail.globex.test", "reference": "INV-7731"}, key)
    assert st == 201 and v["id"] == "SHP-88229" and v["contact"] == "ana.souza@mail.globex.test", "the app gets the real answer"
    assert SEEN[-1]["auth"] == "Bearer sk_sandbox_123", "the key goes to the provider as the app sent it"
    api("POST", "/globex-shipping/v2/shipments", {"destination": {"city": "Lisbon", "postal_code": "1000-001"}, "contact": "bruno@globex.example"}, key)
    st, v, _ = api("POST", "/globex-shipping/v2/shipments", {"destination": {"city": "Faro"}}, key)
    assert st == 422, (st, v)
    api("GET", "/globex-shipping/v2/shipments/SHP-88229", headers=key)
    api("GET", "/globex-shipping/v2/shipments/SHP-88230", headers=key)
    st, v, _ = api("GET", "/globex-shipping/v2/shipments/SHP-00000", headers=key)
    assert st == 404, st
    st, v, h = api("POST", "/globex-shipping/v2/shipments/SHP-88229/labels", {}, key)
    assert st == 200 and v == PDF and h["content-type"] == "application/pdf", (st, h)

    # What is kept: secrets as secret://, e-mails and the extra field masked.
    rows = call("GET", "/apis/calls?service=globex-shipping")["calls"]
    assert len(rows) == 7 and all(r["answer"] == "recorded" for r in rows), rows
    first = call("GET", f"/apis/calls/{rows[-1]['id']}?raw=true")
    kept = json.dumps(first)
    assert "sk_sandbox_123" not in kept and "secret://globex-shipping-authorization" in kept, kept[:600]
    assert "ana.souza@mail.globex.test" not in kept and "INV-7731" not in kept, kept[:600]
    status = call("GET", "/plugins/globex-shipping/recording")
    assert status["calls"] == 7 and status["recording"] is True, status

    # Stop and review: grouped paths, answers, the rule from the 422.
    rv = call("DELETE", "/plugins/globex-shipping/recording")
    routes = {r["route"]: r for r in rv["review"]["routes"]}
    assert set(routes) == {"POST /v2/shipments", "GET /v2/shipments/{id}", "POST /v2/shipments/{id}/labels"}, routes.keys()
    post = routes["POST /v2/shipments"]
    assert post["default"] == "created" and post["rules"][0]["when"] == "!body.destination.postal_code" and post["rules"][0]["answer"] == "invalid", post
    assert any(g["route"] == "GET /v2/shipments/{id}" for g in rv["review"]["grouped"]), rv["review"]["grouped"]
    assert rv["masked"] >= 4, rv

    # The plugin: answers alone, with the rule, ids and dates as templates, the PDF as a file.
    made = call("POST", "/plugins/globex-shipping/recording/plugin", {"service": "globex-shipping"})
    assert len(made["routes"]) == 3 and made["answers"] >= 5, made
    SEEN.clear()
    st, v, _ = api("POST", "/globex-shipping/v2/shipments", {"destination": {"city": "Braga", "postal_code": "4700-001"}})
    assert st == 201 and v["destination"]["city"] == "Braga" and v["id"].startswith("SHP-") and v["id"] not in ("SHP-88229", "SHP-88230"), v
    st, v, _ = api("POST", "/globex-shipping/v2/shipments", {"destination": {"city": "Braga"}})
    assert st == 422 and v["error"] == "postal_code invalid", (st, v)
    st, v, _ = api("GET", "/globex-shipping/v2/shipments/SHP-12345")
    assert st == 200 and v["id"] == "SHP-12345", v
    st, v, h = api("POST", "/globex-shipping/v2/shipments/SHP-1/labels", {})
    assert st == 200 and v == PDF and h["content-type"] == "application/pdf", (st, h)
    assert SEEN == [], "the provider is no longer called"
    y = call("GET", "/plugins/globex-shipping")["yaml"]
    assert "record:" not in y and "globex.test" not in y and "sk_sandbox" not in y, y

    # mode unknown on an existing service: its routes answer; only the rest goes to the sandbox.
    call("POST", "/plugins/globex-shipping/recording", {"service": "globex-shipping", "upstream": up, "mode": "unknown"})
    st, v, _ = api("GET", "/globex-shipping/v2/shipments/SHP-88229")
    assert st == 200 and SEEN == [], "a route answered"
    st, v, _ = api("GET", "/globex-shipping/v2/rates")
    assert st == 404 and SEEN and SEEN[-1]["path"] == "/v2/rates", "unknown calls go to the sandbox"
    call("POST", "/plugins/globex-shipping/recording/pause", {"service": "globex-shipping"})
    n = len(SEEN)
    st, _, _ = api("GET", "/globex-shipping/v2/rates")
    assert st == 404 and len(SEEN) == n, "paused: nothing goes"

    names = json.dumps(call("GET", "/tools"))
    for t in ["import_spec", "reimport_spec", "start_recording", "recording_status", "pause_recording", "stop_recording", "make_plugin"]:
        assert t in names, t


def main():
    data = tempfile.mkdtemp(prefix="stuntman-spec-")
    servers = [http.server.ThreadingHTTPServer(("127.0.0.1", FILES), Files), http.server.ThreadingHTTPServer(("127.0.0.1", SANDBOX), Sandbox)]
    for s in servers:
        threading.Thread(target=s.serve_forever, daemon=True).start()
    env = dict(os.environ, STUNTMAN_DATA=data, STUNTMAN_PORT=str(HTTP), STUNTMAN_HOOKS_PORT=str(HOOKS), STUNTMAN_SMTP_PORT=str(SMTP), STUNTMAN_OTLP_HTTP_PORT=str(OTLP),
               STUNTMAN_OTLP_GRPC_PORT=str(OTLP_G), STUNTMAN_BLOB_PORT=str(BLOB), STUNTMAN_S3_PORT=str(S3), STUNTMAN_APIS_PORT=str(APIS), STUNTMAN_APIS_TLS_PORT=str(TLS),
               STUNTMAN_KEYVAULT_PORT=str(KV), STUNTMAN_AMQP_PORT=str(AMQP), STUNTMAN_AMQPS_PORT=str(AMQPS), STUNTMAN_WEB="/nonexistent")
    log = data + ".log"
    srv = subprocess.Popen([BIN], env=env, stdout=open(log, "w"), stderr=subprocess.STDOUT)
    try:
        def up():
            try:
                socket.create_connection(("127.0.0.1", HTTP), 0.2).close()
                return True
            except OSError:
                return False
        wait(up, "server")
        openapi()
        record()
        print("e2e spec and recording: all checks passed")
    except Exception:
        print(open(log).read()[-3000:])
        raise
    finally:
        srv.terminate()
        for s in servers:
            s.shutdown()
        shutil.rmtree(data, ignore_errors=True)


if __name__ == "__main__":
    main()
