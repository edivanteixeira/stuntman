"""End-to-end check of `usage` (GET /api/usage): calls the way apps do (AI chat, a Pub/Sub REST
publish, Kinesis through boto3 when installed, an OTLP/HTTP batch) and sees them counted per service
kind, live, while services nobody called stay at zero; your own tool calls do not count; the counts
survive a restart (the last ones written on shutdown). Also: a request no route took shows in the Timeline's "Not answered".

    cargo build -p stuntman && python3 tests/e2e_usage.py
"""
import http.client, json, os, subprocess, time

from harness import Stuntman, wait


def post(port, path, body, headers=None):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
    h = {"Content-Type": "application/json", "Authorization": "Bearer sk-test", "User-Agent": "acme-shop/2.0"}
    h.update(headers or {})
    c.request("POST", path, json.dumps(body).encode(), h)
    r = c.getresponse()
    return r.status, r.read()


def main():
    s = Stuntman()
    s.__enter__()
    try:
        u = s.call("GET", "/usage")
        assert all(v["today"] == 0 and not v["live"] for v in u.values()), u
        # Your tool calls are not the app's.
        s.call("PUT", "/pubsub/subscriptions/orders-billing", {"topic": "orders"})
        topic = s.call("GET", "/pubsub/topics")["topics"][0]["name"]
        s.call("POST", "/pubsub/topics/orders/publish", {"data": {"order": "A-1001"}})
        assert s.call("GET", "/usage")["pubsub"]["today"] == 0
        # The app: AI chat, Pub/Sub REST publish (twice), an OTLP/HTTP batch.
        st, _ = post(s.PORT, "/openai/v1/chat/completions", {"model": "gpt-6.1", "messages": [{"role": "user", "content": "Where is order A-1001?"}]})
        assert st == 200, st
        for i in range(2):
            st, raw = post(s.PUBSUB_PORT, f"/v1/{topic}:publish", {"messages": [{"data": "QS0xMDAx", "attributes": {"n": str(i)}}]})
            assert st == 200, (st, raw)
        span = {"resourceSpans": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "acme-shop"}}]},
                "scopeSpans": [{"spans": [{"traceId": "5b8efff798038103d269b633813fc60c", "spanId": "eee19b7ec3c1b174", "name": "GET /orders", "kind": 2,
                                           "startTimeUnixNano": str(time.time_ns() - 5_000_000), "endTimeUnixNano": str(time.time_ns())}]}]}]}
        st, raw = post(s.OTLP_HTTP_PORT, "/v1/traces", span)
        assert st == 200, (st, raw)
        kinesis = False
        try:
            import boto3
            k = boto3.client("kinesis", endpoint_url=f"http://127.0.0.1:{s.KINESIS_PORT}", region_name="us-east-1", aws_access_key_id="AKIAEXAMPLE", aws_secret_access_key="any")
            k.create_stream(StreamName="orders", ShardCount=1)
            k.get_waiter("stream_exists").wait(StreamName="orders", WaiterConfig={"Delay": 1, "MaxAttempts": 10})  # CREATING for 1 s
            k.put_record(StreamName="orders", Data=b"A-1001", PartitionKey="A-1001")
            kinesis = True
        except ImportError:
            print("skip kinesis: pip install boto3")
        u = wait(lambda: (lambda v: v if v["telemetry"]["today"] >= 1 else None)(s.call("GET", "/usage")), "telemetry counted")
        assert u["ai"]["today"] >= 1 and u["ai"]["live"], u["ai"]
        assert u["pubsub"]["today"] == 2 and u["pubsub"]["week"] == 2 and u["pubsub"]["lastAt"], u["pubsub"]
        assert u["telemetry"]["live"], u["telemetry"]
        if kinesis:
            assert u["kinesis"]["today"] >= 2, u["kinesis"]
        assert u["sms"] == {"today": 0, "week": 0, "lastAt": None, "live": False}, u["sms"]
        print("ok   usage counts the app's calls per service kind, not your tool calls")

        # A request no route took: listed for the Timeline's "Not answered".
        c = http.client.HTTPConnection("127.0.0.1", s.APIS_PORT, timeout=10)
        c.request("POST", "/v1/refunds", json.dumps({"order": "A-1001"}), {"Content-Type": "application/json", "User-Agent": "acme-shop/2.0"})
        assert c.getresponse().status == 404
        g = wait(lambda: s.call("GET", "/apis/unmatched?since=15m")["groups"], "not answered listed")
        assert g[0]["latest"]["path"] == "/v1/refunds", g
        print("ok   a request no route took is listed as not answered")

        # Restart on the same data folder: the counts were written on shutdown.
        before = s.call("GET", "/usage")
        post(s.PORT, "/openai/v1/chat/completions", {"model": "gpt-6.1", "messages": [{"role": "user", "content": "And order A-1002?"}]})
        s.proc.terminate()
        s.proc.wait(10)
        env = dict(os.environ, STUNTMAN_DATA=s.data, STUNTMAN_WEB="/nonexistent", STUNTMAN_ENV=os.environ.get("STUNTMAN_ENV", "stg-a"), **{f"STUNTMAN_{k}": str(v) for k, v in s.ports.items()})
        s.proc = subprocess.Popen([__import__("harness").BIN], env=env, stdout=open(s.log, "a"), stderr=subprocess.STDOUT)
        def up():
            try:
                return s.call("GET", "/env")
            except OSError:
                return None
        wait(up, "restart", 30)
        after = s.call("GET", "/usage")
        assert after["ai"]["today"] == before["ai"]["today"] + 1, (before["ai"], after["ai"])
        for k in ("pubsub", "telemetry", "kinesis"):
            assert after[k]["today"] == before[k]["today"], (k, before[k], after[k])
        print("ok   usage survives a restart")
    except BaseException as e:
        s.__exit__(type(e), e, None)
        raise
    s.__exit__(None, None, None)
    print("usage e2e: ok")


if __name__ == "__main__":
    main()
