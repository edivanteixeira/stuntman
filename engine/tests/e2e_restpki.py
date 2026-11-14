"""Lacuna REST PKI + Web PKI as a real app runs them, against the restpki catalog plugin: the app starts a
PAdES signature (PascalCase, chunked), the browser signs the attributes with its own key, the app
collects the signed PDF; checked with pdfsig, openssl cms and qpdf. Synthetic data only.
Run with Stuntman up:  python3 tests/e2e_restpki.py
  STUNTMAN_API (default http://127.0.0.1:8025/api), STUNTMAN_APIS (default http://127.0.0.1:8028)
Needs: python cryptography, pdfsig (poppler), openssl, qpdf, mutool, pypdf (or uv to fetch it).
"""
import base64, datetime, hashlib, http.client, json, os, re, subprocess, tempfile, urllib.error, urllib.parse, urllib.request, zlib
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa
from cryptography.x509.oid import NameOID

API = os.environ.get("STUNTMAN_API", "http://127.0.0.1:8025/api")
PKI = os.environ.get("STUNTMAN_APIS", "http://127.0.0.1:8028") + "/restpki"
TMP = tempfile.mkdtemp(prefix="restpki-")
CM = 1 / 0.0352778


def call(method, url, body=None, headers=None):
    h = {"content-type": "application/json", **(headers or {})}
    r = urllib.request.Request(url, data=json.dumps(body).encode() if body is not None else None, method=method, headers=h)
    try:
        with urllib.request.urlopen(r) as x:
            return x.status, dict(x.headers), json.loads(x.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), json.loads(e.read() or b"null")


def chunked(path, body):
    """As the .NET RestPki client sends the PDF: Transfer-Encoding: chunked."""
    u = urllib.parse.urlparse(PKI + path)
    c = http.client.HTTPConnection(u.hostname, u.port)
    raw = json.dumps(body).encode()
    c.request("POST", u.path, body=iter([raw[i:i + 4096] for i in range(0, len(raw), 4096)]), headers={"Content-Type": "application/json", "Authorization": "Bearer any"}, encode_chunked=True)
    r = c.getresponse()
    return r.status, json.loads(r.read() or b"null")


# ------------------------------------------------------------------ throwaway PKI, PDF and PNG

def cert(subject, issuer, key, sign_key, ca):
    now = datetime.datetime.now(datetime.timezone.utc)
    b = (x509.CertificateBuilder().subject_name(subject).issuer_name(issuer).public_key(key.public_key()).serial_number(x509.random_serial_number())
         .not_valid_before(now - datetime.timedelta(days=1)).not_valid_after(now + datetime.timedelta(days=365))
         .add_extension(x509.BasicConstraints(ca=ca, path_length=None), critical=True))
    return b.sign(sign_key, hashes.SHA256())


ca_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
ca_name = x509.Name([x509.NameAttribute(NameOID.COUNTRY_NAME, "BR"), x509.NameAttribute(NameOID.ORGANIZATION_NAME, "Stuntman E2E"), x509.NameAttribute(NameOID.COMMON_NAME, "E2E Throwaway CA")])
ca = cert(ca_name, ca_name, ca_key, ca_key, True)
leaf_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
leaf_name = x509.Name([x509.NameAttribute(NameOID.COUNTRY_NAME, "BR"), x509.NameAttribute(NameOID.ORGANIZATION_NAME, "ICP-Brasil"), x509.NameAttribute(NameOID.COMMON_NAME, "Ava Morgan:52998224725")])
leaf = cert(leaf_name, ca_name, leaf_key, ca_key, False)
leaf_der, ca_der = leaf.public_bytes(serialization.Encoding.DER), ca.public_bytes(serialization.Encoding.DER)


def pdf2():
    """Two A4 pages; qpdf rewrites it with object streams and an xref stream (the hard case)."""
    objs = [b"<< /Type /Catalog /Pages 2 0 R >>", b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << >> >>", b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << >> >>"]
    out, offs = bytearray(b"%PDF-1.7\n"), []
    for i, o in enumerate(objs, 1):
        offs.append(len(out))
        out += b"%d 0 obj\n%s\nendobj\n" % (i, o)
    x = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1) + b"".join(b"%010d 00000 n \n" % o for o in offs)
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, x)
    src, dst = f"{TMP}/in.pdf", f"{TMP}/in-objstm.pdf"
    open(src, "wb").write(out)
    subprocess.run(["qpdf", "--object-streams=generate", src, dst], check=True)
    return open(dst, "rb").read()


def png(w=40, h=20):
    rows = b"".join(b"\x00" + b"".join(bytes([30, 60, 200, 255 if (x + y) % 7 else 0]) for x in range(w)) for y in range(h))
    chunk = lambda t, d: len(d).to_bytes(4, "big") + t + d + zlib.crc32(t + d).to_bytes(4, "big")
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", w.to_bytes(4, "big") + h.to_bytes(4, "big") + b"\x08\x06\x00\x00\x00") + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")


PDF, PNG = pdf2(), png()
B = lambda b: base64.b64encode(b).decode()


def start(visual=True):
    body = {"PdfToSign": {"Content": B(PDF)}, "SignaturePolicyId": "78d20b33-014d-440e-ad07-929f05de4f3c", "SecurityContextId": "803517ad-3bbc-4169-b085-60053a8f6dbf"}
    if visual:
        body["VisualRepresentation"] = {"Position": {"PageNumber": 2, "MeasurementUnits": "Centimeters", "Manual": {"Left": 2.0, "Bottom": 3.0, "Width": 5.0, "Height": 2.5}},
                                        "Image": {"Resource": {"Content": B(PNG), "MimeType": "image/png"}}}
    s, r = chunked("/Api/v2/PadesSignatures", body)
    assert s == 200 and r["token"] and r["toSignData"] is None, (s, r)
    return r["token"]


def webpki(token, good=True):
    s, h, r = call("POST", f"{PKI}/__webpki/{token}/start", {"certificate": B(leaf_der), "chain": [B(ca_der)]}, {"Origin": "http://localhost:3000"})
    assert s == 200 and r["digestAlgorithm"] == "SHA-256" and h.get("access-control-allow-origin") == "*", (s, h, r)
    tbs = base64.b64decode(r["toSignData"])
    key = leaf_key if good else rsa.generate_private_key(public_exponent=65537, key_size=2048)
    return call("POST", f"{PKI}/__webpki/{token}/signature", {"signature": B(key.sign(tbs, padding.PKCS1v15(), hashes.SHA256()))})


def finish(token):
    return chunked(f"/Api/v2/PadesSignatures/{token}/SignedBytes", {})


def strict_structure(f, pdf):
    """The update numbers its objects above the original /Size, lists each one in an xref stream (the
    original has one: iText follows /Prev from a table only into a table) and the page tree resolves."""
    q = subprocess.run(["qpdf", "--check", f], capture_output=True, text=True)
    assert q.returncode == 0 and "WARNING" not in q.stdout + q.stderr, q.stdout + q.stderr
    upd = pdf[len(PDF):]
    size0 = int(re.findall(rb"/Size\s+(\d+)", PDF)[-1])
    new = [int(n) for n in re.findall(rb"(?m)^(\d+) 0 obj", upd)]
    shown = subprocess.run(["qpdf", "--show-xref", f], capture_output=True, text=True).stdout
    assert b"/Type/XRef" in upd.replace(b" ", b"") and not re.search(rb"(?m)^xref", upd), "the update's xref is a stream like the original's"
    assert all(f"{n}/0: uncompressed" in shown for n in new), (new, shown)
    fresh, old = sorted(n for n in new if n >= size0), {n for n in new if n < size0}
    assert len(fresh) >= 4 and fresh == list(range(size0, size0 + len(fresh))) and fresh[-1] + 1 == int(re.findall(rb"/Size\s+(\d+)", upd)[-1]), (size0, new)
    assert old <= set(range(1, size0)) and len(old) == len(set(n for n in new if n < size0)), (size0, new)
    assert int(re.findall(rb"/Prev\s+(\d+)", upd)[-1]) == int(re.findall(rb"startxref\s+(\d+)", PDF)[-1])
    walk = "import pypdf,sys; r=pypdf.PdfReader(sys.argv[1], strict=True); p=r.trailer['/Root']['/Pages'].get_object(); assert isinstance(p, pypdf.generic.DictionaryObject) and p['/Type']=='/Pages', p; print(len(r.pages), [a.get_object()['/FT'] for a in r.pages[1].get('/Annots', [])])"
    try:
        import pypdf  # noqa: F401
        w = subprocess.run(["python3", "-c", walk, f], capture_output=True, text=True)
    except ImportError:
        w = subprocess.run(["uv", "run", "-q", "--no-project", "--with", "pypdf", "python", "-c", walk, f], capture_output=True, text=True)
    assert w.returncode == 0 and w.stdout.startswith("2 "), w.stdout + w.stderr
    m = subprocess.run(["mutool", "draw", "-o", f"{TMP}/page%d.png", f], capture_output=True, text=True)
    assert m.returncode == 0 and "error" not in m.stderr.lower(), m.stderr


def check_pdf(pdf, name, visible):
    f = f"{TMP}/{name}.pdf"
    open(f, "wb").write(pdf)
    assert pdf.startswith(PDF), "an incremental update keeps the original bytes"
    strict_structure(f, pdf)
    out = subprocess.run(["pdfsig", f], capture_output=True, text=True).stdout
    assert "Signature is Valid" in out and "ETSI.CAdES.detached" in out and "Ava Morgan:52998224725" in out, out
    br = [int(x) for x in re.findall(rb"/ByteRange\s*\[\s*(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s*\]", pdf)[-1]]
    assert br[0] == 0 and br[2] + br[3] == len(pdf), br
    open(f"{TMP}/{name}.bin", "wb").write(pdf[:br[1]] + pdf[br[2]:])
    open(f"{TMP}/{name}.p7s", "wb").write(bytes.fromhex(pdf[br[1] + 1:br[2] - 1].decode()))
    v = subprocess.run(["openssl", "cms", "-verify", "-binary", "-inform", "DER", "-noverify", "-in", f"{TMP}/{name}.p7s", "-content", f"{TMP}/{name}.bin", "-out", os.devnull], capture_output=True, text=True)
    assert v.returncode == 0 and "Verification successful" in v.stderr, v.stderr
    certs = subprocess.run(["openssl", "pkcs7", "-inform", "DER", "-in", f"{TMP}/{name}.p7s", "-print_certs", "-noout"], capture_output=True, text=True).stdout
    assert "Ava Morgan" in certs and "E2E Throwaway CA" in certs, certs
    attrs = subprocess.run(["openssl", "cms", "-cmsout", "-print", "-inform", "DER", "-in", f"{TMP}/{name}.p7s"], capture_output=True, text=True).stdout
    assert "messageDigest" in attrs and "contentType" in attrs and ("signingCertificateV2" in attrs or "1.2.840.113549.1.9.16.2.47" in attrs), attrs
    j = json.loads(subprocess.run(["qpdf", "--json=2", f], capture_output=True, text=True).stdout)
    field = [x for x in j["acroform"]["fields"] if x["fieldtype"] == "/Sig"][-1]
    widget = j["qpdf"][1][f"obj:{field['annotation']['object']}"]["value"]
    rect = [float(x) for x in widget["/Rect"]]
    if visible:
        assert field["pageposfrom1"] == 2, field
        want = [2 * CM, 3 * CM, 7 * CM, 5.5 * CM]
        assert all(abs(a - b) < 0.1 for a, b in zip(rect, want)), (rect, want)
        ap = j["qpdf"][1][f"obj:{widget['/AP']['/N']}"]
        assert "/Img" in ap["stream"]["dict"]["/Resources"]["/XObject"], ap
    else:
        assert rect == [0, 0, 0, 0], rect
    return rect


try:
    s, _, r = call("POST", f"{API}/plugins", {"plugin": "restpki"})
    assert s in (200, 409) or "already" in json.dumps(r), (s, r)
    call("POST", f"{API}/apis/restpki/reset")
    s, _, info = call("GET", f"{PKI}/api/SYSTEM/info")
    assert s == 200 and info["productName"] and info["productVersion"], info

    # 1. Happy path, visible stamp on page 2.
    t = start()
    req = urllib.request.Request(f"{PKI}/__webpki/{t}/start", method="OPTIONS", headers={"Origin": "http://localhost:3000", "Access-Control-Request-Method": "POST"})
    with urllib.request.urlopen(req) as x:
        assert x.status == 204 and x.headers["access-control-allow-origin"] == "*"
    s, _, e = webpki(t, good=False)
    assert s == 422 and e["code"] == "InvalidSignature", (s, e)
    s, e = finish(t)
    assert s == 422 and e["code"] == "SignatureNotFound", (s, e)
    s, _, r = webpki(t)
    assert s == 200 and r == {"ok": True}, (s, r)
    s, r = finish(t)
    assert s == 200 and r["signatureFile"]["mimeType"] == "application/pdf" and r["callbackArgument"] is None, s
    c = r["certificate"]
    assert c["pkiBrazil"]["cpf"] == "52998224725" and c["pkiBrazil"]["responsavel"] == "Ava Morgan", c
    assert c["thumbprint"] == hashlib.sha1(leaf_der).hexdigest() and c["binaryThumbprintSHA256"] == B(hashlib.sha256(leaf_der).digest()), c
    assert c["subjectName"]["allValues"]["2.5.4.3"] == ["Ava Morgan:52998224725"] and c["issuerName"]["commonName"] == "E2E Throwaway CA", c
    rect = check_pdf(base64.b64decode(r["signatureFile"]["content"]), "visible", True)

    # 2. Unknown token, forced failures (start 422, finish 500), then an invisible signature.
    s, e = finish("0" * 32)
    assert s == 404 and e["code"] == "TokenNotFound", (s, e)
    s, _, _ = call("POST", f"{API}/apis/restpki/fail", {"step": "start", "status": "422"})
    assert s == 200
    s, e = chunked("/Api/v2/PadesSignatures", {"pdfToSign": {"content": B(PDF)}})
    assert s == 422 and e["code"] == "ValidationError", (s, e)
    t2 = start(visual=False)
    assert webpki(t2)[0] == 200
    call("POST", f"{API}/apis/restpki/fail", {"step": "finish", "status": "500"})
    s, _ = finish(t2)
    assert s == 500, s
    s, r = finish(t2)
    assert s == 200, (s, r)
    check_pdf(base64.b64decode(r["signatureFile"]["content"]), "invisible", False)

    # 3. Calls and sessions as the UI and agents see them.
    _, _, calls = call("GET", f"{API}/apis/calls?service=restpki&since=10m")
    answers = [x["answer"] for x in calls["calls"]]
    for a in ["info", "started", "to_sign", "invalid_signature", "signature_not_found", "signature_ok", "signed", "token_not_found", "validation_error", "500"]:
        assert a in answers, (a, answers)
    st = [x for x in calls["calls"] if x["answer"] == "started" and x["sent"]["token"] == t][0]
    assert st["sent"]["page"] == 2 and st["sent"]["image"] is True and st["sent"]["position"]["width"] == 5.0, st
    assert any(x["provoked"] for x in calls["calls"] if x["answer"] == "500")
    _, _, ss = call("GET", f"{API}/apis/restpki/sessions")
    assert {x["token"]: x["status"] for x in ss["sessions"]}[t] == "finished", ss
    print(f"restpki ok: info, PAdES on page 2 at {[round(x) for x in rect]} pt with the PNG, invisible one, pdfsig + openssl cms verify, chain in the CMS, "
          "InvalidSignature 422, SignatureNotFound 422, unknown token 404, forced 422 (start) and 500 (finish), CORS preflight, calls recorded")
finally:
    call("POST", f"{API}/apis/restpki/reset")
