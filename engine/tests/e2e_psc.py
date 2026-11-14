"""Cloud-certificate PSC flows as a real app runs them, against the birdid and vidaas catalog plugins.
Run with Stuntman up and the "Brazil · ICP-Brasil" collection installed:  python3 tests/e2e_psc.py
"""
import base64, hashlib, json, os, re, secrets, sys, urllib.error, urllib.parse, urllib.request
from cryptography import x509
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import padding, utils

API = os.environ.get("STUNTMAN_API", "http://127.0.0.1:8025/api")
BASE = os.environ.get("STUNTMAN_APIS", "http://127.0.0.1:8028")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


op = urllib.request.build_opener(NoRedirect)


def call(method, url, body=None, form=False, headers=None):
    h = dict(headers or {})
    data = None
    if body is not None:
        data = urllib.parse.urlencode(body).encode() if form else json.dumps(body).encode()
        h["content-type"] = "application/x-www-form-urlencoded" if form else "application/json"
    req = urllib.request.Request(url, data=data, method=method, headers=h)
    try:
        r = op.open(req)
        return r.status, dict(r.headers), r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read().decode()


def pkce():
    v = base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode()
    c = base64.urlsafe_b64encode(hashlib.sha256(v.encode()).digest()).rstrip(b"=").decode()
    return v, c


def verify(pem_chain, digest, sig_b64):
    leaf = x509.load_pem_x509_certificates(pem_chain.encode())[0]
    leaf.public_key().verify(base64.b64decode(sig_b64), digest, padding.PKCS1v15(), utils.Prehashed(hashes.SHA256()))
    return leaf


def birdid():
    b = f"{BASE}/birdid/v0"
    v, c = pkce()
    q = dict(response_type="code", client_id="acme-api", redirect_uri="https://acme.example.com/callback/birdid", code_challenge_method="S256", code_challenge=c, state="58213", scope="signature_session", lifetime="3600")
    s, _, html = call("GET", f"{b}/oauth/authorize?" + urllib.parse.urlencode(q))
    assert s == 200 and "Who is signing" in html, (s, html[:300])
    action = re.search(r'action="([^"]+)"', html).group(1)
    form = BASE + action
    # wrong code first, then the right one
    s, _, html = call("POST", form, {"action": "authorize", "person": "123.456.789-09", "otp": "000000"}, form=True)
    assert s == 422 and "Invalid code" in html, (s, html[:200])
    s, h, _ = call("POST", form, {"action": "authorize", "person": "123.456.789-09", "otp": "482913"}, form=True)
    assert s == 302, s
    loc = urllib.parse.urlparse(h["location"]); lq = urllib.parse.parse_qs(loc.query)
    assert lq["state"] == ["58213"] and loc.path == "/callback/birdid", h["location"]
    code = lq["code"][0]
    s, _, t = call("POST", f"{b}/oauth/token", {"code": code, "client_id": "acme-api", "client_secret": "x", "redirect_uri": q["redirect_uri"], "grant_type": "authorization_code", "code_verifier": "wrong"})
    assert s == 400, (s, t)
    s, _, t = call("POST", f"{b}/oauth/token", {"code": code, "client_id": "acme-api", "client_secret": "x", "redirect_uri": q["redirect_uri"], "grant_type": "authorization_code", "code_verifier": v})
    assert s == 200, (s, t)
    tok = json.loads(t); assert tok["authorized_identification"] == "12345678909", tok
    s, _, t = call("POST", f"{b}/oauth/token", {"code": code, "client_id": "acme-api", "grant_type": "authorization_code", "code_verifier": v})
    assert s == 400 and "already" in t, (s, t)
    auth = {"authorization": "Bearer " + tok["access_token"]}
    s, _, t = call("GET", f"{b}/oauth/certificate-discovery", headers=auth)
    d = json.loads(t); assert s == 200 and d["certificates"][0]["alias"] == "emma-carter-a3", t[:200]
    digest = hashlib.sha256(b"Service agreement").digest()
    s, _, t = call("POST", f"{b}/oauth/signature", {"certificate_alias": "emma-carter-a3", "include_chain": True, "hashes": [{"id": "doc_0", "alias": "Documento 1", "hash": digest.hex(), "hash_algorithm": "2.16.840.1.101.3.4.2.1", "signature_format": "RAW"}]}, headers=auth)
    assert s == 200, (s, t)
    leaf = verify(d["certificates"][0]["certificate"], digest, json.loads(t)["signatures"][0]["raw_signature"])
    assert "EMMA CARTER" in leaf.subject.rfc4514_string()
    # OTP grant (pwd_authorize) for the expired one: the token works, the certificate is expired
    s, _, t = call("POST", f"{b}/oauth/pwd_authorize", {"grant_type": "password", "client_id": "acme-api", "client_secret": "x", "username": "98765432100", "password": "111111", "scope": "signature_session"}, form=True)
    assert s == 200, (s, t)
    s, _, t = call("GET", f"{b}/oauth/certificate-discovery", headers={"authorization": "Bearer " + json.loads(t)["access_token"]})
    leaf = x509.load_pem_x509_certificates(json.loads(t)["certificates"][0]["certificate"].encode())[0]
    import datetime
    assert leaf.not_valid_after_utc < datetime.datetime.now(datetime.timezone.utc), "expected an expired certificate"
    s, _, _ = call("POST", f"{b}/oauth/pwd_authorize", {"grant_type": "password", "username": "98765432100", "password": "000000"}, form=True)
    assert s == 400
    print("birdid ok: page, wrong OTP, PKCE, single-use code, discovery, signature verified, OTP grant, expired certificate")


def vidaas():
    b = f"{BASE}/vidaas"
    v, c = pkce()
    q = dict(response_type="code", client_id="acme-api", redirect_uri="push://", code_challenge_method="S256", code_challenge=c, login_hint="32165498791", scope="signature_session", lifetime="3600")
    s, _, body = call("GET", f"{b}/v0/oauth/authorize?" + urllib.parse.urlencode(q, safe=":/"), headers={"accept": "application/json"})
    m = re.search(r"code=([^&]+)", body); assert s == 200 and m, (s, body)
    code = m.group(1)
    s, _, t = call("GET", f"{b}/valid/api/v1/trusted-services/authentications?code={code}")
    assert s == 200 and json.loads(t)["authorizationToken"] is None, t
    s, _, t = call("POST", f"{API}/apis/steps/push", {"step": code, "answer": "approve"})
    assert s == 200, (s, t)
    s, _, t = call("GET", f"{b}/valid/api/v1/trusted-services/authentications?code={code}")
    grant = json.loads(t)["authorizationToken"]; assert grant, t
    s, _, t = call("POST", f"{b}/v0/oauth/token", {"grant_type": "authorization_code", "client_id": "acme-api", "client_secret": "x", "code": grant, "code_verifier": v, "redirect_uri": "https://acme.example.com/callback/vcert", "lifetime": "3600"}, form=True)
    assert s == 200, (s, t)
    auth = {"authorization": "Bearer " + json.loads(t)["access_token"]}
    s, _, t = call("POST", f"{b}/v0/oauth/user-discovery", {"client_id": "acme-api", "client_secret": "x", "user_cpf_cnpj": "CPF", "val_cpf_cnpj": "32165498791"})
    assert json.loads(t)["status"] == "S", t
    s, _, t = call("GET", f"{b}/v0/oauth/certificate-discovery", headers=auth)
    pem = json.loads(t)["certificates"][0]["certificate"]
    digest = hashlib.sha256(b"Service agreement").digest()
    s, _, t = call("POST", f"{b}/v0/oauth/signature", {"certificate_alias": "CERT-01", "hashes": [{"id": "doc_0", "alias": "Documento 1", "hash": base64.b64encode(digest).decode(), "hash_algorithm": "2.16.840.1.101.3.4.2.1", "signature_format": "RAW"}]}, headers=auth)
    verify(pem, digest, json.loads(t)["signatures"][0]["raw_signature"])
    # rejected push: the polls never get a token
    s, _, body = call("GET", f"{b}/v0/oauth/authorize?" + urllib.parse.urlencode(q, safe=":/"))
    code2 = re.search(r"code=([^&]+)", body).group(1)
    call("POST", f"{API}/apis/steps/push", {"step": code2, "answer": "reject"})
    s, _, t = call("GET", f"{b}/valid/api/v1/trusted-services/authentications?code={code2}")
    assert json.loads(t)["authorizationToken"] is None
    # A validity window on purpose: expired yesterday, then not yet valid, then back to the plugin's.
    import datetime
    now = datetime.datetime.now(datetime.timezone.utc)
    s, _, t = call("POST", f"{API}/apis/people/321.654.987-91/certificate", {})
    r = json.loads(t); assert s == 200 and r["state"] == "expired" and r["subject"] == "OLIVIA BROOKS:32165498791" and "PRIVATE KEY" not in t, (s, t[:300])
    s, _, t = call("GET", f"{b}/v0/oauth/certificate-discovery", headers=auth)
    pem = json.loads(t)["certificates"][0]["certificate"]
    leaf, ca = x509.load_pem_x509_certificates(pem.encode())[:2]
    assert pem == r["certificate"] and now - datetime.timedelta(days=2) < leaf.not_valid_after_utc < now, leaf.not_valid_after_utc
    assert leaf.issuer == ca.subject and "Test CA" in ca.subject.rfc4514_string(), (leaf.issuer, ca.subject)
    leaf.verify_directly_issued_by(ca)
    s, _, t = call("POST", f"{b}/v0/oauth/signature", {"certificate_alias": "CERT-01", "hashes": [{"id": "doc_0", "alias": "Documento 1", "hash": base64.b64encode(digest).decode(), "hash_algorithm": "2.16.840.1.101.3.4.2.1", "signature_format": "RAW"}]}, headers=auth)
    verify(pem, digest, json.loads(t)["signatures"][0]["raw_signature"])
    s, _, t = call("POST", f"{API}/apis/people/32165498791/certificate", {"not_before": "+2d", "not_after": "2040-01-01"})
    r = json.loads(t); assert s == 200 and r["state"] == "not_yet_valid" and r["notAfter"] == "2040-01-01T00:00:00Z", (s, t[:300])
    s, _, t = call("GET", f"{b}/v0/oauth/certificate-discovery", headers=auth)
    leaf = x509.load_pem_x509_certificates(json.loads(t)["certificates"][0]["certificate"].encode())[0]
    assert leaf.not_valid_before_utc > now + datetime.timedelta(days=1), leaf.not_valid_before_utc
    s, _, t = call("POST", f"{API}/apis/people/00000000191/certificate", {})
    assert s == 400 and "pass name" in t, (s, t)
    s, _, t = call("DELETE", f"{API}/apis/people/32165498791/certificate")
    assert s == 200 and json.loads(t)["cleared"] is True, t
    s, _, t = call("GET", f"{b}/v0/oauth/certificate-discovery", headers=auth)
    leaf = x509.load_pem_x509_certificates(json.loads(t)["certificates"][0]["certificate"].encode())[0]
    assert leaf.not_valid_before_utc < now < leaf.not_valid_after_utc, (leaf.not_valid_before_utc, leaf.not_valid_after_utc)
    print("vidaas ok: push, poll pending, approve, poll token, token, user discovery, signature verified, rejected push, "
          "certificate expired yesterday / not yet valid by CPF (Test CA, same key), cleared back to valid")


if __name__ == "__main__":
    birdid()
    vidaas()
