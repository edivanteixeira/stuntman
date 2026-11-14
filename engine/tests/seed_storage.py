"""Synthetic storage data for the screens (S1–S5): two Azure Blob accounts and one S3 account, the
app's uploads, people's files, a seed folder, links, rules and subscriptions.

    python3 tests/seed_storage.py http://localhost:8025 10000 9000 8027 [data dir]
"""
import json, os, sys, time, urllib.error, urllib.parse, urllib.request, zlib

API, BLOB, S3, HOOKS = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
DATA = sys.argv[5] if len(sys.argv) > 5 else os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "data")


def call(method, path, body=None, ok=(200, 201, 409)):
    req = urllib.request.Request(API + "/api" + urllib.parse.quote(path, safe="/?&=:%"), method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "X-Stuntman-Client": "seed"})
    try:
        with urllib.request.urlopen(req) as r:
            return json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        if e.code not in ok:
            raise SystemExit(f"{method} {path}: {e.code} {e.read()[:300]}")
        return None


def put(url, data, app, ctype="application/pdf", headers=None):
    h = {"x-ms-blob-type": "BlockBlob", "Content-Type": ctype, "x-ms-blob-content-type": ctype, "Authorization": "SharedKey acme:any", "X-Stuntman-App": app, **(headers or {})}
    req = urllib.request.Request(url, method="PUT", data=data, headers=h)
    urllib.request.urlopen(req).read()


def get(url, app="acme-api", ua=None):
    h = {"Authorization": "SharedKey acme:any", "X-Stuntman-App": app}
    if ua:
        h = {"User-Agent": ua}
    try:
        urllib.request.urlopen(urllib.request.Request(url, headers=h)).read()
    except urllib.error.HTTPError:
        pass


def pdf(text):
    """A small valid PDF whose page shows `text` (so the preview renders)."""
    content = f"BT /F1 18 Tf 72 720 Td ({text}) Tj ET 0.8 w 72 690 m 470 690 l S BT /F1 11 Tf 72 660 Td (Mutual non-disclosure agreement between Acme Corp and Globex Inc.) Tj ET".encode()
    objs = [b"<< /Type /Catalog /Pages 2 0 R >>", b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
            b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream", b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
    out, offs = b"%PDF-1.4\n", []
    for i, o in enumerate(objs, 1):
        offs.append(len(out))
        out += b"%d 0 obj\n" % i + o + b"\nendobj\n"
    x = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1) + b"".join(b"%010d 00000 n \n" % o for o in offs)
    return out + b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, x)


def png(w=64, h=40, rgb=(0, 112, 201)):
    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))
    def chunk(t, d):
        return len(d).to_bytes(4, "big") + t + d + zlib.crc32(t + d).to_bytes(4, "big")
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", w.to_bytes(4, "big") + h.to_bytes(4, "big") + b"\x08\x02\x00\x00\x00") + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


# Accounts (S5)
call("POST", "/services/azure-blob/accounts", {"account": "acme-docs-stg", "containers": {"documents": {"access": "private"}, "signed": {"access": "private"}, "thumbnails": {"access": "blob"}},
                                               "settings": {"behaviour": {"versions": True, "soft_delete": "7d"}}})
call("POST", "/services/azure-blob/accounts", {"account": "acme-temp-stg", "containers": ["uploads-tmp"]})
call("POST", "/services/s3/accounts", {"account": "globex-stg-files", "containers": ["attachments"]})

# A seed folder (loaded on reset), then loaded now
seed = os.path.join(DATA, "environments", "stg-a", "seed", "storage", "acme-docs-stg", "documents")
os.makedirs(os.path.join(seed, "2026", "11"), exist_ok=True)
open(os.path.join(seed, "2026", "11", "invoice-0042.pdf"), "wb").write(pdf("Invoice 0042 - Acme Corp"))
os.makedirs(os.path.join(seed, "templates"), exist_ok=True)
open(os.path.join(seed, "templates", "nda-template.pdf"), "wb").write(pdf("NDA template"))
call("POST", "/storage/reset", {"accounts": ["acme-docs-stg"]})

# Subscriptions (S4): the app's webhook is a Stuntman webhook inbox; a Service Bus topic waits for Queues
call("POST", "/webhooks/inboxes", {"name": "blob-events", "keep": True})
call("POST", "/storage/subscriptions", {"name": "acme-api webhook", "url": f"http://localhost:{HOOKS}/in/blob-events", "under": "documents/**", "format": "s3"})
call("POST", "/storage/subscriptions", {"name": "acme-bus-stg / blob-events", "to": "queue", "entity": "acme-bus-stg/blob-events", "events": ["created"], "under": "thumbnails/**"})
call("POST", "/storage/subscriptions", {"name": "globex webhook", "url": f"http://localhost:{HOOKS}/in/blob-events", "format": "s3", "account": "globex-stg-files"})

# The app's traffic
base = f"http://127.0.0.1:{BLOB}/acme-docs-stg"
for name, text, app in [("nda-acme-corp.pdf", "NDA Acme Corp", "acme-api"), ("nda-acme-corp-signed.pdf", "NDA Acme Corp  -  SIGNED by Emma Carter", "acme-api"),
                        ("lease-northwind-signed.pdf", "Lease Northwind  -  SIGNED", "acme-api")]:
    put(f"{base}/documents/2026/11/{name}", pdf(text), app, headers={"x-ms-meta-operationid": "3f2c9e1a-77b0-4c1e"})
for i in range(3):
    get(f"{base}/documents/2026/11/nda-acme-corp-signed.pdf")
get(f"{base}/documents/2026/11/nda-acme-corp.pdf")
put(f"{base}/documents/2026/11/export-globex-2026-11.zip", os.urandom(180_000), "batch-signer", "application/zip")
for i in range(1, 4):
    put(f"{base}/documents/2026/11/operations/op-{i:04d}.json", json.dumps({"operation": i, "signers": ["olivia.brooks@example.com"]}).encode(), "acme-worker", "application/json")
for i in range(1, 4):
    put(f"{base}/thumbnails/2026/11/nda-acme-p{i}.png", png(rgb=(0, 112 + i * 20, 201)), "acme-worker", "image/png")
put(f"http://127.0.0.1:{BLOB}/acme-temp-stg/uploads-tmp/7f3c/selfie.jpg", png(rgb=(200, 120, 90)), "acme-api", "image/jpeg")

# A person's file, cut off on purpose; a next read armed
call("PUT", "/storage/acme-docs-stg/documents/objects/2026/11/corrupted-sample.pdf", {"content_base64": __import__("base64").b64encode(pdf("Corrupted sample")[:600]).decode(), "truncated": True})
call("PUT", "/storage/acme-docs-stg/documents/next-read/2026/11/lease-northwind-signed.pdf", {"result": "403"})

# Links (S3): Stuntman-signed, one used by a browser, one expired, one revoked
l1 = call("POST", "/storage/acme-docs-stg/documents/links/2026/11/nda-acme-corp-signed.pdf", {"valid_for": "1h"})
for _ in range(3):
    get(l1["url"], ua="Mozilla/5.0 (Macintosh)")
l2 = call("POST", "/storage/acme-docs-stg/documents/links/2026/10/msa-globex.pdf", {"valid_for": "1h"})
call("POST", f"/storage/links/{l2['id']}/revoke")
get(l2["url"], ua="Mozilla/5.0 (Macintosh)")
call("POST", "/storage/acme-temp-stg/uploads-tmp/links/7f3c/selfie.jpg", {"allows": ["write"], "valid_for": "10m"})
call("POST", "/storage/acme-docs-stg/thumbnails/links/2026/11/nda-acme-p1.png", {"valid_for": "24h"})

# S3 (Globex): the app's attachments and a presigned link
s3 = f"http://127.0.0.1:{S3}/attachments"
for k in ["2026/11/invoice-0042.pdf", "2026/11/contract-northwind.pdf"]:
    urllib.request.urlopen(urllib.request.Request(f"{s3}/{k}", method="PUT", data=pdf(k), headers={"Content-Type": "application/pdf", "Authorization": "AWS4-HMAC-SHA256 Credential=x/20261113/auto/s3/aws4_request, SignedHeaders=host, Signature=x", "X-Stuntman-App": "globex-api"})).read()
l3 = call("POST", "/storage/globex-stg-files/attachments/links/2026/11/invoice-0042.pdf", {"valid_for": "6d"})
get(l3["url"], ua="Mozilla/5.0 (Macintosh)")

# Rules (S2)
call("POST", "/services/azure-blob/rules", {"rule": "Contract uploads fail twice", "when": {"op": "put", "account": "acme-docs-stg", "container": "documents", "path": "contracts/**"}, "then": {"respond": "503"}, "how_often": {"first": 2}})
call("POST", "/services/azure-blob/rules", {"rule": "Slow thumbnails", "when": {"op": "get", "container": "thumbnails", "path": "thumbnails/**"}, "then": {"respond": "slow", "seconds": 2}})
call("POST", "/services/azure-blob/rules", {"rule": "Links expire fast", "when": {"op": "link"}, "then": {"respond": "expires", "seconds": 10}, "enabled": False})
for i in range(2):
    try:
        put(f"{base}/documents/contracts/2026/11/nda-acme.pdf", pdf("Contract NDA Acme"), "acme-api")
    except urllib.error.HTTPError:
        pass
put(f"{base}/documents/contracts/2026/11/nda-acme.pdf", pdf("Contract NDA Acme"), "acme-api")
put(f"{base}/documents/contract/2026/11/lease-northwind.pdf", pdf("Lease"), "qa-suite")
print("storage seeded")
