"""End-to-end check of the Brazil catalog plugins: pix, mercado-pago, pagarme, asaas, nfse, nfse-abrasf,
correios, melhor-envio, viacep and brasilapi. Each is validated, installed from engine/plugins, its main
flow, an error and a forced failure are called as an app would, and its webhooks are received here and
checked the way the provider's docs say (Mercado Pago's x-signature, Melhor Envio's X-ME-Signature,
Asaas's asaas-access-token…). Mercado Pago is driven through its official SDK when it is installed.

    cargo build && python3 tests/e2e_plugins_brazil.py
"""
import base64, gzip, hashlib, hmac, http.client, http.server, json, os, socket, ssl, tempfile, threading, time, urllib.parse

from harness import Stuntman, free_port, wait

HOOK = free_port()
GOT = []
PLUGINS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "plugins")
NAMES = ["pix", "mercado-pago", "pagarme", "asaas", "nfse", "nfse-abrasf", "correios", "melhor-envio", "viacep", "brasilapi"]


class Hooks(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        GOT.append({"path": self.path, "headers": {k.lower(): v for k, v in self.headers.items()}, "body": self.rfile.read(n).decode()})
        self.send_response(200); self.end_headers()

    def log_message(self, *a):
        pass


def hooks(prefix):
    return [g for g in GOT if g["path"].startswith(prefix)]


def crc16(s):
    c = 0xFFFF
    for b in s.encode():
        c ^= b << 8
        for _ in range(8):
            c = ((c << 1) ^ 0x1021) if c & 0x8000 else c << 1
            c &= 0xFFFF
    return "%04X" % c


def brcode_ok(code):
    """The EMV fields parse, the Pix GUI is there and the CRC16 matches (as a bank app checks it)."""
    i, f = 0, {}
    while i < len(code):
        tag, n = code[i:i + 2], int(code[i + 2:i + 4])
        f[tag] = code[i + 4:i + 4 + n]
        i += 4 + n
    return f["00"] == "01" and f["26"].startswith("0014br.gov.bcb.pix") and f["53"] == "986" and f["58"] == "BR" and code[-4:] == crc16(code[:-4]), f


def install(s, name):
    text = open(os.path.join(PLUGINS, name + ".yaml")).read()
    v = s.call("POST", f"/plugins/{name}/validate", {"yaml": text})
    assert v["ok"] and v["errors"] == [], (name, v)
    r = s.call("PUT", f"/plugins/{name}", {"yaml": text})
    assert not r.get("errors"), r


def settings(s, name, **kw):
    rows = s.call("GET", f"/plugins/{name}/data/settings")
    row = dict((rows.get("rows") or [{}])[0], **kw)
    s.call("PUT", f"/plugins/{name}/data/settings", {"rows": [row]})


# ---------------------------------------------------------------- Pix (BCB)
def pix(s):
    install(s, "pix")
    st, tok, _ = s.api("POST", "/pix/oauth/token", b"grant_type=client_credentials", {"Content-Type": "application/x-www-form-urlencoded", "Authorization": "Basic " + base64.b64encode(b"acme-client:acme-secret").decode()})
    assert st == 200 and tok["token_type"] == "Bearer", tok
    A = {"Authorization": "Bearer " + tok["access_token"]}
    st, v, _ = s.api("GET", "/pix/cob/abcdefghijklmnopqrstuvwxyz0", None, {"Authorization": "Bearer nope"})
    assert st == 401, v

    st, v, _ = s.api("PUT", "/pix/webhook/acme@example.com", {"webhookUrl": f"http://127.0.0.1:{HOOK}/pix-hook"}, A)
    assert st == 204, (st, v)
    st, v, _ = s.api("GET", "/pix/webhook/acme@example.com", None, A)
    assert st == 200 and v["webhookUrl"].endswith("/pix-hook") and v["chave"] == "acme@example.com", v

    txid = "acmepedido7781abcdefghijkl01"
    cob = {"calendario": {"expiracao": 3600}, "devedor": {"cpf": "52998224725", "nome": "Emma Carter Souza"}, "valor": {"original": "123.45"}, "chave": "acme@example.com", "solicitacaoPagador": "Pedido 7781"}
    st, c, _ = s.api("PUT", "/pix/cob/" + txid, cob, A)
    assert st == 201 and c["txid"] == txid and c["status"] == "ATIVA" and c["loc"]["tipoCob"] == "cob" and c["valor"]["original"] == "123.45", c
    ok, f = brcode_ok(c["pixCopiaECola"])
    assert ok and f["54"] == "123.45" and f["59"] == "ACME TECNOLOGIA LTDA" and c["location"] in f["26"], (c["pixCopiaECola"], f)
    st, again, _ = s.api("GET", "/pix/cob/" + txid, None, A)
    assert st == 200 and again["pixCopiaECola"] == c["pixCopiaECola"] and again["calendario"]["criacao"] == c["calendario"]["criacao"], again
    st, v, _ = s.api("PUT", "/pix/cob/" + txid, cob, A)
    assert st == 400 and v["type"].endswith("/CobOperacaoInvalida"), v
    st, v, h = s.api("PUT", "/pix/cob/acmepedido7782abcdefghijkl01", dict(cob, valor={"original": "12,30"}), A)
    assert st == 400 and v["violacoes"][0]["propriedade"] == "cob.valor.original" and h["content-type"] == "application/problem+json", v
    st, v, _ = s.api("PUT", "/pix/cob/acmepedido7783abcdefghijkl01", dict(cob, devedor={"cpf": "52998224700", "nome": "X"}), A)
    assert st == 400 and v["violacoes"][0]["propriedade"] == "cob.devedor.cpf", v
    st, v, _ = s.api("GET", "/pix/cob/naoexisteabcdefghijklmnopqr", None, A)
    assert st == 404 and v["type"].endswith("/CobNaoEncontrado") and v["status"] == 404, v
    st, gen, _ = s.api("POST", "/pix/cob", dict(cob, valor={"original": "10.00"}), A)
    assert st == 201 and 26 <= len(gen["txid"]) <= 35 and brcode_ok(gen["pixCopiaECola"])[0], gen

    # Paying (sandbox route) → CONCLUIDA, the webhook as PSPs send it, GET /pix/{e2eid}.
    st, paid, _ = s.api("POST", "/pix/sandbox/pagamentos", {"txid": txid, "infoPagador": "Pedido 7781"})
    assert st == 200 and paid["status"] == "CONCLUIDA" and len(paid["endToEndId"]) == 32 and paid["endToEndId"].startswith("E"), paid
    e2e = paid["endToEndId"]
    g = wait(lambda: hooks("/pix-hook/pix"), "pix webhook")[0]
    body = json.loads(g["body"])
    assert body["pix"][0] == {"endToEndId": e2e, "txid": txid, "valor": "123.45", "chave": "acme@example.com", "horario": body["pix"][0]["horario"], "infoPagador": "Pedido 7781"}, body
    st, v, _ = s.api("GET", "/pix/cob/" + txid, None, A)
    assert v["status"] == "CONCLUIDA" and v["pix"][0]["endToEndId"] == e2e, v
    st, v, _ = s.api("GET", "/pix/pix/" + e2e, None, A)
    assert st == 200 and v["txid"] == txid and v["valor"] == "123.45", v
    st, v, _ = s.api("POST", "/pix/sandbox/pagamentos", {"txid": txid})
    assert st == 409, v

    # Refunds.
    st, d, _ = s.api("PUT", f"/pix/pix/{e2e}/devolucao/dev1", {"valor": "23.45", "descricao": "Item fora de estoque"}, A)
    assert st == 201 and d["status"] == "EM_PROCESSAMENTO" and d["rtrId"].startswith("D") and len(d["rtrId"]) == 32, d
    st, v, _ = s.api("GET", f"/pix/pix/{e2e}/devolucao/dev1", None, A)
    assert st == 200 and v["status"] == "DEVOLVIDO" and v["rtrId"] == d["rtrId"], v
    st, v, _ = s.api("PUT", f"/pix/pix/{e2e}/devolucao/dev2", {"valor": "999.00"}, A)
    assert st == 400 and v["type"].endswith("/DevolucaoOperacaoInvalida"), v
    g = wait(lambda: [x for x in hooks("/pix-hook/pix") if "devolucoes" in x["body"]], "refund webhook")[0]
    assert json.loads(g["body"])["pix"][0]["devolucoes"][0]["status"] == "DEVOLVIDO"

    # Due-date charge.
    cobv = {"calendario": {"dataDeVencimento": "2026-12-10", "validadeAposVencimento": 30}, "devedor": {"cnpj": "44555666000181", "nome": "Globex Comercio de Alimentos Ltda"},
            "valor": {"original": "567.89", "multa": {"modalidade": "2", "valorPerc": "2.00"}}, "chave": "acme@example.com"}
    st, v, _ = s.api("PUT", "/pix/cobv/acmeboletopix0001abcdefghij", cobv, A)
    assert st == 201 and v["loc"]["tipoCob"] == "cobv" and v["calendario"]["dataDeVencimento"] == "2026-12-10" and "/cobv/" in v["location"] and brcode_ok(v["pixCopiaECola"])[0], v
    st, v, _ = s.api("PUT", "/pix/cobv/acmeboletopix0002abcdefghij", dict(cobv, calendario={}), A)
    assert st == 400 and v["violacoes"][0]["propriedade"] == "cobv.calendario.dataDeVencimento", v

    s.call("POST", "/apis/pix/force", {"answer": "servico_indisponivel", "count": 1})
    st, v, _ = s.api("GET", "/pix/cob/" + txid, None, A)
    assert st == 503 and v["title"] == "Serviço Indisponível", v
    st, v, _ = s.api("DELETE", "/pix/webhook/acme@example.com", None, A)
    assert st == 204
    st, v, _ = s.api("GET", "/pix/webhook/acme@example.com", None, A)
    assert st == 404, v


# ---------------------------------------------------------------- Mercado Pago (official SDK when installed)
MP_SECRET = "mp-acme-secret-signature"


def mp_verified(g):
    """The notification as the SDK's WebhookSignatureValidator (or the docs' manifest) checks it."""
    q = urllib.parse.parse_qs(urllib.parse.urlparse(g["path"]).query)
    h = g["headers"]
    try:
        from mercadopago.webhook import WebhookSignatureValidator
        WebhookSignatureValidator.validate(h.get("x-signature"), h.get("x-request-id"), q["data.id"][0], MP_SECRET, tolerance_seconds=300)
        return True
    except ImportError:
        parts = dict(x.split("=", 1) for x in h["x-signature"].split(","))
        manifest = f"id:{q['data.id'][0]};request-id:{h['x-request-id']};ts:{parts['ts']};"
        return hmac.compare_digest(parts["v1"], hmac.new(MP_SECRET.encode(), manifest.encode(), hashlib.sha256).hexdigest())
    except Exception as e:
        print("mercado pago signature refused:", e)
        return False


def mp_hooks(pid, action):
    out = []
    for g in hooks("/mp"):
        b = json.loads(g["body"])
        if b["data"]["id"] == str(pid) and b["action"] == action:
            out.append(g)
    return out


def mercado_pago(s):
    install(s, "mercado-pago")
    settings(s, "mercado-pago", webhook_url=f"http://127.0.0.1:{HOOK}/mp")
    base = f"http://127.0.0.1:{s.APIS_PORT}/mercado-pago"
    try:
        import mercadopago
        from mercadopago.config.config import Config
        Config._Config__api_base_url = base
        sdk = mercadopago.SDK("TEST-2009873441-acme")
        def create(kind, obj, idem=None):
            ro = None
            if idem:
                from mercadopago.config import RequestOptions
                ro = RequestOptions(custom_headers={"x-idempotency-key": idem})
            r = getattr(sdk, kind)().create(obj, ro) if ro else getattr(sdk, kind)().create(obj)
            return r["status"], r["response"]
        def get(kind, i):
            r = getattr(sdk, kind)().get(i)
            return r["status"], r["response"]
        def refund(pid, obj):
            r = sdk.refund().create(pid, obj)
            return r["status"], r["response"]
        print("  mercado pago via the official SDK", mercadopago.__name__)
    except ImportError:
        A = {"Authorization": "Bearer TEST-2009873441-acme"}
        paths = {"payment": "/v1/payments", "card_token": "/v1/card_tokens", "preference": "/checkout/preferences"}
        def create(kind, obj, idem=None):
            st, v, _ = s.api("POST", "/mercado-pago" + paths[kind], obj, dict(A, **({"X-Idempotency-Key": idem} if idem else {})))
            return st, v
        def get(kind, i):
            st, v, _ = s.api("GET", f"/mercado-pago{paths[kind]}/{i}", None, A)
            return st, v
        def refund(pid, obj):
            st, v, _ = s.api("POST", f"/mercado-pago/v1/payments/{pid}/refunds", obj, A)
            return st, v

    payer = {"email": "emma.carter@example.com", "first_name": "Emma", "last_name": "Carter", "identification": {"type": "CPF", "number": "52998224725"}}
    st, p = create("payment", {"transaction_amount": 150.5, "description": "Pedido 7781", "payment_method_id": "pix", "payer": payer, "external_reference": "acme-7781"})
    assert st == 201 and p["status"] == "pending" and p["status_detail"] == "pending_waiting_transfer" and p["payment_type_id"] == "bank_transfer", p
    qr = p["point_of_interaction"]["transaction_data"]["qr_code"]
    ok, f = brcode_ok(qr)
    assert ok and f["54"] == "150.50" and p["point_of_interaction"]["transaction_data"]["qr_code_base64"], f
    g = wait(lambda: mp_hooks(p["id"], "payment.created"), "payment.created")[0]
    assert g["path"].startswith("/mp?data.id=") and "type=payment" in g["path"] and mp_verified(g), g
    st, v = get("payment", p["id"])
    assert st == 200 and v["id"] == p["id"] and v["status"] == "pending", v
    s.call("POST", "/clock/advance", {"by": "31s"})
    g = wait(lambda: mp_hooks(p["id"], "payment.updated"), "payment.updated")[0]
    assert mp_verified(g)
    st, v = get("payment", p["id"])
    assert v["status"] == "approved" and v["status_detail"] == "accredited" and v["date_approved"], v
    s.call("POST", "/clock/advance", {"reset": True})

    # Cards: the test cardholder name decides.
    card = {"card_number": "5031433215406351", "expiration_month": 11, "expiration_year": 2030, "security_code": "123", "cardholder": {"name": "APRO", "identification": {"type": "CPF", "number": "52998224725"}}}
    st, t = create("card_token", card)
    assert st == 201 and len(t["id"]) == 32 and t["last_four_digits"] == "6351", t
    st, c = create("payment", {"transaction_amount": 89.9, "token": t["id"], "installments": 1, "payment_method_id": "master", "payer": payer})
    assert st == 201 and c["status"] == "approved" and c["status_detail"] == "accredited" and c["card"]["last_four_digits"] == "6351", c
    st, t2 = create("card_token", dict(card, cardholder={"name": "FUND"}))
    st, r = create("payment", {"transaction_amount": 89.9, "token": t2["id"], "installments": 1, "payment_method_id": "master", "payer": payer})
    assert st == 201 and r["status"] == "rejected" and r["status_detail"] == "cc_rejected_insufficient_amount", r

    # Idempotency: the same key gets the same payment back.
    st1, a = create("payment", {"transaction_amount": 10, "payment_method_id": "pix", "payer": payer}, idem="acme-7790")
    st2, b = create("payment", {"transaction_amount": 10, "payment_method_id": "pix", "payer": payer}, idem="acme-7790")
    assert st1 == st2 == 201 and a["id"] == b["id"], (a["id"], b["id"])

    # Errors.
    st, v = create("payment", {"payment_method_id": "pix", "payer": payer})
    assert st == 400 and v["cause"][0]["code"] == 4037 and v["error"] == "bad_request", v
    st, v = get("payment", 1999999999)
    assert st == 404 and v["cause"][0]["code"] == 2000, v
    st, v, _ = s.api("GET", f"/mercado-pago/v1/payments/{p['id']}", None, {"Authorization": "Bearer invalid"})
    assert st == 401 and v["message"] == "invalid access token", v

    # Refunds.
    st, rf = refund(c["id"], {"amount": 9.9})
    assert st == 201 and rf["payment_id"] == c["id"] and rf["amount"] == 9.9 and rf["status"] == "approved", rf
    st, rf = refund(c["id"], None)
    st, v = get("payment", c["id"])
    assert v["status"] == "refunded" and v["transaction_amount_refunded"] == 89.9, v
    assert wait(lambda: [g for g in mp_hooks(c["id"], "payment.updated") if mp_verified(g)], "refund webhook")
    st, v = refund(r["id"], None)
    assert st == 400, v

    # Checkout Pro.
    st, pref = create("preference", {"items": [{"title": "Plano Pro", "quantity": 1, "unit_price": 99.9, "currency_id": "BRL"}], "back_urls": {"success": "https://shop.example.com/ok"}, "external_reference": "acme-7800"})
    assert st == 201 and pref["init_point"].endswith("pref_id=" + pref["id"]) and pref["sandbox_init_point"].startswith("https://sandbox.mercadopago.com.br/"), pref
    st, v = get("preference", pref["id"])
    assert st == 200 and v["items"][0]["title"] == "Plano Pro", v
    st, v = create("preference", {"items": []})
    assert st == 400, v

    s.call("POST", "/apis/mercado-pago/force", {"answer": "internal_error", "count": 1})
    st, v = get("payment", p["id"])
    assert st == 500 and v["error"] == "internal_server_error", v


# ---------------------------------------------------------------- Pagar.me v5
def pagarme_hooks(typ):
    return [(g, json.loads(g["body"])) for g in hooks("/pagarme") if json.loads(g["body"])["type"] == typ]


def pagarme(s):
    install(s, "pagarme")
    settings(s, "pagarme", webhook_url=f"http://127.0.0.1:{HOOK}/pagarme", webhook_user="acme-hooks", webhook_password="hook-pass-1")
    K = {"Authorization": "Basic " + base64.b64encode(b"sk_test_acme0001:").decode()}
    pg = lambda m, path, body=None: s.api(m, "/pagarme/core/v5" + path, body, K)[:2]
    # v5 webhooks carry no signature: the Basic credential configured on the webhook is what the app checks.
    hook_ok = lambda g: g["headers"].get("authorization") == "Basic " + base64.b64encode(b"acme-hooks:hook-pass-1").decode()

    st, cus = pg("POST", "/customers", {"name": "Emma Carter Souza", "email": "emma.carter@example.com", "document": "52998224725", "type": "individual", "phones": {"mobile_phone": {"country_code": "55", "area_code": "11", "number": "999990001"}}})
    assert st == 200 and cus["id"].startswith("cus_") and cus["document_type"] == "CPF", cus
    assert pg("GET", "/customers/" + cus["id"])[1]["email"] == "emma.carter@example.com"
    st, v = pg("POST", "/customers", {"name": "X", "document": "52998224700"})
    assert st == 422 and "customer.document" in v["errors"], v

    items = [{"amount": 2990, "description": "Plano Pro", "quantity": 1, "code": "pro"}, {"amount": 1000, "description": "Setup", "quantity": 2, "code": "setup"}]
    st, o = pg("POST", "/orders", {"items": items, "customer_id": cus["id"], "payments": [{"payment_method": "pix", "pix": {"expires_in": 3600}}]})
    ch = o["charges"][0]
    assert st == 200 and o["id"].startswith("or_") and o["amount"] == 4990 and o["status"] == "pending" and ch["status"] == "pending" and ch["last_transaction"]["status"] == "waiting_payment", o
    ok, f = brcode_ok(ch["last_transaction"]["qr_code"])
    assert ok and f["54"] == "49.90", f
    g, e = wait(lambda: pagarme_hooks("order.created"), "order.created")[0]
    assert e["data"]["id"] == o["id"] and e["id"].startswith("hook_") and hook_ok(g), (e, g["headers"])
    assert wait(lambda: pagarme_hooks("charge.created"), "charge.created")[0][1]["data"]["id"] == ch["id"]
    st, v = pg("POST", f"/charges/{ch['id']}/confirm-payment", {})
    assert st == 200 and v["status"] == "paid", v
    g, e = wait(lambda: pagarme_hooks("charge.paid"), "charge.paid")[0]
    assert e["data"]["id"] == ch["id"] and e["data"]["status"] == "paid" and hook_ok(g), e
    assert wait(lambda: pagarme_hooks("order.paid"), "order.paid")
    assert pg("GET", "/orders/" + o["id"])[1]["status"] == "paid"
    assert pg("GET", "/charges/" + ch["id"])[1]["last_transaction"]["status"] == "paid"
    assert pg("POST", f"/charges/{ch['id']}/confirm-payment", {})[0] == 412

    st, b = pg("POST", "/orders", {"items": items[:1], "customer": {"name": "Globex Comercio de Alimentos Ltda", "email": "billing@globex.example.com", "document": "44555666000181", "type": "company"},
                                   "payments": [{"payment_method": "boleto", "boleto": {"instructions": "Pagar até o vencimento", "due_at": "2026-12-10T00:00:00Z"}}]})
    tx = b["charges"][0]["last_transaction"]
    assert st == 200 and tx["transaction_type"] == "boleto" and tx["status"] == "generated" and tx["pdf"].endswith("/pdf") and len(tx["line"].replace(" ", "").replace(".", "")) == 47, tx
    st, v = pg("DELETE", "/charges/" + b["charges"][0]["id"])
    assert st == 200 and v["status"] == "canceled", v
    assert wait(lambda: pagarme_hooks("charge.canceled"), "charge.canceled")

    card = {"number": "4000000000000010", "holder_name": "EMMA C SOUZA", "exp_month": 12, "exp_year": 2030, "cvv": "123"}
    st, c = pg("POST", "/orders", {"items": items[:1], "customer_id": cus["id"], "payments": [{"payment_method": "credit_card", "credit_card": {"installments": 1, "statement_descriptor": "ACME", "card": card}}]})
    assert st == 200 and c["status"] == "paid" and c["charges"][0]["last_transaction"]["status"] == "captured" and c["charges"][0]["last_transaction"]["card"]["last_four_digits"] == "0010", c
    st, d = pg("POST", "/orders", {"items": items[:1], "customer_id": cus["id"], "payments": [{"payment_method": "credit_card", "credit_card": {"card": dict(card, number="4000000000000028")}}]})
    assert st == 200 and d["status"] == "failed" and d["charges"][0]["last_transaction"]["status"] == "not_authorized", d
    g, e = wait(lambda: pagarme_hooks("charge.payment_failed"), "charge.payment_failed")[0]
    assert e["data"]["id"] == d["charges"][0]["id"] and hook_ok(g)
    st, v = pg("DELETE", "/charges/" + c["charges"][0]["id"])
    assert v["status"] == "canceled" and wait(lambda: pagarme_hooks("charge.refunded"), "charge.refunded")

    st, v = pg("POST", "/orders", {"customer_id": cus["id"], "payments": [{"payment_method": "pix"}]})
    assert st == 422 and v["errors"] == {"order.items": ["The items field is required."]}, v
    st, v, _ = s.api("GET", "/pagarme/core/v5/orders/" + o["id"], None, {"Authorization": "Basic " + base64.b64encode(b"pk_test_x:").decode()})
    assert st == 401 and v["message"] == "Authorization has been denied for this request.", v
    assert pg("GET", "/orders/or_nobody")[0] == 404
    s.call("POST", "/apis/pagarme/force", {"answer": "too_many_requests", "count": 1})
    assert pg("GET", "/orders/" + o["id"])[0] == 429


# ---------------------------------------------------------------- Asaas
def asaas_hooks(event, pid=None):
    return [(g, json.loads(g["body"])) for g in hooks("/asaas") if json.loads(g["body"])["event"] == event and (pid is None or json.loads(g["body"])["payment"]["id"] == pid)]


def asaas(s):
    install(s, "asaas")
    settings(s, "asaas", webhook_url=f"http://127.0.0.1:{HOOK}/asaas")
    K = {"access_token": "$aact_YTU5YTE0M2M2N2I4MTliNzk0YTI5N2U5MzdjNWZmNDQ6OjAwMDAwMDAwMDAwMDAwMDAwMDA6OiRhYWNoXzAwMDAwMDAw"}
    a = lambda m, path, body=None: s.api(m, "/asaas/v3" + path, body, K)[:2]
    # Asaas authenticates webhooks with the token chosen for the webhook, in asaas-access-token.
    hook_ok = lambda g: hmac.compare_digest(g["headers"].get("asaas-access-token", ""), "whk-acme-asaas-0001")

    st, cus = a("POST", "/customers", {"name": "Emma Carter Souza", "cpfCnpj": "529.982.247-25", "email": "emma.carter@example.com", "postalCode": "01310-100", "addressNumber": "120"})
    assert st == 200 and cus["object"] == "customer" and cus["cpfCnpj"] == "52998224725" and cus["personType"] == "FISICA", cus
    assert a("GET", "/customers/" + cus["id"])[1]["name"] == "Emma Carter Souza"
    st, v = a("POST", "/customers", {"name": "X", "cpfCnpj": "52998224700"})
    assert st == 400 and v["errors"][0]["code"] == "invalid_cpfCnpj", v

    st, p = a("POST", "/payments", {"customer": cus["id"], "billingType": "PIX", "value": 129.9, "dueDate": "2026-11-20", "description": "Pedido 7781", "externalReference": "acme-7781"})
    assert st == 200 and p["id"].startswith("pay_") and p["status"] == "PENDING" and p["billingType"] == "PIX" and p["invoiceUrl"].startswith("https://sandbox.asaas.com/i/"), p
    g, e = wait(lambda: asaas_hooks("PAYMENT_CREATED", p["id"]), "PAYMENT_CREATED")[0]
    assert hook_ok(g) and e["payment"]["value"] == 129.9 and e["id"].startswith("evt_"), e
    st, q = a("GET", f"/payments/{p['id']}/pixQrCode")
    ok, f = brcode_ok(q["payload"])
    assert st == 200 and ok and f["54"] == "129.90" and q["encodedImage"], q
    st, v = a("POST", f"/sandbox/payment/{p['id']}/confirm", {})
    assert st == 200 and v["status"] == "RECEIVED" and v["paymentDate"], v
    g, e = wait(lambda: asaas_hooks("PAYMENT_RECEIVED", p["id"]), "PAYMENT_RECEIVED")[0]
    assert hook_ok(g) and e["payment"]["status"] == "RECEIVED", e
    assert a("GET", "/payments/" + p["id"])[1]["status"] == "RECEIVED"
    st, v = a("POST", f"/payments/{p['id']}/refund", {})
    assert st == 200 and v["status"] == "REFUNDED", v
    assert hook_ok(wait(lambda: asaas_hooks("PAYMENT_REFUNDED", p["id"]), "PAYMENT_REFUNDED")[0][0])

    st, b = a("POST", "/payments", {"customer": cus["id"], "billingType": "BOLETO", "value": 59.9, "dueDate": "2026-11-25"})
    assert st == 200 and b["bankSlipUrl"] and b["nossoNumero"], b
    st, v = a("GET", f"/payments/{b['id']}/identificationField")
    assert st == 200 and len(v["identificationField"]) == 47 and len(v["barCode"]) == 44, v
    assert a("GET", f"/payments/{b['id']}/pixQrCode")[0] == 400

    holder = {"name": "Emma Carter Souza", "email": "emma.carter@example.com", "cpfCnpj": "52998224725", "postalCode": "01310-100", "addressNumber": "120", "phone": "11999990001"}
    st, c = a("POST", "/payments", {"customer": cus["id"], "billingType": "CREDIT_CARD", "value": 89.9, "dueDate": "2026-11-15",
                                    "creditCard": {"holderName": "EMMA C SOUZA", "number": "5162306219378829", "expiryMonth": "05", "expiryYear": "2030", "ccv": "318"}, "creditCardHolderInfo": holder})
    assert st == 200 and c["status"] == "CONFIRMED" and c["creditCard"]["creditCardNumber"] == "8829", c
    assert hook_ok(wait(lambda: asaas_hooks("PAYMENT_CONFIRMED", c["id"]), "PAYMENT_CONFIRMED")[0][0])
    st, v = a("POST", "/payments", {"customer": cus["id"], "billingType": "CREDIT_CARD", "value": 89.9, "dueDate": "2026-11-15",
                                    "creditCard": {"holderName": "X", "number": "4000000000000002", "expiryMonth": "05", "expiryYear": "2030", "ccv": "318"}, "creditCardHolderInfo": holder})
    assert st == 400 and v["errors"][0]["code"] == "invalid_creditCard", v

    st, v = a("POST", "/payments", {"customer": cus["id"], "billingType": "PIX", "dueDate": "2026-11-20"})
    assert st == 400 and v["errors"][0]["code"] == "invalid_value", v
    st, v, _ = s.api("GET", "/asaas/v3/payments/" + p["id"])
    assert st == 401 and v["errors"][0]["code"] == "access_token_not_found", v
    assert a("GET", "/payments/pay_nobody")[0] == 404
    s.call("POST", "/apis/asaas/force", {"answer": "too_many_requests", "count": 1})
    assert a("GET", "/payments/" + p["id"])[0] == 429


# ---------------------------------------------------------------- ViaCEP and BrasilAPI
def viacep(s):
    install(s, "viacep")
    st, v, _ = s.api("GET", "/viacep/ws/01310100/json/")
    assert st == 200 and v["logradouro"] == "Rua das Palmeiras" and v["localidade"] == "São Paulo" and v["ibge"] == "3550308" and v["cep"] == "01310-100", v
    st, v, _ = s.api("GET", "/viacep/ws/20040002/json")
    assert v["uf"] == "RJ" and v["ddd"] == "21", v
    st, v, _ = s.api("GET", "/viacep/ws/99999999/json/")
    assert st == 200 and v == {"erro": "true"}, v
    st, v, h = s.api("GET", "/viacep/ws/01310-100/json/")
    assert st == 400 and h["content-type"].startswith("text/html"), (st, v)
    s.call("POST", "/apis/viacep/force", {"answer": "unavailable", "count": 1})
    assert s.api("GET", "/viacep/ws/01310100/json/")[0] == 503


def brasilapi(s):
    install(s, "brasilapi")
    b = lambda path: s.api("GET", "/brasilapi/api" + path)[:2]
    st, v = b("/cep/v2/01310-100")
    assert st == 200 and v["street"] == "Rua das Palmeiras" and v["location"]["coordinates"]["latitude"] == "-23.5643", v
    st, v = b("/cep/v1/80010000")
    assert st == 200 and v["city"] == "Curitiba" and "location" not in v, v
    st, v = b("/cep/v2/1234")
    assert st == 400 and v["type"] == "validation_error", v
    st, v = b("/cep/v2/99999999")
    assert st == 404 and v["name"] == "CepPromiseError", v
    st, v = b("/cnpj/v1/11222333000181")
    assert st == 200 and v["razao_social"] == "ACME TECNOLOGIA LTDA" and v["descricao_situacao_cadastral"] == "ATIVA", v
    assert b("/cnpj/v1/11222333000100")[0] == 400
    assert b("/cnpj/v1/11444777000161")[0] == 404
    st, v = b("/banks/v1")
    assert st == 200 and {"ispb": "00000000", "name": "BCO DO BRASIL S.A.", "code": 1, "fullName": "Banco do Brasil S.A."} in v, v
    assert b("/banks/v1/237")[1]["ispb"] == "60746948" and b("/banks/v1/999")[0] == 404
    st, v = b("/feriados/v1/2026")
    days = {x["name"]: x["date"] for x in v}
    assert st == 200 and days["Páscoa"] == "2026-04-05" and days["Carnaval"] == "2026-02-17" and days["Sexta-feira Santa"] == "2026-04-03" and days["Corpus Christi"] == "2026-06-04" and days["Dia da consciência negra"] == "2026-11-20", days
    days = {x["name"]: x["date"] for x in b("/feriados/v1/2019")[1]}
    assert days["Páscoa"] == "2019-04-21" and days["Carnaval"] == "2019-03-05" and "Dia da consciência negra" not in days, days
    assert {x["name"]: x["date"] for x in b("/feriados/v1/1950")[1]}["Páscoa"] == "1950-04-09"
    assert b("/feriados/v1/2300")[0] == 404
    st, v = b("/ddd/v1/21")
    assert st == 200 and v["state"] == "RJ" and "RIO DE JANEIRO" in v["cities"], v
    assert b("/ddd/v1/00")[0] == 404
    s.call("POST", "/apis/brasilapi/force", {"answer": "rate_limited", "count": 1})
    assert b("/ddd/v1/11")[0] == 429


# ---------------------------------------------------------------- Correios
def correios(s):
    install(s, "correios")
    basic = {"Authorization": "Basic " + base64.b64encode(b"acme-correios:codigo-api-0001").decode()}
    st, t, _ = s.api("POST", "/correios/token/v1/autentica/cartaopostagem", {"numero": "0067599079"}, basic)
    assert st == 201 and t["ambiente"] == "HOMOLOGACAO" and t["cartaoPostagem"]["numero"] == "0067599079" and t["token"].count(".") == 2, t
    assert s.api("POST", "/correios/token/v1/autentica/cartaopostagem", {"numero": "1"})[0] == 401
    A = {"Authorization": "Bearer " + t["token"]}
    c = lambda path: s.api("GET", "/correios" + path, None, A)[:2]
    st, v = c("/preco/v1/nacional/03220?cepOrigem=01310100&cepDestino=20040002&psObjeto=1500&tpObjeto=2&comprimento=20&largura=15&altura=10")
    assert st == 200 and v["pcFinal"] == "46,40" and v["coProduto"] == "03220", v
    st, v = c("/preco/v1/nacional/03298?cepOrigem=01310100&cepDestino=04538000&psObjeto=300")
    assert v["pcFinal"] == "18,90", v
    st, v = c("/prazo/v1/nacional/03298?cepOrigem=01310100&cepDestino=80010000")
    assert st == 200 and v["prazoEntrega"] == 9 and v["dataMaxima"].endswith("T23:59:00"), v
    st, v = c("/preco/v1/nacional/03220?cepOrigem=01310100")
    assert st == 400 and v["msgs"][0].startswith("PRC-103"), v
    assert c("/preco/v1/nacional/04227?cepOrigem=01310100&cepDestino=20040002&psObjeto=900")[1]["msgs"][0].startswith("PRC-110")
    st, v = c("/srorastro/v1/objetos/AA123456785BR")
    ev = v["objetos"][0]["eventos"]
    assert st == 200 and ev[0]["codigo"] == "BDE" and ev[-1]["codigo"] == "PO" and len(ev) == 5, v
    st, v = c("/srorastro/v1/objetos/AB987654326BR?resultado=U")
    assert len(v["objetos"][0]["eventos"]) == 1 and v["objetos"][0]["eventos"][0]["descricao"].startswith("Objeto em transferência"), v
    st, v = c("/srorastro/v1/objetos/QQ000000000BR")
    assert st == 200 and v["objetos"][0]["mensagem"].startswith("SRO-020"), v
    st, v = c("/cep/v2/enderecos/20040002")
    assert st == 200 and v["localidade"] == "Rio de Janeiro", v
    assert c("/cep/v2/enderecos/99999999")[0] == 404
    assert s.api("GET", "/correios/srorastro/v1/objetos/AA123456785BR", None, {"Authorization": "Bearer forged"})[0] == 401
    s.call("POST", "/clock/advance", {"by": "25h"})
    assert c("/srorastro/v1/objetos/AA123456785BR")[0] == 401, "the token lasts 24 h"
    s.call("POST", "/clock/advance", {"reset": True})
    s.call("POST", "/apis/correios/force", {"answer": "unavailable", "count": 1})
    assert c("/srorastro/v1/objetos/AA123456785BR")[0] == 503


# ---------------------------------------------------------------- Melhor Envio
ME_SECRET = "me-acme-client-secret"


def me_hooks(event):
    return [(g, json.loads(g["body"])) for g in hooks("/melhor-envio") if json.loads(g["body"])["event"] == event]


def me_verified(g):
    want = base64.b64encode(hmac.new(ME_SECRET.encode(), g["body"].encode(), hashlib.sha256).digest()).decode()
    return hmac.compare_digest(g["headers"].get("x-me-signature", ""), want)


def melhor_envio(s):
    install(s, "melhor-envio")
    settings(s, "melhor-envio", webhook_url=f"http://127.0.0.1:{HOOK}/melhor-envio")
    st, _, h = s.api("GET", "/melhor-envio/oauth/authorize?client_id=4821&redirect_uri=" + urllib.parse.quote("https://shop.example.com/me/callback") + "&response_type=code&state=xyz&scope=cart-read%20cart-write")
    assert st == 302 and h["location"].startswith("https://shop.example.com/me/callback?code=def50200") and h["location"].endswith("&state=xyz"), h
    code = urllib.parse.parse_qs(urllib.parse.urlparse(h["location"]).query)["code"][0]
    form = {"Content-Type": "application/x-www-form-urlencoded"}
    st, v, _ = s.api("POST", "/melhor-envio/oauth/token", urllib.parse.urlencode({"grant_type": "authorization_code", "client_id": "4821", "client_secret": ME_SECRET, "redirect_uri": "https://shop.example.com/me/callback", "code": "def50200forged"}).encode(), form)
    assert st == 400 and v["error"] == "invalid_grant", v
    st, tok, _ = s.api("POST", "/melhor-envio/oauth/token", urllib.parse.urlencode({"grant_type": "authorization_code", "client_id": "4821", "client_secret": ME_SECRET, "redirect_uri": "https://shop.example.com/me/callback", "code": code}).encode(), form)
    assert st == 200 and tok["token_type"] == "Bearer" and tok["refresh_token"].startswith("def50200"), tok
    st, tok2, _ = s.api("POST", "/melhor-envio/oauth/token", {"grant_type": "refresh_token", "client_id": "4821", "client_secret": ME_SECRET, "refresh_token": tok["refresh_token"]})
    assert st == 200 and tok2["access_token"] != tok["access_token"], tok2
    A = {"Authorization": "Bearer " + tok2["access_token"], "Accept": "application/json", "User-Agent": "Acme Shop (dev@acme.example.com)"}
    me = lambda path, body: s.api("POST", "/melhor-envio/api/v2/me" + path, body, A)[:2]

    st, q = me("/shipment/calculate", {"from": {"postal_code": "01310100"}, "to": {"postal_code": "20040002"}, "package": {"height": 4, "width": 12, "length": 17, "weight": 1.2}})
    by = {x["name"]: x for x in q}
    assert st == 200 and by["PAC"]["price"] == "31.00" and by["PAC"]["delivery_range"] == {"min": 8, "max": 9} and by["SEDEX"]["delivery_time"] == 4 and by[".Com"]["error"] and by["Mini Envios"]["error"] == "Peso excedido.", q
    st, v = me("/shipment/calculate", {"to": {"postal_code": "20040002"}, "package": {"weight": 1}})
    assert st == 422 and "from.postal_code" in v["errors"], v
    assert s.api("POST", "/melhor-envio/api/v2/me/shipment/calculate", {}, {"Authorization": "Bearer forged"})[1] == {"message": "Unauthenticated."}

    addr = lambda name, cep, doc: {"name": name, "phone": "11999990001", "email": "ops@acme.example.com", "document": doc, "address": "Rua das Palmeiras", "number": "120", "district": "Jardim Exemplo", "city": "São Paulo", "state_abbr": "SP", "postal_code": cep}
    st, o = me("/cart", {"service": 2, "from": addr("Acme Tecnologia", "01310100", "52998224725"), "to": dict(addr("Liam Foster Lima", "20040002", "11144477735"), city="Rio de Janeiro", state_abbr="RJ"),
                         "products": [{"name": "Caneca", "quantity": 1, "unitary_value": 49.9}], "volumes": [{"height": 4, "width": 12, "length": 17, "weight": 1.2}], "options": {"insurance_value": 49.9, "receipt": False, "own_hand": False}})
    assert st == 201 and o["status"] == "pending" and o["price"] == 44.2 and o["protocol"].startswith("ORD-"), o
    g, e = wait(lambda: me_hooks("order.created"), "order.created")[0]
    assert me_verified(g) and e["data"]["id"] == o["id"], (g["headers"], e)
    st, v = me("/shipment/generate", {"orders": [o["id"]]})
    assert st == 422, v
    st, v = me("/shipment/checkout", {"orders": [o["id"]]})
    assert st == 200 and v["purchase"]["status"] == "paid" and v["purchase"]["orders"][0]["status"] == "released", v
    assert me_verified(wait(lambda: me_hooks("order.released"), "order.released")[0][0])
    st, v = me("/shipment/generate", {"orders": [o["id"]]})
    assert st == 200 and v[o["id"]]["status"] is True, v
    g, e = wait(lambda: me_hooks("order.generated"), "order.generated")[0]
    assert me_verified(g) and e["data"]["self_tracking"].startswith("ME"), e
    st, v = me("/shipment/print", {"mode": "private", "orders": [o["id"]]})
    assert st == 200 and v["url"].startswith("https://sandbox.melhorenvio.com.br/imprimir/"), v
    st, v = me("/shipment/tracking", {"orders": [o["id"]]})
    assert v[o["id"]]["status"] == "generated", v
    s.call("POST", "/clock/advance", {"by": "3h"})
    g, e = wait(lambda: me_hooks("order.posted"), "order.posted")[0]
    assert me_verified(g) and e["data"]["status"] == "posted"
    assert me("/shipment/tracking", {"orders": [o["id"]]})[1][o["id"]]["status"] == "posted"
    s.call("POST", "/clock/advance", {"by": "2d"})
    assert me_verified(wait(lambda: me_hooks("order.delivered"), "order.delivered")[0][0])
    st, v, _ = s.api("GET", "/melhor-envio/api/v2/me/orders/" + o["id"], None, A)
    assert st == 200 and v["status"] == "delivered" and v["delivered_at"], v
    s.call("POST", "/clock/advance", {"reset": True})
    st, v = me("/cart", {"service": 99, "from": addr("A", "01310100", "52998224725"), "to": addr("B", "20040002", "11144477735")})
    assert st == 422 and "service" in v["errors"], v
    s.call("POST", "/apis/melhor-envio/force", {"answer": "too_many_requests", "count": 1})
    assert me("/shipment/tracking", {"orders": [o["id"]]})[0] == 429


# ---------------------------------------------------------------- NFS-e Nacional (mTLS e-CNPJ)
NFSE_HOST = "sefin.producaorestrita.nfse.gov.br"


def pem_files(cert):
    d = tempfile.mkdtemp()
    c, k = os.path.join(d, "c.pem"), os.path.join(d, "k.pem")
    open(c, "w").write(cert["certificate"])
    open(k, "w").write(cert["key"])
    return c, k


def tls(s, cert, method, path, body=None, host=NFSE_HOST, headers=None):
    ctx = ssl.create_default_context(cafile=s.ca)
    if cert:
        ctx.load_cert_chain(*cert)
    c = http.client.HTTPSConnection("127.0.0.1", s.APIS_TLS_PORT, context=ctx, timeout=20)
    c.sock = ctx.wrap_socket(socket.create_connection(("127.0.0.1", s.APIS_TLS_PORT)), server_hostname=host)
    h = dict({"Host": host}, **(headers or {}))
    data = None
    if body is not None:
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        h.setdefault("Content-Type", "application/json")
    c.request(method, path, data, h)
    r = c.getresponse()
    raw = r.read().decode()
    try:
        return r.status, json.loads(raw)
    except ValueError:
        return r.status, raw


def test_ca(s):
    if not getattr(s, "ca", None):
        fd, s.ca = tempfile.mkstemp(suffix=".pem")
        os.write(fd, s.call("GET", "/apis/test-ca")["pem"].encode())
        os.close(fd)
        s.acme = pem_files(s.call("POST", "/apis/test-certificates", {"holder": "Acme Tecnologia Ltda", "cnpj": "11.222.333/0001-81"}))
        s.globex = pem_files(s.call("POST", "/apis/test-certificates", {"holder": "Globex Comercio Ltda", "cnpj": "44.555.666/0001-81"}))


def gz(text):
    return base64.b64encode(gzip.compress(text.encode())).decode()


def gunz(b64):
    return gzip.decompress(base64.b64decode(b64)).decode()


def dps_xml(n, cnpj="11222333000181", tp_amb="2", v="1500.00", id_ok=True):
    did = f"DPS3550308" + "2" + cnpj + "00900" + str(n).zfill(15)
    if not id_ok:
        did = did[:-1] + "9"
    return (f'<?xml version="1.0" encoding="UTF-8"?><DPS xmlns="http://www.sped.fazenda.gov.br/nfse" versao="1.00"><infDPS Id="{did}"><tpAmb>{tp_amb}</tpAmb>'
            f'<dhEmi>2026-11-15T10:00:00-03:00</dhEmi><verAplic>acme-erp 4.2</verAplic><serie>900</serie><nDPS>{n}</nDPS><dCompet>2026-11-15</dCompet><tpEmit>1</tpEmit><cLocEmi>3550308</cLocEmi>'
            f'<prest><CNPJ>{cnpj}</CNPJ><regTrib><opSimpNac>1</opSimpNac><regEspTrib>0</regEspTrib></regTrib></prest>'
            f'<toma><CPF>52998224725</CPF><xNome>Emma Carter Souza</xNome><end><endNac><cMun>3550308</cMun><CEP>01310100</CEP></endNac><xLgr>Rua das Palmeiras</xLgr><nro>120</nro><xBairro>Jardim Exemplo</xBairro></end><email>emma.carter@example.com</email></toma>'
            f'<serv><locPrest><cLocPrestacao>3550308</cLocPrestacao></locPrest><cServ><cTribNac>010101</cTribNac><xDescServ>Desenvolvimento de software sob encomenda</xDescServ></cServ></serv>'
            f'<valores><vServPrest><vServ>{v}</vServ></vServPrest><trib><tribMun><tribISSQN>1</tribISSQN><pAliq>2.50</pAliq><tpRetISSQN>1</tpRetISSQN></tribMun></trib></valores></infDPS></DPS>'), did


def chave_dv_ok(k):
    w, total = 2, 0
    for ch in reversed(k[:49]):
        total += int(ch) * w
        w = 2 if w == 9 else w + 1
    r = 11 - total % 11
    return len(k) == 50 and int(k[49]) == (0 if r >= 10 else r)


def nfse(s):
    install(s, "nfse")
    test_ca(s)
    base = "/SefinNacional"
    xml, did = dps_xml(123)
    st, v = tls(s, s.acme, "POST", base + "/nfse", {"dpsXmlGZipB64": gz(xml)})
    assert st == 201 and v["idDps"] == did and v["tipoAmbiente"] == 2 and chave_dv_ok(v["chaveAcesso"]), (st, v)
    chave = v["chaveAcesso"]
    nf = gunz(v["nfseXmlGZipB64"])
    assert f'<infNFSe Id="NFS{chave}">' in nf and "<vISSQN>37.50</vISSQN>" in nf and "<cStat>100</cStat>" in nf and did in nf, nf
    st, v = tls(s, s.acme, "GET", f"{base}/nfse/{chave}")
    assert st == 200 and v["chaveAcesso"] == chave and gunz(v["nfseXmlGZipB64"]) == nf, v
    st, v = tls(s, s.acme, "GET", f"{base}/dps/{did}")
    assert st == 200 and v["chaveAcesso"] == chave, v
    # Refusals: duplicate, wrong environment, Id, the certificate's CNPJ, not gzip.
    assert tls(s, s.acme, "POST", base + "/nfse", {"dpsXmlGZipB64": gz(xml)})[1]["erros"][0]["Codigo"] == "E0014"
    assert tls(s, s.acme, "POST", base + "/nfse", {"dpsXmlGZipB64": gz(dps_xml(124, tp_amb="1")[0])})[1]["erros"][0]["Codigo"] == "E0004"
    assert tls(s, s.acme, "POST", base + "/nfse", {"dpsXmlGZipB64": gz(dps_xml(125, id_ok=False)[0])})[1]["erros"][0]["Codigo"] == "E0006"
    st, v = tls(s, s.globex, "POST", base + "/nfse", {"dpsXmlGZipB64": gz(dps_xml(126)[0])})
    assert st == 400 and v["erros"][0]["Codigo"] == "E0160", v
    assert tls(s, s.acme, "POST", base + "/nfse", {"dpsXmlGZipB64": base64.b64encode(xml.encode()).decode()})[1]["erros"][0]["Codigo"] == "E0001"
    assert tls(s, s.acme, "GET", f"{base}/nfse/{chave[:-1]}{(int(chave[-1]) + 1) % 10}")[0] == 404
    try:
        tls(s, None, "GET", f"{base}/nfse/{chave}")
        raise AssertionError("no client certificate: TLS should fail")
    except (ssl.SSLError, ConnectionError, http.client.HTTPException, OSError):
        pass
    # The cancel event.
    ped = (f'<pedRegEvento xmlns="http://www.sped.fazenda.gov.br/nfse" versao="1.00"><infPedReg Id="PRE{chave}101101001"><tpAmb>2</tpAmb><verAplic>acme-erp 4.2</verAplic>'
           f'<dhEvento>2026-11-15T11:00:00-03:00</dhEvento><CNPJAutor>11222333000181</CNPJAutor><chNFSe>{chave}</chNFSe><nPedRegEvento>1</nPedRegEvento>'
           f'<e101101><xDesc>Cancelamento de NFS-e</xDesc><cMotivo>1</cMotivo><xMotivo>Erro na emissão: valor do serviço</xMotivo></e101101></infPedReg></pedRegEvento>')
    st, v = tls(s, s.acme, "POST", f"{base}/nfse/{chave}/eventos", {"pedidoRegistroEventoXmlGZipB64": gz(ped)})
    assert st == 201 and f'Id="EVT{chave}101101001"' in gunz(v["eventoXmlGZipB64"]), (st, v)
    st, v = tls(s, s.acme, "POST", f"{base}/nfse/{chave}/eventos", {"pedidoRegistroEventoXmlGZipB64": gz(ped)})
    assert st == 400 and v["erro"][0]["Codigo"] == "E0844", v
    s.call("POST", "/apis/nfse/force", {"answer": "indisponivel", "count": 1})
    assert tls(s, s.acme, "GET", f"{base}/nfse/{chave}")[0] == 503


# ---------------------------------------------------------------- NFS-e ABRASF 2.04 (SOAP, mTLS)
ABRASF_HOST = "nfse-homologacao.example.gov.br"
CABEC = '<cabecalho xmlns="http://www.abrasf.org.br/nfse.xsd" versao="2.04"><versaoDados>2.04</versaoDados></cabecalho>'


def abrasf(s, cert, op, dados, v12=False):
    from xml.sax.saxutils import escape, unescape
    inner = f'<{op}Request xmlns="http://nfse.abrasf.org.br"><nfseCabecMsg>{escape(CABEC)}</nfseCabecMsg><nfseDadosMsg>{escape(dados)}</nfseDadosMsg></{op}Request>'
    if v12:
        env = f'<soap12:Envelope xmlns:soap12="http://www.w3.org/2003/05/soap-envelope"><soap12:Body>{inner}</soap12:Body></soap12:Envelope>'
        h = {"Content-Type": f'application/soap+xml; charset=utf-8; action="http://nfse.abrasf.org.br/{op}"'}
    else:
        env = f'<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body>{inner}</soap:Body></soap:Envelope>'
        h = {"Content-Type": "text/xml; charset=utf-8", "SOAPAction": f'"http://nfse.abrasf.org.br/{op}"'}
    st, body = tls(s, cert, "POST", "/NfseWSService", env.encode(), ABRASF_HOST, h)
    if "<outputXML>" not in body:
        return st, body
    return st, unescape(body.split("<outputXML>")[1].split("</outputXML>")[0])


def rps(n, cnpj="11222333000181", valor="1500.00", serie="A"):
    return (f'<GerarNfseEnvio xmlns="http://www.abrasf.org.br/nfse.xsd"><Rps><InfDeclaracaoPrestacaoServico Id="rps{n}"><Rps><IdentificacaoRps><Numero>{n}</Numero><Serie>{serie}</Serie><Tipo>1</Tipo></IdentificacaoRps>'
            f'<DataEmissao>2026-11-15</DataEmissao><Status>1</Status></Rps><Competencia>2026-11-15</Competencia><Servico><Valores><ValorServicos>{valor}</ValorServicos><Aliquota>2.00</Aliquota></Valores>'
            f'<IssRetido>2</IssRetido><ItemListaServico>01.01</ItemListaServico><Discriminacao>Smith &amp; Sons: consultoria</Discriminacao><CodigoMunicipio>3550308</CodigoMunicipio><ExigibilidadeISS>1</ExigibilidadeISS></Servico>'
            f'<Prestador><CpfCnpj><Cnpj>{cnpj}</Cnpj></CpfCnpj><InscricaoMunicipal>12345678</InscricaoMunicipal></Prestador><TomadorServico><IdentificacaoTomador><CpfCnpj><Cpf>52998224725</Cpf></CpfCnpj></IdentificacaoTomador>'
            f'<RazaoSocial>Emma Carter Souza</RazaoSocial></TomadorServico><OptanteSimplesNacional>2</OptanteSimplesNacional><IncentivoFiscal>2</IncentivoFiscal></InfDeclaracaoPrestacaoServico></Rps></GerarNfseEnvio>')


def nfse_abrasf(s):
    import xml.etree.ElementTree as ET
    install(s, "nfse-abrasf")
    test_ca(s)
    NS = {"n": "http://www.abrasf.org.br/nfse.xsd"}
    st, out = abrasf(s, s.acme, "GerarNfse", rps(41))
    doc = ET.fromstring(out.split("?>", 1)[1])
    num = doc.find(".//n:InfNfse/n:Numero", NS).text
    assert st == 200 and doc.find(".//n:ValorIss", NS).text == "30.00" and doc.find(".//n:Discriminacao", NS).text == "Smith & Sons: consultoria", out
    st, out = abrasf(s, s.acme, "GerarNfse", rps(41), v12=True)
    assert ET.fromstring(out.split("?>", 1)[1]).find(".//n:Codigo", NS).text == "E10", out
    st, out = abrasf(s, s.globex, "GerarNfse", rps(42))
    assert ET.fromstring(out.split("?>", 1)[1]).find(".//n:Codigo", NS).text == "E157", out
    cons = '<ConsultarNfseRpsEnvio xmlns="http://www.abrasf.org.br/nfse.xsd"><IdentificacaoRps><Numero>41</Numero><Serie>A</Serie><Tipo>1</Tipo></IdentificacaoRps><Prestador><CpfCnpj><Cnpj>11222333000181</Cnpj></CpfCnpj><InscricaoMunicipal>12345678</InscricaoMunicipal></Prestador></ConsultarNfseRpsEnvio>'
    st, out = abrasf(s, s.acme, "ConsultarNfsePorRps", cons)
    doc = ET.fromstring(out.split("?>", 1)[1])
    assert doc.find(".//n:InfNfse/n:Numero", NS).text == num and doc.find(".//n:NfseCancelamento", NS) is None, out
    st, out = abrasf(s, s.acme, "ConsultarNfsePorRps", cons.replace("<Numero>41<", "<Numero>49<"))
    assert ET.fromstring(out.split("?>", 1)[1]).find(".//n:Codigo", NS).text == "E4", out
    canc = f'<CancelarNfseEnvio xmlns="http://www.abrasf.org.br/nfse.xsd"><Pedido><InfPedidoCancelamento Id="pc{num}"><IdentificacaoNfse><Numero>{num}</Numero><CpfCnpj><Cnpj>11222333000181</Cnpj></CpfCnpj><InscricaoMunicipal>12345678</InscricaoMunicipal><CodigoMunicipio>3550308</CodigoMunicipio></IdentificacaoNfse><CodigoCancelamento>1</CodigoCancelamento></InfPedidoCancelamento></Pedido></CancelarNfseEnvio>'
    st, out = abrasf(s, s.acme, "CancelarNfse", canc)
    assert ET.fromstring(out.split("?>", 1)[1]).find(".//n:Confirmacao", NS) is not None, out
    st, out = abrasf(s, s.acme, "CancelarNfse", canc)
    assert ET.fromstring(out.split("?>", 1)[1]).find(".//n:Codigo", NS).text == "E79", out
    st, out = abrasf(s, s.acme, "ConsultarNfsePorRps", cons)
    assert ET.fromstring(out.split("?>", 1)[1]).find(".//n:NfseCancelamento", NS) is not None, out
    s.call("POST", "/apis/nfse-abrasf/force", {"answer": "indisponivel", "count": 1})
    st, out = abrasf(s, s.acme, "ConsultarNfsePorRps", cons)
    assert st == 500 and "Server was unable to process request." in out, (st, out)


def main():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", HOOK), Hooks)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    only = os.environ.get("ONLY", "").split(",") if os.environ.get("ONLY") else None
    with Stuntman("stuntman-brazil-") as s:
        cat = s.call("GET", "/plugins")
        cards = {p["name"]: p for p in cat["plugins"]}
        col = [c for c in cat["collections"] if c["name"] == "Brazil · payments, tax and shipping"][0]
        assert [x if isinstance(x, str) else x.get("name") for x in col["plugins"]] == NAMES, col
        assert all(cards[n]["region"] == "BR" for n in NAMES), {n: cards[n].get("region") for n in NAMES}
        for f in [pix, mercado_pago, pagarme, asaas, viacep, brasilapi, correios, melhor_envio, nfse, nfse_abrasf]:
            if only and f.__name__ not in only:
                continue
            f(s)
            print(f"{f.__name__}: ok")
    print("e2e_plugins_brazil: all passed")


if __name__ == "__main__":
    main()
