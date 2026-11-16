"""End-to-end check of one Stuntman serving many environments (a pipeline makes one per branch):
templates with ${…} values, the public address of identity (issuer, discovery, CORS for an SPA),
real e-mail domains, one SMTP port shared by every environment, connection strings that name the
environment's own domain, the connections list, blob reads on the main port, Key Vault by server
name, deletion and expiry that leave nothing behind, a restart that keeps everything, and the root
of a server with STUNTMAN_PUBLIC_URL.

    cargo build -p stuntman && python3 tests/e2e_central.py
"""
import base64, hashlib, http.client, json, os, re, secrets, shutil, smtplib, socket, ssl, sys, time, urllib.error, urllib.parse, urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from harness import Stuntman, wait

PUBLIC = "https://stuntman.acme.example"
PLUGINS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "plugins")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


OPEN = urllib.request.build_opener(NoRedirect)


def req(method, url, form=None, data=None, headers=None):
    body = urllib.parse.urlencode(form).encode() if form is not None else (None if data is None else json.dumps(data).encode())
    h = dict(headers or {})
    if form is not None: h["Content-Type"] = "application/x-www-form-urlencoded"
    elif data is not None: h["Content-Type"] = "application/json"
    r = urllib.request.Request(url, data=body, method=method, headers=h)
    try:
        with OPEN.open(r, timeout=60) as x:
            return x.status, x.read().decode(errors="replace"), {k.lower(): v for k, v in x.headers.items()}
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace"), {k.lower(): v for k, v in e.headers.items()}


def b64u(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def claims(tok):
    p = tok.split(".")[1]
    return json.loads(base64.urlsafe_b64decode(p + "=" * (-len(p) % 4)))


class Env:
    """The control API of one environment, through /e/<name>/api."""

    def __init__(self, s, name):
        self.s, self.name = s, name
        self.base = f"http://127.0.0.1:{s.PORT}/e/{name}"

    def call(self, method, path, body=None, expect=200):
        st, t, _ = req(method, f"{self.base}/api{path}", data=body, headers={"X-Stuntman-Client": "qa-suite"})
        v = json.loads(t) if t[:1] in "{[" else t
        assert expect is None or st == expect, (self.name, method, path, st, v)
        return v


def smtp_send(port, user, to, subject):
    with smtplib.SMTP("127.0.0.1", port, timeout=15) as c:
        c.ehlo("acme-api")
        if user:
            c.login(user, "any")
        c.sendmail("noreply@acme.example", [to], f"From: noreply@acme.example\r\nTo: {to}\r\nSubject: {subject}\r\n\r\nHello.\r\n")


def make_template(s, stg):
    """templates/acme-preview, made from what the default environment has, with ${…} in it."""
    stg.call("PUT", "/identity/providers/entra", {"tenant": "contoso-stg", "flavour": "entra-v2"})
    stg.call("PUT", "/identity/entra/users/emma.carter", {"name": "Emma Carter", "email": "emma.carter@contoso.example.com"})
    stg.call("PUT", "/identity/entra/apps/acme-backoffice-api", {"type": "api", "app_id_uri": "api://acme-backoffice", "exposes": ["user_impersonation"]})
    stg.call("PUT", "/identity/entra/apps/backoffice", {"type": "spa", "redirect_uris": ["https://bo-${name}.${domain}"],
                                                       "permissions": ["openid", "profile", "api://acme-backoffice/user_impersonation"]})
    stg.call("POST", "/identity/entra/apps/backoffice/admin-consent", {})
    stg.call("PUT", "/identity/entra/apps/portal", {"type": "web", "redirect_uris": ["https://${name}.${domain}/auth/callback"], "any_secret": True})
    src = os.path.join(s.data, "environments", "stg-a")
    t = os.path.join(s.data, "templates", "acme-preview")
    shutil.copytree(os.path.join(src, "identity"), os.path.join(t, "identity"))
    shutil.copytree(os.path.join(src, "email"), os.path.join(t, "email"))
    if os.path.exists(os.path.join(src, "secrets.yaml")):
        shutil.copy(os.path.join(src, "secrets.yaml"), os.path.join(t, "secrets.yaml"))
    with open(os.path.join(t, "environment.yaml"), "w") as f:
        f.write("name: template\nbase_domain: ${name}.localhost\ncallbacks:\n  default: https://${name}.${domain}/hooks\n")


def authorize_code(env, client, redirect, scope):
    auth = f"{env.base}/login/contoso-stg"
    verifier = b64u(secrets.token_bytes(32))
    q = {"client_id": client, "redirect_uri": redirect, "response_type": "code", "scope": scope, "state": "s1", "nonce": "n1",
         "code_challenge": b64u(hashlib.sha256(verifier.encode()).digest()), "code_challenge_method": "S256", "response_mode": "query"}
    st, html, _ = req("GET", f"{auth}/oauth2/v2.0/authorize?{urllib.parse.urlencode(q)}")
    pid = re.search(r'name="pending" value="([^"]+)"', html).group(1)
    form = {"pending": pid, "do": "pick", "user": "emma.carter"}
    for _ in range(3):
        st, body, h = req("POST", f"{auth}/oauth2/v2.0/authorize", form=form)
        if st in (302, 303):
            code = urllib.parse.parse_qs(urllib.parse.urlparse(h["location"]).query)["code"][0]
            return code, verifier
        assert "wants to:" in body, body[:400]
        form = {"pending": pid, "do": "consent"}
    raise AssertionError("no code")


def run(s):
    stg = Env(s, "stg-a")
    make_template(s, stg)

    # ---- 2.1 templates and their values
    t = stg.call("GET", "/environments/templates")["templates"]
    assert [x["name"] for x in t] == ["acme-preview"], t
    vs = {v["var"]: v for v in t[0]["vars"]}
    assert set(vs) == {"name", "domain"} and vs["name"]["auto"] and "identity/entra.yaml" in vs["domain"]["files"], vs
    v = stg.call("POST", "/environments", {"name": "a", "from": "acme-preview"}, expect=400)
    assert "${domain}" in v["error"] and v["detail"]["missing"][0]["var"] == "domain", v
    assert not os.path.exists(os.path.join(s.data, "environments", "a")), "nothing is made when a value is missing"
    for n in ["a", "b"]:
        v = stg.call("POST", "/environments", {"name": n, "from": "acme-preview", "vars": {"domain": "preview.acme.example"}, "expires": "72h"})
        assert v["baseDomain"] == f"{n}.localhost" and v["clonedFrom"] == "templates/acme-preview", v
        assert v["callbacks"]["default"] == f"https://{n}.preview.acme.example/hooks", v
        assert v["addresses"]["apps"] == f"<service>.{n}.localhost", v
    ident = open(os.path.join(s.data, "environments", "a", "identity", "entra.yaml")).read()
    assert "https://bo-a.preview.acme.example" in ident and "${" not in ident, ident[:600]
    long = "x" * 24
    stg.call("POST", "/environments", {"name": long, "from": "empty"})
    stg.call("DELETE", f"/environments/{long}")
    a, b = Env(s, "a"), Env(s, "b")

    # ---- 2.2 SMTP: one port, the login names the environment
    port = s.SMTP_PORT
    svc = a.call("GET", "/services")["services"][0]
    assert svc["smtpUser"] == "a" and set(svc["smtpSharedWith"]) == {"a", "b", "stg-a"}, svc
    try:
        smtp_send(port, None, "emma@acme.example", "no login")
        raise AssertionError("a send without a login went through")
    except smtplib.SMTPException as e:
        assert "530" in str(e) or "Log in" in str(e), e
    try:
        smtp_send(port, "zz", "emma@acme.example", "wrong env")
        raise AssertionError("an unknown environment logged in")
    except smtplib.SMTPAuthenticationError as e:
        assert e.smtp_code == 535 and b"a, b, stg-a" in e.smtp_error, e
    smtp_send(port, "a", "emma@acme.example", "for a")
    smtp_send(port, "noreply@b.localhost", "liam@acme.example", "for b")  # the sender address at b's base domain picks b
    wait(lambda: a.call("GET", "/messages")["messages"], "the message in a")
    ma = [m["subject"] for m in a.call("GET", "/messages")["messages"]]
    mb = [m["subject"] for m in b.call("GET", "/messages")["messages"]]
    assert ma == ["for a"] and mb == ["for b"], (ma, mb)

    # ---- 2.2 connection strings name the environment's own domain; :8028, blob and Key Vault by environment
    a.call("POST", "/services/azure-service-bus/accounts", {"account": "acme-bus"})
    ns = [n for n in a.call("GET", "/messaging/namespaces")["namespaces"] if n["namespace"] == "acme-bus"][0]
    assert "Endpoint=sb://acme-bus.a.localhost:" in ns["connection"], ns["connection"]
    v = a.call("POST", "/webhooks/inboxes", {"name": "orders", "keep": True})
    assert v["url"] == f"http://a.localhost:{s.HOOKS_PORT}/in/orders", v["url"]
    a.call("PUT", "/plugins/viacep", {"yaml": open(os.path.join(PLUGINS, "viacep.yaml")).read()})
    st, v, _ = s.api("GET", "/e/a/viacep/ws/01310100/json/")
    assert st == 200 and v["cep"] == "01310-100", (st, v)
    st, v, _ = s.api("GET", "/e/b/viacep/ws/01310100/json/")
    assert st != 200 or v.get("cep") != "01310-100", "b has no viacep"
    a.call("POST", "/services/azure-blob/accounts", {"account": "acmedev", "containers": {"exports": {"access": "private"}}})
    c = http.client.HTTPConnection("127.0.0.1", s.BLOB_PORT, timeout=10)
    c.request("PUT", "/e/a/acmedev/exports/report.csv", b"id,total\n1,120.00\n", {"x-ms-blob-type": "BlockBlob", "Authorization": "SharedKey acmedev:x"})
    assert c.getresponse().status == 201
    link = a.call("POST", "/storage/acmedev/exports/links/report.csv", {"allows": ["read"], "valid_for": "1h"})
    pub = link["publicUrl"]
    assert pub.startswith(f"http://127.0.0.1:{s.PORT}/e/a/blob/acmedev/exports/report.csv?"), pub
    st, body, _ = req("GET", pub)
    assert st == 200 and body.startswith("id,total"), (st, body[:200])
    st, _, _ = req("GET", pub.replace("/e/a/blob/", "/e/b/blob/"))
    assert st in (400, 404), ("b has no acmedev", st)
    a.call("PUT", "/keyvault/vaults/acme-kv", {"any_token": True})
    a.call("PUT", "/keyvault/secrets/smtp-password", {"value": "synthetic-a", "vault": "acme-kv"})
    b.call("PUT", "/keyvault/vaults/acme-kv", {"any_token": True})
    b.call("PUT", "/keyvault/secrets/smtp-password", {"value": "synthetic-b", "vault": "acme-kv"})
    ca = req("GET", f"http://127.0.0.1:{s.PORT}/api/test-ca.pem")[1]
    for n, want in [("a", "synthetic-a"), ("b", "synthetic-b")]:
        host = f"acme-kv.vault.{n}.localhost"
        ctx = ssl.create_default_context(cadata=ca)
        sock = ctx.wrap_socket(socket.create_connection(("127.0.0.1", s.KEYVAULT_PORT), 10), server_hostname=host)
        kc = http.client.HTTPSConnection(host, s.KEYVAULT_PORT, context=ctx); kc.sock = sock
        kc.request("GET", "/secrets/smtp-password?api-version=7.4", headers={"Authorization": "Bearer t", "User-Agent": "acme-api/1.0"})
        r = kc.getresponse(); v = json.loads(r.read())
        assert r.status == 200 and v["value"] == want, (n, r.status, v)

    # ---- connections asked through another environment still name this one's address
    v = stg.call("GET", "/environments/a/connections")
    auth = [i["value"] for i in v["items"] if i["key"] == "IDENTITY_AUTHORITY"][0]
    assert auth == f"http://127.0.0.1:{s.PORT}/e/a/login/contoso-stg/v2.0", auth

    # ---- 1.1 the address identity names: the one the call came by, else public_url
    disc_url = f"{a.base}/login/contoso-stg/v2.0/.well-known/openid-configuration"
    st, t, h = req("GET", disc_url, headers={"X-Forwarded-Host": "stuntman.acme.example", "X-Forwarded-Proto": "https"})
    d = json.loads(t)
    assert d["issuer"] == f"{PUBLIC}/e/a/login/contoso-stg/v2.0" and d["token_endpoint"] == f"{PUBLIC}/e/a/login/contoso-stg/oauth2/v2.0/token", d
    assert h.get("access-control-allow-origin") == "*", h
    st, t, _ = req("GET", disc_url)
    assert json.loads(t)["issuer"] == f"http://127.0.0.1:{s.PORT}/e/a/login/contoso-stg/v2.0", t
    v = stg.call("PUT", "/environments/a", {"public_url": f"{PUBLIC}/e/a/"})
    assert v["publicUrl"] == f"{PUBLIC}/e/a", v
    assert stg.call("PUT", "/environments/a", {"public_url": "stuntman.acme.example"}, expect=400)
    st, t, _ = req("GET", disc_url)
    assert json.loads(t)["issuer"] == f"{PUBLIC}/e/a/login/contoso-stg/v2.0", "public_url wins whatever address a call came by"

    # ---- 1.2 CORS: preflight, and the token only for an spa from its redirect origin
    st, _, h = req("OPTIONS", f"{a.base}/login/contoso-stg/oauth2/v2.0/token", headers={"Origin": "https://bo-a.preview.acme.example", "Access-Control-Request-Method": "POST", "Access-Control-Request-Headers": "content-type"})
    assert st == 204 and h["access-control-allow-origin"] == "https://bo-a.preview.acme.example" and "POST" in h["access-control-allow-methods"], (st, h)

    # ---- 1.4 the spa with PKCE and an api scope: aud and scp of the api, iss of public_url
    scope = "openid profile api://acme-backoffice/user_impersonation"
    code, verifier = authorize_code(a, "backoffice", "https://bo-a.preview.acme.example", scope)
    form = {"client_id": "backoffice", "grant_type": "authorization_code", "code": code, "redirect_uri": "https://bo-a.preview.acme.example", "code_verifier": verifier, "scope": scope}
    st, t, h = req("POST", f"{a.base}/login/contoso-stg/oauth2/v2.0/token", form=form, headers={"Origin": "https://bo-a.preview.acme.example"})
    assert st == 200, (st, t)
    assert h["access-control-allow-origin"] == "https://bo-a.preview.acme.example", h
    at = claims(json.loads(t)["access_token"])
    assert at["aud"] == "api://acme-backoffice" and at["scp"] == "user_impersonation" and at["iss"] == f"{PUBLIC}/e/a/login/contoso-stg/v2.0", at
    code, verifier = authorize_code(a, "backoffice", "https://bo-a.preview.acme.example", scope)
    form.update(code=code, code_verifier=verifier)
    st, t, _ = req("POST", f"{a.base}/login/contoso-stg/oauth2/v2.0/token", form=form, headers={"Origin": "https://evil.example"})
    assert st == 400 and "AADSTS9002326" in t, (st, t)
    st, t, _ = req("POST", f"{a.base}/login/contoso-stg/oauth2/v2.0/token", form={"client_id": "portal", "grant_type": "authorization_code", "code": "x", "client_secret": "s"},
                   headers={"Origin": "https://a.preview.acme.example"})
    assert st == 400 and "Single-Page Application" in t, (st, t)

    # ---- 1.3 real e-mail domains, per environment
    a.call("PUT", "/identity/entra/users/ana", {"name": "Ana Souza", "email": "ana@contoso.com"}, expect=400)
    stg.call("PUT", "/environments/a", {"allowed_email_domains": ["contoso.com"]})
    a.call("PUT", "/identity/entra/users/ana", {"name": "Ana Souza", "email": "ana@contoso.com"})
    b.call("PUT", "/identity/entra/users/ana", {"name": "Ana Souza", "email": "ana@contoso.com"}, expect=400)

    # ---- 2.4 connections
    v = a.call("GET", "/environments/a/connections")
    items = {(i["service"], i["key"]): i["value"] for i in v["items"]}
    assert items[("E-mail", "SMTP_USER")] == "a" and items[("E-mail", "SMTP_HOST")] == "a.localhost", items
    assert items[("Identity", "IDENTITY_AUTHORITY")] == f"{PUBLIC}/e/a/login/contoso-stg/v2.0", items
    assert "acmedev.blob.a.localhost" in items[("Storage", "STORAGE_CONNECTION")] or "a.localhost" in items[("Storage", "STORAGE_CONNECTION")], items
    assert "SMTP_USER=a\n" in v["env"] and "OTEL_EXPORTER_OTLP_ENDPOINT=" in v["env"], v["env"]
    v = a.call("GET", "/environments/a/connections?base=http://stuntman.stuntman.svc")
    items = {(i["service"], i["key"]): i["value"] for i in v["items"]}
    assert items[("Telemetry", "OTEL_EXPORTER_OTLP_ENDPOINT")] == f"http://stuntman.stuntman.svc:{s.OTLP_HTTP_PORT}/e/a", items
    assert "Endpoint=sb://acme-bus.a.localhost" in items[("Queues", "QUEUES_CONNECTION")], "Service Bus keeps the host name"

    # ---- 2.6 a paused environment does not stop the others
    stg.call("POST", "/environments/b/pause", {"paused": True})
    smtp_send(port, "a", "emma@acme.example", "a while b is paused")
    try:
        smtp_send(port, "b", "liam@acme.example", "b paused")
        raise AssertionError("a paused environment took a message")
    except (smtplib.SMTPException, OSError):
        pass
    stg.call("POST", "/environments/b/pause", {"paused": False})

    # ---- 2.6 a restart keeps every environment and its state
    s.restart()
    assert {e["name"] for e in stg.call("GET", "/environments")["environments"]} >= {"stg-a", "a", "b"}
    assert [m["subject"] for m in b.call("GET", "/messages")["messages"]] == ["for b"]
    assert stg.call("GET", "/environments/a")["publicUrl"] == f"{PUBLIC}/e/a"
    smtp_send(port, "b", "liam@acme.example", "after restart")

    # ---- 2.5 delete: everything goes, the shared port keeps serving the others; the name is free again
    folder = os.path.join(s.data, "environments", "a")
    stg.call("DELETE", "/environments/a")
    stg.call("GET", "/environments/a", expect=404)
    assert not os.path.exists(folder), os.listdir(os.path.dirname(folder))
    assert req("GET", disc_url)[0] == 404
    smtp_send(port, "b", "liam@acme.example", "after a went")
    try:
        smtp_send(port, "a", "emma@acme.example", "to a deleted environment")
        raise AssertionError("a deleted environment took a message")
    except smtplib.SMTPAuthenticationError as e:
        assert e.smtp_code == 535
    stg.call("POST", "/environments", {"name": "a", "from": "acme-preview", "vars": {"domain": "preview.acme.example"}})
    assert a.call("GET", "/messages")["messages"] == [] and a.call("GET", "/messaging/namespaces")["namespaces"] == [], "a new a starts clean"
    assert stg.call("GET", "/environments/a")["publicUrl"] == "", "nothing of the old a is left"

    # ---- 2.5 expiry deletes the same way (the sweep runs every 30 s)
    stg.call("POST", "/environments", {"name": "gone-soon", "from": "acme-preview", "vars": {"domain": "x.example"}, "expires": "1s"})
    wait(lambda: not os.path.exists(os.path.join(s.data, "environments", "gone-soon")), "the expired environment to go", 45)
    stg.call("GET", "/environments/gone-soon", expect=404)


def shared():
    """STUNTMAN_PUBLIC_URL: the root says it is shared and every environment's issuer is <url>/e/<name>."""
    with Stuntman(env={"STUNTMAN_PUBLIC_URL": PUBLIC + "/"}) as s:
        e = s.call("GET", "/env")
        assert e["shared"] is True and e["publicUrl"] == f"{PUBLIC}/e/stg-a", e
        s.call("POST", "/environments", {"name": "c", "from": "empty"})
        c = Env(s, "c")
        c.call("PUT", "/identity/providers/entra", {"tenant": "contoso-stg"})
        st, t, _ = req("GET", f"{c.base}/login/contoso-stg/v2.0/.well-known/openid-configuration")
        assert json.loads(t)["issuer"] == f"{PUBLIC}/e/c/login/contoso-stg/v2.0", t
        assert c.call("GET", "/env")["publicUrl"] == f"{PUBLIC}/e/c"


if __name__ == "__main__":
    with Stuntman() as s:
        run(s)
    shared()
    print("e2e central: all checks passed")
