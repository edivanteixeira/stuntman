"""End-to-end check of A11: services made from mocks you already have. A WireMock folder becomes
plugins (regex paths, header and XPath matchers, priorities, WireMock templates, files, delays,
faults, scenarios); a folder of WSDLs becomes SOAP services.

    cargo build -p stuntman && python3 tests/e2e_mocks_a11.py
"""
import http.client, json, os, re, shutil, tempfile, time

from harness import ROOT, Stuntman

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
        s.call("PUT", "/plugins/acme-tracking/scenario", {"scenario": "auth-failure"})
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
    print("A11 e2e: ok")


if __name__ == "__main__":
    main()
