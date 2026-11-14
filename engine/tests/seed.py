"""Synthetic demo data for a running Stuntman: the e-mails of the E1–E4 screens, sent the way an app
would (SMTP and the SendGrid API), plus kept and temporary inboxes and two extractors.

    python3 tests/seed.py [http://localhost:8025] [smtp port 2525]
"""
import json, smtplib, ssl, sys, urllib.error, urllib.request
from email.message import EmailMessage

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8025"
SMTP = int(sys.argv[2]) if len(sys.argv) > 2 else 2525


def api(method, path, body=None):
    req = urllib.request.Request(BASE + "/api" + path, method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "X-Stuntman-Client": "qa-suite"})
    try:
        return json.loads(urllib.request.urlopen(req).read() or b"null")
    except urllib.error.HTTPError as e:
        return {"error": e.read().decode()}


def smtp(to, subject, text, html=None, attach=(), lang=None):
    m = EmailMessage()
    m["From"] = "Acme <no-reply@acme.example.com>"; m["To"] = to; m["Subject"] = subject
    if lang:
        m["Content-Language"] = lang
    m.set_content(text)
    if html:
        m.add_alternative(html, subtype="html")
    for name, data, sub in attach:
        m.add_attachment(data, maintype="application" if sub == "pdf" else "text", subtype=sub, filename=name)
    with smtplib.SMTP("127.0.0.1", SMTP) as s:
        s.ehlo(); s.starttls(context=ssl._create_unverified_context()); s.ehlo(); s.login("api-staging", "any"); s.send_message(m)


def sendgrid(to, subject, html, args=None):
    body = {"personalizations": [{"to": [{"email": to}], "custom_args": args or {}}], "from": {"email": "no-reply@acme.example.com", "name": "Acme"},
            "subject": subject, "content": [{"type": "text/html", "value": html}]}
    req = urllib.request.Request(BASE + "/sendgrid/v3/mail/send", method="POST", data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "Authorization": "Bearer SG.stga.demo", "User-Agent": "acme-api/2.50"})
    urllib.request.urlopen(req)


OTP_HTML = """<div style="font-family:Arial,sans-serif;max-width:480px;margin:24px auto;padding:28px;background:#fff">
<p style="font-weight:bold;font-size:16px">Acme</p><h2>Confirm your signature</h2>
<p style="color:#444">Use this code to confirm your signature on NDA · Acme Corp:</p>
<div style="background:#f1f1ee;padding:18px;text-align:center;font:600 32px monospace;letter-spacing:6px"><strong>482 913</strong></div>
<p style="color:#666;font-size:13px">It expires in 10 minutes. If you did not ask for it, ignore this e-mail.</p>
<p><a href="https://account.acme.example.com/help">Need help? account.acme.example.com/help</a></p></div>"""

SIGN_HTML = """<div style="font-family:Arial,sans-serif;max-width:480px;margin:24px auto;padding:28px;background:#fff">
<p style="font-weight:bold">Acme</p><h2>{title}</h2><p>{body}</p>
<p><a href="https://acme-app.stg.example.com/operation/3f2c9e1a-77b0-4c1e-9d0a-5b1c2e7f0a11/2026-11-12" style="background:#16181b;color:#fff;padding:10px 18px;border-radius:6px;text-decoration:none">{cta}</a></p>
<p style="font-size:12px"><a href="https://acme.example.com/privacy">Privacy</a> · <a href="https://account.acme.example.com/help">Help</a> · <a href="https://acme.example.com/unsubscribe?u=1">Unsubscribe</a></p></div>"""

api("POST", "/channels/email/extractors", {"name": "otp", "from": "text", "rule": r"code.{0,80}?(\d{3} ?\d{3})", "strip": True})
api("POST", "/channels/email/extractors", {"name": "signLink", "from": "link", "rule": r"/operation/[0-9a-f-]{36}/[^\s\"]+",
                                           "onlyOn": {"when": "subject", "value": "Action required|Ação necess"}})
api("POST", "/channels/email/extractors", {"name": "resetLink", "from": "link", "rule": r"/reset-password\?token="})
for a, label in [("dev@acme.example.com", "seeded user"), ("manager@acme.example.com", "seeded user")]:
    api("POST", "/inboxes", {"address": a, "keep": True, "label": label})
api("POST", "/inboxes", {"address": "bounces@acme.example.com", "keep": True, "label": "rule: always bounce", "onArrival": "bounce"})
t = api("POST", "/inboxes", {"localPart": "signer-7f3c", "ttl": "30m", "steps": ["delivered +1s", "opened +30s ±5s", "click:signLink +500ms"]})
signer = t.get("address", "signer-7f3c@stg-a.inbox.stuntman.dev")

smtp("dev@acme.example.com", "Reset your password", "Reset it here: https://acme-app.stg.example.com/reset-password?token=7Hq2xZ0p\nThe link works for 1 hour.")
smtp("ops@acme.example.com", "Tu exportación está lista", "Hola. Tu exportación de grupos está lista. Gracias por usar el servicio: https://acme-app.stg.example.com/exports/9f2c",
     attach=[("grupos-2026-11-12.csv", b"id,nombre,miembros,creado\n31,Ventas,12,2026-10-02 09:14\n32,Legal,4,2026-10-02 09:20\n33,Compras,7,2026-10-15 16:41\n34,Direccion,3,2026-11-03 11:05\n", "csv"),
             ("como-importar-grupos.pdf", b"%PDF-1.4\n% synthetic demo file\n", "pdf")], lang="es")
smtp("bounces@acme.example.com", "Action required: Lease · Northwind", "Please sign the lease.", html=SIGN_HTML.format(title="Action required", body="Northwind sent you a lease to sign.", cta="Review and sign"))
sendgrid("liam.brooks@example.com", "Document completed: NDA · Acme Corp", SIGN_HTML.format(title="Document completed", body="Everyone signed NDA · Acme Corp.", cta="Download"))
sendgrid(signer, "Action required: NDA · Acme Corp", SIGN_HTML.format(title="Action required", body="Acme Corp sent you NDA · Acme Corp to sign.", cta="Review and sign"), {"operation_id": "58213"})
smtp("emma.carter@example.com", "Your verification code", "Use this code to confirm your signature on NDA · Acme Corp: 482 913. It expires in 10 minutes.", html=OTP_HTML)
print("seeded", BASE)
