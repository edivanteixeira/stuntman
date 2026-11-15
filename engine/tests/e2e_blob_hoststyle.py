"""Azure Blob through the connection string get_account gives, when it names the account's own host
(<account>.blob.<base domain>): Microsoft's Python SDK (azure-storage-blob) and, when dotnet is
installed, Azure.Storage.Blobs (tests/dotnet-azure-blob). Run on a port the SDKs take as an
emulator's (11100–11104: account in the path) and on an ordinary one (account in the host).
Regression: the .NET SDK read the container as the account on port 10000 and every upload went to
a blob with no name; include=Metadata (any case) must list metadata.

    cargo build -p stuntman && python3 tests/e2e_blob_hoststyle.py   (needs azure-storage-blob)
"""
import http.client, os, socket, subprocess, sys, urllib.parse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from harness import ROOT, Stuntman

from azure.storage.blob import BlobServiceClient

# The SDKs resolve <account>.blob.stg-a.stuntman.local: answer loopback, as a hosts entry would.
_gai = socket.getaddrinfo
socket.getaddrinfo = lambda host, *a, **k: _gai("127.0.0.1" if str(host).endswith(".stuntman.local") else host, *a, **k)


def free_path_style_port():
    for p in (11100, 11101, 11102, 11103, 11104, 11000, 11001, 11002, 11003, 11004):
        s = socket.socket()
        try:
            s.bind(("127.0.0.1", p)); s.close(); return p
        except OSError:
            s.close()
    raise SystemExit("no free path-style port (11000–11004, 11100–11104)")


def check(path_style):
    os.environ["STUNTMAN_HOST"] = "stuntman.test:8025"  # a name, not an IP: host-style endpoints
    s = Stuntman()
    if path_style:
        s.ports["BLOB_PORT"] = free_path_style_port()
    with s:
        s.call("POST", "/services/azure-blob/accounts", {"account": "acmedocs", "containers": {"uploads": {"access": "private"}}})
        conn = s.call("GET", "/services/azure-blob/accounts/acmedocs")["connection"]
        ep = conn.split("BlobEndpoint=")[1].rstrip(";")
        assert ep.startswith(f"http://acmedocs.blob.stg-a.stuntman.local:{s.BLOB_PORT}"), ep
        assert ep.endswith("/acmedocs") == path_style, ep
        svc = BlobServiceClient.from_connection_string(conn, max_single_put_size=1024 * 1024, max_block_size=1024 * 1024)  # 5 MB goes in blocks
        c = svc.get_container_client("uploads")
        files = {"report.pdf": b"%PDF-1 report", "2026/11/invoice-0042.pdf": b"%PDF-1 invoice", "a/b/c/deep name.txt": b"deep"}
        for name, data in files.items():
            c.upload_blob(name, data, overwrite=True, metadata={"origin": name.split("/")[0]})
        # A big one goes through Put Block / Put Block List.
        big = os.urandom(5 * 1024 * 1024)
        c.get_blob_client("big/blob.bin").upload_blob(big, overwrite=True)
        for name, data in files.items():
            assert c.download_blob(name).readall() == data, name
            assert c.get_blob_client(name).get_blob_properties().metadata == {"origin": name.split("/")[0]}, name
        assert c.download_blob("big/blob.bin").readall() == big
        listed = {b.name: b.metadata for b in c.list_blobs(include=["metadata"])}
        assert set(listed) == set(files) | {"big/blob.bin"}, listed
        assert listed["report.pdf"] == {"origin": "report.pdf"}, listed
        assert [b.name for b in c.walk_blobs(name_starts_with="2026/")] == ["2026/11/"]
        # Metadata, copy, SAS, delete
        b = c.get_blob_client("report.pdf")
        b.set_blob_metadata({"stage": "signed"})
        assert b.get_blob_properties().metadata == {"stage": "signed"}
        cp = c.get_blob_client("copies/report.pdf")
        cp.start_copy_from_url(b.url)
        assert cp.download_blob().readall() == files["report.pdf"]
        from azure.storage.blob import generate_blob_sas, BlobSasPermissions
        import datetime
        key = conn.split("AccountKey=")[1].split(";")[0]
        sas = generate_blob_sas("acmedocs", "uploads", "2026/11/invoice-0042.pdf", account_key=key, permission=BlobSasPermissions(read=True), expiry=datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1))
        u = urllib.parse.urlsplit(f"{c.get_blob_client('2026/11/invoice-0042.pdf').url}?{sas}")
        h = http.client.HTTPConnection("127.0.0.1", u.port, timeout=10)
        h.request("GET", f"{u.path}?{u.query}", headers={"Host": u.netloc})
        r = h.getresponse()
        assert r.status == 200 and r.read() == files["2026/11/invoice-0042.pdf"], r.status
        cp.delete_blob()
        assert "copies/report.pdf" not in [x.name for x in c.list_blobs()]
        # Nothing landed under an empty name, and a request without a blob name is refused.
        assert "" not in [x.name for x in c.list_blobs()]
        u = urllib.parse.urlsplit(ep)
        h = http.client.HTTPConnection("127.0.0.1", s.BLOB_PORT, timeout=10)
        h.request("PUT", f"{u.path}/uploads", b"x", {"Host": u.netloc, "x-ms-version": "2024-11-04", "x-ms-blob-type": "BlockBlob", "Authorization": "SharedKey acmedocs:x"})
        r = h.getresponse(); r.read()
        assert r.status == 400 and r.getheader("x-ms-error-code") == "InvalidUri", r.status
        # include / comp / restype in any case, as the .NET SDK and Azure take them
        meta_list = raw_list(s.BLOB_PORT, ep, "restype=Container&comp=List&include=Metadata,Snapshots")
        assert "<origin>2026</origin>" in meta_list, meta_list[:400]
        assert "<Metadata>" not in raw_list(s.BLOB_PORT, ep, "restype=container&comp=list"), "no include, no metadata"
        if subprocess.run(["which", "dotnet"], capture_output=True).returncode == 0:
            p = subprocess.run(["dotnet", "run", "--project", os.path.join(ROOT, "tests", "dotnet-azure-blob"), "--", conn, "127.0.0.1"], capture_output=True, text=True, timeout=600)
            print(p.stdout[-1500:])
            assert p.returncode == 0, p.stdout[-3000:] + p.stderr[-2000:]
        else:
            print("dotnet not installed: .NET check skipped")
    print(f"ok  {'path-style port' if path_style else 'ordinary port'}: {ep}")


def raw_list(port, ep, q):
    """List Blobs with a hand-written query (any_key is on for new accounts, so SharedKey needs no real signature)."""
    u = urllib.parse.urlsplit(ep)
    h = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    h.request("GET", f"{u.path}/uploads?{q}", headers={"Host": u.netloc, "x-ms-version": "2024-11-04", "Authorization": "SharedKey acmedocs:x"})
    r = h.getresponse()
    body = r.read().decode()
    assert r.status == 200, (r.status, body)
    return body


if __name__ == "__main__":
    check(path_style=True)
    check(path_style=False)
    print("e2e blob host-style: all checks passed")
