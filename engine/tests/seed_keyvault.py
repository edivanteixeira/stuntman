"""Demo vault for K1–K5 in a running Stuntman (synthetic values only): acme-kv-stg with secrets, keys,
certificates, three identities and an hour of traffic through the real Azure REST API.

    python3 tests/seed_keyvault.py [http://localhost:8025] [env]
"""
import base64, hashlib, http.client, json, socket, ssl, sys, urllib.error, urllib.parse, urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8025"
ENV = sys.argv[2] if len(sys.argv) > 2 else ""
PFX = f"/e/{ENV}" if ENV else ""
V = "acme-kv-stg"


def call(method, path, body=None):
    req = urllib.request.Request(f"{BASE}{PFX}/api{path}", method=method, data=None if body is None else json.dumps(body).encode(), headers={"Content-Type": "application/json", "X-Stuntman-Client": "seed"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return {"error": e.code, "body": e.read().decode()[:200]}


with urllib.request.urlopen(f"{BASE}/api/test-ca.pem") as r:
    CA = r.read().decode()
call("PUT", f"/keyvault/vaults/{V}", {"any_token": True})
vs = call("GET", "/keyvault/vaults")
url = next(v["url"] for v in vs["vaults"] if v["name"] == V)
HOST, PORT = urllib.parse.urlparse(url).hostname, urllib.parse.urlparse(url).port


def token(identity):
    req = urllib.request.Request(f"{BASE}{PFX}/metadata/identity/oauth2/token?resource=https%3A%2F%2Fvault.azure.net&client_id={identity}", headers={"Metadata": "true"})
    with urllib.request.urlopen(req) as r:
        return json.loads(r.read())["access_token"]


def kv(method, path, tok, body=None, client="acme-api/1.0"):
    ctx = ssl.create_default_context(cadata=CA)
    c = http.client.HTTPSConnection(HOST, PORT, context=ctx)
    c.sock = ctx.wrap_socket(socket.create_connection(("127.0.0.1", PORT)), server_hostname=HOST)
    h = {"Content-Type": "application/json", "User-Agent": f"{client} azsdk-net-Security.KeyVault/4.7.0"}
    if tok:
        h["Authorization"] = f"Bearer {tok}"
    c.request(method, f"{path}{'&' if '?' in path else '?'}api-version=7.4", body=None if body is None else json.dumps(body), headers=h)
    r = c.getresponse(); raw = r.read()
    return r.status, (json.loads(raw) if raw else None)


# Earlier test items in this vault (made by the e2e/SDK checks): delete and purge.
admin = token("qa-suite")
for kind, n in [("secrets", "dotnet-check"), ("secrets", "old-key"), ("keys", "dotnet-signing"), ("keys", "signing"), ("certificates", "dotnet-cert"), ("keys", "wrap"), ("secrets", "stripe"), ("keys", "dotnet-yearly"), ("keys", "yearly"), ("keys", "backup-wrap"), ("certificates", "partner-api-tls"), ("certificates", "imported-test")]:
    st, _ = kv("DELETE", f"/{kind}/{n}", admin)
    if st == 200:
        kv("DELETE", f"/deleted{kind}/{n}", admin)

secrets = {"apm-license-key": "demo-apm-0000-1111", "blob-account-key": "ZGVtby1ibG9iLWtleQ==", "db-connection": "Host=db.stg-a.example.com;Database=acme;Username=acme_app;Password=demo-pass",
           "signing-hmac-key": "demo-hmac-7f3a9c", "smtp-password": "demo-smtp-pass", "whatsapp-app-secret": "demo-wa-app-secret", "whatsapp-token": "demo-wa-token-v3",
           "whatsapp-verify-token": "demo-verify", "zenvia-token": "demo-zenvia-token"}
for n, v in secrets.items():
    call("PUT", f"/keyvault/secrets/{n}", {"value": v, "vault": V, "content_type": "text/plain", "tags": {"owner": "messaging" if n.startswith(("whatsapp", "zenvia", "smtp")) else "platform"}, "by": "seed"})
call("PUT", "/keyvault/secrets/whatsapp-token", {"value": "demo-wa-token-v4", "vault": V, "tags": {"owner": "messaging"}, "expires": "27d", "by": "seed"})
call("PATCH", "/keyvault/secrets/smtp-password", {"enabled": False, "vault": V})
for n, ops in [("data-encryption", ["wrapKey", "unwrapKey", "encrypt", "decrypt"]), ("document-signing", ["sign", "verify"]), ("jwt-issuer", ["sign", "verify"])]:
    call("POST", f"/keyvault/keys/{n}", {"vault": V, "size": 2048, "ops": ops})
call("POST", "/keyvault/keys/document-signing", {"vault": V, "size": 2048, "ops": ["sign", "verify"]})
call("POST", "/keyvault/certificates/acme-signing-cert", {"vault": V, "subject": "CN=Acme Signing STG", "days": 12})
call("POST", "/keyvault/certificates/partner-mtls-client", {"vault": V, "subject": "CN=partner-mtls-client", "days": 90})
call("PUT", f"/keyvault/vaults/{V}", {"identities": [{"id": "acme-api-mi", "roles": ["secrets-user", "crypto-user"], "scope": []},
                                                     {"id": "acme-worker-mi", "roles": ["secrets-user"], "scope": ["zenvia-*", "whatsapp-*"]},
                                                     {"id": "qa-suite", "roles": ["administrator"], "scope": []}]})

api, worker = token("acme-api-mi"), token("acme-worker-mi")
digest = base64.urlsafe_b64encode(hashlib.sha256(b"nda-acme.pdf").digest()).rstrip(b"=").decode()
for i in range(120):
    kv("GET", "/secrets/signing-hmac-key", api)  # on every request: not cached
for _ in range(12):
    kv("POST", "/keys/document-signing/sign", api, {"alg": "RS256", "value": digest})
for n in ["whatsapp-token", "db-connection", "blob-account-key", "apm-license-key"]:
    kv("GET", f"/secrets/{n}", api)
kv("GET", "/secrets/acme-signing-cert", api)
for n in ["zenvia-token", "whatsapp-token"]:
    kv("GET", f"/secrets/{n}", worker, client="acme-worker/1.0")
kv("GET", "/secrets/smtp-password", worker, client="acme-worker/1.0")  # out of its scope: would fail
kv("GET", "/secrets/db-connection", "not-a-jwt", client="curl/8.4")  # 401
st, w = kv("POST", "/keys/data-encryption/wrapkey", api, {"alg": "RSA-OAEP", "value": base64.urlsafe_b64encode(b"k" * 32).rstrip(b"=").decode()})
if st == 200:
    kv("POST", "/keys/data-encryption/unwrapkey", api, {"alg": "RSA-OAEP", "value": w["value"]})
print(json.dumps(call("GET", f"/keyvault/vaults")["vaults"], indent=1))
