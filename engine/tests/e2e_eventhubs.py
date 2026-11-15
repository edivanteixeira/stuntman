"""Stuntman's Azure Event Hubs side, checked with the official Python SDK (azure-eventhub, its own
AMQP 1.0 stack, in emulator mode): get_eventhub_properties and get_partition_properties
($management), batches with a partition key (hashed as the service does) and to a partition,
receiving a partition from the start, from a sequence number and from an offset, consumer groups,
owner-level (epoch) receivers that take a partition from each other, a provoked ServerBusy the SDK
retries, QuotaExceeded, entity not found, dropped connections the SDK recovers from, retention on
the environment clock, the official emulator's "localhost" connection string, the Kafka endpoint
(confluent-kafka with SASL PLAIN $ConnectionString) and a Pub/Sub topic ingesting from the hub.

    pip install azure-eventhub confluent-kafka google-cloud-pubsub
    cargo build -p stuntman && python3 tests/e2e_eventhubs.py
"""
import json, os, threading, time, urllib.parse

from harness import Stuntman, wait
from azure.eventhub import EventData, EventHubConsumerClient, EventHubProducerClient

NS, HUB = "acme-hubs-stg", "orders"


def ed(body, properties=None):
    e = EventData(body)
    if properties:
        e.properties = properties
    return e


def receive(cs, partition, want, group="$Default", **kw):
    """Receives up to `want` events of one partition (10 s at most)."""
    c = EventHubConsumerClient.from_connection_string(cs, consumer_group=group)
    got, errors = [], []
    done = threading.Event()

    def on_batch(_, evs):
        got.extend(evs)
        if len(got) >= want:
            done.set()

    t = threading.Thread(target=c.receive_batch, kwargs=dict(on_event_batch=on_batch, on_error=lambda _, e: errors.append(e), partition_id=partition, max_wait_time=1, **kw), daemon=True)
    t.start()
    done.wait(10)
    c.close()
    t.join(5)
    return got, errors


def main():
    with Stuntman("stuntman-eventhubs-") as s:
        CS = f"Endpoint=sb://{NS}.localhost:{s.AMQP_PORT}/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=any=;EntityPath={HUB};UseDevelopmentEmulator=true"
        v = s.call("POST", f"/eventhubs/{NS}/hubs", {"hub": HUB, "partitions": 4, "consumer_groups": ["billing"]})
        assert v["hubs"][0]["connection"].startswith(f"Endpoint=sb://{NS}.localhost:{s.AMQP_PORT}/;") and "EntityPath=orders" in v["hubs"][0]["connection"], v
        print("ok   create_eventhub: 4 partitions, $Default and billing, the connection string")

        p = EventHubProducerClient.from_connection_string(CS)
        props = p.get_eventhub_properties()
        assert props["eventhub_name"] == HUB and props["partition_ids"] == ["0", "1", "2", "3"], props
        pp = p.get_partition_properties("0")
        assert pp["is_empty"] and pp["last_enqueued_sequence_number"] == -1, pp
        print("ok   get_eventhub_properties and get_partition_properties ($management)")

        # A batch with a partition key: lookup3 of "ORD-1042" over 4 partitions is partition 0.
        b = p.create_batch(partition_key="ORD-1042")
        for i in range(3):
            b.add(ed(json.dumps({"orderId": "ORD-1042", "step": i}), {"region": "us-east"} if i == 0 else None))
        p.send_batch(b)
        b = p.create_batch(partition_id="2")
        b.add(EventData("SHP-7 created"))
        b.add(EventData("SHP-7 shipped"))
        p.send_batch(b)
        pp = p.get_partition_properties("0")
        assert (pp["beginning_sequence_number"], pp["last_enqueued_sequence_number"], pp["is_empty"]) == (0, 2, False), pp
        ev = s.call("GET", f"/eventhubs/{NS}/{HUB}/events?partition=0")["events"]
        assert len(ev) == 3 and all(e["partitionKey"] == "ORD-1042" and e["via"] == "amqp" for e in ev) and ev[-1]["properties"] == {"region": "us-east"}, ev
        assert p.get_partition_properties("2")["last_enqueued_sequence_number"] == 1
        print("ok   batches: a partition key hashed to its partition, a batch to partition 2, properties kept")

        got, _ = receive(CS, "0", 3, starting_position="-1")
        assert [e.sequence_number for e in got] == [0, 1, 2] and got[0].partition_key == b"ORD-1042" and got[0].properties == {b"region": b"us-east"}, got
        assert json.loads(got[1].body_as_str())["step"] == 1 and got[0].enqueued_time is not None and got[1].offset is not None
        print("ok   receive from the start: sequence numbers, offsets, enqueued time, partition key, properties")
        after_seq, _ = receive(CS, "0", 1, starting_position=1)
        assert [e.sequence_number for e in after_seq] == [2], after_seq
        incl, _ = receive(CS, "0", 2, starting_position=got[1].offset, starting_position_inclusive=True)
        assert [e.sequence_number for e in incl] == [1, 2], incl
        print("ok   receive after a sequence number, and from an offset (inclusive)")

        billing, _ = receive(CS, "2", 2, group="billing", starting_position="-1")
        assert [e.body_as_str() for e in billing] == ["SHP-7 created", "SHP-7 shipped"], billing
        # An unknown consumer group: the link is refused with amqp:not-found (pyamqp retries quietly).
        none, _ = receive(CS, "2", 1, group="nope", starting_position="-1")
        assert not none, none
        print("ok   consumer groups: billing reads; an unknown one is not found")

        # Owner level: a receiver with epoch 2 takes partition 1 from the one with epoch 1.
        c1 = EventHubConsumerClient.from_connection_string(CS, consumer_group="$Default")
        e1 = []
        t1 = threading.Thread(target=c1.receive, kwargs=dict(on_event=lambda *_: None, on_error=lambda _, e: e1.append(e), partition_id="1", owner_level=1, starting_position="-1"), daemon=True)
        t1.start()
        wait(lambda: any(r["epoch"] == 1 for g in s.call("GET", "/eventhubs")["namespaces"][0]["hubs"][0]["consumerGroups"] for r in g["readers"]), "the epoch 1 receiver")
        c2 = EventHubConsumerClient.from_connection_string(CS, consumer_group="$Default")
        t2 = threading.Thread(target=c2.receive, kwargs=dict(on_event=lambda *_: None, partition_id="1", owner_level=2, starting_position="-1"), daemon=True)
        t2.start()
        wait(lambda: e1, "the epoch 1 receiver to lose its partition")
        assert "epoch" in str(e1[0]).lower(), e1
        readers = wait(lambda: [r for r in s.call("GET", "/eventhubs")["namespaces"][0]["hubs"][0]["consumerGroups"][0]["readers"] if r["partition"] == "1" and r["epoch"] == 2], "epoch 2 reading")
        print("ok   owner level: epoch 2 disconnects epoch 1 (" + type(e1[0]).__name__ + ")")
        # "Partition owner stolen" provoked: the epoch 2 receiver goes too (the client then comes back).
        r = s.call("PUT", f"/eventhubs/{NS}/settings", {"steal_owner": HUB})
        assert r["done"]["stolen"] >= 1, r
        print("ok   set_eventhubs steal_owner disconnects the reading receivers")
        c1.close(); c2.close()

        # ServerBusy: the next send is rejected; the SDK sends again on its open link and the event arrives.
        p.send_event(EventData("warm up"), partition_id="3")
        s.call("PUT", f"/eventhubs/{NS}/settings", {"server_busy": 1})
        p.send_event(EventData("after busy"), partition_id="3")
        assert s.call("GET", "/eventhubs")["namespaces"][0]["provoke"]["server_busy"] == 0
        assert p.get_partition_properties("3")["last_enqueued_sequence_number"] == 1
        # Busy management requests are retried too.
        s.call("PUT", f"/eventhubs/{NS}/settings", {"server_busy": 1})
        assert p.get_partition_properties("3")["last_enqueued_sequence_number"] == 1
        assert s.call("GET", "/eventhubs")["namespaces"][0]["provoke"]["server_busy"] == 0
        print("ok   a provoked ServerBusy the SDK retries (a send and a management request)")
        # QuotaExceeded that lasts longer than the SDK's retries fails the send.
        s.call("PUT", f"/eventhubs/{NS}/settings", {"quota_exceeded": 10})
        try:
            p.send_event(EventData("over quota"), partition_id="3")
            raise AssertionError("the quota did not fail the send: " + json.dumps(s.call("GET", "/eventhubs")["namespaces"][0]["provoke"]))
        except Exception as e:
            assert "QuotaExceeded" in str(e), e
            kind = type(e).__name__
        s.call("PUT", f"/eventhubs/{NS}/settings", {"reset": True})
        print("ok   QuotaExceeded fails the send (" + kind + ")")
        s.call("PUT", f"/eventhubs/{NS}/settings", {"not_found": [HUB]})
        try:
            EventHubProducerClient.from_connection_string(CS, retry_total=0).get_eventhub_properties()
            raise AssertionError("not found did not fail")
        except Exception as e:
            assert "could not be found" in str(e), e
        s.call("PUT", f"/eventhubs/{NS}/settings", {"reset": True})
        print("ok   entity not found while provoked, then back to normal")

        # Dropped connections: the SDK reconnects and sends again; slow sends wait.
        r = s.call("PUT", f"/eventhubs/{NS}/settings", {"drop_connections": True})
        assert r["done"]["dropped"] >= 1, r
        p.send_event(EventData("after drop"), partition_id="3")
        s.call("PUT", f"/eventhubs/{NS}/settings", {"slow_sends": "1s"})
        t0 = time.time()
        p.send_event(EventData("slow"), partition_id="3")
        assert time.time() - t0 >= 0.9, time.time() - t0
        s.call("PUT", f"/eventhubs/{NS}/settings", {"reset": True})
        assert p.get_partition_properties("3")["last_enqueued_sequence_number"] == 3, p.get_partition_properties("3")
        print("ok   drop_connections (the SDK reconnects) and slow sends")
        p.close()

        # wait_for_eventhub_event sees the app's event; eventhub_send is "another producer".
        def later():
            time.sleep(0.5)
            with EventHubProducerClient.from_connection_string(CS) as q:
                q.send_event(ed(json.dumps({"orderId": "ORD-2001"}), {"kind": "OrderPaid"}))
        threading.Thread(target=later).start()
        w = s.call("GET", f"/eventhubs/{NS}/{HUB}/wait?timeout=10s&where=" + urllib.parse.quote('{"kind":"OrderPaid"}'))
        assert json.loads(w["event"]["body"])["orderId"] == "ORD-2001", w
        r = s.call("POST", f"/eventhubs/{NS}/{HUB}/events", {"body": {"orderId": "ORD-3001"}, "partition_key": "ORD-1042", "properties": {"source": "partner"}})
        assert r["partition"] == "0", r
        got, _ = receive(CS, "0", 1, starting_position=r["sequenceNumbers"][0] - 1)
        assert got and json.loads(got[0].body_as_str()) == {"orderId": "ORD-3001"} and got[0].properties == {b"source": b"partner"}, got
        print("ok   wait_for_eventhub_event; eventhub_send reaches the app's receiver")

        # The official emulator's connection string: host localhost, the hub names the namespace.
        local = f"Endpoint=sb://localhost:{s.AMQP_PORT};SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=SAS_KEY_VALUE;UseDevelopmentEmulator=true;EntityPath={HUB}"
        with EventHubProducerClient.from_connection_string(local) as q:
            assert q.get_eventhub_properties()["partition_ids"] == ["0", "1", "2", "3"]
        print("ok   the emulator's Endpoint=sb://localhost connection string")

        # Keys: with any_key off, a wrong key is refused and the namespace key works.
        s.call("PUT", f"/eventhubs/{NS}/settings", {"any_key": False})
        try:
            EventHubProducerClient.from_connection_string(CS, retry_total=0).get_eventhub_properties()
            raise AssertionError("a wrong key was accepted")
        except Exception as e:
            assert "401" in str(e) or "nvalid" in str(e) or "uthoriz" in str(e), e
        with EventHubProducerClient.from_connection_string(v["hubs"][0]["connection"]) as q:
            assert q.get_eventhub_properties()["eventhub_name"] == HUB
        s.call("PUT", f"/eventhubs/{NS}/settings", {"any_key": True})
        print("ok   SAS keys: a wrong one is refused, the namespace's own works")

        # Retention on the environment clock.
        s.call("POST", f"/eventhubs/{NS}/hubs", {"hub": "audit", "partitions": 1, "retention": "1h"})
        s.call("POST", f"/eventhubs/{NS}/audit/events", {"body": "login"})
        s.call("POST", "/clock/advance", {"by": "2h"})
        wait(lambda: s.call("GET", f"/eventhubs/{NS}/audit/events")["partitions"][0]["isEmpty"], "retention to drop the event")
        part = s.call("GET", f"/eventhubs/{NS}/audit/events")["partitions"][0]
        assert part["beginSequenceNumber"] == 1 and part["lastEnqueuedSequenceNumber"] == 0, part
        s.call("POST", "/clock/advance", {"reset": True})
        print("ok   retention on the environment clock (the begin sequence number moves on)")

        kafka(s, CS)
        pubsub(s)


def kafka(s, cs):
    try:
        from confluent_kafka import Consumer, Producer
    except ImportError:
        print("skip Kafka endpoint (pip install confluent-kafka)")
        return
    conf = {"bootstrap.servers": f"127.0.0.1:{s.KAFKA_PORT}", "security.protocol": "SASL_PLAINTEXT", "sasl.mechanism": "PLAIN", "sasl.username": "$ConnectionString", "sasl.password": cs}
    pr = Producer(conf)
    pr.produce(HUB, key=b"ORD-1042", value=b'{"orderId": "ORD-4001"}', headers=[("kind", b"OrderShipped")], partition=0)
    assert pr.flush(10) == 0
    ev = s.call("GET", f"/eventhubs/{NS}/{HUB}/events?partition=0&last=1")["events"][0]
    assert ev["via"] == "kafka" and ev["partitionKey"] == "ORD-1042" and ev["properties"] == {"kind": "OrderShipped"}, ev
    got, _ = receive(cs, "0", 1, starting_position=int(ev["sequenceNumber"]) - 1)
    assert got and json.loads(got[0].body_as_str())["orderId"] == "ORD-4001", got
    co = Consumer(dict(conf, **{"group.id": "kafka-readers", "auto.offset.reset": "earliest"}))
    co.subscribe([HUB])
    seen = []
    end = time.time() + 20
    while time.time() < end and not ((0, 0, b'{"orderId": "ORD-1042", "step": 0}') in seen and any(v == b"SHP-7 created" for _, _, v in seen)):
        m = co.poll(1)
        if m is not None and not m.error():
            seen.append((m.partition(), m.offset(), m.value()))
    co.close()
    assert (0, 0, b'{"orderId": "ORD-1042", "step": 0}') in seen and any(v == b"SHP-7 created" for _, _, v in seen), seen
    print("ok   Kafka endpoint: confluent-kafka with $ConnectionString produces to the hub and reads AMQP events")


def pubsub(s):
    try:
        os.environ["PUBSUB_EMULATOR_HOST"] = f"127.0.0.1:{s.PUBSUB_PORT}"
        from google.cloud import pubsub_v1
    except ImportError:
        print("skip Pub/Sub ingestion (pip install google-cloud-pubsub)")
        return
    pub, sub = pubsub_v1.PublisherClient(), pubsub_v1.SubscriberClient()
    src = {"resource_group": "acme-rg", "namespace": NS, "event_hub": HUB, "client_id": "11111111-2222-3333-4444-555555555555", "tenant_id": "66666666-7777-8888-9999-000000000000",
           "subscription_id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "gcp_service_account": "ingest@acme-stg.iam.gserviceaccount.com"}
    t = pub.create_topic(request={"name": "projects/acme-stg/topics/orders-from-eventhubs", "ingestion_data_source_settings": {"azure_event_hubs": src}})
    assert t.ingestion_data_source_settings.azure_event_hubs.state == 1, t
    sp = "projects/acme-stg/subscriptions/orders-from-eventhubs-app"
    sub.create_subscription(request={"name": sp, "topic": t.name})
    msgs = []

    def pull():
        r = sub.pull(request={"subscription": sp, "max_messages": 50}, timeout=10)
        if r.received_messages:
            sub.acknowledge(request={"subscription": sp, "ack_ids": [m.ack_id for m in r.received_messages]})
        msgs.extend(m.message for m in r.received_messages)
        return len(msgs) >= 5 or None

    wait(pull, "events ingested from the event hub", 30)
    first = [m for m in msgs if m.data == b'{"orderId": "ORD-1042", "step": 0}']
    assert first and first[0].ordering_key == "ORD-1042" and first[0].attributes["region"] == "us-east", msgs[:3]
    gone = pub.create_topic(request={"name": "projects/acme-stg/topics/from-missing-hub", "ingestion_data_source_settings": {"azure_event_hubs": dict(src, event_hub="nope")}})
    assert gone.ingestion_data_source_settings.azure_event_hubs.state == 5, gone
    nsg = pub.create_topic(request={"name": "projects/acme-stg/topics/from-missing-ns", "ingestion_data_source_settings": {"azure_event_hubs": dict(src, namespace="nope-hubs")}})
    assert nsg.ingestion_data_source_settings.azure_event_hubs.state == 4, nsg
    print("ok   Pub/Sub topic ingests from the event hub (partition key → ordering key, properties → attributes); EVENT_HUB_NOT_FOUND, NAMESPACE_NOT_FOUND")


if __name__ == "__main__":
    main()
    print("all Event Hubs checks passed")
