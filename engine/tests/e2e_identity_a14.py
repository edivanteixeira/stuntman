"""End-to-end check of A14: SAML 2.0 (metadata, SP-initiated SSO by redirect, a SAMLResponse whose
signature an independent library checks, provocations), the password grant (ROPC), an LDAP
directory (bind, search, compare, AD error codes, provocations) and Microsoft Teams' Bot Framework
(signed activities to the bot, the connector the bot replies to).

    pip install signxml ldap3 cryptography   (for the independent checks)
    cargo build -p stuntman && python3 tests/e2e_identity_a14.py
"""
import base64, html, http.client, http.server, json, re, threading, urllib.parse, zlib

from harness import Stuntman, free_port, wait

BOT_PORT = free_port()
BOT = {"got": [], "s": None}


def form(s, path, fields, port=None):
    c = http.client.HTTPConnection("127.0.0.1", port or s.PORT, timeout=30)
    c.request("POST", path, urllib.parse.urlencode(fields).encode(), {"Content-Type": "application/x-www-form-urlencoded"})
    r = c.getresponse()
    b = r.read()
    try:
        return r.status, json.loads(b)
    except Exception:
        return r.status, b.decode()


def get(s, path):
    c = http.client.HTTPConnection("127.0.0.1", s.PORT, timeout=30)
    c.request("GET", path)
    r = c.getresponse()
    return r.status, r.read().decode(), dict(r.getheaders())


def b64url(s):
    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


class Bot(http.server.BaseHTTPRequestHandler):
    """A Teams bot: checks the Bot Connector's JWT with the published keys, then replies."""

    def do_POST(self):
        from cryptography.hazmat.primitives import hashes
        from cryptography.hazmat.primitives.asymmetric import padding, rsa
        s = BOT["s"]
        act = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        auth = self.headers.get("Authorization", "")
        ok = False
        if auth.startswith("Bearer "):
            tok = auth[7:]
            h, p, sig = tok.split(".")
            claims = json.loads(b64url(p))
            _, cfg, _ = get(s, "/botframework/v1/.well-known/openidconfiguration")
            jwks = json.loads(get(s, urllib.parse.urlparse(json.loads(cfg)["jwks_uri"]).path)[1])
            k = jwks["keys"][0]
            pub = rsa.RSAPublicNumbers(int.from_bytes(b64url(k["e"]), "big"), int.from_bytes(b64url(k["n"]), "big")).public_key()
            try:
                pub.verify(b64url(sig), f"{h}.{p}".encode(), padding.PKCS1v15(), hashes.SHA256())
                import time
                ok = claims["iss"] == "https://api.botframework.com" and claims["aud"] == "bot-app-1" and claims["exp"] > time.time() and claims["serviceurl"] == act["serviceUrl"]
            except Exception:
                ok = False
        BOT["got"].append({"ok": ok, "activity": act})
        if not ok:
            self.send_response(401); self.end_headers(); return
        # The bot's own token, then its reply to the connector.
        st, tok = form(s, "/login/botframework.com/oauth2/v2.0/token", {"grant_type": "client_credentials", "client_id": "bot-app-1", "client_secret": "x", "scope": "https://api.botframework.com/.default"})
        su = urllib.parse.urlparse(act["serviceUrl"])
        c = http.client.HTTPConnection(su.hostname, su.port, timeout=10)
        c.request("POST", su.path + f"v3/conversations/{act['conversation']['id']}/activities", json.dumps({"type": "message", "text": "Case created: C-1042", "from": {"id": "28:bot-app-1"}}).encode(),
                  {"Content-Type": "application/json", "Authorization": "Bearer " + tok["access_token"]})
        c.getresponse().read()
        self.send_response(200); self.end_headers()

    def log_message(self, *a):
        pass


def main():
    from signxml import XMLVerifier
    import ldap3
    with Stuntman("stuntman-a14-") as s:
        BOT["s"] = s
        v = s.call("PUT", "/identity/providers/acme", {"tenant": "acme-stg", "flavour": "okta", "saml": True, "ldap": True, "password_grant": True})
        assert v["saml"]["metadata"].endswith("/login/acme-stg/saml/metadata") and v["ldap"]["baseDn"] == "dc=acme,dc=stg", v
        s.call("PUT", "/identity/acme/users/ana", {"name": "Ana Lima", "email": "ana@acme.example.com", "groups": ["Finance"], "roles": ["approver"], "password": "s3cret"})
        s.call("PUT", "/identity/acme/users/bob", {"name": "Bob Reis", "email": "bob@acme.example.com", "mfa": True})
        s.call("PUT", "/identity/acme/users/carl", {"name": "Carl Dias", "email": "carl@acme.example.com", "locked": True})
        s.call("PUT", "/identity/acme/apps/https:%2F%2Fsp.acme.example%2Fsaml", {"name": "Acme SP", "type": "web", "redirect_uris": ["https://sp.acme.example/saml/acs"], "any_secret": True})
        s.call("PUT", "/identity/acme/apps/acme-cli", {"name": "Acme CLI", "type": "web", "redirect_uris": ["http://localhost/cb"], "any_secret": True})
        s.call("PUT", "/identity/acme/apps/bot-app-1", {"name": "Acme bot", "type": "daemon", "any_secret": True})

        # SAML: metadata, AuthnRequest by redirect, the response.
        st, md, h = get(s, "/login/acme-stg/saml/metadata")
        cert = re.search(r"<ds:X509Certificate>([^<]+)<", md).group(1)
        entity = re.search(r'entityID="([^"]+)"', md).group(1)
        assert st == 200 and "samlmetadata" in h["content-type"] and "HTTP-Redirect" in md, md
        req = f'<samlp:AuthnRequest xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="_req42" Version="2.0" IssueInstant="2026-11-15T10:00:00Z" AssertionConsumerServiceURL="https://sp.acme.example/saml/acs"><saml:Issuer>https://sp.acme.example/saml</saml:Issuer></samlp:AuthnRequest>'
        co = zlib.compressobj(9, zlib.DEFLATED, -15)
        enc = base64.b64encode(co.compress(req.encode()) + co.flush()).decode()
        st, page, _ = get(s, "/login/acme-stg/saml/sso?" + urllib.parse.urlencode({"SAMLRequest": enc, "RelayState": "/dashboard"}))
        assert st == 200 and 'action="../oauth2/v2.0/authorize"' in page, page[:400]
        r = s.call("POST", "/identity/acme/sign-in-as", {"user": "ana", "app": "https://sp.acme.example/saml"})
        f = dict(r["form_post"])
        assert r["callback"] == "https://sp.acme.example/saml/acs" and f["RelayState"] == "/dashboard", r
        xml = base64.b64decode(f["SAMLResponse"])
        ok = XMLVerifier().verify(xml, x509_cert=cert, expect_references=1)
        a = ok.signed_xml
        ns = {"saml": "urn:oasis:names:tc:SAML:2.0:assertion"}
        assert a.find("saml:Subject/saml:NameID", ns).text == "ana@acme.example.com", xml
        assert a.find("saml:Conditions/saml:AudienceRestriction/saml:Audience", ns).text == "https://sp.acme.example/saml"
        assert a.find("saml:Subject/saml:SubjectConfirmation/saml:SubjectConfirmationData", ns).get("InResponseTo") == "_req42"
        assert a.find("saml:Issuer", ns).text == entity
        vals = [x.text for x in a.iter("{urn:oasis:names:tc:SAML:2.0:assertion}AttributeValue")]
        assert "Finance" in vals and "approver" in vals, vals
        # POST binding + a bad signature on purpose.
        s.call("POST", "/identity/acme/provoke", {"answer": "bad_signature", "count": 1})
        form(s, "/login/acme-stg/saml/sso", {"SAMLRequest": base64.b64encode(req.encode()).decode()})
        r = s.call("POST", "/identity/acme/sign-in-as", {"user": "ana", "app": "https://sp.acme.example/saml"})
        try:
            XMLVerifier().verify(base64.b64decode(dict(r["form_post"])["SAMLResponse"]), x509_cert=cert)
            raise AssertionError("a bad signature must not verify")
        except Exception as e:
            assert "verif" in str(e).lower() or "signature" in str(e).lower(), e
        # The person cancels: status RequestDenied.
        s.call("POST", "/identity/acme/provoke", {"answer": "user_cancels", "count": 1})
        form(s, "/login/acme-stg/saml/sso", {"SAMLRequest": base64.b64encode(req.encode()).decode()})
        r = s.call("POST", "/identity/acme/sign-in-as", {"user": "ana", "app": "https://sp.acme.example/saml"})
        assert b"RequestDenied" in base64.b64decode(dict(r["form_post"])["SAMLResponse"]), r

        # Password grant.
        tok = "/login/acme-stg/oauth2/v2.0/token"
        st, v = form(s, tok, {"grant_type": "password", "client_id": "acme-cli", "client_secret": "x", "username": "ana@acme.example.com", "password": "s3cret", "scope": "openid offline_access"})
        assert st == 200 and v["access_token"] and v["refresh_token"], (st, v)
        st, v = form(s, tok, {"grant_type": "password", "client_id": "acme-cli", "client_secret": "x", "username": "ana@acme.example.com", "password": "nope"})
        assert st == 400 and "AADSTS50126" in v["error_description"], v
        st, v = form(s, tok, {"grant_type": "password", "client_id": "acme-cli", "client_secret": "x", "username": "bob", "password": "any"})
        assert v["error"] == "interaction_required" and "AADSTS50076" in v["error_description"], v
        st, v = form(s, tok, {"grant_type": "password", "client_id": "acme-cli", "client_secret": "x", "username": "carl", "password": "any"})
        assert "AADSTS50053" in v["error_description"], v

        # LDAP.
        srv = ldap3.Server("127.0.0.1", port=s.LDAP_PORT, get_info=ldap3.NONE)
        c = ldap3.Connection(srv, "uid=ana,ou=people,dc=acme,dc=stg", "s3cret")
        assert c.bind(), c.result
        c.search("dc=acme,dc=stg", "(&(objectClass=person)(mail=ana@*))", attributes=["cn", "mail", "memberOf", "sAMAccountName"])
        e = c.entries[0]
        assert str(e.cn) == "Ana Lima" and "cn=Finance,ou=groups,dc=acme,dc=stg" in e.memberOf.values, e
        c.search("ou=groups,dc=acme,dc=stg", "(cn=Finance)", attributes=["member"])
        assert c.entries[0].member.values == ["uid=ana,ou=people,dc=acme,dc=stg"], c.entries
        assert c.compare("uid=ana,ou=people,dc=acme,dc=stg", "mail", "ana@acme.example.com")
        c.unbind()
        c = ldap3.Connection(srv, "ana@acme.example.com", "s3cret")
        assert c.bind(), c.result
        c = ldap3.Connection(srv, "ACME\\ana", "wrong")
        assert not c.bind() and c.result["result"] == 49 and "data 52e" in c.result["message"], c.result
        c = ldap3.Connection(srv, "uid=carl,ou=people,dc=acme,dc=stg", "x")
        assert not c.bind() and "data 775" in c.result["message"], c.result
        s.call("PUT", "/identity/providers/acme", {"ldap_provoke": "busy"})
        c = ldap3.Connection(srv, "uid=ana,ou=people,dc=acme,dc=stg", "s3cret")
        assert not c.bind() and c.result["result"] == 51, c.result
        s.call("PUT", "/identity/providers/acme", {"ldap_provoke": "normal"})
        signins = s.call("GET", "/identity/sign-ins?provider=acme")
        assert "LDAP bind" in json.dumps(signins), signins

        # Bot Framework.
        bot = http.server.ThreadingHTTPServer(("127.0.0.1", BOT_PORT), Bot)
        threading.Thread(target=bot.serve_forever, daemon=True).start()
        r = s.call("POST", "/identity/bot/activities", {"endpoint": f"http://127.0.0.1:{BOT_PORT}/api/messages", "bot_app_id": "bot-app-1", "text": "new case"})
        assert r["status"] == 200 and BOT["got"][-1]["ok"], (r, BOT["got"])
        msgs = wait(lambda: [m for m in s.call("GET", "/identity/bot/messages")["messages"] if m["dir"] == "from bot"], "the bot's reply")
        assert msgs[0]["activity"]["text"] == "Case created: C-1042", msgs
        for twist in ["bad_signature", "expired", "wrong_audience", "no_token"]:
            r = s.call("POST", "/identity/bot/activities", {"endpoint": f"http://127.0.0.1:{BOT_PORT}/api/messages", "bot_app_id": "bot-app-1", "value": {"action": "close"}, "twist": twist})
            assert r["status"] == 401, (twist, r)
        bot.shutdown()
    print("A14 e2e: ok")


if __name__ == "__main__":
    main()
