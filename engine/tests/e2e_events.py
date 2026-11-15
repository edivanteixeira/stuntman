"""End-to-end check of product events (A13) and of GELF/StatsD in Telemetry: PostHog (JSON and
gzip), Segment, Mixpanel (form base64), GA4 Measurement Protocol (and its debug validation), plans
checked, personal data flagged, forced collector failures; GELF over UDP and HTTP, StatsD over UDP.
With the posthog SDK installed (pip install posthog) it also runs it.

    cargo build -p stuntman && python3 tests/e2e_events.py
"""
import base64, gzip, http.client, json, socket, time, urllib.parse

from harness import Stuntman, wait


def post(s, path, body, headers=None, raw=False):
    c = http.client.HTTPConnection("127.0.0.1", s.PORT, timeout=30)
    h = {"Content-Type": "application/json", "User-Agent": "acme-web/3.1"}
    h.update(headers or {})
    c.request("POST", path, body if raw else json.dumps(body).encode(), h)
    r = c.getresponse()
    b = r.read()
    try:
        return r.status, json.loads(b)
    except Exception:
        return r.status, b


def main():
    with Stuntman("stuntman-events-") as s:
        st, v = post(s, "/posthog/batch/", {"api_key": "phc_test", "batch": [
            {"event": "signup_started", "distinct_id": "u-7f3a", "properties": {"source": "ads"}},
            {"event": "signup_completed", "distinct_id": "u-7f3a", "properties": {"plan": "pro", "workspace_id": "ws_81", "email": "user-7f3a@example.com"}}]})
        assert st == 200 and v == {"status": 1}, (st, v)
        gz = gzip.compress(json.dumps([{"event": "$pageview", "distinct_id": "u-7f3a", "properties": {"$current_url": "/onboarding"}}]).encode())
        st, v = post(s, "/posthog/e/?compression=gzip-js", gz, {"Content-Type": "text/plain"}, raw=True)
        assert st == 200, st
        st, v = post(s, "/posthog/decide/?v=3", {"api_key": "phc_test", "distinct_id": "u-7f3a"})
        assert v["featureFlags"] == {}, v
        st, v = post(s, "/segment/v1/batch", {"batch": [{"type": "track", "event": "workspace_created", "userId": "u-7f3a", "properties": {"size": 3}},
                                                       {"type": "identify", "userId": "u-7f3a", "traits": {"cpf": "111.444.777-35"}}]}, {"Authorization": "Basic " + base64.b64encode(b"wk:").decode()})
        assert v == {"success": True}, v
        data = base64.b64encode(json.dumps([{"event": "invite_sent", "properties": {"distinct_id": "u-7f3a", "role": "admin", "token": "mp"}}]).encode()).decode()
        st, v = post(s, "/mixpanel/track?verbose=1", ("data=" + urllib.parse.quote(data)).encode(), {"Content-Type": "application/x-www-form-urlencoded"}, raw=True)
        assert v == {"status": 1, "error": None}, v
        st, v = post(s, "/ga4/mp/collect?measurement_id=G-TEST&api_secret=x", {"client_id": "c.1", "user_id": "u-7f3a", "events": [{"name": "trial_started", "params": {"plan": "pro"}}]})
        assert st == 204, st
        st, v = post(s, "/ga4/debug/mp/collect?measurement_id=G-TEST&api_secret=x", {"client_id": "c.1", "events": [{"name": "first_visit"}, {"name": "1bad"}]})
        assert [m["validationCode"] for m in v["validationMessages"]] == ["NAME_RESERVED", "NAME_INVALID"], v

        evs = s.call("GET", "/events?user=u-7f3a")["events"]
        names = sorted(e["name"] for e in evs)
        assert names == sorted(["signup_started", "signup_completed", "$pageview", "workspace_created", "identify", "invite_sent", "trial_started"]), names
        pii = {e["name"]: e["pii"] for e in evs if e["pii"]}
        assert pii == {"signup_completed": ["email: e-mail"], "identify": ["cpf: CPF"]}, pii

        # The plan.
        s.call("PUT", "/events/plans", {"name": "signup", "events": ["signup_started", {"event": "signup_completed", "props": ["plan", "workspace_id"]}, {"event": "workspace_created", "props": ["plan"]}, "first_case_created"]})
        r = s.call("POST", "/events/check", {"plan": "signup"})
        st_ = {e["event"]: e["state"] for e in r["events"]}
        assert r["ok"] is False and st_ == {"signup_started": "ok", "signup_completed": "ok", "workspace_created": "props_missing", "first_case_created": "missing"}, r
        assert r["missing"] == ["workspace_created", "first_case_created"] and len(r["pii"]) == 2, r
        got = s.call("GET", "/events/wait?name=trial_started&within=60s")
        assert got["source"] == "ga4", got

        # Collector failures.
        s.call("POST", "/events/force", {"source": "posthog", "kind": "429"})
        st, v = post(s, "/posthog/capture/", {"api_key": "phc", "event": "x", "distinct_id": "u"})
        assert st == 429, st
        s.call("POST", "/events/force", {"kind": "drop_half"})
        post(s, "/segment/v1/batch", {"batch": [{"type": "track", "event": "a", "userId": "d"}, {"type": "track", "event": "b", "userId": "d"}]})
        assert len(s.call("GET", "/events?user=d")["events"]) == 1

        # GELF over UDP (gzip) and HTTP, StatsD over UDP.
        u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        u.sendto(gzip.compress(json.dumps({"version": "1.1", "host": "worker-1", "short_message": "CT-e 3526 rejected by SEFAZ", "level": 3, "_app": "acme-tms", "_cstat": 539}).encode()), ("127.0.0.1", s.GELF_PORT))
        st, _ = post(s, "/gelf", {"version": "1.1", "host": "worker-2", "short_message": "batch done", "level": 6, "_app": "acme-tms"})
        assert st == 202, st
        u.sendto(b"orders.created:3|c|#service:acme-tms,region:br\nqueue.depth:7|g\nhttp.latency:120|ms", ("127.0.0.1", s.STATSD_PORT))
        logs = wait(lambda: [l for l in s.call("GET", "/telemetry/logs?q=SEFAZ").get("logs", []) if "SEFAZ" in json.dumps(l)], "GELF log")
        assert "ERROR" in json.dumps(logs[0]) and "acme-tms" in json.dumps(logs[0]), logs[0]
        m = wait(lambda: [x for x in s.call("GET", "/telemetry/metrics").get("metrics", []) if x.get("name") == "orders.created"], "StatsD metric")
        assert m, m

        try:
            import posthog
            ph = posthog.Posthog("phc_sdk", host=f"http://127.0.0.1:{s.PORT}/posthog", sync_mode=True)
            ph.capture("first_case_created", distinct_id="u-7f3a", properties={"template": "intake"})
            ph.flush()
            wait(lambda: s.call("GET", "/events?name=first_case_created")["events"], "SDK event")
            print("  posthog SDK: ok")
        except ImportError:
            print("  (posthog SDK not installed: skipped)")
    print("Events e2e: ok")


if __name__ == "__main__":
    main()
