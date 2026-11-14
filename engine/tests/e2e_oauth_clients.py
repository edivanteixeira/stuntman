"""End-to-end check of OAuth clients (I6): Stuntman runs the ten MCP authorization checks as the client
of an app's OAuth server. The app here is a small fake with two flaws on purpose (it accepts a request
without PKCE, and its access token has no audience); Stuntman must flag exactly those two.

    cargo build -p stuntman && python3 tests/e2e_oauth_clients.py
"""
import base64, hashlib, http.server, json, os, shutil, socket, subprocess, tempfile, threading, time, urllib.error, urllib.parse, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "stuntman")


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


HTTP, HOOKS, SMTP, OTLP, OTLP_G, BLOB, S3, APIS, TLS, KV, APP = (free_port() for _ in range(11))
A = f"http://127.0.0.1:{APP}"
PENDING, CODES, REFRESH, USED = {}, {}, {}, set()


def b64u(b): return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def jwt(claims): return f"{b64u(json.dumps({'alg': 'none'}).encode())}.{b64u(json.dumps(claims).encode())}.x"


class App(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass

    def send(self, st, body=None, headers=None):
        b = json.dumps(body).encode() if body is not None else b""
        self.send_response(st)
        for k, v in (headers or {}).items(): self.send_header(k, v)
        if body is not None: self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)

    def do_GET(self):
        u = urllib.parse.urlparse(self.path); q = dict(urllib.parse.parse_qsl(u.query))
        if u.path == "/.well-known/oauth-protected-resource":
            return self.send(200, {"resource": f"{A}/mcp", "authorization_servers": [A], "scopes_supported": ["read", "offline_access"]})
        if u.path == "/.well-known/oauth-authorization-server":
            return self.send(200, {"issuer": A, "authorization_endpoint": f"{A}/authorize", "token_endpoint": f"{A}/token", "registration_endpoint": f"{A}/register", "code_challenge_methods_supported": ["S256"]})
        if u.path == "/authorize":
            # Flaw 1: no check of code_challenge. Everything goes to the login page.
            rid = f"req{len(PENDING)}"; PENDING[rid] = q
            return self.send(302, headers={"Location": f"{A}/login?req={rid}"})
        if u.path == "/approve":  # the person signs in and approves
            req = PENDING[q["req"]]; code = f"code{len(CODES)}"; CODES[code] = req
            return self.send(302, headers={"Location": f"{req['redirect_uri']}?code={code}&state={req['state']}"})
        self.send(404, {"error": "nope"})

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0); raw = self.rfile.read(n).decode()
        u = urllib.parse.urlparse(self.path)
        if u.path == "/register":
            return self.send(201, {"client_id": "fake-client-1", **json.loads(raw)})
        if u.path == "/token":
            f = dict(urllib.parse.parse_qsl(raw))
            if f["grant_type"] == "authorization_code":
                req = CODES.pop(f["code"], None)
                if not req or b64u(hashlib.sha256(f["code_verifier"].encode()).digest()) != req.get("code_challenge"):
                    return self.send(400, {"error": "invalid_grant"})
            else:
                if f["refresh_token"] in USED or f["refresh_token"] not in REFRESH: return self.send(400, {"error": "invalid_grant"})
                USED.add(f["refresh_token"])
            rt = f"rt{len(REFRESH)}"; REFRESH[rt] = True
            # Flaw 2: no aud in the access token.
            return self.send(200, {"access_token": jwt({"sub": "emma", "iss": A, "exp": int(time.time()) + 3600}), "token_type": "Bearer", "expires_in": 3600, "refresh_token": rt})
        if u.path == "/mcp":
            if not (self.headers.get("Authorization") or "").startswith("Bearer "):
                return self.send(401, {"error": "unauthorized"}, {"WWW-Authenticate": f'Bearer resource_metadata="{A}/.well-known/oauth-protected-resource"'})
            m = json.loads(raw)
            if "id" not in m: return self.send(202)
            if m["method"] == "tools/list": return self.send(200, {"jsonrpc": "2.0", "id": m["id"], "result": {"tools": [{"name": "a"}, {"name": "b"}]}})
            return self.send(200, {"jsonrpc": "2.0", "id": m["id"], "result": {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "fake"}}})
        self.send(404, {"error": "nope"})


def call(method, path, body=None):
    r = urllib.request.Request(f"http://127.0.0.1:{HTTP}/api{path}", method=method, data=None if body is None else json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(r, timeout=60) as x: return json.loads(x.read() or b"null")


def tool(name, args):
    body = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}
    r = urllib.request.Request(f"http://127.0.0.1:{HTTP}/mcp", data=json.dumps(body).encode(), headers={"Content-Type": "application/json", "Accept": "application/json, text/event-stream"})
    with urllib.request.urlopen(r) as x: res = json.loads(x.read())["result"]
    return res.get("isError", False), json.loads(res["content"][0]["text"])


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k): return None


def wait_run(run, want):
    for _ in range(100):
        r = call("GET", f"/oauth-clients/runs/{run}")
        if r["status"] in want: return r
        time.sleep(0.2)
    raise AssertionError(("run stuck", r))


def run():
    err, v = tool("list_oauth_clients", {})
    assert not err and {c["id"] for c in v["clients"]} >= {"mcp-dcr", "mcp-client"}, v
    run = call("POST", "/oauth-clients/mcp-dcr/runs", {"resource_url": f"{A}/mcp"})["run"]
    r = wait_run(run, ("waiting", "failed", "blocked"))
    assert r["status"] == "waiting" and r["authorizeUrl"].startswith(f"{A}/authorize"), r
    # The person: the app's authorize sends to its login page; approving sends the browser to Stuntman.
    loc = urllib.request.build_opener(NoRedirect)
    try: loc.open(r["authorizeUrl"])
    except urllib.error.HTTPError as e: login = e.headers["Location"]
    req = login.split("req=")[1]
    try: loc.open(f"{A}/approve?req={req}")
    except urllib.error.HTTPError as e: back = e.headers["Location"]
    assert back.startswith(f"http://127.0.0.1:{HTTP}/oauth-clients/callback"), back
    with urllib.request.urlopen(back) as x: assert x.status == 200
    r = wait_run(run, ("passed", "failed"))
    got = {c["n"]: c["ok"] for c in r["checks"]}
    assert got == {1: True, 2: True, 3: True, 4: False, 5: True, 6: True, 7: True, 8: False, 9: True, 10: True}, [(c["n"], c["ok"], c["detail"]) for c in r["checks"]]
    assert "aud missing" in r["checks"][7]["detail"] and "without code_challenge" in r["checks"][3]["detail"]
    assert "tools/list" in r["checks"][8]["detail"] and "2 tools" in r["checks"][8]["detail"]
    whole = jwt({"sub": "emma"}).split(".")[0]  # the start of every token the fake issues
    assert whole + "." not in json.dumps(r), "a whole access token is shown in the run"
    err, v = tool("get_run", {"run_id": run})
    assert not err and v["status"] == "failed" and "verifier" not in v, v
    # A URL client id served from a local Stuntman: the metadata document is served.
    with urllib.request.urlopen(f"http://127.0.0.1:{HTTP}/oauth-clients/mcp-client.json") as x:
        doc = json.loads(x.read())
    assert doc["client_id"].endswith("/oauth-clients/mcp-client.json") and doc["redirect_uris"][0].endswith("/oauth-clients/callback"), doc
    err, v = tool("serve_client_metadata", {"client": "partner", "kind": "preregistered", "client_id": "p-1", "client_secret": "s", "resource_url": f"{A}/mcp"})
    assert not err and v["client"] == "partner", v


def main():
    data = tempfile.mkdtemp(prefix="stuntman-oc-")
    app = http.server.ThreadingHTTPServer(("127.0.0.1", APP), App)
    threading.Thread(target=app.serve_forever, daemon=True).start()
    env = dict(os.environ, STUNTMAN_DATA=data, STUNTMAN_PORT=str(HTTP), STUNTMAN_HOOKS_PORT=str(HOOKS), STUNTMAN_SMTP_PORT=str(SMTP), STUNTMAN_OTLP_HTTP_PORT=str(OTLP),
               STUNTMAN_OTLP_GRPC_PORT=str(OTLP_G), STUNTMAN_BLOB_PORT=str(BLOB), STUNTMAN_S3_PORT=str(S3), STUNTMAN_APIS_PORT=str(APIS), STUNTMAN_APIS_TLS_PORT=str(TLS),
               STUNTMAN_KEYVAULT_PORT=str(KV), STUNTMAN_WEB="/nonexistent")
    log = data + ".log"
    srv = subprocess.Popen([BIN], env=env, stdout=open(log, "w"), stderr=subprocess.STDOUT)
    try:
        for _ in range(150):
            try: socket.create_connection(("127.0.0.1", HTTP), 0.2).close(); break
            except OSError: time.sleep(0.1)
        run()
        print("e2e oauth clients: all checks passed")
    except Exception:
        print(open(log).read()[-2000:]); raise
    finally:
        srv.terminate(); app.shutdown(); shutil.rmtree(data, ignore_errors=True)


if __name__ == "__main__":
    main()
