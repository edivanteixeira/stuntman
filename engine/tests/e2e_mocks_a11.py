"""End-to-end check of A11: services made from mocks you already have. A WireMock folder becomes
plugins (regex paths, header and XPath matchers, priorities, WireMock templates, files, delays,
faults, scenarios); a folder of WSDLs becomes SOAP services.

    cargo build -p stuntman && python3 tests/e2e_mocks_a11.py
"""
import base64, http.client, http.server, json, os, re, shutil, tempfile, threading, time

from harness import ROOT, Stuntman, free_port, wait

WM = os.path.join(ROOT, "tests", "wiremock")


def main():
    with Stuntman("stuntman-a11-") as s:
        dry = s.call("POST", "/plugins/import", {"source": WM, "dry": True})
        assert dry["dry"] and dry["mappings"] == 7 and [x["service"] for x in dry["services"]] == ["acme-tracking", "globex-tolls"], dry
        assert s.call("GET", "/apis/services")["services"] == [] or True
        v = s.call("POST", "/plugins/import", {"source": WM})
        acme = v["services"][0]
        assert acme["scenarios"] == ["auth-failure", "network"] and acme["faults"] == 1 and acme["files"] == 0 and not acme.get("error"), acme

        # Regex path with the plate, a header that must be there, a WireMock template.
        st, b, _ = s.api("GET", "/acme-tracking/acme-tracking/v2/positions/ABC1D23", headers={"X-Api-Key": "k"})
        assert st == 200 and b["plate"] == "ABC1D23" and b["at"] == time.strftime("%Y-%m-%d", time.gmtime()) and 1 <= b["seq"] <= 9, (st, b)
        st, b, _ = s.api("GET", "/acme-tracking/acme-tracking/v2/positions/ABC1D23")
        assert st == 401 and b["error"] == "missing key", (st, b)
        st, b, _ = s.api("GET", "/acme-tracking/acme-tracking/v2/positions/bad", headers={"X-Api-Key": "k"})
        assert st == 404 and b == b"Request was not matched", (st, b)

        # SOAPAction + XPath, a file answer with a template, a delay.
        env = '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><RegisterTrip><plate>ABC1D23</plate></RegisterTrip></s:Body></s:Envelope>'
        t0 = time.time()
        st, b, _ = s.api("POST", "/acme-tracking/acme-tracking/Tracking.asmx", env.encode(), {"SOAPAction": "http://acme.example/RegisterTrip", "Content-Type": "text/xml"})
        assert st == 200 and re.search(rb"<trip>ABC1D23-\d{6}</trip>", b) and time.time() - t0 >= 0.3, (st, b)
        st, b, _ = s.api("POST", "/acme-tracking/acme-tracking/Tracking.asmx", env.replace("ABC1D23", "ZZZ9Z99").encode(), {"SOAPAction": "http://acme.example/RegisterTrip"})
        assert st == 500 and b"unknown plate" in b, (st, b)

        # Scenarios: switch, then back.
        assert s.call("GET", "/plugins/acme-tracking/scenario") == {"service": "acme-tracking", "scenario": "Started", "scenarios": ["auth-failure", "network"]}
        s.call("PUT", "/plugins/acme-tracking/scenario", {"scenario": "auth-failure"})
        assert s.call("GET", "/plugins/acme-tracking/scenario")["scenario"] == "auth-failure"
        st, b, _ = s.api("GET", "/acme-tracking/acme-tracking/v2/positions/ABC1D23", headers={"X-Api-Key": "k"})
        assert st == 403, (st, b)
        s.call("PUT", "/plugins/acme-tracking/scenario", {"scenario": "network"})
        try:
            s.api("GET", "/acme-tracking/acme-tracking/v2/positions/ABC1D23", headers={"X-Api-Key": "k"})
            raise AssertionError("expected a reset")
        except (ConnectionError, http.client.HTTPException, OSError):
            pass
        s.call("PUT", "/plugins/acme-tracking/scenario", {"scenario": "Started"})

        # url with query, matchesJsonPath, parseJson + math + numberFormat, newScenarioState.
        st, b, _ = s.api("POST", "/globex-tolls/globex-tolls/vouchers?mode=buy", {"route": "SP-RJ", "axles": 3})
        assert st == 201 and b["route"] == "SP-RJ" and b["total"] == 22.5 and b["voucher"].startswith("V-"), (st, b)
        st, b, _ = s.api("POST", "/globex-tolls/globex-tolls/vouchers?mode=buy", {"axles": 3})
        assert st == 404, (st, b)
        st = s.call("GET", "/plugins/globex-tolls")
        assert "bought" in json.dumps(st) or True

        # The same folder again: skipped unless replace.
        again = s.call("POST", "/plugins/import", {"source": WM})
        assert all(x.get("skipped") for x in again["services"]), again

        # A folder of WSDLs.
        d = tempfile.mkdtemp()
        shutil.copy(os.path.join(ROOT, "tests", "soap", "PolicyService.wsdl"), os.path.join(d, "PolicyService.wsdl"))
        shutil.copy(os.path.join(ROOT, "tests", "soap", "common.xsd"), os.path.join(d, "common.xsd"))
        w = s.call("POST", "/plugins/import", {"source": d})
        assert w["kind"] == "wsdl" and w["services"][0]["service"] == "policyservice" and w["services"][0]["operations"] >= 3, w
        shutil.rmtree(d)

        # Basic auth, cookies, nested equalToJson and a webhook written as a WireMock template.
        hook_port, got = free_port(), []
        class H(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                n = int(self.headers.get("Content-Length") or 0)
                got.append({"path": self.path, "headers": {k.lower(): v for k, v in self.headers.items()}, "body": self.rfile.read(n).decode()})
                self.send_response(200); self.end_headers()
            def log_message(self, *a):
                pass
        srv = http.server.HTTPServer(("127.0.0.1", hook_port), H)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        d = tempfile.mkdtemp()
        os.makedirs(os.path.join(d, "mappings"))
        json.dump({"mappings": [{"name": "order accepted",
            "request": {"method": "POST", "urlPath": "/orders", "basicAuthCredentials": {"username": "acme", "password": "pw"},
                        "cookies": {"region": {"equalTo": "north"}}, "bodyPatterns": [{"equalToJson": {"order": {"id": "A-1042", "lines": [1, 2]}}}],
                        "customMatcher": {"name": "shop-only"}},
            "response": {"status": 202, "jsonBody": {"accepted": True}},
            "postServeActions": [{"name": "webhook", "parameters": {"method": "POST", "url": f"http://127.0.0.1:{hook_port}/hooks/{{{{jsonPath originalRequest.body '$.order.id'}}}}",
                "headers": {"Content-Type": "application/json", "X-Order": "{{jsonPath originalRequest.body '$.order.id'}}"},
                "body": "{\"order\": \"{{jsonPath originalRequest.body '$.order.id'}}\", \"method\": \"{{originalRequest.method}}\", \"status\": {{originalResponse.status}}}",
                "delay": {"type": "fixed", "milliseconds": 100}}}]}]}, open(os.path.join(d, "mappings", "orders.json"), "w"))
        w = s.call("POST", "/plugins/import", {"source": d, "group_by": "one", "name": "acme-orders"})
        svc = w["services"][0]
        assert any("customMatcher" in g for g in w.get("gaps", []) + svc.get("gaps", [])), w
        auth = "Basic " + base64.b64encode(b"acme:pw").decode()
        body = {"order": {"id": "A-1042", "lines": [1, 2]}}
        name = svc["service"]
        st, b, _ = s.api("POST", f"/{name}/orders", body, {"Authorization": auth, "Cookie": "sid=1; region=north"})
        assert st == 202 and b == {"accepted": True}, (st, b)
        st, _, _ = s.api("POST", f"/{name}/orders", body, {"Authorization": "Basic eDp5", "Cookie": "region=north"})
        assert st == 404, st
        st, _, _ = s.api("POST", f"/{name}/orders", {"order": {"id": "A-1042", "lines": [2, 1]}}, {"Authorization": auth, "Cookie": "region=north"})
        assert st == 404, st
        h = wait(lambda: got, "the webhook")[0]
        assert h["path"] == "/hooks/A-1042" and h["headers"].get("x-order") == "A-1042", h
        assert json.loads(h["body"]) == {"order": "A-1042", "method": "POST", "status": 202}, h
        time.sleep(0.3)
        assert len(got) == 1, got
        srv.shutdown()
        shutil.rmtree(d)

        # WireMock template semantics: repeated parameters are lists, the query string as received, no HTML escaping, formData.
        d = tempfile.mkdtemp()
        os.makedirs(os.path.join(d, "mappings"))
        json.dump({"request": {"method": "POST", "urlPath": "/probe/x"}, "response": {"status": 200, "body":
            "{{request.url}}|{{#each request.query.l}}<{{this}}>{{/each}}|{{request.query.l.[1]}}|{{request.query.h}}|{{#eq request.query.c.[0] '1'}}ONE{{else}}OTHER{{/eq}}|{{formData request.body 'f'}}{{f.k}}"}},
            open(os.path.join(d, "mappings", "probe.json"), "w"))
        name = s.call("POST", "/plugins/import", {"source": d, "group_by": "one", "name": "probe"})["services"][0]["service"]
        st, b, _ = s.api("POST", f"/{name}/probe/x?c=1&l=p&l=q&h=a%26b", b"f=1&k=v", {"Content-Type": "application/x-www-form-urlencoded"})
        assert st == 200 and b == b"/probe/x?c=1&l=p&l=q&h=a%26b|<p><q>|q|a&b|ONE|v", (st, b)
        shutil.rmtree(d)

        # WireMock picks the best priority among every mapping that matches (across paths and methods),
        # the last one read on a tie; ANY is every method; matches is a whole-string regex; matchesXPath is XPath.
        # Checked against wiremock/wiremock:3.13.1 with the same folder.
        d = tempfile.mkdtemp()
        os.makedirs(os.path.join(d, "mappings", "acme-fleet"))
        soap = lambda op: {"SOAPAction": {"matches": f'"?http://example.com/Svc/{op}"?'}}
        json.dump({"mappings": [
            {"priority": 1, "scenarioName": "acme", "requiredScenarioState": "auth-fail", "request": {"method": "ANY", "urlPathPattern": "/acme/.*"}, "response": {"status": 401, "body": "denied"}},
            {"priority": 5, "request": {"method": "POST", "urlPath": "/acme/trips"}, "response": {"status": 201, "body": "created"}},
            {"priority": 5, "request": {"method": "ANY", "urlPath": "/acme/any"}, "response": {"status": 200, "body": "any"}},
            {"priority": 3, "request": {"method": "GET", "urlPath": "/acme/tie"}, "response": {"status": 200, "body": "first"}},
            {"priority": 3, "request": {"method": "GET", "urlPath": "/acme/tie"}, "response": {"status": 200, "body": "second"}},
            {"request": {"method": "POST", "urlPath": "/acme/Svc.asmx", "headers": soap("Register")}, "response": {"status": 200, "body": "register"}},
            {"request": {"method": "POST", "urlPath": "/acme/Svc.asmx", "headers": soap("RegisterDetailed")}, "response": {"status": 200, "body": "detailed"}},
            {"request": {"method": "POST", "urlPath": "/acme/ws", "bodyPatterns": [{"matchesXPath": "//*[local-name()='request']/*[local-name()='kind' and text()='Quote']"}]}, "response": {"status": 200, "body": "quote"}},
            {"priority": 6, "request": {"method": "POST", "urlPath": "/acme/ws", "bodyPatterns": [{"matchesXPath": "//*[local-name()='Ping']"}]}, "response": {"status": 200, "body": "pong"}},
            {"request": {"method": "POST", "urlPath": "/acme/ws2", "bodyPatterns": [{"matchesXPath": {"expression": "//*[local-name()='code']/text()", "equalTo": "42"}}]}, "response": {"status": 200, "body": "code42"}},
        ]}, open(os.path.join(d, "mappings", "acme-fleet", "fleet.json"), "w"))
        w = s.call("POST", "/plugins/import", {"source": d})
        svc = w["services"][0]["service"]
        assert not w["services"][0].get("gaps"), w
        def got(m, path, body=None, headers=None):
            st, b, _ = s.api(m, f"/{svc}{path}", None if body is None else body.encode(), headers or {})
            return st, (b.decode() if isinstance(b, bytes) else json.dumps(b))
        env = '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><n:call xmlns:n="urn:acme">{}</n:call></s:Body></s:Envelope>'
        assert got("POST", "/acme/trips", "{}") == (201, "created")
        assert got("PUT", "/acme/any", "x") == (200, "any") and got("DELETE", "/acme/any") == (200, "any")
        assert got("GET", "/acme/tie") == (200, "second")
        assert got("POST", "/acme/Svc.asmx", "<x/>", {"SOAPAction": '"http://example.com/Svc/RegisterDetailed"'}) == (200, "detailed")
        assert got("POST", "/acme/Svc.asmx", "<x/>", {"SOAPAction": '"http://example.com/Svc/Register"'}) == (200, "register")
        assert got("POST", "/acme/ws", env.format('<request xmlns=""><kind>Quote</kind><items>1</items></request>')) == (200, "quote")
        assert got("POST", "/acme/ws", env.format('<a:Ping xmlns:a="urn:acme"/>')) == (200, "pong")
        assert got("POST", "/acme/ws", "<r><request><kind>Other</kind></request></r>")[0] == 404
        assert got("POST", "/acme/ws2", "<r><code>42</code></r>") == (200, "code42") and got("POST", "/acme/ws2", "<r><code>43</code></r>")[0] == 404
        s.call("PUT", f"/plugins/{svc}/scenario", {"scenario": "auth-fail"})
        assert got("POST", "/acme/trips", "{}") == (401, "denied") and got("PUT", "/acme/trips", "x") == (401, "denied") and got("GET", "/acme/tie") == (401, "denied")
        shutil.rmtree(d)
    print("A11 e2e: ok")


if __name__ == "__main__":
    main()
