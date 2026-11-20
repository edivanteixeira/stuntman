"""End-to-end check of the HTTP APIs service (A1–A5): plugins from the catalog answer like the
providers (SERPRO Datavalid with OAuth2, reCAPTCHA, a partner REST with wait and webhooks), and
everything the screens do works over the HTTP API and MCP.

    cargo build -p stuntman && python3 tests/e2e_apis.py
"""
import base64, hashlib, hmac, http.client, http.server, json, os, shutil, socket, subprocess, tempfile, threading, time, urllib.error, urllib.parse, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
import harness
BIN = harness.BIN


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


HTTP, HOOKS, SMTP, OTLP, OTLP_G, BLOB, S3, APIS, RECV, TLS = (free_port() for _ in range(10))
GOT = []
EMMA, OTHER_FACE, IRREGULAR = "123.456.789-09", "222.222.222-22", "666.666.666-66"
SELFIE = base64.b64encode(b"selfie-emma-1").decode()


class Recv(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        GOT.append({"path": self.path, "headers": dict(self.headers), "body": self.rfile.read(int(self.headers["Content-Length"]))})
        self.send_response(200); self.send_header("Content-Length", "0"); self.end_headers()

    def log_message(self, *a):
        pass


def call(method, path, body=None, expect=200):
    req = urllib.request.Request(f"http://127.0.0.1:{HTTP}/api{urllib.parse.quote(path, safe='/?&=:%')}", method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "X-Stuntman-Client": "qa-suite"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            st, v = r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        st, v = e.code, json.loads(e.read() or b"null")
    assert expect is None or st == expect, (method, path, st, v)
    return v if expect is not None else (st, v)


def api(method, path, body=None, headers=None, form=False, host=None):
    """The app calling the provider through Stuntman's APIs port."""
    c = http.client.HTTPConnection("127.0.0.1", APIS, timeout=30)
    h = {"User-Agent": "acme-api/2.50", **(headers or {})}
    if host:
        h["Host"] = host
    data = b""
    if body is not None:
        if form:
            data = urllib.parse.urlencode(body).encode(); h["Content-Type"] = "application/x-www-form-urlencoded"
        else:
            data = json.dumps(body).encode(); h["Content-Type"] = "application/json"
    c.request(method, path, data, h)
    r = c.getresponse()
    raw = r.read()
    try:
        return r.status, json.loads(raw or b"null"), dict(r.getheaders())
    except ValueError:
        return r.status, raw.decode(errors="replace"), dict(r.getheaders())


def wait(cond, what, t=15):
    end = time.time() + t
    while time.time() < end:
        v = cond()
        if v:
            return v
        time.sleep(0.2)
    raise AssertionError(f"timed out waiting for {what}")


def run(data):
    # ---- Catalog (install every plugin) and validation
    cat = call("GET", "/plugins")["plugins"]
    names = [p["name"] for p in cat]
    assert {"serpro-datavalid", "recaptcha", "turnstile", "gcc-consent", "orders-partner"} <= set(names), names
    for n in names:
        call("POST", "/plugins", {"plugin": n})
        v = call("POST", f"/plugins/{n}/validate", {})
        assert v["ok"], (n, v)
    call("POST", "/plugins", {"plugin": "recaptcha"}, expect=409)
    svcs = {s["name"]: s for s in call("GET", "/apis/services")["services"]}
    assert svcs["serpro-datavalid"]["origin"] == "community" and svcs["serpro-datavalid"]["region"] == "BR" and svcs["serpro-datavalid"]["billable"], svcs["serpro-datavalid"]

    # ---- A5: OAuth2 client credentials (form + Basic, as SERPRO)
    basic = "Basic " + base64.b64encode(b"acme-client:acme-secret").decode()
    st, tok, _ = api("POST", "/serpro-datavalid/token", {"grant_type": "client_credentials"}, {"Authorization": basic}, form=True)
    assert st == 200 and tok["token_type"] == "Bearer" and tok["expires_in"] == 3600, tok
    bearer = {"Authorization": f"Bearer {tok['access_token']}"}

    # ---- A1: answers by the data
    V5 = "/serpro-datavalid/datavalid/v5/pessoa-fisica/validacao"
    ok = {"privacidade": {"rfb": {"id_template": "tpl-1"}, "senatran": {"token": "eyJ.x.y"}}, "cpf": EMMA, "validacao": {"biometria_facial": {"formato": "JPG", "base64": SELFIE, "vivacidade": True}}}
    st, v, _ = api("POST", V5, ok, bearer)
    assert st == 200 and v["biometria_facial"]["similaridade"] == 0.99 and v["biometria_facial"]["vivacidade"] == "real", (st, v)
    st, v, _ = api("POST", V5, {**ok, "cpf": OTHER_FACE}, bearer)
    assert st == 200 and v["biometria_facial"]["similaridade"] == 0.30, v
    st, v, _ = api("POST", V5, {**ok, "cpf": IRREGULAR}, bearer)
    assert v["rfb"]["situacao_cpf"] is False, v
    st, v, _ = api("POST", V5, {"cpf": EMMA, "validacao": ok["validacao"]}, bearer)
    assert st == 422 and v["code"] == "DV207", (st, v)
    st, v, _ = api("POST", V5, {"validacao": ok["validacao"]}, bearer)
    assert st == 400 and "cpf" in v["message"], (st, v)
    st, v, _ = api("POST", V5, ok)
    assert st == 401, (st, v)
    calls = call("GET", "/apis/calls?service=serpro-datavalid")["calls"]
    answers = [c["answer"] for c in reversed(calls)]
    assert answers[:5] == ["token", "approved", "face_mismatch", "cpf_irregular", "template_not_found"], answers
    approved = [c for c in calls if c["answer"] == "approved"][0]
    assert approved["sentence"] == "Validated 123.•••.•••-09 with a selfie: face 0.99, alive", approved["sentence"]
    assert approved["sent"]["cpf"] == EMMA and approved["billable"] and approved["caller"] == "acme-api", approved
    full = call("GET", f"/apis/calls/{approved['id']}?raw=true")
    assert full["request"]["headers"]["authorization"].startswith("Bearer") and [s["step"] for s in full["steps"]] == ["receive", "decide", "respond", "keep"], full["steps"]
    # The data table drives answers: a test adds a suspended CPF
    call("PUT", "/plugins/serpro-datavalid/data/people", {"rows": [{"cpf": "963.741.852-63", "name": "Ruby Lane", "answer": "cpf_irregular"}], "add": True})
    st, v, _ = api("POST", V5, {**ok, "cpf": "963.741.852-63"}, bearer)
    assert v["rfb"]["situacao_cpf"] is False, v

    # ---- Forced answers (A1 "Next calls answer"), billing
    f = call("POST", "/apis/serpro-datavalid/force", {"answer": "face_mismatch", "count": 2})
    assert f["forced"]["left"] == 2 and f["forced"]["by"] == "api:qa-suite", f
    got = [api("POST", V5, ok, bearer)[1]["biometria_facial"]["similaridade"] for _ in range(3)]
    assert got == [0.30, 0.30, 0.99], got
    assert call("GET", "/apis/calls?service=serpro-datavalid&limit=1")["calls"][0]["provoked"] is None
    b = call("GET", "/apis/billing?service=serpro-datavalid")["services"][0]
    assert b["billable"] >= 7 and abs(b["total"] - b["billable"] * 0.2) < 1e-9, b
    st, _ = call("POST", "/apis/serpro-datavalid/force", {"answer": "nope"}, expect=None)
    assert st == 400

    # ---- A5: token expiry, strict client, rate limit, fail a share
    call("POST", "/plugins/serpro-datavalid/tokens/expire")
    st, v, _ = api("POST", V5, ok, bearer)
    assert st == 401 and v["message"] == "Token expired", (st, v)
    with open(os.path.join(data, "environments/stg-a/secrets.yaml"), "a") as fh:
        fh.write("serpro-client-id: acme-client\nserpro-client-secret: acme-secret\n")
    svc = call("GET", "/plugins/serpro-datavalid/service")
    call("PUT", "/plugins/serpro-datavalid/service", {"any_client": False, "version": svc["version"]})
    st, v, _ = api("POST", "/serpro-datavalid/token", {"grant_type": "client_credentials", "client_id": "acme-client", "client_secret": "wrong"}, form=True)
    assert st == 401 and v["error"] == "invalid_client", (st, v)
    st, tok, _ = api("POST", "/serpro-datavalid/token", {"grant_type": "client_credentials"}, {"Authorization": basic}, form=True)
    assert st == 200
    bearer = {"Authorization": f"Bearer {tok['access_token']}"}
    st, _ = call("PUT", "/plugins/serpro-datavalid/service", {"any_client": True, "version": svc["version"]}, expect=None)
    assert st == 409, "a stale version is refused"
    call("PUT", "/plugins/serpro-datavalid/service", {"rate": "2/s", "latency": "none"})
    time.sleep(1.1)
    codes = [api("POST", V5, ok, bearer)[0] for _ in range(4)]
    assert codes.count(429) >= 1 and codes[0] == 200, codes
    time.sleep(1.1)
    call("PUT", "/plugins/serpro-datavalid/service", {"rate": "none", "fail_share": "100%"})
    assert api("POST", V5, ok, bearer)[0] == 503
    call("PUT", "/plugins/serpro-datavalid/service", {"fail_share": 0})

    # ---- Network failures are real (generic answers on one route)
    for gen, code in [("reset", 56), ("empty", 52)]:
        call("POST", "/apis/serpro-datavalid/force", {"answer": gen, "route": "POST /{product}/v5/pessoa-fisica/validacao"})
        r = subprocess.run(["curl", "-s", "-o", "/dev/null", "-X", "POST", "-H", "Content-Type: application/json", "-H", f"Authorization: Bearer {tok['access_token']}", "-d", json.dumps(ok), f"http://127.0.0.1:{APIS}{V5}"], capture_output=True)
        assert r.returncode == code, (gen, r.returncode)
    call("POST", "/apis/serpro-datavalid/force", {"answer": "html", "count": 1})
    st, v, h = api("POST", V5, ok, bearer)
    assert st == 200 and h.get("content-type") == "text/html" and "<html>" in v, (st, h)
    for gen in ("401", "403"):
        call("POST", "/apis/serpro-datavalid/force", {"answer": gen, "count": 1})
        c = http.client.HTTPConnection("127.0.0.1", APIS, timeout=30)
        c.request("POST", V5, json.dumps(ok), {"Content-Type": "application/json", **bearer})
        r = c.getresponse()
        assert r.status == int(gen) and r.read() == b"" and r.getheader("content-length") == "0", (gen, r.status, r.getheaders())
        assert api("POST", V5, ok, bearer)[0] == 200, "count 1: the next call answers again"

    # ---- reCAPTCHA by its real host name, and a token used twice
    st, v, _ = api("POST", "/recaptcha/api/siteverify", {"secret": "x", "response": "tok-1"}, form=True, host="www.google.com")
    assert st == 200 and v["success"] is True and v["score"] == 0.9, (st, v)
    st, v, _ = api("POST", "/recaptcha/api/siteverify", {"secret": "x", "response": "tok-1"}, form=True, host="www.google.com")
    assert v["error-codes"] == ["timeout-or-duplicate"], v
    st, v, _ = api("POST", "/turnstile/turnstile/v0/siteverify", {"secret": "x", "response": "fail-1"})
    assert v["success"] is False, v

    # ---- A partner REST: wait (processing 3 times, then done) and a signed webhook back
    st, v, _ = api("POST", "/orders-partner/api/orders", {"customer": "Globex", "items": [1], "callback_url": f"http://127.0.0.1:{RECV}/hooks/orders"})
    assert st == 201 and v["id"].startswith("ord_"), v
    hook = wait(lambda: GOT, "the partner's webhook")[0]
    sig = hmac.new(b"", hook["body"], hashlib.sha256).hexdigest()
    assert json.loads(hook["body"])["event"] == "order.created" and {k.lower(): v for k, v in hook["headers"].items()}["x-partner-signature"] == sig, hook
    statuses = [api("GET", "/orders-partner/api/orders/ord_9/status")[1]["status"] for _ in range(4)]
    assert statuses == ["processing", "processing", "processing", "done"], statuses
    oc = wait(lambda: [c for c in call("GET", "/apis/calls?service=orders-partner")["calls"] if c["answer"] == "created" and c["sends"]], "the webhook recorded on the call")
    assert oc[0]["sends"][0]["status"] == 200

    # ---- A2: unmatched, the closest route, a route made from the request, ignore
    V4 = "/serpro-datavalid/datavalid/v4/pessoa-fisica/validacao"
    st, v, _ = api("POST", V4, ok, {**bearer, "X-Stuntman-App": "acme-batch"})
    assert st == 404 and v["error"] == "no route", v
    api("POST", "/payments/v1/charges", {"amount": 10}, host="payments.example-pay.com")
    api("GET", "/payments/v1/charges?status=paid&page=2", host="payments.example-pay.com")
    api("GET", "/favicon.ico")
    u = call("GET", "/apis/unmatched?group=path")["groups"]
    v4 = [g for g in u if "v4" in g["key"]][0]
    assert v4["latest"]["closest"]["text"] == "Differs in one segment: v4 here, v5 in the plugin." and v4["latest"]["caller"] == "acme-batch", v4
    one = call("GET", "/apis/unmatched?group=none")
    pay = [c for c in one["calls"] if c["method"] == "POST" and c["path"] == "/payments/v1/charges"]
    assert [c["request"]["query"] for c in one["calls"] if c["method"] == "GET" and c["path"] == "/payments/v1/charges"] == ["status=paid&page=2"], one
    sv = [c for c in one["calls"] if c["service"] == "serpro-datavalid"][0]["request"]
    assert (sv["path"], sv["servicePath"]) == (V4, "/datavalid/v4/pessoa-fisica/validacao"), sv
    assert one["count"] == len(one["calls"]) >= 3 and pay and pay[0]["host"] == "payments.example-pay.com" and '"amount"' in pay[0]["request"]["body"], one
    # One cursor for answered and unmatched calls: oldest first, each with seq and matched.
    t = call("GET", "/apis/traffic")
    seqs = [c["seq"] for c in t["calls"]]
    assert seqs == sorted(seqs) and len(set(seqs)) == len(seqs) and t["next"] == seqs[-1], t
    assert {c["matched"] for c in t["calls"]} == {True, False}, t
    api("GET", "/payments/v1/after-cursor", host="payments.example-pay.com")
    t2 = call("GET", f"/apis/traffic?after={t['next']}")
    assert [(c["path"], c["matched"]) for c in t2["calls"]] == [("/payments/v1/after-cursor", False)] and t2["next"] > t["next"], t2
    by_caller = call("GET", "/apis/unmatched?group=caller")["groups"]
    assert any(g["key"] == "acme-batch" for g in by_caller)
    draft = call("POST", f"/apis/unmatched/{v4['latest']['id']}/route")
    assert draft["service"] == "serpro-datavalid" and draft["route"]["route"] == "POST /datavalid/v4/pessoa-fisica/validacao" and draft["route"]["decide"], draft
    call("PUT", "/plugins/serpro-datavalid/routes", {"method": "POST", "path": "/datavalid/v4/pessoa-fisica/validacao", "steps": draft["route"]})
    st, v, _ = api("POST", V4, ok, bearer)
    assert st == 200 and v["biometria_facial"]["similaridade"] == 0.99, (st, v)
    fav = [g for g in call("GET", "/apis/unmatched")["groups"] if "favicon" in g["key"]][0]
    call("POST", f"/apis/unmatched/{fav['latest']['id']}/ignore")
    assert not any("favicon" in g["key"] for g in call("GET", "/apis/unmatched")["groups"])

    # ---- A3: Try it stores nothing; a broken expression names its line; conflicting edits
    before = len(call("GET", "/apis/calls?service=serpro-datavalid&limit=500")["calls"])
    t = call("POST", "/plugins/serpro-datavalid/try", {"method": "POST", "path": "/{product}/v5/pessoa-fisica/validacao",
              "request": {"path": "/datavalid/v5/pessoa-fisica/validacao", "headers": {"authorization": "Bearer x"}, "body": {**ok, "cpf": OTHER_FACE}}})
    assert t["answer"] == "face_mismatch" and t["status"] == 200 and "respond" in [s["step"] for s in t["steps"]], t
    assert len(call("GET", "/apis/calls?service=serpro-datavalid&limit=500")["calls"]) == before
    p = call("GET", "/plugins/serpro-datavalid")
    bad = p["yaml"].replace("person.answer == 'borderline'", "person.anser == 'borderline'")
    st, e = call("POST", "/plugins/serpro-datavalid/validate", {"yaml": bad}, expect=None)
    assert not e["ok"] and "Did you mean “answer”" in e["errors"][0]["error"] and e["errors"][0]["line"], e
    st, e = call("PUT", "/plugins/serpro-datavalid", {"yaml": bad, "version": p["version"]}, expect=None)
    assert st == 400, (st, e)
    st, e = call("PUT", "/plugins/serpro-datavalid", {"yaml": p["yaml"], "version": "stale"}, expect=None)
    assert st == 409 and "changed elsewhere" in e["error"], e

    # ---- A4: an answer that breaks the schema, one malformed on purpose
    st, e = call("PUT", "/plugins/serpro-datavalid/answers/approved", {"body": '{"rfb_existe": true, "biometria_facial": {"similaridade": "high"}}'}, expect=None)
    assert st == 400 and "“similaridade” must be a number" in e["error"], e
    call("PUT", "/plugins/serpro-datavalid/answers/broken_json", {"status": 200, "body": '{"cpf": {"numero": ', "raw": True, "sentence": "Answered truncated JSON on purpose"})
    call("POST", "/apis/serpro-datavalid/force", {"answer": "broken_json"})
    st, v, _ = api("POST", V5, ok, bearer)
    assert st == 200 and v == '{"cpf": {"numero": ', v
    ans = {a["answer"]["name"]: a for a in call("GET", "/plugins/serpro-datavalid/answers")["answers"]}
    assert ans["serpro_down"]["usedBy"] == [] and ans["approved"]["usedBy"], "serpro_down exists only to be forced"
    cpf = call("GET", "/apis/synthetic-cpf")
    assert len(cpf["cpf"]) == 11

    # ---- Environments: a copy has its own plugins (by prefix), paused answers 503
    call("POST", "/environments", {"name": "pr-9", "from": "stg-a"})
    st, v, _ = api("POST", "/e/pr-9/recaptcha/recaptcha/api/siteverify", {"secret": "x", "response": "tok-1"}, form=True)
    assert v["success"] is True, "pr-9 has its own state: tok-1 was never seen there"
    call("POST", "/environments/pr-9/pause", {"paused": True})
    assert api("POST", "/e/pr-9/recaptcha/recaptcha/api/siteverify", {"secret": "x", "response": "t"}, form=True)[0] == 503

    # ---- A5: real host over HTTPS with the Stuntman Test CA, mTLS, session-cookie portals
    tmp = tempfile.mkdtemp(prefix="stuntman-tls-")
    ca = os.path.join(tmp, "ca.pem")
    with urllib.request.urlopen(f"http://127.0.0.1:{HTTP}/api/keyvault/test-ca.pem") as r:
        open(ca, "wb").write(r.read())
    host = "gateway.apiserpro.serpro.gov.br"
    curl = lambda *a: subprocess.run(["curl", "-s", "--resolve", f"{a[0]}:{TLS}:127.0.0.1", *a[1:]], capture_output=True, text=True)
    r = curl(host, "--cacert", ca, "-u", "acme-client:acme-secret", "-X", "POST", "-d", "grant_type=client_credentials", f"https://{host}:{TLS}/token")
    assert r.returncode == 0 and "access_token" in r.stdout, (r.returncode, r.stdout, r.stderr)
    r = curl(host, "-X", "POST", f"https://{host}:{TLS}/token")
    assert r.returncode == 60, f"without the CA curl must refuse the certificate: {r.returncode}"
    refused = wait(lambda: [c for c in call("GET", "/apis/calls?service=serpro-datavalid")["calls"] if c["answer"] == "tls_refused"], "the refused handshake on Calls")
    assert "does not trust the Stuntman Test CA" in refused[0]["provoked"], refused[0]
    bank = "api.bank-gateway.example.com"
    boleto = ["-H", "Content-Type: application/json", "-d", json.dumps({"valor": "150.00", "pagador": "Globex"})]
    r = curl(bank, "--cacert", ca, *boleto, f"https://{bank}:{TLS}/boletos")
    assert r.returncode != 0, "no client certificate: the handshake fails"
    nocert = wait(lambda: [c for c in call("GET", "/apis/calls?service=bank-gateway")["calls"] if c["answer"] == "tls_refused"], "mTLS refusal on Calls")
    assert "no client certificate" in nocert[0]["provoked"], nocert[0]
    cc = call("POST", "/apis/client-certificates", {"name": "acme-billing"})
    open(os.path.join(tmp, "c.pem"), "w").write(cc["certificate"]); open(os.path.join(tmp, "c.key"), "w").write(cc["key"])
    r = curl(bank, "--cacert", ca, "--cert", os.path.join(tmp, "c.pem"), "--key", os.path.join(tmp, "c.key"), *boleto, f"https://{bank}:{TLS}/boletos")
    assert r.returncode == 0 and "nossoNumero" in r.stdout, (r.returncode, r.stdout, r.stderr)
    ok_call = call("GET", "/apis/calls?service=bank-gateway&answer=registered")["calls"][0]
    assert ok_call["caller"] == "acme-billing" and "by acme-billing" in ok_call["sentence"], ok_call
    subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=stranger", "-days", "1", "-keyout", os.path.join(tmp, "s.key"), "-out", os.path.join(tmp, "s.pem")], capture_output=True)
    r = curl(bank, "--cacert", ca, "--cert", os.path.join(tmp, "s.pem"), "--key", os.path.join(tmp, "s.key"), *boleto, f"https://{bank}:{TLS}/boletos")
    assert r.returncode != 0, "a certificate the Stuntman Test CA did not sign is refused"
    shutil.rmtree(tmp, ignore_errors=True)
    st, v, _ = api("GET", "/partner-portal/api/shipments")
    assert st == 401 and v["message"] == "Not logged in", (st, v)
    c = http.client.HTTPConnection("127.0.0.1", APIS, timeout=10)
    c.request("POST", "/partner-portal/login", urllib.parse.urlencode({"username": "acme-edi", "password": "x"}), {"Content-Type": "application/x-www-form-urlencoded"})
    r = c.getresponse(); r.read()
    cookie = r.getheader("set-cookie").split(";")[0]
    assert r.status == 200 and cookie.startswith("JSESSIONID=ses_") is False and cookie.startswith("JSESSIONID="), cookie
    st, v, _ = api("GET", "/partner-portal/api/shipments/shp_1042", headers={"Cookie": cookie})
    assert st == 200 and v["id"] == "shp_1042" and len(v["updated"]) == 19, (st, v)
    call("POST", "/plugins/partner-portal/tokens/expire")
    st, v, _ = api("GET", "/partner-portal/api/shipments", headers={"Cookie": cookie})
    assert st == 401 and v["message"] == "Session expired", (st, v)

    # ---- Who forced what (A1/A4)
    log = call("GET", "/apis/forced?service=serpro-datavalid")["forced"]
    assert log[0]["answer"] == "broken_json" and any(x["answer"] == "face_mismatch" and x["by"] == "api:qa-suite" for x in log), log[:3]

    # ---- MCP: the same tools by the same names (get_service with service= is the plugin's)
    def mcp(body, sid=None):
        req = urllib.request.Request(f"http://127.0.0.1:{HTTP}/mcp", method="POST", data=json.dumps(body).encode(), headers={"Content-Type": "application/json", **({"Mcp-Session-Id": sid} if sid else {})})
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.headers.get("mcp-session-id"), json.loads(r.read() or b"null")
    sid, _ = mcp({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "claude-code"}}})
    def tool(name, args):
        _, r = mcp({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": name, "arguments": args}}, sid)
        return r["result"]["isError"], json.loads(r["result"]["content"][0]["text"])
    err, v = tool("force_answer", {"service": "serpro-datavalid", "answer": "liveness_fake", "count": 1})
    assert not err and v["forced"]["by"] == "mcp:claude-code", v
    api("POST", V5, ok, bearer)
    err, v = tool("wait_for_api_call", {"service": "serpro-datavalid", "answer": "liveness_fake", "timeout": "5s"})
    assert not err and v["provoked"] == "forced by claude-code", v
    err, v = tool("get_service", {"service": "serpro-datavalid"})
    assert not err and v["service"]["real_host"] == "gateway.apiserpro.serpro.gov.br", v
    err, v = tool("set_data", {"service": "serpro-datavalid", "table": "people", "rows": [{"cpf": "666.666.666-66", "name": "Lucas Hayes", "answer": "cpf_irregular"}]})
    assert not err and len(v["rows"]) == 1


def main():
    data = tempfile.mkdtemp(prefix="duble-api-")
    recv = http.server.ThreadingHTTPServer(("127.0.0.1", RECV), Recv)
    threading.Thread(target=recv.serve_forever, daemon=True).start()
    env = dict(os.environ, STUNTMAN_ENV=os.environ.get("STUNTMAN_ENV", "stg-a"), STUNTMAN_DATA=data, STUNTMAN_PORT=str(HTTP), STUNTMAN_HOOKS_PORT=str(HOOKS), STUNTMAN_SMTP_PORT=str(SMTP), STUNTMAN_OTLP_HTTP_PORT=str(OTLP),
               STUNTMAN_OTLP_GRPC_PORT=str(OTLP_G), STUNTMAN_BLOB_PORT=str(BLOB), STUNTMAN_S3_PORT=str(S3), STUNTMAN_APIS_PORT=str(APIS), STUNTMAN_APIS_TLS_PORT=str(TLS), STUNTMAN_WEB="/nonexistent")
    # Every other port free too: never the defaults a running Stuntman holds.
    env.update({f"STUNTMAN_{k}": str(free_port()) for k in harness.PORTS if f"STUNTMAN_{k}" not in env})
    log = os.path.join(data, "..", os.path.basename(data) + ".log")
    srv = subprocess.Popen([BIN], env=env, stdout=open(log, "w"), stderr=subprocess.STDOUT)
    try:
        def up():
            try:
                socket.create_connection(("127.0.0.1", HTTP), 0.2).close()
                return True
            except OSError:
                return False
        wait(up, "server")
        run(data)
        print("e2e apis: all checks passed")
    except Exception:
        print(open(log).read()[-3000:])
        raise
    finally:
        srv.terminate(); recv.shutdown(); shutil.rmtree(data, ignore_errors=True)


if __name__ == "__main__":
    main()
