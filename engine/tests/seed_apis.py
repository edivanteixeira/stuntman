"""Synthetic API traffic for the screens (A1–A5): plugins from the catalog, the app's calls, a
forced answer and unmatched requests (an old SERPRO V4 path, a provider with no plugin).

    python3 tests/seed_apis.py http://localhost:8025 8028
"""
import base64, http.client, json, sys, urllib.error, urllib.parse, urllib.request

API, PORT = sys.argv[1], int(sys.argv[2])


def call(method, path, body=None):
    req = urllib.request.Request(API + "/api" + path, method=method, data=None if body is None else json.dumps(body).encode(), headers={"Content-Type": "application/json", "X-Stuntman-Client": "seed"})
    try:
        return json.loads(urllib.request.urlopen(req).read() or b"null")
    except urllib.error.HTTPError as e:
        return {"status": e.code}


def app(method, path, body=None, app_name="acme-api", headers=None, form=False, host=None):
    c = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
    h = {"X-Stuntman-App": app_name, **(headers or {})}
    if host:
        h["Host"] = host
    data = b""
    if body is not None:
        data = urllib.parse.urlencode(body).encode() if form else json.dumps(body).encode()
        h["Content-Type"] = "application/x-www-form-urlencoded" if form else "application/json"
    c.request(method, path, data, h)
    r = c.getresponse()
    out = r.read()
    try:
        return json.loads(out)
    except ValueError:
        return out


for p in ["serpro-datavalid", "gcc-consent", "recaptcha", "turnstile", "orders-partner"]:
    call("POST", "/plugins", {"plugin": p})
call("PUT", "/plugins/serpro-datavalid/service", {"latency": "none"})
tok = app("POST", "/serpro-datavalid/token", {"grant_type": "client_credentials"}, headers={"Authorization": "Basic " + base64.b64encode(b"acme-api:s").decode()}, form=True)
B = {"Authorization": "Bearer " + tok["access_token"]}
V5 = "/serpro-datavalid/datavalid/v5/pessoa-fisica/validacao"
PRIV = {"rfb": {"id_template": "tpl-acme"}, "senatran": {"token": "eyJ.x.y", "cnpj_anuente": "11222333000181"}}
face = lambda cpf, nome: {"privacidade": PRIV, "cpf": cpf, "validacao": {"nome": nome, "rfb": {"situacao_cpf": "REGULAR"}, "biometria_facial": {"formato": "JPG", "base64": base64.b64encode(b"selfie").decode(), "vivacidade": True}}}
people = [("123.456.789-09", "Emma Carter"), ("456.123.789-05", "Liam Foster"), ("321.654.987-91", "Olivia Brooks")]
for cpf, nome in people:
    app("POST", "/gcc-consent/consentimentos-ciencias", {"tipo_titular": 1, "cpf_cnpj": cpf, "tipo_de_usuario": 1, "cnpj_anuente": "11222333000181", "cnpj_provedor": "11222333000181", "template_id": "tpl-acme"}, headers={"Api-Key": "acme-gcc-key"})
    app("POST", V5, face(cpf, nome), headers=B)
app("POST", V5, face("666.666.666-66", "Lucas Hayes"), headers=B)
app("POST", V5, face("222.222.222-22", "Olivia Brooks"), headers=B)
app("POST", V5, {"privacidade": PRIV, "cpf": "456.123.789-05", "validacao": {"nome": "Liam Foster", "rfb": {"situacao_cpf": "REGULAR"}}}, headers=B)
for i in range(12):
    app("POST", "/recaptcha/api/siteverify", {"secret": "s", "response": f"tok-{i}" if i != 5 else "bot-1"}, form=True, host="www.google.com")
app("POST", "/recaptcha/api/siteverify", {"secret": "s", "response": "tok-3"}, form=True, host="www.google.com")
o = app("POST", "/orders-partner/api/orders", {"customer": "Globex", "items": [1, 2]}, app_name="acme-worker")
for i in range(4):
    app("GET", f"/orders-partner/api/orders/{o['id']}/status", app_name="acme-worker")
app("PUT", "/orders-partner/api/orders/58213/cancel", app_name="acme-worker")
# Unmatched: an old client on SERPRO V4, a provider with no plugin, a path the partner doesn't have
for _ in range(3):
    app("POST", "/serpro-datavalid/datavalid/v4/pessoa-fisica/validacao", {"cpf": "55566677788", "validacao": {}}, app_name="acme-batch", headers=B)
app("GET", "/v21.0/105954558954427/media", host="graph.facebook.com")
app("POST", "/v2/channels/whatsapp/messages", {"to": "5511900000000"}, host="api.zenvia.com")
app("PUT", "/orders-partner/api/order/58213/cancel", app_name="acme-worker")
# A forced answer that is still on
call("POST", "/apis/serpro-datavalid/force", {"answer": "face_mismatch", "count": 2})
print("apis seeded")
