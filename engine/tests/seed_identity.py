"""Demo identity provider for I1–I5 in a running Stuntman (synthetic people only): Microsoft Entra ID
stand-in "entra" for tenant contoso-stg, the frames' people (each one a situation) and two apps.

    python3 tests/seed_identity.py [http://localhost:8025] [env]
"""
import json, sys, urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8025"
ENV = sys.argv[2] if len(sys.argv) > 2 else ""
PFX = f"/e/{ENV}" if ENV else ""


def call(method, path, body=None):
    r = urllib.request.Request(f"{BASE}{PFX}/api{path}", method=method, data=None if body is None else json.dumps(body).encode(), headers={"Content-Type": "application/json", "X-Stuntman-Client": "seed"})
    with urllib.request.urlopen(r) as x:
        return json.loads(x.read() or b"null")


p = call("PUT", "/identity/providers/entra", {"tenant": "contoso-stg", "flavour": "entra-v2", "other_tenants": ["fabrikam-stg"]})
people = [
    ("emma.carter", {"name": "Emma Carter", "email": "emma.carter@contoso.example.com", "groups": ["Finance", "Signers"], "roles": ["Approver"], "mfa": True, "extra": {"employeeId": "E-10442", "ctry": "US"}}),
    ("liam.walker", {"name": "Liam Walker", "email": "liam.walker@fabrikam.example.com", "tenant": "fabrikam-stg"}),
    ("noah.brooks", {"name": "Noah Brooks", "email": "noah.brooks@contoso.example.com", "groups": ["Signers"]}),
    ("olivia.bennett", {"name": "Olivia Bennett", "email": "olivia@gmail.example.com", "guest": True}),
    ("ava.morgan", {"name": "Ava Morgan", "email": "ava.morgan@contoso.example.com", "mfa": True, "phone_lost": True}),
    ("jack.reed", {"name": "Jack Reed", "email": "jack.reed@contoso.example.com", "enabled": False}),
]
for uid, body in people:
    call("PUT", f"/identity/entra/users/{uid}", body)
call("PUT", "/identity/entra/apps/acme-api", {"name": "acme-api", "type": "api", "exposes": ["orders.read"], "app_id_uri": "api://acme-api"})
call("PUT", "/identity/entra/apps/acme-app", {"name": "acme-app", "type": "web", "optional_claims": ["groups", "email", "employeeId"],
     "redirect_uris": ["https://acme-app.stg.example.com/signin-oidc", "http://localhost:5173/signin-oidc", "http://127.0.0.1:5999/signin-oidc"],
     "permissions": ["openid", "profile", "email", "offline_access", "api://acme-api/orders.read"]})
call("POST", "/identity/entra/apps/acme-app/admin-consent", {})
call("PUT", "/identity/entra/users/emma.carter", {"name": "Emma Carter"})
print(json.dumps({"authority": p["authority"], "users": len(people)}))

# An hour of sign-ins, as the I1 frame tells it (through the real login page and token endpoint).
import base64, hashlib, re, secrets, urllib.error, urllib.parse
A = p["authority"].rsplit("/v2.0", 1)[0]
CB = "http://127.0.0.1:5999/signin-oidc"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k): return None


OPEN = urllib.request.build_opener(NoRedirect)


def go(method, url, form=None):
    r = urllib.request.Request(url, data=urllib.parse.urlencode(form).encode() if form else None, method=method, headers={"Content-Type": "application/x-www-form-urlencoded"} if form else {})
    try:
        with OPEN.open(r) as x: return x.status, x.read().decode(), {k.lower(): v for k, v in x.headers.items()}
    except urllib.error.HTTPError as e: return e.code, e.read().decode(), {k.lower(): v for k, v in e.headers.items()}


def page():
    v = base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode()
    q = {"client_id": "acme-app", "redirect_uri": CB, "response_type": "code", "scope": "openid profile email offline_access", "state": "s", "nonce": "n",
         "code_challenge": base64.urlsafe_b64encode(hashlib.sha256(v.encode()).digest()).rstrip(b"=").decode(), "code_challenge_method": "S256"}
    st, html, _ = go("GET", f"{A}/oauth2/v2.0/authorize?{urllib.parse.urlencode(q)}")
    return re.search(r'name="pending" value="([^"]+)"', html).group(1), v


def do(pid, **f):
    f["pending"] = pid
    return go("POST", f"{A}/oauth2/v2.0/authorize", f)


secret = open(__import__("os").path.expanduser(f"~/Desktop/Projects/duble/engine/data/environments/{ENV or 'stg-a'}/secrets.yaml")).read().split("acme-app-client-secret:")[1].split("\n")[0].strip().strip("'\"")
pid, v = page(); do(pid, do="pick", user="ava.morgan")
for _ in range(3): do(pid, do="mfa", code="111111")
pid, v = page(); do(pid, do="pick", user="liam.walker")
pid, v = page(); do(pid, do="pick", user="noah.brooks"); do(pid, do="cancel")
pid, v = page(); do(pid, do="pick", user="emma.carter"); st, html, h = do(pid, do="mfa", code="482913")
if "wants to:" in html: st, html, h = do(pid, do="consent")
code = urllib.parse.parse_qs(urllib.parse.urlparse(h["location"]).query)["code"][0]
st, t, _ = go("POST", f"{A}/oauth2/v2.0/token", {"grant_type": "authorization_code", "code": code, "redirect_uri": CB, "code_verifier": v, "client_id": "acme-app", "client_secret": secret})
go("POST", f"{A}/oauth2/v2.0/token", {"grant_type": "refresh_token", "refresh_token": json.loads(t)["refresh_token"], "client_id": "acme-app", "client_secret": secret})
pid, v = page(); do(pid, do="pick", user="emma.carter"); do(pid, do="mfa", code="482913")
print("sign-ins seeded")
