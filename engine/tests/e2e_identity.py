"""End-to-end check of Identity / SSO (I1–I5): an app signs people in with OIDC authorization code +
PKCE against Stuntman (login page, MFA, consent, form answers), redeems the code, refreshes, and gets
the provider's errors (wrong tenant, disabled, redirect mismatch, wrong secret, code reuse); tokens are
verified RS256 against the JWKS; sign_in_as, provocations, key rotation and client credentials.

    cargo build -p stuntman && python3 tests/e2e_identity.py
"""
import base64, hashlib, http.cookiejar, json, os, re, secrets, shutil, socket, subprocess, tempfile, time, urllib.error, urllib.parse, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "stuntman")


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


HTTP, HOOKS, SMTP, OTLP, OTLP_G, BLOB, S3, APIS, TLS, KV = (free_port() for _ in range(10))
CB = "http://127.0.0.1:5999/signin-oidc"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


OPEN = urllib.request.build_opener(NoRedirect)


def req(method, url, data=None, headers=None, form=None):
    body = urllib.parse.urlencode(form).encode() if form is not None else (None if data is None else json.dumps(data).encode())
    h = dict(headers or {})
    if form is not None: h["Content-Type"] = "application/x-www-form-urlencoded"
    elif data is not None: h["Content-Type"] = "application/json"
    r = urllib.request.Request(url, data=body, method=method, headers=h)
    try:
        with OPEN.open(r, timeout=60) as x:
            return x.status, x.read().decode(), {k.lower(): v for k, v in x.headers.items()}
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(), {k.lower(): v for k, v in e.headers.items()}


def call(method, path, body=None, expect=200):
    st, t, _ = req(method, f"http://127.0.0.1:{HTTP}/api{path}", body)
    assert expect is None or st == expect, (method, path, st, t[:400])
    return json.loads(t or "null")


def tool(name, args):
    body = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}
    st, t, _ = req("POST", f"http://127.0.0.1:{HTTP}/mcp", body, {"Accept": "application/json, text/event-stream"})
    res = json.loads(t)["result"]
    return res.get("isError", False), json.loads(res["content"][0]["text"])


def b64u(b): return base64.urlsafe_b64encode(b).rstrip(b"=").decode()
def unb64u(s): return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


def verify_rs256(tok, jwks):
    h, p, s = tok.split(".")
    head = json.loads(unb64u(h))
    key = next((k for k in jwks["keys"] if k["kid"] == head["kid"]), None)
    if key is None: return False
    n, e = int.from_bytes(unb64u(key["n"]), "big"), int.from_bytes(unb64u(key["e"]), "big")
    m = pow(int.from_bytes(unb64u(s), "big"), e, n).to_bytes((n.bit_length() + 7) // 8, "big")
    digest_info = bytes.fromhex("3031300d060960864801650304020105000420") + hashlib.sha256(f"{h}.{p}".encode()).digest()
    return m.endswith(digest_info) and m.startswith(b"\x00\x01")


def claims(tok): return json.loads(unb64u(tok.split(".")[1]))


AUTH = None


def authorize(client="acme-app", redirect=CB, scope="openid profile email offline_access", extra=None):
    verifier = b64u(secrets.token_bytes(32))
    q = {"client_id": client, "redirect_uri": redirect, "response_type": "code", "scope": scope, "state": "st-123", "nonce": "n-456",
         "code_challenge": b64u(hashlib.sha256(verifier.encode()).digest()), "code_challenge_method": "S256", "response_mode": "query"}
    q.update(extra or {})
    st, html, _ = req("GET", f"{AUTH}/oauth2/v2.0/authorize?{urllib.parse.urlencode(q)}")
    m = re.search(r'name="pending" value="([^"]+)"', html)
    return st, html, (m.group(1) if m else None), verifier


def post(pending, **form):
    form["pending"] = pending
    return req("POST", f"{AUTH}/oauth2/v2.0/authorize", form=form)


def code_of(location):
    q = urllib.parse.parse_qs(urllib.parse.urlparse(location).query)
    return q.get("code", [None])[0], q


def token(form, secret=None):
    f = dict(form); f.setdefault("client_id", "acme-app")
    if secret is not None: f["client_secret"] = secret
    st, t, _ = req("POST", f"{AUTH}/oauth2/v2.0/token", form=f)
    return st, json.loads(t)


def run():
    global AUTH
    v = call("PUT", "/identity/providers/entra", {"tenant": "contoso-stg", "flavour": "entra-v2", "other_tenants": ["fabrikam-stg"]})
    AUTH = v["authority"].rsplit("/v2.0", 1)[0]
    assert AUTH.endswith("/login/contoso-stg"), AUTH
    users = [("emma.carter", "Emma Carter", "emma.carter@contoso.example.com", {"groups": ["Finance", "Signers"], "roles": ["Approver"], "mfa": True, "extra": {"employeeId": "E-10442"}}),
             ("liam.walker", "Liam Walker", "liam.walker@fabrikam.example.com", {"tenant": "fabrikam-stg"}),
             ("noah.brooks", "Noah Brooks", "noah.brooks@contoso.example.com", {"groups": ["Signers"]}),
             ("jack.reed", "Jack Reed", "jack.reed@contoso.example.com", {"enabled": False})]
    for uid, name, email, more in users:
        call("PUT", f"/identity/entra/users/{uid}", dict({"name": name, "email": email}, **more))
    err, v = tool("set_user", {"provider": "entra", "user": "someone", "email": "someone@gmail.com"})
    assert err, v  # real-looking domains refused
    app = call("PUT", "/identity/entra/apps/acme-app", {"name": "acme-app", "type": "web", "redirect_uris": [CB], "optional_claims": ["groups"]})["app"]
    assert app["secret"].startswith("secret://"), app
    secret = open(os.path.join(DATA, "environments", "stg-a", "secrets.yaml")).read().split("acme-app-client-secret:")[1].split("\n")[0].strip().strip("'\"")
    disc = json.loads(req("GET", f"{AUTH}/v2.0/.well-known/openid-configuration")[1])
    assert disc["issuer"] == f"{AUTH}/v2.0" and disc["token_endpoint"].endswith("/oauth2/v2.0/token"), disc
    jwks = json.loads(req("GET", disc["jwks_uri"])[1])
    assert len(jwks["keys"]) == 2 and jwks["keys"][0]["kty"] == "RSA", jwks

    # Emma: MFA, then consent, then a code back at the redirect URI with the state.
    st, html, pid, verifier = authorize()
    assert st == 200 and "Pick who is signing in" in html and "Emma Carter" in html, (st, html[:300])
    st, html, _ = post(pid, do="pick", user="emma.carter")
    assert "Enter the code from your authenticator" in html, html[:300]
    st, html, _ = post(pid, do="mfa", code="000000")
    assert "didn't work (1 of 3)" in html, html[:400]
    st, html, _ = post(pid, do="mfa", code="482913")
    assert "wants to:" in html and "Keep access" in html, html[:400]
    st, body, h = post(pid, do="consent")
    assert st in (302, 303) and h.get("location", "").startswith(CB), (st, h, body[:600])
    code, q = code_of(h["location"])
    assert code and q["state"] == ["st-123"], q
    st, t = token({"grant_type": "authorization_code", "code": code, "redirect_uri": CB, "code_verifier": "wrong"}, secret)
    assert st == 400 and "AADSTS501481" in t["error_description"], t
    st, t = token({"grant_type": "authorization_code", "code": code, "redirect_uri": CB, "code_verifier": verifier}, "not-the-secret")
    assert st == 401 and "AADSTS7000215" in t["error_description"], t
    st, t = token({"grant_type": "authorization_code", "code": code, "redirect_uri": CB, "code_verifier": verifier}, secret)
    assert st == 200 and t["id_token"] and t["access_token"] and t["refresh_token"], t
    assert verify_rs256(t["id_token"], jwks) and verify_rs256(t["access_token"], jwks)
    c = claims(t["id_token"])
    assert c["aud"] == "acme-app" and c["iss"] == disc["issuer"] and c["nonce"] == "n-456" and c["amr"] == ["pwd", "mfa"], c
    assert c["groups"] == ["Finance", "Signers"] and c["roles"] == ["Approver"] and c["employeeId"] == "E-10442" and c["exp"] > time.time(), c
    st, t2 = token({"grant_type": "authorization_code", "code": code, "redirect_uri": CB, "code_verifier": verifier}, secret)
    assert st == 400 and "AADSTS54005" in t2["error_description"], t2
    st, r = token({"grant_type": "refresh_token", "refresh_token": t["refresh_token"]}, secret)
    assert st == 200 and r["refresh_token"] != t["refresh_token"], r
    st, r2 = token({"grant_type": "refresh_token", "refresh_token": t["refresh_token"]}, secret)
    assert st == 400 and "AADSTS70043" in r2["error_description"], r2

    # Second sign-in: consent remembered, MFA still asked.
    st, html, pid, verifier = authorize()
    st, html, _ = post(pid, do="pick", user="emma.carter")
    st, _, h = post(pid, do="mfa", code="123456")
    assert st in (302, 303) and code_of(h["location"])[0], (st, h)

    # Errors.
    st, html, pid, _ = authorize()
    st, html, _ = post(pid, do="pick", user="liam.walker")
    assert st == 400 and "AADSTS50020" in html and "Back to the app" in html, html[:400]
    st, html, pid, _ = authorize()
    st, html, _ = post(pid, do="pick", user="jack.reed")
    assert st == 400 and "AADSTS50057" in html, html[:400]
    st, html, _, _ = authorize(redirect="http://localhost:3000/signin")
    assert st == 400 and "AADSTS50011" in html, html[:300]
    st, html, pid, _ = authorize()
    st, _, h = post(pid, do="pick", user="noah.brooks")
    st, _, h = post(pid, do="cancel")
    assert "error=access_denied" in h["location"] and "state=st-123" in h["location"], h

    # form_post answers.
    st, html, pid, verifier = authorize(extra={"response_mode": "form_post"})
    post(pid, do="pick", user="noah.brooks")
    st, html, _ = post(pid, do="consent")
    assert st == 200 and f'action="{CB}"' in html and 'name="code"' in html and 'name="state" value="st-123"' in html, html[:500]

    # Agents: sign_in_as through MCP; the callback carries a code.
    authorize()
    err, v = tool("sign_in_as", {"provider": "entra", "user": "Noah Brooks"})
    assert not err and v["callback"].startswith(CB) and "code=" in v["callback"], v
    err, v = tool("pending_logins", {"provider": "entra"})
    assert not err and v["pending"] == [], v

    # Next sign-ins: tokens already expired; bad signature.
    call("POST", "/identity/entra/provoke", {"answer": "expired", "count": 1})
    st, html, pid, verifier = authorize()
    st, _, h = post(pid, do="pick", user="noah.brooks")
    code, _ = code_of(h["location"])
    st, t = token({"grant_type": "authorization_code", "code": code, "redirect_uri": CB, "code_verifier": verifier}, secret)
    assert claims(t["id_token"])["exp"] < time.time(), claims(t["id_token"])
    call("POST", "/identity/entra/provoke", {"answer": "bad_signature", "count": 1})
    st, html, pid, verifier = authorize()
    st, _, h = post(pid, do="pick", user="noah.brooks")
    code, _ = code_of(h["location"])
    st, t = token({"grant_type": "authorization_code", "code": code, "redirect_uri": CB, "code_verifier": verifier}, secret)
    assert not verify_rs256(t["id_token"], jwks), "bad signature should not verify"
    st, html, pid, verifier = authorize()  # count used up: normal again
    st, _, h = post(pid, do="pick", user="noah.brooks")
    assert code_of(h["location"])[0]

    # Groups overage, revoked consent, disabled user's refresh.
    call("PUT", "/identity/entra/users/emma.carter", {"groups": 210, "mfa": False})
    err, v = tool("sign_in_as", {"provider": "entra", "user": "emma.carter", "app": "acme-app"})
    st, t = token({"grant_type": "authorization_code", "code": code_of(v["callback"])[0], "redirect_uri": CB}, secret)
    c = claims(t["id_token"])
    assert "groups" not in c and c["_claim_names"]["groups"] == "src1", c
    st, g, _ = req("GET", c["_claim_sources"]["src1"]["endpoint"].replace("getMemberObjects", "memberOf"), headers={"Authorization": f"Bearer {t['access_token']}"})
    assert st == 200 and len(json.loads(g)["value"]) == 210, (st, g[:200])
    call("DELETE", "/identity/entra/users/emma.carter/consents/acme-app")
    st, r = token({"grant_type": "refresh_token", "refresh_token": t["refresh_token"]}, secret)
    assert st == 400, r

    # Key rotation: new tokens use the next kid.
    old = jwks["keys"][0]["kid"]
    rot = call("POST", "/identity/entra/keys/rotate")
    assert rot["was"] == old and rot["current"] != old, rot
    err, v = tool("sign_in_as", {"provider": "entra", "user": "noah.brooks", "app": "acme-app"})
    st, t = token({"grant_type": "authorization_code", "code": code_of(v["callback"])[0], "redirect_uri": CB}, secret)
    assert json.loads(unb64u(t["id_token"].split(".")[0]))["kid"] == rot["current"]
    assert verify_rs256(t["id_token"], json.loads(req("GET", disc["jwks_uri"])[1]))
    assert verify_rs256(t["id_token"], jwks)  # the cached JWKS already had it as "next"
    call("POST", "/identity/entra/keys/rotate")  # one more rotation: a JWKS cached forever now fails
    err, v = tool("sign_in_as", {"provider": "entra", "user": "noah.brooks", "app": "acme-app"})
    st, t = token({"grant_type": "authorization_code", "code": code_of(v["callback"])[0], "redirect_uri": CB}, secret)
    assert not verify_rs256(t["id_token"], jwks) and verify_rs256(t["id_token"], json.loads(req("GET", disc["jwks_uri"])[1]))

    # A rotation and a provocation made at the same moment both stay (no lost update).
    import threading
    th = [threading.Thread(target=lambda: call("POST", "/identity/entra/keys/rotate")), threading.Thread(target=lambda: call("POST", "/identity/entra/provoke-idp", {"part": "token", "answer": "slow"}))]
    [x.start() for x in th]; [x.join() for x in th]
    assert call("GET", "/identity/entra/tenant")["provoke"]["token"] == "slow"
    call("POST", "/identity/entra/provoke-idp", {"part": "token", "answer": "normal"})

    # The provider fails.
    call("POST", "/identity/entra/provoke-idp", {"part": "discovery", "answer": "500"})
    assert req("GET", f"{AUTH}/v2.0/.well-known/openid-configuration")[0] == 500
    call("POST", "/identity/entra/provoke-idp", {"part": "discovery", "answer": "normal"})
    call("POST", "/identity/entra/provoke-idp", {"part": "jwks", "answer": "kid_missing"})
    assert len(json.loads(req("GET", disc["jwks_uri"])[1])["keys"]) == 1
    call("POST", "/identity/entra/provoke-idp", {"part": "jwks", "answer": "normal"})

    # Client credentials: an API and a daemon with admin consent.
    call("PUT", "/identity/entra/apps/acme-api", {"type": "api", "exposes": ["orders.read"], "app_id_uri": "api://acme-api"})
    call("PUT", "/identity/entra/apps/acme-worker", {"type": "daemon", "permissions": ["api://acme-api/orders.read"]})
    call("POST", "/identity/entra/apps/acme-worker/admin-consent", {})
    wsec = open(os.path.join(DATA, "environments", "stg-a", "secrets.yaml")).read().split("acme-worker-client-secret:")[1].split("\n")[0].strip().strip("'\"")
    st, t = token({"grant_type": "client_credentials", "client_id": "acme-worker", "scope": "api://acme-api/.default"}, wsec)
    assert st == 200 and claims(t["access_token"])["aud"] == "api://acme-api" and claims(t["access_token"])["roles"] == ["orders.read"], t

    # I1: the log tells the story.
    log = call("GET", "/identity/sign-ins?provider=entra&limit=200")["signIns"]
    sentences = " | ".join(s["sentence"] for s in log)
    for want in ["Emma Carter signed in to acme-app with MFA", "Liam Walker tried from tenant fabrikam-stg", "Jack Reed: account disabled", "cancelled", "Token refreshed for Emma Carter", "acme-worker got an app token"]:
        assert want in sentences, (want, sentences[:1500])
    one = call("GET", f"/identity/sign-ins/{log[-1]['id']}")
    assert one["steps"], one


def main():
    global DATA
    DATA = tempfile.mkdtemp(prefix="stuntman-id-")
    env = dict(os.environ, STUNTMAN_ENV=os.environ.get("STUNTMAN_ENV", "stg-a"), STUNTMAN_DATA=DATA, STUNTMAN_PORT=str(HTTP), STUNTMAN_HOOKS_PORT=str(HOOKS), STUNTMAN_SMTP_PORT=str(SMTP), STUNTMAN_OTLP_HTTP_PORT=str(OTLP),
               STUNTMAN_OTLP_GRPC_PORT=str(OTLP_G), STUNTMAN_BLOB_PORT=str(BLOB), STUNTMAN_S3_PORT=str(S3), STUNTMAN_APIS_PORT=str(APIS), STUNTMAN_APIS_TLS_PORT=str(TLS),
               STUNTMAN_KEYVAULT_PORT=str(KV), STUNTMAN_WEB="/nonexistent")
    log = DATA + ".log"
    srv = subprocess.Popen([BIN], env=env, stdout=open(log, "w"), stderr=subprocess.STDOUT)
    try:
        for _ in range(150):
            try:
                socket.create_connection(("127.0.0.1", HTTP), 0.2).close(); break
            except OSError:
                time.sleep(0.1)
        run()
        print("e2e identity: all checks passed")
    except Exception:
        print(open(log).read()[-3000:])
        raise
    finally:
        srv.terminate(); shutil.rmtree(DATA, ignore_errors=True)


if __name__ == "__main__":
    main()
