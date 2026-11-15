"""End-to-end check of SOAP services (A7): a plugin made from a WSDL answers envelopes like the
provider (operations by SOAPAction or body, XSD checks, faults, WS-Security, SOAP 1.1 vs 1.2,
unknown operations to Unmatched, async callback), serves the WSDL back, and everything works over
the HTTP API (the same tools MCP has).

    cargo build -p stuntman && python3 tests/e2e_soap.py
"""
import http.client, http.server, json, os, shutil, socket, subprocess, tempfile, threading, time, urllib.error, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "stuntman")
WSDL = os.path.join(ROOT, "tests", "soap", "PolicyService.wsdl")
NS12, NS11 = "http://www.w3.org/2003/05/soap-envelope", "http://schemas.xmlsoap.org/soap/envelope/"
ACT = "urn:example:insurance/"


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


HTTP, HOOKS, SMTP, OTLP, OTLP_G, BLOB, S3, APIS, RECV, TLS, KV, AMQP, AMQPS = (free_port() for _ in range(13))
GOT = []


class Recv(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        GOT.append({"path": self.path, "headers": {k.lower(): v for k, v in self.headers.items()}, "body": self.rfile.read(int(self.headers["Content-Length"])).decode()})
        self.send_response(200); self.send_header("Content-Length", "0"); self.end_headers()

    def log_message(self, *a):
        pass


def call(method, path, body=None, expect=200):
    req = urllib.request.Request(f"http://127.0.0.1:{HTTP}/api{path}", method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "X-Stuntman-Client": "qa-suite"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            st, v = r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        st, v = e.code, json.loads(e.read() or b"null")
    assert expect is None or st == expect, (method, path, st, v)
    return v


def soap(payload, action=None, version="1.2", header="", path="/example-insurance/PolicyService", user=None):
    ns = NS12 if version == "1.2" else NS11
    if user:
        header = f'<soap:Header><wsse:Security xmlns:wsse="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd"><wsse:UsernameToken><wsse:Username>{user[0]}</wsse:Username><wsse:Password>{user[1]}</wsse:Password></wsse:UsernameToken></wsse:Security></soap:Header>'
    env = f'<?xml version="1.0"?><soap:Envelope xmlns:soap="{ns}">{header}<soap:Body>{payload}</soap:Body></soap:Envelope>'
    h = {"User-Agent": "acme-freight/3.1"}
    if version == "1.2":
        h["Content-Type"] = "application/soap+xml; charset=utf-8" + (f'; action="{ACT}{action}"' if action else "")
    else:
        h["Content-Type"] = "text/xml; charset=utf-8"
        if action:
            h["SOAPAction"] = f'"{ACT}{action}"'
    c = http.client.HTTPConnection("127.0.0.1", APIS, timeout=30)
    c.request("POST", path, env.encode(), h)
    r = c.getresponse()
    return r.status, r.read().decode(), {k.lower(): v for k, v in r.getheaders()}


def register(number="SHP-20481", value="1180000.00", insurer="globex-mutual", extra=""):
    return (f'<r:RegisterShipmentRequest xmlns:r="urn:example:insurance"><r:shipmentNumber>{number}</r:shipmentNumber><r:cargoValue>{value}</r:cargoValue>'
            f'<r:insurer>{insurer}</r:insurer><r:departure>2026-11-14</r:departure><r:destination><r:city>Springfield</r:city><r:country>US</r:country></r:destination>{extra}</r:RegisterShipmentRequest>')


def wait(f, what, t=15):
    end = time.time() + t
    while time.time() < end:
        v = f()
        if v:
            return v
        time.sleep(0.1)
    raise AssertionError("timed out waiting for " + what)


def run():
    # Import from a file: operations, faults, the callback, the imported XSD.
    im = call("POST", "/plugins/wsdl", {"service": "example-insurance", "source": WSDL, "title": "Example Insurance"})
    assert im["soap"] == "1.2" and im["endpoint"] == "/PolicyService", im
    assert [o["operation"] for o in im["operations"]] == ["RegisterShipment", "CancelShipment", "GetStatus", "Ping"], im
    assert sorted(im["faults"]) == ["coverage_exceeded", "policy_suspended", "shipment_not_found"], im
    assert im["callbacks"] == ["StatusChanged"] and im["warnings"] == [], im
    call("POST", "/plugins/wsdl", {"service": "example-insurance", "source": WSDL}, expect=409)
    ops = call("GET", "/plugins/example-insurance/operations")
    assert ops["types"]["RegisterShipmentRequest"]["destination"] == "Address" and ops["types"]["Address"] == {"city": "string", "country": "string"}, ops["types"]
    assert ops["types"]["RegisterShipmentResponse"]["status"] == "enum(REGISTERED, ACTIVE, CANCELLED)", ops["types"]["RegisterShipmentResponse"]

    # The WSDL and the XSD it imports, served back.
    c = http.client.HTTPConnection("127.0.0.1", APIS, timeout=10)
    c.request("GET", "/example-insurance/PolicyService?wsdl"); r = c.getresponse(); w = r.read().decode()
    assert r.status == 200 and "wsdl:definitions" in w and 'schemaLocation="common.xsd"' in w, (r.status, w[:200])
    c.request("GET", "/example-insurance/common.xsd"); r = c.getresponse(); assert r.status == 200 and "ShipmentStatus" in r.read().decode()

    # Default answers straight from the WSDL: the response echoes what the app sent.
    st, body, h = soap(register(), "RegisterShipment")
    assert st == 200 and h["content-type"].startswith("application/soap+xml"), (st, body)
    assert body.index("<protocol>") < body.index("<status>REGISTERED</status>") < body.index("<insurer>globex-mutual</insurer>"), "the XSD's order and the enum's first value"
    assert "<insurer>globex-mutual</insurer>" in body and f'xmlns:soap="{NS12}"' in body and 'xmlns="urn:example:insurance"' in body, body

    # Steps: keep, decide on body fields, fault with a templated reason, callback as SOAP.
    steps = {"billable": True, "keep": {"state": "shipments", "key": "body.shipmentNumber"},
             "decide": [{"when": "body.cargoValue > 2000000", "answer": "coverage_exceeded"},
                        {"when": "body.insurer in ['suspended-mutual']", "answer": "policy_suspended"},
                        {"answer": "registered"}],
             "send": [{"url": f"http://127.0.0.1:{RECV}/soap/callback", "operation": "StatusChanged",
                       "body": '<StatusChanged xmlns="urn:example:insurance"><protocol>EXP-{{ body.shipmentNumber }}</protocol><status>ACTIVE</status></StatusChanged>'}]}
    call("PUT", "/plugins/example-insurance/answers/registered", {"service": "example-insurance", "name": "registered",
         "body": '<RegisterShipmentResponse xmlns="urn:example:insurance"><protocol>EXP-{{ body.shipmentNumber }}</protocol><status>REGISTERED</status><insurer>{{ body.insurer }}</insurer></RegisterShipmentResponse>',
         "sentence": "{{ caller }} insured {{ body.shipmentNumber }} for {{ body.cargoValue }}"})
    call("PUT", "/plugins/example-insurance/answers/coverage_exceeded", {"service": "example-insurance", "name": "coverage_exceeded",
         "fault": {"code": "Sender", "reason": "Coverage exceeded: cargo value {{ body.cargoValue }}"},
         "body": '<CoverageExceeded xmlns="urn:example:insurance"><limit>2000000</limit><requested>{{ body.cargoValue }}</requested></CoverageExceeded>'})
    call("PUT", "/plugins/example-insurance/operations", {"service": "example-insurance", "operation": "RegisterShipment", "steps": steps})
    GOT.clear()
    st, body, _ = soap(register(), "RegisterShipment")
    assert st == 200 and "<protocol>EXP-SHP-20481</protocol>" in body, body
    cb = wait(lambda: GOT[:1], "the StatusChanged callback")[0]
    assert cb["path"] == "/soap/callback" and 'action="StatusChanged"' in cb["headers"]["content-type"] and "<protocol>EXP-SHP-20481</protocol>" in cb["body"] and NS12 in cb["body"], cb
    st, body, _ = soap(register(insurer="Smith &amp; Sons"), "RegisterShipment")
    assert st == 200 and "<insurer>Smith &amp; Sons</insurer>" in body, body
    st, body, _ = soap(register("SHP-20479", "2450000"), "RegisterShipment")
    assert st == 500 and "<soap:Value>soap:Sender</soap:Value>" in body and "Coverage exceeded: cargo value 2450000" in body and "<requested>2450000</requested>" in body, body

    # The XSD: a wrong value and an element the type does not have.
    st, body, _ = soap(register(value="lots"), "RegisterShipment")
    assert st == 500 and "Validation error: cargoValue is not a decimal" in body, body
    st, body, _ = soap(register(extra="<r:color>red</r:color>"), "RegisterShipment")
    assert st == 500 and "color is not in the XSD" in body, body

    # No SOAPAction: the operation is picked from the body, and the call says so.
    st, body, _ = soap('<GetStatusRequest xmlns="urn:example:insurance"><protocol>EXP-1</protocol></GetStatusRequest>')
    assert st == 200 and "<GetStatusResponse" in body, body
    calls = call("GET", "/apis/calls?service=example-insurance")
    rows = calls["calls"] if isinstance(calls, dict) else calls
    assert any("GetStatus" in c["route"] for c in rows), rows[:2]
    detail = call("GET", "/apis/calls/" + next(c["id"] for c in rows if "GetStatus" in c["route"]))
    assert "from the body (no SOAPAction)" in json.dumps(detail["steps"] if "steps" in detail else detail), detail

    # A SOAP 1.1 client on a 1.2 service.
    st, body, h = soap(register(), "RegisterShipment", version="1.1")
    assert st == 500 and "VersionMismatch" in body and NS11 in body and h["content-type"].startswith("text/xml"), body

    # An operation the WSDL does not have → fault, and listed in Unmatched.
    st, body, h = soap('<RegisterShipmentV3 xmlns="urn:example:insurance"/>', "RegisterShipmentV3")
    assert st == 500 and "is not in this service's WSDL" in body and h.get("x-stuntman") == "no route", body
    un = call("GET", "/apis/unmatched")
    assert "RegisterShipmentV3" in json.dumps(un), un

    # WS-Security: UsernameToken checked against the service's users.
    p = call("GET", "/plugins/example-insurance")
    y = p["yaml"].replace("  validate: true\n", "  validate: true\n  security: username_token\n  users:\n    acme-freight: secret-pw\n")
    call("PUT", "/plugins/example-insurance", {"service": "example-insurance", "yaml": y, "version": p["version"]})
    st, body, _ = soap(register(), "RegisterShipment", user=("acme-freight", "wrong"))
    assert st == 500 and "security token could not be authenticated" in body, body
    st, body, _ = soap(register(), "RegisterShipment")
    assert st == 500 and "security token" in body, "no token at all is refused too"
    st, body, _ = soap(register(), "RegisterShipment", user=("acme-freight", "secret-pw"))
    assert st == 200, body

    # Force a fault for the next call, like any named answer.
    call("POST", "/apis/example-insurance/force", {"service": "example-insurance", "answer": "policy_suspended", "count": 1})
    st, body, _ = soap(register(), "RegisterShipment", user=("acme-freight", "secret-pw"))
    assert st == 500 and "Policy suspended" in body, body

    # Try it: nothing kept, nothing sent.
    GOT.clear()
    env = (f'<soap:Envelope xmlns:soap="{NS12}"><soap:Header><wsse:Security xmlns:wsse="x"><wsse:UsernameToken><wsse:Username>acme-freight</wsse:Username>'
           f'<wsse:Password>secret-pw</wsse:Password></wsse:UsernameToken></wsse:Security></soap:Header><soap:Body>{register("SHP-TRY", "3000000")}</soap:Body></soap:Envelope>')
    t = call("POST", "/plugins/example-insurance/operations/try", {"service": "example-insurance", "envelope": env})
    assert t["operation"] == "RegisterShipment" and t["answer"] == "coverage_exceeded" and t["status"] == 500, t
    time.sleep(0.5); assert not GOT, "Try it sends no callback"

    # send_callback: the provider calls the app on demand.
    r = call("POST", "/plugins/example-insurance/callback", {"service": "example-insurance", "url": f"http://127.0.0.1:{RECV}/hand", "operation": "StatusChanged",
             "body": '<StatusChanged xmlns="urn:example:insurance"><protocol>EXP-9</protocol><status>CANCELLED</status></StatusChanged>'})
    assert r["status"] == 200 and GOT[-1]["path"] == "/hand", r

    # Re-import a WSDL that adds an operation: steps and answers stay.
    w2 = open(WSDL).read().replace('<wsdl:operation name="Ping"><wsdl:input', '<wsdl:operation name="RegisterShipmentV3"><wsdl:input message="tns:RegisterShipmentIn"/><wsdl:output message="tns:RegisterShipmentOut"/></wsdl:operation>\n    <wsdl:operation name="Ping"><wsdl:input', 1)
    re_ = call("POST", "/plugins/example-insurance/wsdl", {"service": "example-insurance", "wsdl": w2})
    assert re_["added"] == ["RegisterShipmentV3"] and re_["removed"] == [], re_
    ops = call("GET", "/plugins/example-insurance/operations")
    reg = next(o for o in ops["operations"] if o["operation"] == "RegisterShipment")
    assert reg["answers"] == ["coverage_exceeded", "policy_suspended", "registered"] and ops["security"] == "username_token", ops

    # Tools are listed for agents (MCP uses the same list).
    names = json.dumps(call("GET", "/tools"))
    for n in ["import_wsdl", "reimport_wsdl", "list_operations", "set_operation", "try_operation", "send_callback"]:
        assert n in names, n


def main():
    data = tempfile.mkdtemp(prefix="stuntman-soap-")
    recv = http.server.ThreadingHTTPServer(("127.0.0.1", RECV), Recv)
    threading.Thread(target=recv.serve_forever, daemon=True).start()
    env = dict(os.environ, STUNTMAN_ENV=os.environ.get("STUNTMAN_ENV", "stg-a"), STUNTMAN_DATA=data, STUNTMAN_PORT=str(HTTP), STUNTMAN_HOOKS_PORT=str(HOOKS), STUNTMAN_SMTP_PORT=str(SMTP), STUNTMAN_OTLP_HTTP_PORT=str(OTLP),
               STUNTMAN_OTLP_GRPC_PORT=str(OTLP_G), STUNTMAN_BLOB_PORT=str(BLOB), STUNTMAN_S3_PORT=str(S3), STUNTMAN_APIS_PORT=str(APIS), STUNTMAN_APIS_TLS_PORT=str(TLS),
               STUNTMAN_KEYVAULT_PORT=str(KV), STUNTMAN_AMQP_PORT=str(AMQP), STUNTMAN_AMQPS_PORT=str(AMQPS), STUNTMAN_WEB="/nonexistent")
    log = data + ".log"
    srv = subprocess.Popen([BIN], env=env, stdout=open(log, "w"), stderr=subprocess.STDOUT)
    try:
        def up():
            try:
                socket.create_connection(("127.0.0.1", HTTP), 0.2).close()
                return True
            except OSError:
                return False
        wait(up, "server")
        run()
        print("e2e soap: all checks passed")
    except Exception:
        print(open(log).read()[-3000:])
        raise
    finally:
        srv.terminate(); recv.shutdown(); shutil.rmtree(data, ignore_errors=True)


if __name__ == "__main__":
    main()
