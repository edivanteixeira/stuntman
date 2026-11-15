"""Starts a Stuntman for a test on free ports, with its own data folder. Shared by the e2e tests.

    from harness import Stuntman
    with Stuntman() as s:
        s.call("GET", "/env")             # the HTTP API (the same tools MCP has)
        s.api("POST", "/svc/x", {...})    # the APIs port, as an app would call it
"""
import http.client, json, os, shutil, socket, subprocess, tempfile, time, urllib.error, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(os.environ.get("CARGO_TARGET_DIR", os.path.join(ROOT, "target")), "debug", "stuntman")
PORTS = ["PORT", "HOOKS_PORT", "SMTP_PORT", "OTLP_HTTP_PORT", "OTLP_GRPC_PORT", "BLOB_PORT", "S3_PORT", "APIS_PORT", "APIS_TLS_PORT", "KEYVAULT_PORT",
         "AMQP_PORT", "AMQPS_PORT", "IDENTITY_PORT", "LDAP_PORT", "FTP_PORT", "SFTP_PORT", "IMAP_PORT", "POP3_PORT", "KAFKA_PORT", "SCHEMA_REGISTRY_PORT",
         "GELF_PORT", "STATSD_PORT", "RABBITMQ_MGMT_PORT", "ACTIVEMQ_PORT", "PUBSUB_PORT"]


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


class Stuntman:
    def __init__(self, prefix="stuntman-e2e-"):
        self.ports = {k: free_port() for k in PORTS}
        self.data = tempfile.mkdtemp(prefix=prefix)
        self.log = self.data + ".log"

    def __getattr__(self, k):
        if k.isupper() and k in self.__dict__.get("ports", {}):
            return self.ports[k]
        raise AttributeError(k)

    def __enter__(self):
        env = dict(os.environ, STUNTMAN_DATA=self.data, STUNTMAN_WEB="/nonexistent", **{f"STUNTMAN_{k}": str(v) for k, v in self.ports.items()})
        self.proc = subprocess.Popen([BIN], env=env, stdout=open(self.log, "w"), stderr=subprocess.STDOUT)
        end = time.time() + 30
        while time.time() < end:
            try:
                self.call("GET", "/env")
                return self
            except Exception:
                time.sleep(0.2)
        self.__exit__(None, None, None)
        raise SystemExit("Stuntman did not start; log: " + self.log)

    def __exit__(self, et, ev, tb):
        self.proc.terminate()
        try:
            self.proc.wait(5)
        except Exception:
            self.proc.kill()
        if et is None:
            shutil.rmtree(self.data, ignore_errors=True)
        else:
            print("kept for inspection:", self.data, self.log)
            print(open(self.log).read()[-4000:])

    def call(self, method, path, body=None, expect=200):
        req = urllib.request.Request(f"http://127.0.0.1:{self.ports['PORT']}/api{path}", method=method, data=None if body is None else json.dumps(body).encode(),
                                     headers={"Content-Type": "application/json", "X-Stuntman-Client": "qa-suite"})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                st, v = r.status, json.loads(r.read() or b"null")
        except urllib.error.HTTPError as e:
            st, v = e.code, json.loads(e.read() or b"null")
        assert expect is None or st == expect, (method, path, st, v)
        return v

    def api(self, method, path, body=None, headers=None, port=None):
        c = http.client.HTTPConnection("127.0.0.1", port or self.ports["APIS_PORT"], timeout=30)
        h = {"User-Agent": "acme-shop/2.0"}
        h.update(headers or {})
        data = None
        if body is not None:
            data = body if isinstance(body, bytes) else json.dumps(body).encode()
            h.setdefault("Content-Type", "application/json")
        c.request(method, path, data, h)
        r = c.getresponse()
        raw = r.read()
        try:
            v = json.loads(raw)
        except Exception:
            v = raw
        return r.status, v, {k.lower(): v for k, v in r.getheaders()}


def wait(f, what, t=15):
    end = time.time() + t
    while time.time() < end:
        v = f()
        if v:
            return v
        time.sleep(0.1)
    raise AssertionError("timed out waiting for " + what)
