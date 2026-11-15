"""End-to-end check of the catalog plugins stripe, resend, onesignal and comtele: installed from the
catalog, validated, their main routes called as an app would, and their webhooks received here with
the Stripe and Svix signatures checked.

    cargo build -p stuntman && python3 tests/e2e_plugins_payments.py
"""
import base64, hashlib, hmac, http.server, json, os, threading, time, urllib.parse

from harness import Stuntman, free_port, wait

HOOK = free_port()
GOT = []
STRIPE_SECRET = "whsec_stripe_acme_test"
SVIX_KEY = b"resend-acme-signing-key"
RESEND_SECRET = "whsec_" + base64.b64encode(SVIX_KEY).decode()
PLUGINS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "plugins")


class Hooks(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        GOT.append({"path": self.path, "headers": {k.lower(): v for k, v in self.headers.items()}, "body": self.rfile.read(n).decode()})
        self.send_response(200); self.end_headers()

    def log_message(self, *a):
        pass


def events(path, typ):
    out = []
    for g in GOT:
        if g["path"] == path:
            b = json.loads(g["body"])
            if b.get("type") == typ:
                out.append((g, b))
    return out


def stripe_verified(g):
    sig = dict(x.split("=", 1) for x in g["headers"]["stripe-signature"].split(","))
    want = hmac.new(STRIPE_SECRET.encode(), f"{sig['t']}.{g['body']}".encode(), hashlib.sha256).hexdigest()
    return sig["v1"] == want and abs(int(sig["t"]) - time.time()) < 300


def svix_verified(g):
    h = g["headers"]
    want = base64.b64encode(hmac.new(SVIX_KEY, f"{h['svix-id']}.{h['svix-timestamp']}.{g['body']}".encode(), hashlib.sha256).digest()).decode()
    return h["svix-signature"] == "v1," + want


def main():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", HOOK), Hooks)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    with Stuntman("stuntman-payments-") as s:
        for name in ["stripe", "resend", "onesignal", "comtele"]:
            text = open(os.path.join(PLUGINS, name + ".yaml")).read()
            v = s.call("POST", f"/plugins/{name}/validate", {"yaml": text})
            assert v["ok"] and v["errors"] == [], (name, v)
            s.call("POST", "/plugins", {"plugin": name})
        cards = {p["name"]: p for p in s.call("GET", "/plugins")["plugins"]}
        assert cards["comtele"]["region"] == "BR" and "webhooks" in cards["stripe"]["badges"] and cards["stripe"]["installed"], cards["stripe"]
        # The webhook secrets the plugins sign with, and where the webhooks go.
        env = [r for r, _, f in os.walk(s.data) if "stripe.yaml" in f][0]
        with open(os.path.join(os.path.dirname(env), "secrets.yaml"), "a") as f:
            f.write(f"\nstripe-webhook-secret: {STRIPE_SECRET}\nresend-webhook-secret: {RESEND_SECRET}\n")
        for name in ["stripe", "resend"]:
            s.call("PUT", f"/plugins/{name}/data/settings", {"rows": [{"webhook_url": f"http://127.0.0.1:{HOOK}/{name}"}]})

        # ---------------------------------------------------------------- Stripe
        K = {"Authorization": "Bearer sk_test_51Acme", "Content-Type": "application/x-www-form-urlencoded"}
        def st(method, path, form=None, extra=None):
            h = dict(K, **(extra or {}))
            return s.api(method, "/stripe" + path, urllib.parse.urlencode(form or {}).encode() if form is not None else None, h)

        c, cus, _ = st("POST", "/v1/customers", {"email": "emma.carter@example.com", "name": "Emma Carter"})
        assert c == 200 and cus["object"] == "customer" and cus["id"].startswith("cus_") and cus["email"] == "emma.carter@example.com", cus
        c, got, _ = st("GET", "/v1/customers/" + cus["id"])
        assert c == 200 and got["id"] == cus["id"] and got["name"] == "Emma Carter", got
        c, v, _ = st("GET", "/v1/customers/cus_nobody")
        assert c == 404 and v["error"]["code"] == "resource_missing" and v["error"]["type"] == "invalid_request_error", v
        c, v, _ = s.api("POST", "/stripe/v1/customers", b"email=a@example.com", {"Content-Type": "application/x-www-form-urlencoded"})
        assert c == 401 and "API key" in v["error"]["message"], v
        # The real host answers too.
        c, v, _ = s.api("GET", "/v1/customers/" + cus["id"], None, dict(K, Host="api.stripe.com"))
        assert c == 200 and v["id"] == cus["id"], v

        # PaymentIntents: the test payment methods.
        c, pi, _ = st("POST", "/v1/payment_intents", {"amount": 2000, "currency": "usd", "customer": cus["id"], "payment_method": "pm_card_visa", "confirm": "true"})
        assert c == 200 and pi["status"] == "succeeded" and pi["amount_received"] == 2000 and pi["client_secret"].startswith(pi["id"] + "_secret_"), pi
        g, e = wait(lambda: events("/stripe", "payment_intent.succeeded"), "payment_intent.succeeded")[0]
        assert e["object"] == "event" and e["id"].startswith("evt_") and e["data"]["object"]["id"] == pi["id"] and isinstance(e["created"], int), e
        assert stripe_verified(g), g["headers"]
        assert wait(lambda: [x for x in events("/stripe", "payment_intent.created") if x[1]["data"]["object"]["id"] == pi["id"]], "payment_intent.created")

        c, v, _ = st("POST", "/v1/payment_intents", {"amount": 2000, "currency": "usd", "payment_method": "pm_card_chargeDeclined", "confirm": "true"})
        assert c == 402 and v["error"]["type"] == "card_error" and v["error"]["code"] == "card_declined" and v["error"]["decline_code"] == "generic_decline", v
        assert v["error"]["payment_intent"]["status"] == "requires_payment_method", v
        g, e = wait(lambda: events("/stripe", "payment_intent.payment_failed"), "payment_intent.payment_failed")[0]
        assert e["data"]["object"]["last_payment_error"]["decline_code"] == "generic_decline" and stripe_verified(g), e
        c, v, _ = st("POST", "/v1/payment_intents", {"amount": 2000, "currency": "usd", "payment_method": "pm_card_chargeDeclinedInsufficientFunds", "confirm": "true"})
        assert c == 402 and v["error"]["decline_code"] == "insufficient_funds", v

        c, pi3, _ = st("POST", "/v1/payment_intents", {"amount": 4500, "currency": "eur", "payment_method": "pm_card_authenticationRequired", "confirm": "true", "return_url": "https://shop.example.com/checkout/done"})
        assert c == 200 and pi3["status"] == "requires_action" and pi3["next_action"]["type"] == "redirect_to_url", pi3
        assert pi3["next_action"]["redirect_to_url"]["url"].startswith("https://shop.example.com/checkout/done?payment_intent=" + pi3["id"]), pi3
        c, v, _ = st("POST", f"/v1/payment_intents/{pi3['id']}/confirm", {})
        assert c == 200 and v["status"] == "succeeded" and v["amount"] == 4500, v

        c, pi4, _ = st("POST", "/v1/payment_intents", {"amount": 3000, "currency": "usd", "payment_method": "pm_card_mastercard"})
        assert c == 200 and pi4["status"] == "requires_confirmation", pi4
        c, v, _ = st("POST", f"/v1/payment_intents/{pi4['id']}/confirm", {"payment_method": "pm_card_visa"})
        assert c == 200 and v["status"] == "succeeded" and v["payment_method"] == "pm_card_visa", v
        c, v, _ = st("GET", f"/v1/payment_intents/{pi4['id']}")
        assert c == 200 and v["status"] == "succeeded", v

        c, pi5, _ = st("POST", "/v1/payment_intents", {"amount": 5000, "currency": "usd", "payment_method": "pm_card_visa", "confirm": "true", "capture_method": "manual"})
        assert c == 200 and pi5["status"] == "requires_capture" and pi5["amount_capturable"] == 5000, pi5
        c, v, _ = st("POST", f"/v1/payment_intents/{pi5['id']}/capture", {"amount_to_capture": 4000})
        assert c == 200 and v["status"] == "succeeded" and v["amount_received"] == 4000, v

        c, v, _ = st("POST", "/v1/payment_intents", {"currency": "usd"})
        assert c == 400 and v["error"]["code"] == "parameter_missing" and v["error"]["param"] == "amount", v
        c, v, _ = st("POST", "/v1/payment_intents/pi_nobody/confirm", {"payment_method": "pm_card_visa"})
        assert c == 404 and v["error"]["code"] == "resource_missing", v

        # Idempotency-Key: the same request gets the first answer again; another body, 409.
        k = {"Idempotency-Key": "order-7781"}
        c1, a, _ = st("POST", "/v1/payment_intents", {"amount": 1200, "currency": "usd", "payment_method": "pm_card_visa", "confirm": "true"}, k)
        c2, b, h = st("POST", "/v1/payment_intents", {"amount": 1200, "currency": "usd", "payment_method": "pm_card_visa", "confirm": "true"}, k)
        assert c1 == c2 == 200 and a["id"] == b["id"] and h.get("idempotent-replayed") == "true", (a, b, h)
        c, v, _ = st("POST", "/v1/payment_intents", {"amount": 9900, "currency": "usd"}, k)
        assert c == 409 and v["error"]["type"] == "idempotency_error", v

        # Refunds.
        c, v, _ = st("POST", "/v1/refunds", {"payment_intent": pi["id"], "amount": 500})
        assert c == 200 and v["object"] == "refund" and v["amount"] == 500 and v["status"] == "succeeded" and v["payment_intent"] == pi["id"], v
        c, v, _ = st("POST", "/v1/refunds", {"payment_intent": pi["id"]})
        assert c == 200 and v["amount"] == 2000, v
        c, v, _ = st("POST", "/v1/refunds", {"payment_intent": pi["id"], "amount": 999999})
        assert c == 400 and v["error"]["code"] == "amount_too_large", v

        # Checkout and the billing portal.
        c, cs, _ = st("POST", "/v1/checkout/sessions", {"mode": "subscription", "customer": cus["id"], "line_items[0][price]": "price_acme_pro_monthly", "line_items[0][quantity]": 2,
                                                     "success_url": "https://shop.example.com/ok?session_id={CHECKOUT_SESSION_ID}", "cancel_url": "https://shop.example.com/cart"})
        assert c == 200 and cs["object"] == "checkout.session" and cs["status"] == "open" and cs["amount_total"] == 5800 and cs["url"].startswith("https://checkout.stripe.com/"), cs
        c, v, _ = st("POST", "/v1/checkout/sessions", {"success_url": "https://shop.example.com/ok"})
        assert c == 400 and v["error"]["param"] == "mode", v
        c, v, _ = st("POST", "/v1/billing_portal/sessions", {"customer": cus["id"], "return_url": "https://shop.example.com/account"})
        assert c == 200 and v["object"] == "billing_portal.session" and v["url"].startswith("https://billing.stripe.com/") and v["customer"] == cus["id"], v

        # Subscriptions and invoices.
        c, sub, _ = st("POST", "/v1/subscriptions", {"customer": cus["id"], "items[0][price]": "price_acme_pro_monthly"})
        assert c == 200 and sub["object"] == "subscription" and sub["status"] == "active" and sub["items"]["data"][0]["price"]["unit_amount"] == 2900, sub
        g, e = wait(lambda: events("/stripe", "customer.subscription.created"), "customer.subscription.created")[0]
        assert e["data"]["object"]["id"] == sub["id"] and stripe_verified(g), e
        g, e = wait(lambda: events("/stripe", "invoice.paid"), "invoice.paid")[0]
        assert e["data"]["object"]["id"] == sub["latest_invoice"] and e["data"]["object"]["amount_paid"] == 2900 and stripe_verified(g), e
        c, v, _ = st("GET", "/v1/subscriptions/" + sub["id"])
        assert c == 200 and v["id"] == sub["id"] and v["customer"] == cus["id"], v
        c, v, _ = st("GET", "/v1/subscriptions?customer=" + cus["id"])
        assert c == 200 and v["object"] == "list" and [x["id"] for x in v["data"]] == [sub["id"]], v
        c, v, _ = st("POST", "/v1/subscriptions/" + sub["id"], {"cancel_at_period_end": "true"})
        assert c == 200 and v["cancel_at_period_end"] is True, v
        assert wait(lambda: events("/stripe", "customer.subscription.updated"), "customer.subscription.updated")
        c, v, _ = st("GET", "/v1/subscriptions/" + sub["id"])
        assert v["cancel_at_period_end"] is True, v
        c, inv, _ = st("GET", "/v1/invoices/" + sub["latest_invoice"])
        assert c == 200 and inv["object"] == "invoice" and inv["status"] == "paid" and inv["subscription"] == sub["id"], inv
        c, v, _ = st("GET", "/v1/invoices?customer=" + cus["id"])
        assert c == 200 and [x["id"] for x in v["data"]] == [inv["id"]], v
        c, v, _ = st("GET", "/v1/subscriptions/sub_cus_nobody")
        assert c == 404 and v["error"]["code"] == "resource_missing", v

        # invoice.upcoming waits 30 days on the environment's clock (checkout.session.completed 5 s).
        assert not events("/stripe", "invoice.upcoming")
        s.call("POST", "/clock/advance", {"by": "30d"})
        g, e = wait(lambda: events("/stripe", "invoice.upcoming"), "invoice.upcoming")[0]
        assert e["data"]["object"]["customer"] == cus["id"], e
        g, e = wait(lambda: events("/stripe", "checkout.session.completed"), "checkout.session.completed")[0]
        assert e["data"]["object"]["id"] == cs["id"] and e["data"]["object"]["payment_status"] == "paid", e
        s.call("POST", "/clock/advance", {"reset": True})

        c, v, _ = st("DELETE", "/v1/subscriptions/" + sub["id"])
        assert c == 200 and v["status"] == "canceled", v
        assert wait(lambda: events("/stripe", "customer.subscription.deleted"), "customer.subscription.deleted")
        c, v, _ = st("GET", "/v1/subscriptions?customer=" + cus["id"])
        assert v["data"] == [], v
        c, v, _ = st("GET", "/v1/subscriptions?status=all&customer=" + cus["id"])
        assert v["data"][0]["status"] == "canceled", v
        print("stripe ok:", len([g for g in GOT if g["path"] == "/stripe"]), "webhooks received")

        # ---------------------------------------------------------------- Resend
        R = {"Authorization": "Bearer re_acme_123"}
        c, v, _ = s.api("POST", "/resend/emails", {"from": "Acme <billing@acme.example.com>", "to": "emma.carter@example.com", "subject": "Your invoice", "html": "<p>Thanks</p>"}, R)
        assert c == 200 and len(v["id"]) == 36, v
        eid = v["id"]
        g, e = wait(lambda: events("/resend", "email.sent"), "email.sent")[0]
        assert e["data"]["email_id"] == eid and e["data"]["to"] == ["emma.carter@example.com"] and svix_verified(g), (e, g["headers"])
        g, e = wait(lambda: events("/resend", "email.delivered"), "email.delivered", 10)[0]
        assert e["data"]["email_id"] == eid and svix_verified(g), e
        c, v, _ = s.api("GET", "/resend/emails/" + eid, None, R)
        assert c == 200 and v["object"] == "email" and v["last_event"] == "delivered" and v["subject"] == "Your invoice", v
        c, v, _ = s.api("POST", "/resend/emails", {"from": "billing@acme.example.com", "to": ["bounced@resend.dev"], "subject": "Hi", "text": "hi"}, R)
        bid = v["id"]
        g, e = wait(lambda: [x for x in events("/resend", "email.bounced") if x[1]["data"]["email_id"] == bid], "email.bounced", 10)[0]
        assert e["data"]["bounce"]["type"] == "Permanent" and svix_verified(g), e
        c, v, _ = s.api("POST", "/resend/emails", {"from": "billing@acme.example.com", "subject": "Hi"}, R)
        assert c == 422 and v == {"statusCode": 422, "name": "missing_required_field", "message": "Missing `to` field."}, v
        c, v, _ = s.api("POST", "/resend/emails", {"to": "a@example.com", "subject": "Hi"}, R)
        assert c == 422 and "from" in v["message"], v
        c, v, _ = s.api("POST", "/resend/emails", {"from": "a@example.com", "to": "b@example.com", "subject": "Hi"})
        assert c == 401 and v["name"] == "missing_api_key", v
        c, v, _ = s.api("POST", "/resend/emails", {"from": "a@example.com", "to": "b@example.com", "subject": "Hi"}, {"Authorization": "Bearer sk_wrong"})
        assert c == 403 and v["name"] == "invalid_api_key", v
        c, v, _ = s.api("POST", "/resend/emails/batch", [{"from": "a@example.com", "to": "b@example.com", "subject": str(i)} for i in range(3)], R)
        assert c == 200 and len(v["data"]) == 3 and len({x["id"] for x in v["data"]}) == 3, v
        c, v, _ = s.api("GET", "/resend/emails/00000000-0000-0000-0000-000000000000", None, R)
        assert c == 404 and v["name"] == "not_found", v
        print("resend ok")

        # ---------------------------------------------------------------- OneSignal
        O = {"Authorization": "Basic os_rest_acme"}
        app = "5eb5a37e-b458-11e3-ac11-000c2940e62c"
        c, v, _ = s.api("POST", "/onesignal/api/v1/notifications", {"app_id": app, "contents": {"en": "Your order shipped"}, "include_player_ids": ["p-1", "invalid-1", "p-2", "invalid-2"]}, O)
        assert c == 200 and len(v["id"]) == 36 and v["external_id"] is None and v["errors"] == {"invalid_player_ids": ["invalid-1", "invalid-2"]}, v
        c, st1, _ = s.api("GET", f"/onesignal/api/v1/notifications/{v['id']}?app_id={app}", None, O)
        assert c == 200 and st1["successful"] == 2 and st1["failed"] == 0 and st1["remaining"] == 0 and "converted" in st1, st1
        c, v, _ = s.api("POST", "/onesignal/notifications", {"app_id": app, "contents": {"en": "Sale"}, "included_segments": ["Active Users"]}, {"Authorization": "Key os_v2_app_acme"})
        assert c == 200 and "errors" not in v, v
        c, st2, _ = s.api("GET", f"/onesignal/notifications/{v['id']}?app_id={app}", None, O)
        assert st2["successful"] == 830, st2
        c, v, _ = s.api("POST", "/onesignal/api/v1/notifications", {"app_id": app, "contents": {"en": "x"}, "include_subscription_ids": ["invalid-9"]}, O)
        assert c == 200 and v["id"] == "" and v["errors"]["invalid_player_ids"] == ["invalid-9"], v
        c, v, _ = s.api("POST", "/onesignal/api/v1/notifications", {"contents": {"en": "x"}, "included_segments": ["Active Users"]}, O)
        assert c == 400 and "app_id" in v["errors"][0], v
        c, v, _ = s.api("POST", "/onesignal/api/v1/notifications", {"app_id": app, "contents": {"en": "x"}, "included_segments": ["Active Users"]})
        assert c == 400 and "Authorization" in v["errors"][0], v
        c, v, _ = s.api("GET", f"/onesignal/api/v1/notifications/{st1['id']}", None, O)
        assert c == 400, v
        print("onesignal ok")

        # ---------------------------------------------------------------- Comtele
        T = {"auth-key": "comtele-acme-key"}
        c, v, _ = s.api("POST", "/comtele/api/v2/send", {"Sender": "acme", "Receivers": "11999990001,11999990002", "Content": "Seu codigo: 4821"}, T)
        assert c == 200 and v["Success"] is True and len(v["Object"]["requestUniqueId"]) == 36, v
        c, v, _ = s.api("POST", "/comtele/api/v2/send", {"Sender": "acme", "Receivers": "", "Content": "x"}, T)
        assert c == 400 and v["Success"] is False and v["Object"] is None and "Receivers" in v["Message"], v
        c, v, _ = s.api("POST", "/comtele/api/v2/send", {"Receivers": "11999990001", "Content": "x"}, {"auth-key": "invalid-key"})
        assert c == 401 and v["Success"] is False, v
        c, v, _ = s.api("POST", "/comtele/api/v2/send", {"Receivers": "11999990001", "Content": "x"})
        assert c == 401, v
        c, v, _ = s.api("GET", "/comtele/api/v2/reports?startDate=2026-11-01&endDate=2026-11-30&delivered=all", None, T)
        assert c == 200 and v["Success"] and v["Object"][0]["Receiver"] == "11999990001,11999990002", v
        print("comtele ok")
    print("e2e_plugins_payments: all passed")


if __name__ == "__main__":
    main()
