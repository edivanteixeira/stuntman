"""End-to-end check of sign-in on a shared Stuntman (A1–A7). One Stuntman plays the company's identity
provider (its simulated Entra ID); the other has STUNTMAN_AUTH_* pointed at it. Checks: what needs a
sign-in and what stays open, the screen's OIDC sign-in and the list, the admin key, an environment's
token, the central MCP with OAuth (registration, PKCE, refresh, revoke), environment choice per session
and per person, and that a restart keeps sessions.

    cargo build -p stuntman && python3 tests/e2e_auth.py
"""
import base64, hashlib, http.cookiejar, json, os, re, secrets, sys, urllib.error, urllib.parse, urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from harness import Stuntman

ADMIN = "adm-" + secrets.token_hex(8)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


class Browser:
    """Cookies kept, redirects not followed (each hop is checked)."""

    def __init__(self):
        self.jar = http.cookiejar.CookieJar()
        self.op = urllib.request.build_opener(NoRedirect, urllib.request.HTTPCookieProcessor(self.jar))

    def go(self, method, url, form=None, data=None, headers=None):
        body = urllib.parse.urlencode(form).encode() if form is not None else (None if data is None else json.dumps(data).encode())
        h = {"Accept": "text/html"} if method == "GET" and data is None else {}
        h.update(headers or {})
        if form is not None: h["Content-Type"] = "application/x-www-form-urlencoded"
        elif data is not None: h["Content-Type"] = "application/json"
        r = urllib.request.Request(url, data=body, method=method, headers=h)
        try:
            with self.op.open(r, timeout=30) as x:
                return x.status, x.read().decode(errors="replace"), {k.lower(): v for k, v in x.headers.items()}
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode(errors="replace"), {k.lower(): v for k, v in e.headers.items()}


def raw(method, url, data=None, headers=None, form=None):
    return Browser().go(method, url, form=form, data=data, headers=dict({"Accept": "application/json"}, **(headers or {})))


def sign_in(b, base, user):
    """The screen's sign-in through the simulated Entra; returns the last answer."""
    st, _, h = b.go("GET", f"{base}/auth/login?next=%2Fenvironments")
    assert st in (302, 303) and "/oauth2/v2.0/authorize" in h["location"], (st, h)
    st, html, _ = b.go("GET", h["location"])
    pid = re.search(r'name="pending" value="([^"]+)"', html).group(1)
    idp = h["location"].split("/oauth2/")[0]
    form = {"pending": pid, "do": "pick", "user": user}
    for _ in range(3):
        st, body, h2 = b.go("POST", f"{idp}/oauth2/v2.0/authorize", form=form)
        if st in (302, 303):
            break
        form = {"pending": pid, "do": "consent"}
    assert h2["location"].startswith(f"{base}/auth/callback?"), h2
    return b.go("GET", h2["location"])


def mcp(base, path, body, token=None, sid=None):
    h = {"Accept": "application/json, text/event-stream"}
    if token: h["Authorization"] = f"Bearer {token}"
    if sid: h["Mcp-Session-Id"] = sid
    return raw("POST", f"{base}{path}", data=body, headers=h)


def tool(base, token, sid, name, args=None, path="/mcp"):
    st, t, _ = mcp(base, path, {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": name, "arguments": args or {}}}, token, sid)
    assert st == 200, (st, t)
    r = json.loads(t)["result"]
    return r["content"][0]["text"], r["isError"]


def run():
    idp_s = Stuntman(prefix="stuntman-idp-")
    app_s = Stuntman(prefix="stuntman-auth-")
    with idp_s as idp:
        A = f"http://127.0.0.1:{idp.PORT}"
        idp.call("PUT", "/identity/providers/entra", {"tenant": "contoso-stg", "flavour": "entra-v2"})
        idp.call("PUT", "/identity/entra/users/emma.carter", {"name": "Emma Carter", "email": "emma.carter@acme.example"})
        idp.call("PUT", "/identity/entra/users/ana.souza", {"name": "Ana Souza", "email": "ana.souza@contoso.example.com"})
        B = f"http://127.0.0.1:{app_s.PORT}"
        idp.call("PUT", "/identity/entra/apps/stuntman", {"type": "web", "redirect_uris": [f"{B}/auth/callback"], "any_secret": True})
        idp.call("POST", "/identity/entra/apps/stuntman/admin-consent", {"scopes": ["openid", "email", "profile"]}, expect=None)
        app_s.extra = {"STUNTMAN_AUTH_ISSUER": f"{A}/login/contoso-stg/v2.0", "STUNTMAN_AUTH_CLIENT_ID": "stuntman", "STUNTMAN_AUTH_CLIENT_SECRET": "s3cret",
                       "STUNTMAN_AUTH_ALLOWED_DOMAINS": "acme.example", "STUNTMAN_ADMIN_KEY": ADMIN}
        def up():
            try:
                return raw("GET", f"{B}/api/test-ca.pem")[0] == 200
            except OSError:
                return False
        app_s._up = up
        app_s.start()
        from harness import wait
        wait(app_s._up, "the signed-in Stuntman", 30)
        try:
            check(app_s, B)
        finally:
            app_s.proc.terminate()


def check(s, B):
    adm = {"Authorization": f"Bearer {ADMIN}"}

    # ---- what needs someone, what stays open
    st, _, h = raw("GET", f"{B}/environments", headers={"Accept": "text/html"})
    assert st in (302, 303) and h["location"] == "/auth/login?next=%2Fenvironments", (st, h)
    assert raw("GET", f"{B}/api/env")[0] == 401
    st, _, h = mcp(B, "/mcp", {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
    assert st == 401 and f'resource_metadata="{B}/.well-known/oauth-protected-resource/mcp"' in h["www-authenticate"], (st, h)
    assert raw("GET", f"{B}/api/test-ca.pem")[0] == 200, "the Test CA stays open"
    st, _, _ = raw("POST", f"{B}/sendgrid/v3/mail/send", data={"personalizations": [{"to": [{"email": "liam@acme.example"}]}], "from": {"email": "noreply@acme.example"},
                                                              "subject": "Hello", "content": [{"type": "text/plain", "value": "Hi"}]}, headers={"Authorization": "Bearer SG.any"})
    assert st == 202, ("provider routes stay open", st)
    st, _, _ = raw("GET", f"{B}/api/env", headers={"Authorization": "Bearer nope"})
    assert st == 401

    # ---- the screen's sign-in: on the list, then not
    emma = Browser()
    st, _, h = sign_in(emma, B, "emma.carter")
    assert st in (302, 303) and h["location"] == "/environments", (st, h)
    st, t, _ = emma.go("GET", f"{B}/api/whoami", headers={"Accept": "application/json"})
    w = json.loads(t)
    assert st == 200 and w["who"] == "person:emma.carter@acme.example" and w["auth"] is True, w
    st, _, _ = emma.go("GET", f"{B}/environments")
    assert st not in (301, 302, 303, 401), ("the screen opens with a session (404 here: no web build in tests)", st)
    ana = Browser()
    st, html, _ = sign_in(ana, B, "ana.souza")
    assert st == 403 and "No access" in html and "ana.souza@contoso.example.com" in html, (st, html[:300])
    assert ana.go("GET", f"{B}/api/env", headers={"Accept": "application/json"})[0] == 401

    # ---- the admin key, an environment's token
    st, t, _ = raw("POST", f"{B}/api/environments", data={"name": "b1", "from": "empty"}, headers=adm)
    v = json.loads(t)
    assert st == 200 and v["token"].startswith("stm_env_"), (st, t)
    tok = v["token"]
    assert raw("GET", f"{B}/e/b1/api/env", headers={"Authorization": f"Bearer {tok}"})[0] == 200
    st, t, _ = raw("GET", f"{B}/e/stg-a/api/env", headers={"Authorization": f"Bearer {tok}"})
    assert st == 403 and "for b1" in t, (st, t)
    assert raw("GET", f"{B}/api/env", headers={"Authorization": f"Bearer {tok}"})[0] == 403
    st, t, _ = mcp(B, "/e/b1/mcp", {"jsonrpc": "2.0", "id": 1, "method": "tools/list"}, tok)
    assert st == 200 and "list_inboxes" in t, (st, t[:200])
    st, t, _ = emma.go("POST", f"{B}/api/environments/b1/token", data={}, headers={"Accept": "application/json"})
    tok2 = json.loads(t)["token"]
    assert tok2 != tok and raw("GET", f"{B}/e/b1/api/env", headers={"Authorization": f"Bearer {tok}"})[0] == 401, "the old token stops at once"
    st, t, _ = emma.go("GET", f"{B}/api/environments/b1/token", headers={"Accept": "application/json"})
    assert json.loads(t)["token"] == tok2

    # ---- the central MCP: OAuth with registration and PKCE
    st, t, _ = raw("GET", f"{B}/.well-known/oauth-protected-resource/mcp")
    assert json.loads(t)["resource"] == f"{B}/mcp" and json.loads(t)["authorization_servers"] == [B], t
    meta = json.loads(raw("GET", f"{B}/.well-known/oauth-authorization-server")[1])
    st, t, _ = raw("POST", meta["registration_endpoint"], data={"client_name": "Claude Code", "redirect_uris": ["http://localhost:33418/callback"]})
    client = json.loads(t)["client_id"]
    verifier = base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode()
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    q = urllib.parse.urlencode({"response_type": "code", "client_id": client, "redirect_uri": "http://localhost:33418/callback", "code_challenge": challenge,
                                "code_challenge_method": "S256", "state": "xyz", "resource": f"{B}/mcp"})
    st, _, h = Browser().go("GET", f"{meta['authorization_endpoint']}?{q}")
    assert st in (302, 303) and h["location"].startswith("/auth/login?next="), "no session: the company's sign-in first"
    st, html, _ = emma.go("GET", f"{meta['authorization_endpoint']}?{q}")
    assert st == 200 and "Allow Claude Code?" in html and "emma.carter@acme.example" in html, html[:300]
    form = dict(re.findall(r'name="(client_id|redirect_uri|code_challenge|state|resource)" value="([^"]*)"', html))
    st, _, h = emma.go("POST", f"{B}/oauth/authorize", form=dict(form, decision="allow"))
    code = urllib.parse.parse_qs(urllib.parse.urlparse(h["location"]).query)["code"][0]
    tk = json.loads(raw("POST", meta["token_endpoint"], form={"grant_type": "authorization_code", "code": code, "client_id": client,
                                                               "redirect_uri": "http://localhost:33418/callback", "code_verifier": "wrong"})[1])
    assert tk["error"] == "invalid_grant", tk
    st, _, h = emma.go("POST", f"{B}/oauth/authorize", form=dict(form, decision="allow"))
    code = urllib.parse.parse_qs(urllib.parse.urlparse(h["location"]).query)["code"][0]
    tk = json.loads(raw("POST", meta["token_endpoint"], form={"grant_type": "authorization_code", "code": code, "client_id": client,
                                                               "redirect_uri": "http://localhost:33418/callback", "code_verifier": verifier})[1])
    at, rt = tk["access_token"], tk["refresh_token"]
    assert at.startswith("stm_at_") and tk["expires_in"] == 8 * 3600, tk

    # ---- one address, the environment per session and per person
    st, t, h = mcp(B, "/mcp", {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "claude-code"}}}, at)
    sid = h["mcp-session-id"]
    names = [x["name"] for x in json.loads(mcp(B, "/mcp", {"jsonrpc": "2.0", "id": 1, "method": "tools/list"}, at, sid)[1])["result"]["tools"]]
    assert names[0] == "use_environment", names[:3]
    txt, e = tool(B, at, sid, "list_inboxes")
    assert e and "Pick an environment" in txt and "b1" in txt, txt
    emma.go("PUT", f"{B}/api/users/emma.carter@acme.example/current-environment", data={"environment": "stg-a"}, headers={"Accept": "application/json"})
    st, t, _ = emma.go("PUT", f"{B}/api/users/liam.walker@acme.example/current-environment", data={"environment": "stg-a"}, headers={"Accept": "application/json"})
    assert st == 403, "a person sets only their own"
    sid2 = mcp(B, "/mcp", {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}, at)[2]["mcp-session-id"]
    txt, e = tool(B, at, sid2, "list_inboxes")
    assert not e and txt.startswith("[stg-a] "), txt
    txt, _ = tool(B, at, sid, "use_environment", {"name": "b1"})
    assert txt.startswith("[b1] "), txt
    assert tool(B, at, sid, "list_inboxes")[0].startswith("[b1] ")
    assert tool(B, at, sid, "list_inboxes", {"environment": "stg-a"})[0].startswith("[stg-a] ")
    assert tool(B, at, sid2, "list_inboxes")[0].startswith("[stg-a] "), "the other session keeps the person's environment"
    txt, e = tool(B, at, sid, "create_environment", {"name": "x1"})
    assert e and "admin key" in txt, txt
    txt, _ = tool(B, at, sid, "whoami")
    assert '"agent:emma.carter@acme.example"' in txt, txt

    # ---- refresh, revoke
    t2 = json.loads(raw("POST", meta["token_endpoint"], form={"grant_type": "refresh_token", "refresh_token": rt, "client_id": client})[1])
    at2 = t2["access_token"]
    assert at2 != at and mcp(B, "/mcp", {"jsonrpc": "2.0", "id": 1, "method": "ping"}, at)[0] == 401, "the old access token stops"
    grants = json.loads(emma.go("GET", f"{B}/api/mcp-access", headers={"Accept": "application/json"})[1])["grants"]
    assert grants[0]["client"] == "Claude Code" and grants[0]["email"] == "emma.carter@acme.example", grants
    emma.go("DELETE", f"{B}/api/mcp-access/{grants[0]['id']}", headers={"Accept": "application/json"})
    assert mcp(B, "/mcp", {"jsonrpc": "2.0", "id": 1, "method": "ping"}, at2)[0] == 401, "revoked"
    t3 = json.loads(raw("POST", meta["token_endpoint"], form={"grant_type": "refresh_token", "refresh_token": rt, "client_id": client})[1])
    assert t3["error"] == "invalid_grant", t3

    # ---- an environment that goes: its token and the sessions that used it
    st, _, _ = raw("DELETE", f"{B}/api/environments/b1", headers=adm)
    assert st == 200
    assert raw("GET", f"{B}/e/b1/api/env", headers={"Authorization": f"Bearer {tok2}"})[0] in (401, 404)

    # ---- a restart keeps the session (the signing key is in /data/keys)
    s.restart = None
    s.proc.terminate(); s.proc.wait(10); s.start()
    from harness import wait
    wait(s._up, "restart", 30)
    assert emma.go("GET", f"{B}/api/whoami", headers={"Accept": "application/json"})[0] == 200


if __name__ == "__main__":
    run()
    print("e2e auth: all checks passed")
