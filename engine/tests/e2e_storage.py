"""End-to-end check of storage (S1–S5) with the real clients: the Azure CLI (Microsoft's Python SDK)
for Azure Blob and the AWS CLI (botocore) for S3, plus the HTTP API and MCP.

    cargo build -p stuntman && python3 tests/e2e_storage.py
"""
import base64, hashlib, http.client, http.server, json, os, shutil, socket, subprocess, tempfile, threading, time, urllib.error, urllib.parse, urllib.request

import sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "stuntman")


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


HTTP, HOOKS, SMTP, OTLP, OTLP_G, BLOB, S3, RECV = (free_port() for _ in range(8))
GOT = []
RECV_MODE = {"status": 200}


class Recv(http.server.BaseHTTPRequestHandler):
    """The app's webhook: answers Event Grid's handshake, records deliveries."""
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])) or b"null")
        if self.path == "/nope":
            self.send_response(404); self.send_header("Content-Length", "0"); self.end_headers()
            return
        if self.headers.get("aeg-event-type") == "SubscriptionValidation":
            code = body[0]["data"]["validationCode"]
            out = json.dumps({"validationResponse": code}).encode()
            self.send_response(200); self.send_header("Content-Length", str(len(out))); self.end_headers(); self.wfile.write(out)
            return
        GOT.append({"path": self.path, "headers": dict(self.headers), "body": body})
        self.send_response(RECV_MODE["status"]); self.send_header("Content-Length", "0"); self.end_headers()

    def log_message(self, *a):
        pass


def call(method, path, body=None, expect=200):
    req = urllib.request.Request(f"http://127.0.0.1:{HTTP}/api{urllib.parse.quote(path, safe='/?&=:%')}", method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "X-Stuntman-Client": "qa-suite"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            st, v = r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        st, v = e.code, json.loads(e.read() or b"null")
    assert expect is None or st == expect, (method, path, st, v)
    return v if expect is not None else (st, v)


def raw(method, url, body=b"", headers=None):
    u = urllib.parse.urlsplit(url)
    c = http.client.HTTPConnection(u.hostname, u.port, timeout=30)
    c.request(method, u.path + (f"?{u.query}" if u.query else ""), body, headers or {})
    r = c.getresponse()
    return r.status, dict((k.lower(), v) for k, v in r.getheaders()), r.read()


def wait(cond, what, t=15):
    end = time.time() + t
    while time.time() < end:
        v = cond()
        if v:
            return v
        time.sleep(0.2)
    raise AssertionError(f"timed out waiting for {what}")


def sh(args, env, ok=True):
    p = subprocess.run(args, env=env, capture_output=True, text=True, timeout=180)
    if ok and p.returncode != 0:
        raise AssertionError(f"{' '.join(args[:4])}…: {p.stderr[-800:]}")
    return p


def history(acct, cont, path):
    return [h["status"] for h in reversed(call("GET", f"/storage/{acct}/{cont}/objects/{urllib.parse.quote(path)}")["history"])]


def run(data, tmp):
    # ---- S5: accounts, as the screen makes them
    az = call("POST", "/services/azure-blob/accounts", {"account": "acme-docs-stg", "containers": {"documents": {"access": "private"}, "signed": {"access": "private"}, "thumbnails": {"access": "blob"}}})
    assert az["kind"] == "azure-blob" and [k["name"] for k in az["keys"]] == ["key1", "key2"], az
    s3 = call("POST", "/services/s3/accounts", {"account": "globex-stg-files", "containers": ["attachments"]})
    conn = az["connection"]
    assert f"BlobEndpoint=http://127.0.0.1:{BLOB}/acme-docs-stg" in conn, conn
    key_id = s3["keys"][0]["name"]
    secret = dict(x.split("=", 1) for x in s3["connection"].split(";"))["SecretKey"]
    st, e = call("POST", "/services/azure-blob/accounts", {"account": "Acme_Docs"}, expect=None)
    assert st == 400 and "lowercase" in e["error"], e
    schema = call("GET", "/services/azure-blob/schema")
    assert any(f["key"] == "behaviour.limit" for f in schema["fields"])

    azenv = dict(os.environ, AZURE_CONFIG_DIR=os.path.join(tmp, "az"), AZURE_CORE_ONLY_SHOW_ERRORS="true", AZURE_CORE_COLLECT_TELEMETRY="false", AZURE_STORAGE_CONNECTION_STRING=conn.replace("localhost", "127.0.0.1"))
    awsenv = dict(os.environ, AWS_ACCESS_KEY_ID=key_id, AWS_SECRET_ACCESS_KEY=secret, AWS_DEFAULT_REGION="us-east-1", AWS_CONFIG_FILE="/dev/null",
                  AWS_SHARED_CREDENTIALS_FILE="/dev/null", AWS_EC2_METADATA_DISABLED="true", AWS_PAGER="")
    awsep = ["--endpoint-url", f"http://127.0.0.1:{S3}"]

    # ---- Events first: a webhook in Event Grid schema (handshake), and one in S3 schema
    v = call("POST", "/storage/subscriptions", {"name": "acme-api webhook", "url": f"http://127.0.0.1:{RECV}/hooks/blob", "under": "documents/**"})
    assert v["health"] == "delivering" and v["error"] == "", v
    v = call("POST", "/storage/subscriptions", {"name": "bad webhook", "url": f"http://127.0.0.1:{RECV}/nope", "under": "nothing/**"})
    v = call("POST", "/storage/subscriptions", {"name": "globex s3 hook", "url": f"http://127.0.0.1:{RECV}/hooks/s3", "format": "s3", "account": "globex-stg-files"})

    # ---- Azure with Microsoft's SDK (az CLI): upload, properties, metadata, list, download
    pdf = os.path.join(tmp, "nda.pdf")
    with open(pdf, "wb") as f:
        f.write(b"%PDF-1.4 nda acme corp signed " * 300)
    sh(["az", "storage", "blob", "upload", "-c", "documents", "-n", "2026/11/nda-acme-corp-signed.pdf", "-f", pdf, "--metadata", "operationId=3f2c9e1a", "--content-type", "application/pdf"], azenv)
    p = json.loads(sh(["az", "storage", "blob", "show", "-c", "documents", "-n", "2026/11/nda-acme-corp-signed.pdf"], azenv).stdout)
    assert p["properties"]["contentLength"] == os.path.getsize(pdf) and p["metadata"]["operationid"] == "3f2c9e1a", p
    assert p["properties"]["contentSettings"]["contentType"] == "application/pdf", p
    lst = json.loads(sh(["az", "storage", "blob", "list", "-c", "documents", "--prefix", "2026/", "--include", "m"], azenv).stdout)
    assert [b["name"] for b in lst] == ["2026/11/nda-acme-corp-signed.pdf"] and lst[0]["metadata"]["operationid"] == "3f2c9e1a", lst
    out = os.path.join(tmp, "down.pdf")
    sh(["az", "storage", "blob", "download", "-c", "documents", "-n", "2026/11/nda-acme-corp-signed.pdf", "-f", out], azenv)
    assert open(out, "rb").read() == open(pdf, "rb").read()
    # Copy (finishes at once) and delete
    sh(["az", "storage", "blob", "copy", "start", "--destination-container", "signed", "--destination-blob", "copy.pdf", "--source-uri", f"http://127.0.0.1:{BLOB}/acme-docs-stg/documents/2026/11/nda-acme-corp-signed.pdf"], azenv)
    p = json.loads(sh(["az", "storage", "blob", "show", "-c", "signed", "-n", "copy.pdf"], azenv).stdout)
    assert p["properties"]["copy"]["status"] == "success", p["properties"]["copy"]
    sh(["az", "storage", "blob", "delete", "-c", "signed", "-n", "copy.pdf"], azenv)
    assert not json.loads(sh(["az", "storage", "blob", "exists", "-c", "signed", "-n", "copy.pdf"], azenv).stdout)["exists"]
    # Event Grid delivered BlobCreated in its schema
    ev = wait(lambda: [g for g in GOT if g["path"] == "/hooks/blob"], "BlobCreated delivery")[0]
    assert ev["headers"]["aeg-event-type"] == "Notification" and ev["body"][0]["eventType"] == "Microsoft.Storage.BlobCreated", ev
    assert ev["body"][0]["subject"] == "/blobServices/default/containers/documents/blobs/2026/11/nda-acme-corp-signed.pdf"
    subs = {s["name"]: s for s in call("GET", "/storage/subscriptions")["subscriptions"]}
    assert subs["bad webhook"]["health"] == "not subscribed" and "404" in subs["bad webhook"]["error"], subs["bad webhook"]

    # ---- Signatures checked like Azure: any key off → the SDK's SharedKey must match
    call("PATCH", "/services/azure-blob/accounts/acme-docs-stg", {"patch": {"access": {"any_key": False}}})
    sh(["az", "storage", "blob", "upload", "-c", "documents", "-n", "2026/11/lease northwind.pdf", "-f", pdf, "--overwrite"], azenv)
    bad = dict(azenv, AZURE_STORAGE_CONNECTION_STRING=azenv["AZURE_STORAGE_CONNECTION_STRING"].replace("AccountKey=", "AccountKey=AAAA"))
    r = sh(["az", "storage", "blob", "upload", "-c", "documents", "-n", "x.pdf", "-f", pdf, "--overwrite"], bad, ok=False)
    assert r.returncode != 0 and "Authentication" in r.stderr + r.stdout, r.stderr[-400:]
    # SAS made by the SDK (account key), used by a browser: listed at first use
    expiry = time.strftime("%Y-%m-%dT%H:%MZ", time.gmtime(time.time() + 3600))
    sas = sh(["az", "storage", "blob", "generate-sas", "-c", "documents", "-n", "2026/11/nda-acme-corp-signed.pdf", "--permissions", "r", "--expiry", expiry, "-o", "tsv"], azenv).stdout.strip()
    url = f"http://127.0.0.1:{BLOB}/acme-docs-stg/documents/2026/11/nda-acme-corp-signed.pdf?{sas}"
    st, h, b = raw("GET", url, headers={"User-Agent": "Mozilla/5.0"})
    assert st == 200 and b == open(pdf, "rb").read(), (st, b[:200])
    links = call("GET", "/storage/links?kind=sas")["links"]
    assert len(links) == 1 and links[0]["signedWith"] == "key1" and links[0]["allows"] == "read", links
    lid = links[0]["id"]
    st, _, b = raw("PUT", url, b"x", {"x-ms-blob-type": "BlockBlob"})
    assert st == 403 and b"AuthorizationPermissionMismatch" in b, b
    call("PUT", f"/storage/links/{lid}/next-use", {"result": "expired"})
    st, _, b = raw("GET", url)
    assert st == 403 and b"AuthenticationFailed" in b
    assert raw("GET", url)[0] == 200, "once, then normal"
    call("POST", f"/storage/links/{lid}/revoke")
    assert raw("GET", url)[0] == 403
    uses = call("GET", f"/storage/links/{lid}")["uses"]
    assert [u["status"] for u in uses] == [200, 403, 403, 200, 403], [u["status"] for u in uses]
    # Rotating key1: apps still signing with the old key get 403
    rot = call("POST", "/services/azure-blob/accounts/acme-docs-stg/keys/key1/rotate")
    r = sh(["az", "storage", "blob", "upload", "-c", "documents", "-n", "y.pdf", "-f", pdf, "--overwrite"], azenv, ok=False)
    assert r.returncode != 0, "old key must fail after rotation"
    azenv["AZURE_STORAGE_CONNECTION_STRING"] = rot["connection"].replace("localhost", "127.0.0.1")
    sh(["az", "storage", "blob", "upload", "-c", "documents", "-n", "y.pdf", "-f", pdf, "--overwrite"], azenv)
    call("PATCH", "/services/azure-blob/accounts/acme-docs-stg", {"patch": {"access": {"any_key": True}}})

    # Conditional upload (If-Match) → 412, like an encryption worker expects
    st, _, b = raw("PUT", f"http://127.0.0.1:{BLOB}/acme-docs-stg/documents/y.pdf", b"new", {"x-ms-blob-type": "BlockBlob", "If-Match": "\"0xNOTIT\"", "Authorization": "SharedKey acme-docs-stg:x"})
    assert st == 412 and b"ConditionNotMet" in b, (st, b)
    # "\" in a blob name is "/", as in Azure: written with "\", read, listed and deleted with "/"
    sk = {"Authorization": "SharedKey acme-docs-stg:x"}
    base = f"http://127.0.0.1:{BLOB}/acme-docs-stg/documents/"
    assert raw("PUT", base + "inbound%5Corders%5Cn1.txt", b"order-1", {"x-ms-blob-type": "BlockBlob", **sk})[0] == 201
    assert raw("PUT", base + "inbound\\orders\\n2.txt", b"order-2", {"x-ms-blob-type": "BlockBlob", **sk})[0] == 201
    st, _, b = raw("GET", base + "inbound/orders/n1.txt", headers=sk)
    assert st == 200 and b == b"order-1", (st, b)
    assert raw("HEAD", base + "inbound/orders/n2.txt", headers=sk)[0] == 200
    st, _, b = raw("GET", f"http://127.0.0.1:{BLOB}/acme-docs-stg/documents?restype=container&comp=list&prefix=inbound%5Corders%5C", headers=sk)
    assert b"<Name>inbound/orders/n1.txt</Name>" in b and b"<Name>inbound/orders/n2.txt</Name>" in b and b"\\" not in b, b
    assert raw("DELETE", base + "inbound%5Corders%5Cn1.txt", headers=sk)[0] == 202
    assert raw("GET", base + "inbound/orders/n1.txt", headers=sk)[0] == 404
    assert raw("DELETE", base + "inbound/orders/n2.txt", headers=sk)[0] == 202
    # A large file goes up in blocks (Put Block + Put Block List)
    big = os.path.join(tmp, "big.bin")
    with open(big, "wb") as f:
        f.write(os.urandom(70 * 1024 * 1024))
    sh(["az", "storage", "blob", "upload", "-c", "documents", "-n", "export-globex-2026-11.zip", "-f", big, "--max-connections", "2"], azenv)
    p = json.loads(sh(["az", "storage", "blob", "show", "-c", "documents", "-n", "export-globex-2026-11.zip"], azenv).stdout)
    assert p["properties"]["contentLength"] == 70 * 1024 * 1024, p["properties"]["contentLength"]

    # ---- S3 with botocore (AWS CLI): put, head, get, list v2, metadata, presign, multipart, delete
    txt = os.path.join(tmp, "invoice.txt")
    with open(txt, "w") as f:
        f.write("invoice 0042\n" * 100)
    sh(["aws", *awsep, "s3api", "put-object", "--bucket", "attachments", "--key", "2026/11/invoice-0042.txt", "--body", txt, "--content-type", "text/plain", "--metadata", "origin=globex"], awsenv)
    hd = json.loads(sh(["aws", *awsep, "s3api", "head-object", "--bucket", "attachments", "--key", "2026/11/invoice-0042.txt"], awsenv).stdout)
    assert hd["ContentLength"] == os.path.getsize(txt) and hd["Metadata"] == {"origin": "globex"} and hd["ContentType"] == "text/plain", hd
    lst = json.loads(sh(["aws", *awsep, "s3api", "list-objects-v2", "--bucket", "attachments", "--prefix", "2026/"], awsenv).stdout)
    assert [o["Key"] for o in lst["Contents"]] == ["2026/11/invoice-0042.txt"], lst
    sh(["aws", *awsep, "s3", "cp", big, "s3://attachments/exports/big.bin"], awsenv)  # multipart (8 MB parts)
    hd = json.loads(sh(["aws", *awsep, "s3api", "head-object", "--bucket", "attachments", "--key", "exports/big.bin"], awsenv).stdout)
    assert hd["ContentLength"] == 70 * 1024 * 1024 and hd["ETag"].endswith("-9\""), hd
    out = os.path.join(tmp, "big.down")
    sh(["aws", *awsep, "s3", "cp", "s3://attachments/exports/big.bin", out], awsenv)
    assert hashlib.md5(open(out, "rb").read()).digest() == hashlib.md5(open(big, "rb").read()).digest()
    # Signatures checked: any key off → botocore's SigV4 must match; a wrong secret is refused
    call("PATCH", "/services/s3/accounts/globex-stg-files", {"patch": {"access": {"any_key": False}}})
    sh(["aws", *awsep, "s3api", "put-object", "--bucket", "attachments", "--key", "signed.txt", "--body", txt], awsenv)
    r = sh(["aws", *awsep, "s3api", "put-object", "--bucket", "attachments", "--key", "x.txt", "--body", txt], dict(awsenv, AWS_SECRET_ACCESS_KEY="wrong"), ok=False)
    assert r.returncode != 0 and "SignatureDoesNotMatch" in r.stderr, r.stderr[-300:]
    pre = sh(["aws", *awsep, "s3", "presign", "s3://attachments/2026/11/invoice-0042.txt", "--expires-in", "3600"], awsenv).stdout.strip()
    st, _, b = raw("GET", pre)
    assert st == 200 and b.startswith(b"invoice 0042"), (st, b[:200])
    assert any(l["kind"] == "presigned" and l["signedWith"] == key_id for l in call("GET", "/storage/links")["links"])
    # A presigned link Stuntman makes, with the download name Globex signs
    lk = call("POST", f"/storage/globex-stg-files/attachments/links/2026/11/invoice-0042.txt", {"valid_for": "1h"})
    st, h, _ = raw("GET", lk["url"].replace("localhost", "127.0.0.1") + "&response-content-disposition=" + urllib.parse.quote('attachment; filename="invoice.txt"'))
    assert st in (200, 403), st  # the override is not part of Stuntman's own signature
    st, h, _ = raw("GET", lk["url"].replace("127.0.0.1", "localhost"))
    assert st == 403, "the host is signed: another host name is another signature"
    st, h, _ = raw("GET", lk["url"])
    assert st == 200, (st, lk["url"])
    call("PATCH", "/services/s3/accounts/globex-stg-files", {"patch": {"access": {"any_key": True}}})
    sh(["aws", *awsep, "s3api", "delete-object", "--bucket", "attachments", "--key", "signed.txt"], awsenv)
    # S3-schema event for the Globex hook
    ev = wait(lambda: [g for g in GOT if g["path"] == "/hooks/s3" and g["body"]["Records"][0]["eventName"] == "ObjectRemoved:Delete"], "S3 ObjectRemoved")
    assert ev[0]["body"]["Records"][0]["s3"]["bucket"]["name"] == "attachments"

    # ---- Provoked failures (S1 next read, S2 rules) and the client's retries
    call("PUT", "/storage/globex-stg-files/attachments/next-read/2026/11/invoice-0042.txt", {"result": "503", "times": 2})
    sh(["aws", *awsep, "s3api", "get-object", "--bucket", "attachments", "--key", "2026/11/invoice-0042.txt", os.path.join(tmp, "o.txt")], awsenv)
    assert history("globex-stg-files", "attachments", "2026/11/invoice-0042.txt")[-3:] == [503, 503, 200], "botocore retried twice"
    rule = {"rule": "Contract uploads fail twice", "when": {"op": "put", "account": "acme-docs-stg", "container": "documents", "path": "contracts/**"}, "then": {"respond": "503"}, "how_often": {"first": 2}}
    t = call("POST", "/services/azure-blob/rules/test", {**rule, "when": {**rule["when"], "path": "contract/**"}})
    assert t["matched"] == 0
    call("POST", "/services/azure-blob/rules", rule)
    sh(["az", "storage", "blob", "upload", "-c", "documents", "-n", "contracts/2026/11/msa-globex.pdf", "-f", pdf], azenv)
    assert history("acme-docs-stg", "documents", "contracts/2026/11/msa-globex.pdf") == [503, 503, 201], history("acme-docs-stg", "documents", "contracts/2026/11/msa-globex.pdf")
    rules = call("GET", "/services/azure-blob/rules")["rules"]
    assert rules[0]["hitsToday"] == 2 and rules[0]["meta"] == "PUT contracts/** → 503×2", rules
    t = call("POST", "/services/azure-blob/rules/test", {"rule": "draft", "when": {"op": "put", "container": "documents", "path": "contract/**"}, "then": {"respond": "503"}})
    assert t["matched"] == 0 and t["nearMiss"]["fix"] == "contract*/**", t
    call("PUT", "/services/azure-blob/rules/Contract uploads fail twice/enabled", {"enabled": False})
    # Network failures are real: reset, empty reply, cut off halfway
    surl = f"http://127.0.0.1:{S3}/attachments/2026/11/invoice-0042.txt"
    for respond, curl_code in [("reset", 56), ("empty", 52), ("truncate", 18)]:
        call("POST", "/services/s3/rules", {"rule": f"get {respond}", "when": {"op": "get", "path": "2026/**"}, "then": {"respond": respond}, "how_often": {"first": 1}})
        r = subprocess.run(["curl", "-s", "-o", "/dev/null", surl], capture_output=True)
        assert r.returncode == curl_code, (respond, r.returncode)
        call("DELETE", f"/services/s3/rules/get {respond}")
    # A whole account unreachable
    call("PUT", "/storage/globex-stg-files/reachability", {"mode": "503"})
    assert raw("GET", surl)[0] == 503
    call("PUT", "/storage/globex-stg-files/reachability", {"mode": "ok"})
    assert raw("GET", surl)[0] == 200

    # ---- S4: retries, next delivery, send by hand
    RECV_MODE["status"] = 503
    n0 = len(GOT)
    sh(["az", "storage", "blob", "upload", "-c", "documents", "-n", "retry.pdf", "-f", pdf], azenv)
    wait(lambda: [e for e in call("GET", "/storage/events?subscription=acme-api webhook")["events"] if e["path"] == "retry.pdf" and e["status"] == "retrying"], "retrying")
    RECV_MODE["status"] = 200
    call("PUT", "/storage/subscriptions/acme-api webhook/next-delivery", {"behavior": "drop"})
    sh(["az", "storage", "blob", "upload", "-c", "documents", "-n", "dropped.pdf", "-f", pdf], azenv)
    wait(lambda: [e for e in call("GET", "/storage/events?subscription=acme-api webhook")["events"] if e["path"] == "dropped.pdf" and e["status"] == "dropped"], "dropped")
    call("PUT", "/storage/subscriptions/acme-api webhook/next-delivery", {"behavior": "missing_field"})
    e = call("POST", "/storage/events", {"subscription": "acme-api webhook", "type": "created", "path": "documents/corrupted-sample.pdf"})
    assert e["by"] == "sent by qa-suite", e
    d = call("GET", "/storage/events/wait?subscription=acme-api webhook&path=corrupted-sample")
    got = [g for g in GOT if g["path"] == "/hooks/blob" and g["body"][0]["subject"].endswith("corrupted-sample.pdf")]
    assert got and "contentLength" not in got[-1]["body"][0]["data"], got

    # ---- S1 from the screen's side: list, upload by a person, seed, reset
    v = call("GET", "/storage/acme-docs-stg/documents/objects?prefix=2026/11/")
    assert any(o["name"] == "nda-acme-corp-signed.pdf" and o["reads"] >= 1 for o in v["objects"]), v["objects"]
    call("PUT", "/storage/acme-docs-stg/documents/objects/2026/11/corrupted-sample.pdf", {"content": "%PDF-1.4 tru", "truncated": True})
    o = call("GET", "/storage/acme-docs-stg/documents/objects/2026/11/corrupted-sample.pdf")
    assert o["by"] == "qa-suite" and o["via"] == "api" and o["truncated"], o
    seed = os.path.join(data, "environments/stg-a/seed/storage/acme-docs-stg/documents/invoices")
    os.makedirs(seed)
    open(os.path.join(seed, "invoice-0042.pdf"), "wb").write(b"%PDF-1.4 seed")
    r = call("POST", "/storage/reset", {"accounts": ["acme-docs-stg"]})
    assert r["accounts"][0]["loaded"] == 1, r
    v = call("GET", "/storage/acme-docs-stg/documents/objects?prefix=invoices/")
    assert [(o["name"], o["by"]) for o in v["objects"]] == [("invoice-0042.pdf", "seed")], v

    # ---- MCP: the same tools by the same names
    def mcp(body, sid=None):
        req = urllib.request.Request(f"http://127.0.0.1:{HTTP}/mcp", method="POST", data=json.dumps(body).encode(), headers={"Content-Type": "application/json", **({"Mcp-Session-Id": sid} if sid else {})})
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.headers.get("mcp-session-id"), json.loads(r.read() or b"null")
    sid, _ = mcp({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "claude-code"}}})
    def tool(name, args):
        _, r = mcp({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": name, "arguments": args}}, sid)
        return r["result"]["isError"], json.loads(r["result"]["content"][0]["text"])
    err, v = tool("list_storage_accounts", {})
    assert not err and {a["account"] for a in v["accounts"]} == {"acme-docs-stg", "globex-stg-files"}, v
    err, v = tool("set_next_read", {"account": "acme-docs-stg", "container": "documents", "path": "invoices/invoice-0042.pdf", "result": "404"})
    assert not err
    st, _, _ = raw("GET", f"http://127.0.0.1:{BLOB}/acme-docs-stg/documents/invoices/invoice-0042.pdf", headers={"Authorization": "SharedKey acme-docs-stg:x"})
    assert st == 404
    err, v = tool("create_rule", {"service": "s3", "name": "Slow thumbnails", "when": {"op": "get", "path": "thumbs/**"}, "then": {"respond": "slow", "seconds": 1}})
    assert not err and v["meta"] == "GET thumbs/** → +1 s", v
    err, v = tool("wait_for_delivery", {"subscription": "acme-api webhook", "timeout": "1s"})
    assert not err or "No delivery" in v["error"], v
    err, v = tool("create_link", {"account": "acme-docs-stg", "container": "documents", "path": "invoices/invoice-0042.pdf", "valid_for": "10m"})
    assert not err and v["issuedBy"] == "claude-code" and v["kind"] == "sas", v
    assert raw("GET", v["url"])[0] == 200

    # ---- Environments: a copy has its own storage (by prefix on the provider port)
    call("POST", "/environments", {"name": "pr-7", "from": "stg-a"})
    pr = f"http://127.0.0.1:{BLOB}/e/pr-7/acme-docs-stg/documents/invoices/invoice-0042.pdf"
    st, _, b = raw("GET", pr, headers={"Authorization": "SharedKey acme-docs-stg:x"})
    assert st == 200 and b == b"%PDF-1.4 seed", (st, b)  # the copy loaded its seed
    assert raw("PUT", f"http://127.0.0.1:{BLOB}/e/pr-7/acme-docs-stg/documents/only-pr.pdf", b"x", {"x-ms-blob-type": "BlockBlob", "Authorization": "SharedKey acme-docs-stg:x"})[0] == 201
    assert raw("GET", f"http://127.0.0.1:{BLOB}/acme-docs-stg/documents/only-pr.pdf", headers={"Authorization": "SharedKey acme-docs-stg:x"})[0] == 404
    call("POST", "/environments/pr-7/pause", {"paused": True})
    assert raw("GET", pr)[0] == 503


def main():
    data = tempfile.mkdtemp(prefix="stuntman-sto-")
    tmp = tempfile.mkdtemp(prefix="stuntman-sto-files-")
    recv = http.server.ThreadingHTTPServer(("127.0.0.1", RECV), Recv)
    threading.Thread(target=recv.serve_forever, daemon=True).start()
    env = dict(os.environ, STUNTMAN_ENV=os.environ.get("STUNTMAN_ENV", "stg-a"), STUNTMAN_DATA=data, STUNTMAN_PORT=str(HTTP), STUNTMAN_HOOKS_PORT=str(HOOKS), STUNTMAN_SMTP_PORT=str(SMTP), STUNTMAN_OTLP_HTTP_PORT=str(OTLP),
               STUNTMAN_OTLP_GRPC_PORT=str(OTLP_G), STUNTMAN_BLOB_PORT=str(BLOB), STUNTMAN_S3_PORT=str(S3), STUNTMAN_WEB="/nonexistent")
    # Every other port too: the defaults (5672, 10000, …) may belong to a Stuntman already running.
    import harness
    env.update({f"STUNTMAN_{k}": str(free_port()) for k in harness.PORTS if f"STUNTMAN_{k}" not in env})
    log = open(os.path.join(tmp, "server.log"), "w")
    srv = subprocess.Popen([BIN], env=env, stdout=log, stderr=subprocess.STDOUT)
    try:
        def up():
            try:
                socket.create_connection(("127.0.0.1", HTTP), 0.2).close()
                return True
            except OSError:
                return False
        wait(up, "server")
        run(data, tmp)
        print("e2e storage: all checks passed")
    except Exception:
        print(open(os.path.join(tmp, "server.log")).read()[-3000:])
        raise
    finally:
        srv.terminate(); recv.shutdown(); shutil.rmtree(data, ignore_errors=True); shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
