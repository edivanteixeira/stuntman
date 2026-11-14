"""Demo WhatsApp data for an environment (synthetic people, numbers in the 555 range).

    python3 tests/seed_whatsapp.py [base]   # default http://localhost:8025 (stg-a)
"""
import json, sys, time, urllib.error, urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8025"


def call(method, path, body=None, base=BASE + "/api", ua="acme-api/2.1"):
    r = urllib.request.Request(base + path, method=method, data=None if body is None else json.dumps(body).encode(),
                               headers={"Content-Type": "application/json", "Authorization": "Bearer demo", "User-Agent": ua, "X-Stuntman-Client": "seed"})
    try:
        with urllib.request.urlopen(r, timeout=30) as x: return x.status, json.loads(x.read() or b"null")
    except urllib.error.HTTPError as e: return e.code, json.loads(e.read() or b"null")


def tpl(name, to, params, lang="en_US"):
    return {"messaging_product": "whatsapp", "to": to, "type": "template",
            "template": {"name": name, "language": {"code": lang}, "components": [{"type": "body", "parameters": [{"type": "text", "text": p} for p in params]}]}}


st, inbox = call("POST", "/webhooks/inboxes", {"name": "acme-whatsapp", "keep": True})
st, n = call("GET", "/whatsapp/number")
call("PUT", "/whatsapp/number", {"webhook_url": BASE.replace("8025", "8027") + "/in/acme-whatsapp" if "/e/" not in BASE else BASE.replace("8025", "8027").replace("/e/", "/e/") + "/in/acme-whatsapp"})
pnid = n["number"]["phone_number_id"]
T = [
    {"name": "order_confirmation", "header": "Order confirmed", "footer": "Acme Store", "body": "Hi {{1}}, your order {{2}} is confirmed.\nTotal: {{3}}. Delivery: {{4}}.",
     "buttons": [{"text": "Track order", "url": "https://shop.example.com/track/{{1}}"}, {"text": "Talk to us", "quick_reply": "TALK_TO_US"}]},
    {"name": "payment_reminder", "body": "Hi {{1}}, invoice {{2}} of {{3}} is due on {{4}}."},
    {"name": "delivery_update", "body": "Your package {{1}} is on its way. It should arrive on {{2}}."},
    {"name": "invoice_ready", "language": "en_GB", "body": "Your invoice {{1}} is ready."},
    {"name": "login_code", "category": "authentication", "body": "{{1}} is your verification code. For your security, do not share this code."},
    {"name": "spring_sale", "category": "marketing", "body": "Spring sale: {{1}} off everything until {{2}}."},
    {"name": "back_in_stock", "category": "marketing", "body": "Good news, {{1}} is back in stock."},
]
for t in T: call("POST", "/whatsapp/templates", t)
call("PATCH", "/whatsapp/templates/order_confirmation", {"languages": ["en_US", "es_MX"], "variables": ["customer first name", "order number", "total", "delivery date"]})
call("PUT", "/whatsapp/templates/spring_sale/status", {"status": "rejected", "reason": "INVALID_FORMAT"})
call("PUT", "/whatsapp/templates/back_in_stock/status", {"status": "paused"})
call("PUT", "/whatsapp/recipients/+14155550142", {"profile_name": "Emma Carter", "steps": [{"delivered_after": "1s"}, {"read_after": "90s"}, {"on_buttons": {"tap": "Talk to us", "after": "10s"}}, {"on_text": {"reply": "Yes, tomorrow morning", "after": "20s"}}]})
call("PUT", "/whatsapp/recipients/+13125550110", {"whatsapp": False})
call("PUT", "/whatsapp/recipients/+12065550133", {"profile_name": "Noah Brooks", "steps": [{"delivered_after": "1s"}, {"never_read": True}]})
call("PUT", "/whatsapp/recipients/+16465550187", {"profile_name": "Liam Walker", "steps": [{"delivered_after": "1s"}, {"read_after": "5s"}]})
call("PUT", "/whatsapp/recipients/*", {"steps": [{"delivered_after": "1s"}, {"read_after": "30s"}]})
G = BASE + "/whatsapp/v21.0"
call("POST", "/whatsapp/numbers/%2B14155550142/reply", {"text": "Is my order already confirmed?", "by": "seed"})
print(call("POST", f"/{pnid}/messages", tpl("order_confirmation", "+14155550142", ["Emma", "58213", "$1,290.00", "Nov 14"]), base=G)[0])
call("POST", f"/{pnid}/messages", tpl("delivery_update", "+16465550187", ["58109", "Nov 13"]), base=G)
call("POST", "/whatsapp/numbers/%2B16465550187/reply", {"text": "Yes, deliver tomorrow morning", "by": "seed"})
call("POST", f"/{pnid}/messages", tpl("delivery_update", "+13125550110", ["58300", "Nov 15"]), base=G)
call("POST", f"/{pnid}/messages", tpl("invoice_ready", "+442079460958", ["INV-2207"], "en_GB"), base=G)
call("POST", f"/{pnid}/messages", {"messaging_product": "whatsapp", "to": "+12065550133", "type": "text", "text": {"body": "Thanks, see you then"}}, base=G)
call("POST", f"/{pnid}/messages", tpl("shipping_update", "+14155550142", ["58213", "UPS", "Nov 14"]), base=G)
print("seeded WhatsApp demo data")
