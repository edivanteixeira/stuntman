"""End-to-end check of `encode:` on an answer: the body is a template in text, compressed only on
the way out (gzip, deflate, zip; raw bytes, no Content-Encoding). Calls keeps the text.

    cargo build -p stuntman && python3 tests/e2e_encode.py
"""
import gzip, io, json, time, zipfile, zlib

from harness import Stuntman

PLUGIN = """
service:
  name: example-tracker
  title: Example Tracker
  base_url: https://api.tracker.example.com
  idempotency:
    header: Idempotency-Key
routes:
  - route: GET /positions.gz
    decide:
      - answer: gz
  - route: GET /positions.deflate
    decide:
      - answer: df
  - route: POST /positions.zip
    decide:
      - answer: zp
answers:
  - name: gz
    encode: gzip
    body: '<positions><at>{{ now() }}</at></positions>'
  - name: df
    encode: deflate
    body: '<positions><at>{{ now() }}</at></positions>'
  - name: zp
    encode: zip
    headers: {content-type: application/x-zip-compressed}
    body: '<positions><at>{{ now() }}</at></positions>'
"""


def main():
    with Stuntman("stuntman-encode-") as s:
        s.call("PUT", "/plugins/example-tracker", {"yaml": PLUGIN})
        bad = s.call("PUT", "/plugins/example-tracker", {"yaml": PLUGIN.replace("encode: deflate", "encode: brotli")}, expect=None)
        assert "gzip | deflate | zip" in json.dumps(bad), bad

        year = time.strftime("%Y")
        st, raw, h = s.api("GET", "/example-tracker/positions.gz")
        assert st == 200 and h["content-type"] == "application/octet-stream" and "content-encoding" not in h, (st, h)
        text = gzip.decompress(raw).decode()
        assert text.startswith("<positions><at>" + year) and "{{" not in text, text

        st, raw, h = s.api("GET", "/example-tracker/positions.deflate")
        assert zlib.decompress(raw, -15).decode().startswith("<positions><at>" + year), raw

        st, raw, h = s.api("POST", "/example-tracker/positions.zip", b"", {"Idempotency-Key": "k1"})
        assert h["content-type"] == "application/x-zip-compressed", h
        z = zipfile.ZipFile(io.BytesIO(raw))
        assert z.namelist() == ["body.xml"] and z.read("body.xml").decode().startswith("<positions><at>" + year), z.namelist()
        # A replay of the same key sends the same compressed bytes.
        st, raw2, _ = s.api("POST", "/example-tracker/positions.zip", b"", {"Idempotency-Key": "k1"})
        assert raw2 == raw, "idempotent replay differs"

        # Calls shows the text, with what it was sent as.
        calls = s.call("GET", "/apis/calls?since=1h&service=example-tracker")
        items = calls if isinstance(calls, list) else calls.get("calls") or calls.get("items")
        gz = [c for c in items if c.get("answer") == "gz"][0]
        c = s.call("GET", f"/apis/calls/{gz['id']}?raw=true")
        assert c["response"]["body"].startswith("<positions><at>" + year) and c["response"]["encode"] == "gzip", c["response"]
        assert any("sent as gzip" in x.get("text", "") for x in c["steps"]), c["steps"]
    print("e2e_encode ok")


if __name__ == "__main__":
    main()
