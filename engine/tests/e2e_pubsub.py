"""End-to-end check of Pub/Sub with the official clients pointed at PUBSUB_EMULATOR_HOST: topics and
subscriptions made by google-cloud-pubsub, three messages with attributes and an ordering key,
received over StreamingPull and acked; Pull, ModifyAckDeadline and redelivery after the deadline;
a dead-letter topic; a provoked UNAVAILABLE the client retries; NOT_FOUND; the REST publish; what
the tools show. The .NET client (Google.Cloud.PubSub.V1) publishes too when dotnet is installed.

    pip install google-cloud-pubsub
    cargo build -p stuntman && python3 tests/e2e_pubsub.py
"""
import http.client, json, os, shutil, subprocess, tempfile, threading, time

from harness import Stuntman, wait

P = "projects/acme-stg"
TOPIC, BILLING, AUDIT = f"{P}/topics/shipments", f"{P}/subscriptions/shipments-billing", f"{P}/subscriptions/shipments-audit"


def main():
    with Stuntman("stuntman-pubsub-") as s:
        os.environ["PUBSUB_EMULATOR_HOST"] = f"127.0.0.1:{s.PUBSUB_PORT}"
        from google.api_core import exceptions as gx
        from google.cloud import pubsub_v1

        pub = pubsub_v1.PublisherClient(publisher_options=pubsub_v1.types.PublisherOptions(enable_message_ordering=True))
        sub = pubsub_v1.SubscriberClient()

        # Admin: a topic, an ordered subscription and a plain one.
        assert pub.create_topic(name=TOPIC).name == TOPIC
        try:
            pub.create_topic(name=TOPIC)
            raise AssertionError("a second CreateTopic must fail")
        except gx.AlreadyExists:
            pass
        assert sub.create_subscription(request={"name": BILLING, "topic": TOPIC, "enable_message_ordering": True, "ack_deadline_seconds": 20}).enable_message_ordering
        sub.create_subscription(request={"name": AUDIT, "topic": TOPIC})
        assert [t.name for t in pub.list_topics(request={"project": P})] == [TOPIC]
        assert sorted(pub.list_topic_subscriptions(request={"topic": TOPIC})) == [AUDIT, BILLING]
        assert sub.get_subscription(request={"subscription": AUDIT}).ack_deadline_seconds == 10
        assert pub.get_topic(request={"topic": TOPIC}).name == TOPIC
        print("ok   admin: topics, subscriptions, ALREADY_EXISTS")

        # Three messages with attributes and one ordering key.
        texts = ["SHP-1040 created", "SHP-1040 picked up", "SHP-1040 delivered"]
        ids = [pub.publish(TOPIC, t.encode(), ordering_key="SHP-1040", carrier="Globex Freight", step=str(i)).result(10) for i, t in enumerate(texts)]
        assert len(set(ids)) == 3, ids
        print("ok   publish with attributes and an ordering key:", ids)

        # StreamingPull: in order, attributes and key kept, acked.
        got, lock = [], threading.Lock()

        def on(m):
            with lock:
                got.append((m.data.decode(), dict(m.attributes), m.ordering_key, m.message_id))
            m.ack()
        fut = sub.subscribe(BILLING, on)
        wait(lambda: len(got) >= 3, "3 messages over StreamingPull")
        assert [g[0] for g in got[:3]] == texts, got
        assert got[0][1] == {"carrier": "Globex Freight", "step": "0"} and got[0][2] == "SHP-1040" and [g[3] for g in got[:3]] == ids, got
        wait(lambda: s.call("GET", "/pubsub/messages?subscription=shipments-billing")["subscription"]["acked"] == 3, "3 acked on billing")
        print("ok   streaming pull, ordered, acked")

        # A message published as another service reaches the same stream; the REST publish too.
        s.call("POST", "/pubsub/topics/shipments/publish", {"data": {"shipment": "SHP-1041", "status": "CREATED"}, "attributes": {"source": "partner"}})
        c = http.client.HTTPConnection("127.0.0.1", s.PUBSUB_PORT, timeout=10)
        c.request("POST", f"/v1/{TOPIC}:publish", json.dumps({"messages": [{"data": "U0hQLTEwNDIgY3JlYXRlZA==", "attributes": {"source": "rest"}}]}), {"Content-Type": "application/json"})
        r = c.getresponse()
        assert r.status == 200 and len(json.loads(r.read())["messageIds"]) == 1
        wait(lambda: len(got) >= 5, "tool and REST messages over StreamingPull")
        assert {g[0] for g in got[3:5]} == {'{"shipment":"SHP-1041","status":"CREATED"}', "SHP-1042 created"}, got
        fut.cancel()
        try:
            fut.result(5)
        except Exception:
            pass
        print("ok   pubsub_publish tool and REST :publish delivered")

        # Pull + ModifyAckDeadline: a nack comes back at once; a short deadline comes back after it.
        r = sub.pull(request={"subscription": AUDIT, "max_messages": 10}, timeout=10)
        assert len(r.received_messages) == 5, r
        first, second = r.received_messages[0], r.received_messages[1]
        sub.modify_ack_deadline(request={"subscription": AUDIT, "ack_ids": [first.ack_id], "ack_deadline_seconds": 0})
        sub.modify_ack_deadline(request={"subscription": AUDIT, "ack_ids": [second.ack_id], "ack_deadline_seconds": 1})
        sub.acknowledge(request={"subscription": AUDIT, "ack_ids": [m.ack_id for m in r.received_messages[2:]]})
        r2 = sub.pull(request={"subscription": AUDIT, "max_messages": 10}, timeout=10)
        assert [m.message.message_id for m in r2.received_messages] == [first.message.message_id], r2
        time.sleep(1.3)
        r3 = sub.pull(request={"subscription": AUDIT, "max_messages": 10}, timeout=10)
        assert [m.message.message_id for m in r3.received_messages] == [second.message.message_id], r3
        sub.acknowledge(request={"subscription": AUDIT, "ack_ids": [r2.received_messages[0].ack_id, r3.received_messages[0].ack_id]})
        a = s.call("GET", "/pubsub/messages?subscription=shipments-audit")
        assert a["subscription"]["acked"] == 5 and {m["deliveryAttempts"] for m in a["messages"]} == {1, 2}, a
        print("ok   pull, modack 0 (nack) and deadline expiry redelivered, acked")

        # Dead letter after 5 attempts, delivery_attempt counted.
        s.call("PUT", "/pubsub/subscriptions/shipments-retry", {"topic": "shipments", "dead_letter_topic": "shipments-dlq", "max_delivery_attempts": 5})
        pub.publish(TOPIC, b"SHP-1043 lost").result(10)
        attempts = []
        for _ in range(5):
            m = sub.pull(request={"subscription": f"{P}/subscriptions/shipments-retry", "max_messages": 1}, timeout=10).received_messages[0]
            attempts.append(m.delivery_attempt)
            sub.modify_ack_deadline(request={"subscription": f"{P}/subscriptions/shipments-retry", "ack_ids": [m.ack_id], "ack_deadline_seconds": 0})
        assert attempts == [1, 2, 3, 4, 5], attempts
        assert not sub.pull(request={"subscription": f"{P}/subscriptions/shipments-retry", "max_messages": 1, "return_immediately": True}, timeout=10).received_messages
        dlq = s.call("GET", "/pubsub/messages?topic=shipments-dlq")["messages"]
        assert dlq[0]["data"]["value"] == "SHP-1043 lost" and dlq[0]["attributes"]["CloudPubSubDeadLetterSourceSubscription"] == "shipments-retry", dlq
        print("ok   dead-letter topic after 5 delivery attempts")

        # Provoked UNAVAILABLE twice: the client retries and the publish lands.
        s.call("PUT", "/pubsub/config", {"provoke": "unavailable", "times": 2})
        t0 = time.time()
        mid = pub.publish(TOPIC, b"SHP-1044 created", ordering_key="SHP-1044").result(30)
        assert mid and s.call("GET", "/pubsub/topics")["provoke"] is None
        print(f"ok   UNAVAILABLE x2 retried by the client ({time.time() - t0:.1f}s), id {mid}")

        # NOT_FOUND is not retried.
        s.call("PUT", "/pubsub/config", {"provoke": "not_found", "times": 1})
        plain = pubsub_v1.PublisherClient()
        try:
            plain.publish(TOPIC, b"SHP-1045 created").result(15)
            raise AssertionError("NOT_FOUND expected")
        except gx.NotFound:
            pass
        print("ok   NOT_FOUND surfaces to the app")

        # The tools: what the app published, ack states per subscription.
        w = s.call("GET", "/pubsub/topics/shipments/wait?since=0&contains=delivered&timeout=2s")
        assert w["data"]["value"] == "SHP-1040 delivered" and w["publisher"].startswith("app:gl-python"), w
        topics = {t["name"]: t for t in s.call("GET", "/pubsub/topics")["topics"]}
        subs = {x["name"]: x for x in topics[TOPIC]["subscriptions"]}
        assert subs[BILLING]["acked"] == 5 and subs[BILLING]["unacked"] == 2 and subs[f"{P}/subscriptions/shipments-retry"]["deadLettered"] == 1, subs
        msgs = s.call("GET", "/pubsub/messages?topic=shipments")["messages"]
        states = {m["data"]["value"]: {x["subscription"].rsplit("/", 1)[1]: x["state"] for x in m["subscriptions"]} for m in msgs if m["data"]["format"] == "text"}
        assert states["SHP-1040 created"] == {"shipments-audit": "acked", "shipments-billing": "acked"}, states
        print("ok   tools: list_pubsub_topics, list_pubsub_messages, wait_for_pubsub_message")

        # .NET: PublisherClientBuilder with EmulatorDetection.EmulatorOrProduction.
        if shutil.which("dotnet"):
            art = tempfile.mkdtemp(prefix="stuntman-pubsub-dotnet-")
            proj = os.path.join(os.path.dirname(os.path.abspath(__file__)), "pubsub_dotnet")
            out = subprocess.run(["dotnet", "run", "--project", proj, "--artifacts-path", art, "--", "acme-stg", "shipments"], capture_output=True, text=True, timeout=600,
                                 env=dict(os.environ, PUBSUB_EMULATOR_HOST=f"localhost:{s.PUBSUB_PORT}"))
            shutil.rmtree(art, ignore_errors=True)
            assert out.returncode == 0, out.stdout + out.stderr
            assert len([l for l in out.stdout.splitlines() if l.startswith("id ")]) == 3, out.stdout
            dn = s.call("GET", "/pubsub/topics/shipments/wait?contains=SHP-2001%20delivered&timeout=5s")
            assert dn["publisher"].startswith("app:gl-dotnet"), dn
            print("ok   .NET PublisherClient.PublishAsync x3 via EmulatorDetection")
        else:
            print("skip .NET (dotnet not installed)")
        print("pubsub e2e: all ok")


if __name__ == "__main__":
    main()
