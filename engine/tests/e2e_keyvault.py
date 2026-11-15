"""End-to-end check of Key Vault (K1–K5): a managed identity token from the IMDS endpoint, the Azure
Key Vault REST API over HTTPS with the Stuntman Test CA (secrets, keys, certificates, soft delete),
provocations, identities and roles, the access log and its findings, over the HTTP API and MCP.

    cargo build -p stuntman && python3 tests/e2e_keyvault.py
"""
import base64, hashlib, http.client, json, os, shutil, socket, ssl, subprocess, tempfile, time, urllib.error, urllib.parse, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "stuntman")


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


HTTP, HOOKS, SMTP, OTLP, OTLP_G, BLOB, S3, APIS, TLS, KV = (free_port() for _ in range(10))
DATA = None
HOST = None
CA = []


def call(method, path, body=None, expect=200):
    req = urllib.request.Request(f"http://127.0.0.1:{HTTP}/api{urllib.parse.quote(path, safe='/?&=:%')}", method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            st, v = r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        st, v = e.code, json.loads(e.read() or b"null")
    assert expect is None or st == expect, (method, path, st, v)
    return v


def tool(name, args):
    body = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}
    req = urllib.request.Request(f"http://127.0.0.1:{HTTP}/mcp", data=json.dumps(body).encode(), headers={"Content-Type": "application/json", "Accept": "application/json, text/event-stream"})
    with urllib.request.urlopen(req, timeout=60) as r:
        res = json.loads(r.read())["result"]
    return res.get("isError", False), json.loads(res["content"][0]["text"])


def token(identity):
    req = urllib.request.Request(f"http://127.0.0.1:{HTTP}/metadata/identity/oauth2/token?api-version=2018-02-01&resource=https%3A%2F%2Fvault.azure.net&client_id={identity}", headers={"Metadata": "true"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())["access_token"]


def kv(method, path, body=None, tok=None, host=None):
    """A request to the vault over TLS, checking the certificate against the Stuntman Test CA."""
    ctx = ssl.create_default_context(cadata=CA[0])
    h = host or HOST
    sock = ctx.wrap_socket(socket.create_connection(("127.0.0.1", KV), 10), server_hostname=h)
    c = http.client.HTTPSConnection(h, KV, context=ctx)
    c.sock = sock
    hd = {"Content-Type": "application/json", "User-Agent": "acme-api/1.0 azsdk-net-Security.KeyVault.Secrets/4.6.0"}
    if tok:
        hd["Authorization"] = f"Bearer {tok}"
    sep = "&" if "?" in path else "?"
    c.request(method, f"{path}{sep}api-version=7.4", body=None if body is None else json.dumps(body), headers=hd)
    r = c.getresponse()
    raw = r.read()
    return r.status, (json.loads(raw) if raw else None), {k.lower(): v for k, v in r.getheaders()}


def b64u(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def run():
    global HOST
    # Vault created from the screen: imports this environment's secret:// values (none here) and seeds.
    with urllib.request.urlopen(f"http://127.0.0.1:{HTTP}/api/test-ca.pem") as r:
        CA.append(r.read().decode())
    v = call("PUT", "/keyvault/vaults/acme-kv-stg", {"any_token": True, "soft_delete_days": 90})
    assert v["created"] is True, v
    vs = call("GET", "/keyvault/vaults")
    assert vs["vaults"][0]["name"] == "acme-kv-stg" and vs["port"] == KV, vs
    HOST = urllib.parse.urlparse(vs["vaults"][0]["url"]).hostname
    assert HOST.startswith("acme-kv-stg.vault."), HOST

    # K1 · secrets. No token: the SDK's challenge with the resource to ask for.
    st, b, hd = kv("GET", "/secrets/smtp-password")
    assert st == 401 and "resource=" in hd.get("www-authenticate", ""), (st, b, hd)
    t = token("acme-api-mi")
    call("PUT", "/keyvault/secrets/smtp-password", {"value": "synthetic-pass-1", "vault": "acme-kv-stg", "tags": {"owner": "messaging"}})
    st, b, _ = kv("GET", "/secrets/smtp-password", tok=t)
    assert st == 200 and b["value"] == "synthetic-pass-1" and b["tags"]["owner"] == "messaging", (st, b)
    first = b["id"].rsplit("/", 1)[1]
    st, b, _ = kv("PUT", "/secrets/smtp-password", {"value": "synthetic-pass-2"}, tok=t)
    assert st == 200 and b["value"] == "synthetic-pass-2", (st, b)
    st, b, _ = kv("GET", f"/secrets/smtp-password/{first}", tok=t)
    assert st == 200 and b["value"] == "synthetic-pass-1", b
    st, b, _ = kv("GET", "/secrets/smtp-password/versions", tok=t)
    assert st == 200 and len(b["value"]) == 2, b
    st, b, _ = kv("GET", "/secrets", tok=t)
    assert any(x["id"].endswith("/secrets/smtp-password") for x in b["value"]), b
    st, b, _ = kv("GET", "/secrets/nope", tok=t)
    assert st == 404 and b["error"]["code"] == "SecretNotFound", b
    for _ in range(3):
        kv("GET", "/secrets/smtp-password", tok=t)
    d = call("GET", "/keyvault/secrets/smtp-password/detail?vault=acme-kv-stg")
    assert len(d["versions"]) == 2 and d["versions"][0]["state"] == "current" and d["versions"][0]["reads"] >= 3, d["versions"]
    assert d["readers"] and d["readers"][0]["identity"] == "acme-api-mi", d["readers"]

    # Provocations on the next reads.
    call("POST", "/keyvault/secrets/smtp-password/provoke", {"answer": "403", "vault": "acme-kv-stg"})
    st, b, _ = kv("GET", "/secrets/smtp-password", tok=t)
    assert st == 403 and b["error"]["code"] == "Forbidden", b
    call("POST", "/keyvault/secrets/smtp-password/provoke", {"answer": "disabled", "vault": "acme-kv-stg"})
    st, b, _ = kv("GET", "/secrets/smtp-password", tok=t)
    assert st == 403 and b["error"]["code"] == "Forbidden" and "disabled" in b["error"]["message"].lower(), b
    call("POST", "/keyvault/secrets/smtp-password/provoke", {"answer": "429", "vault": "acme-kv-stg"})
    st, b, hd = kv("GET", "/secrets/smtp-password", tok=t)
    assert st == 429 and "retry-after" in hd, (st, hd)
    call("POST", "/keyvault/secrets/smtp-password/provoke", {"answer": "normal", "vault": "acme-kv-stg"})
    st, b, _ = kv("GET", "/secrets/smtp-password", tok=t)
    assert st == 200, b
    # A disabled version is refused like Azure does.
    call("PATCH", "/keyvault/secrets/smtp-password", {"enabled": False, "vault": "acme-kv-stg"})
    st, b, _ = kv("GET", "/secrets/smtp-password", tok=t)
    assert st == 403 and "disabled" in b["error"]["message"].lower(), b
    call("PATCH", "/keyvault/secrets/smtp-password", {"enabled": True, "vault": "acme-kv-stg"})

    # Soft delete: recreate is 409 until recovered.
    st, b, _ = kv("PUT", "/secrets/old-key", {"value": "x1"}, tok=t)
    st, b, _ = kv("DELETE", "/secrets/old-key", tok=t)
    assert st == 200 and "recoveryId" in b, b
    st, b, _ = kv("PUT", "/secrets/old-key", {"value": "x2"}, tok=t)
    assert st == 409 and b["error"]["innererror"]["code"] == "ObjectIsDeletedButRecoverable", b
    st, b, _ = kv("GET", "/deletedsecrets", tok=t)
    assert any("old-key" in x["recoveryId"] for x in b["value"]), b
    st, b, _ = kv("POST", "/deletedsecrets/old-key/recover", tok=t)
    assert st == 200, b
    st, b, _ = kv("GET", "/secrets/old-key", tok=t)
    assert st == 200 and b["value"] == "x1", b

    # K2 · keys: create, sign, verify, wrong signature, rotate.
    st, b, _ = kv("POST", "/keys/signing/create", {"kty": "RSA", "key_size": 2048, "key_ops": ["sign", "verify"]}, tok=t)
    assert st == 200 and b["key"]["kty"] == "RSA" and b["key"]["n"], b
    digest = b64u(hashlib.sha256(b"contract.pdf").digest())
    st, s, _ = kv("POST", "/keys/signing/sign", {"alg": "RS256", "value": digest}, tok=t)
    assert st == 200 and s["value"], s
    st, b, _ = kv("POST", "/keys/signing/verify", {"alg": "RS256", "digest": digest, "value": s["value"]}, tok=t)
    assert st == 200 and b["value"] is True, b
    call("POST", "/keyvault/keys/signing/provoke", {"answer": "wrong_signature", "vault": "acme-kv-stg"})
    st, s2, _ = kv("POST", "/keys/signing/sign", {"alg": "RS256", "value": digest}, tok=t)
    st, b, _ = kv("POST", "/keys/signing/verify", {"alg": "RS256", "digest": digest, "value": s2["value"]}, tok=t)
    assert b["value"] is False, b
    call("POST", "/keyvault/keys/signing/provoke", {"answer": "normal", "vault": "acme-kv-stg"})
    st, b, _ = kv("POST", "/keys/signing/encrypt", {"alg": "RSA-OAEP", "value": b64u(b"hi")}, tok=t)
    assert st == 403 or st == 400, (st, b)  # encrypt not in key_ops
    st, b, _ = kv("POST", "/keys/signing/rotate", {}, tok=t)
    assert st == 200, b
    kd = call("GET", "/keyvault/keys/signing/detail?vault=acme-kv-stg")
    assert len(kd["versions"]) == 2 and kd["operations"][0]["op"] == "Sign" and kd["operations"][0]["count"] >= 2, kd["operations"]
    # Expiry: a key made to expire in a year signs; expired now, the operation is refused like Azure.
    call("POST", "/keyvault/keys/yearly", {"vault": "acme-kv-stg", "ops": ["sign", "verify"], "expires": "365d", "tags": {"owner": "signing"}})
    st, b, _ = kv("GET", "/keys/yearly", tok=t)
    assert st == 200 and b["attributes"]["exp"] > time.time() + 360 * 86400 and b["tags"]["owner"] == "signing", b
    call("PATCH", "/keyvault/keys/yearly", {"expires": "now", "vault": "acme-kv-stg"})
    st, b, _ = kv("POST", "/keys/yearly/sign", {"alg": "RS256", "value": digest}, tok=t)
    assert st == 403 and b["error"]["innererror"]["code"] == "KeyExpired", (st, b)
    pem = call("GET", "/keyvault/keys/signing/public?vault=acme-kv-stg&format=pem")["pem"]
    assert pem.startswith("-----BEGIN PUBLIC KEY-----"), pem

    # K3 · certificates: issued by the Test CA, its .pfx as a secret, provocations.
    call("POST", "/keyvault/certificates/acme-signing", {"subject": "CN=Acme Signing STG", "days": 365, "vault": "acme-kv-stg"})
    st, b, _ = kv("GET", "/certificates/acme-signing", tok=t)
    assert st == 200 and b["cer"] and b["x5t"], b
    st, b, _ = kv("GET", "/secrets/acme-signing", tok=t)
    assert st == 200 and b["contentType"] == "application/x-pkcs12" and len(base64.b64decode(b["value"])) > 1000, b.get("contentType")
    cd = call("GET", "/keyvault/certificates/acme-signing/detail?vault=acme-kv-stg")
    assert "Acme Signing STG" in cd["subject"] and "Test CA" in cd["issuer"] and 360 <= cd["daysLeft"] <= 365, cd
    call("POST", "/keyvault/certificates/acme-signing/provoke", {"answer": "expired", "vault": "acme-kv-stg"})
    cd = call("GET", "/keyvault/certificates/acme-signing/detail?vault=acme-kv-stg")
    assert cd["daysLeft"] < 0, cd["daysLeft"]
    call("POST", "/keyvault/certificates/acme-signing/provoke", {"answer": "missing", "vault": "acme-kv-stg"})
    st, b, _ = kv("GET", "/certificates/acme-signing", tok=t)
    assert st == 404, b
    call("POST", "/keyvault/certificates/acme-signing/provoke", {"answer": "normal", "vault": "acme-kv-stg"})
    call("POST", "/keyvault/certificates/acme-signing/renew", {"vault": "acme-kv-stg"})
    assert len(call("GET", "/keyvault/certificates/acme-signing/detail?vault=acme-kv-stg")["versions"]) == 2

    # K4 · access: with any token on, a role the identity lacks is allowed but marked would_fail.
    call("PUT", "/keyvault/identities/acme-api-mi", {"roles": ["secrets-user"], "scope": ["smtp-*"], "vault": "acme-kv-stg"})
    st, b, _ = kv("GET", "/secrets/old-key", tok=t)
    assert st == 200, b
    wf = call("GET", "/keyvault/would-fail?vault=acme-kv-stg")["operations"]
    assert any(x["item"] == "old-key" for x in wf), wf
    call("PUT", "/keyvault/vaults/acme-kv-stg", {"any_token": False})
    st, b, _ = kv("GET", "/secrets/old-key", tok=t)
    assert st == 403 and b["error"]["code"] == "Forbidden", b
    st, b, _ = kv("GET", "/secrets/smtp-password", tok=t)
    assert st == 200, b
    st, b, _ = kv("GET", "/secrets/smtp-password", tok=token("billing-worker-mi"))
    assert st == 403, b
    st, b, _ = kv("GET", "/secrets/smtp-password", tok="not-a-jwt")
    assert st == 401, (st, b)
    g = call("GET", "/keyvault/vaults/acme-kv-stg")
    assert "any_token: false" in g["yaml"] and "acme-api-mi" in g["yaml"], g["yaml"]
    call("PUT", "/keyvault/vaults/acme-kv-stg", {"any_token": True})

    # IMDS provocations.
    call("POST", "/keyvault/imds/provoke", {"answer": "500"})
    try:
        token("acme-api-mi"); raise AssertionError("IMDS should fail")
    except urllib.error.HTTPError as e:
        assert e.code == 500
    call("POST", "/keyvault/imds/provoke", {"answer": "normal"})
    token("acme-api-mi")

    # Throttling by the vault block.
    call("PUT", "/keyvault/vaults/acme-kv-stg", {"throttle": "3/10s"})
    codes = [kv("GET", "/secrets/smtp-password", tok=t)[0] for _ in range(5)]
    assert 429 in codes, codes
    call("PUT", "/keyvault/vaults/acme-kv-stg", {"throttle": "2000/10s"})

    # K5 · access log, entry detail and findings.
    log = call("GET", "/keyvault/log?vault=acme-kv-stg&limit=1000")["entries"]
    assert any(e["op"] == "GetSecret" and e["status"] == 403 for e in log) and any(e["op"] == "Sign" for e in log), {e["op"] for e in log}
    e = call("GET", f"/keyvault/log/{log[0]['id']}")
    assert e["request"] and e["identity"], e
    f = call("GET", "/keyvault/patterns?vault=acme-kv-stg")["findings"]
    assert f, f

    # The same over MCP.
    err, v = tool("list_secrets", {"vault": "acme-kv-stg"})
    assert not err and any(s["name"] == "smtp-password" for s in v["secrets"]), v
    err, v = tool("get_secret", {"name": "smtp-password", "vault": "acme-kv-stg", "by": "agent"})
    assert not err and v["value"] == "synthetic-pass-2", v
    err, v = tool("create_vault_key", {"name": "wrap", "ops": ["wrapKey", "unwrapKey"], "vault": "acme-kv-stg"})
    assert not err, v
    err, v = tool("set_secret", {"name": "stripe", "value": "sk_live_abc", "vault": "acme-kv-stg"})
    assert err, v
    err, v = tool("vault_patterns", {"vault": "acme-kv-stg"})
    assert not err, v


def main():
    global DATA
    DATA = tempfile.mkdtemp(prefix="stuntman-kv-")
    env = dict(os.environ, STUNTMAN_ENV=os.environ.get("STUNTMAN_ENV", "stg-a"), STUNTMAN_DATA=DATA, STUNTMAN_PORT=str(HTTP), STUNTMAN_HOOKS_PORT=str(HOOKS), STUNTMAN_SMTP_PORT=str(SMTP), STUNTMAN_OTLP_HTTP_PORT=str(OTLP),
               STUNTMAN_OTLP_GRPC_PORT=str(OTLP_G), STUNTMAN_BLOB_PORT=str(BLOB), STUNTMAN_S3_PORT=str(S3), STUNTMAN_APIS_PORT=str(APIS), STUNTMAN_APIS_TLS_PORT=str(TLS),
               STUNTMAN_KEYVAULT_PORT=str(KV), STUNTMAN_WEB="/nonexistent")
    log = DATA + ".log"
    srv = subprocess.Popen([BIN], env=env, stdout=open(log, "w"), stderr=subprocess.STDOUT)
    try:
        for _ in range(150):
            try:
                socket.create_connection(("127.0.0.1", KV), 0.2).close(); break
            except OSError:
                time.sleep(0.1)
        run()
        print("e2e keyvault: all checks passed")
    except Exception:
        print(open(log).read()[-3000:])
        raise
    finally:
        srv.terminate(); shutil.rmtree(DATA, ignore_errors=True)


if __name__ == "__main__":
    main()
