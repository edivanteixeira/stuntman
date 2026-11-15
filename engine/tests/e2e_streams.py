"""End-to-end check of Streams (A17) with real clients: librdkafka (confluent-kafka) producing and
consuming in a group, with SASL PLAIN and SCRAM, Avro values through the Schema Registry; a message
produced "as the partner"; lag; a rebalance; provocations. kafka-python checks the same broker.

    pip install "confluent-kafka[schemaregistry,avro]" kafka-python
    cargo build -p stuntman && python3 tests/e2e_streams.py
"""
import json, time

from harness import Stuntman, wait

SCHEMA = json.dumps({"type": "record", "name": "ShipmentEvent", "namespace": "acme", "fields": [
    {"name": "shipment", "type": "string"}, {"name": "status", "type": {"type": "enum", "name": "Status", "symbols": ["CREATED", "IN_TRANSIT", "DELIVERED"]}},
    {"name": "at", "type": "long"}, {"name": "carrier", "type": ["null", "string"], "default": None}]})


def main():
    from confluent_kafka import Consumer, KafkaError, Producer
    from confluent_kafka.admin import AdminClient, NewTopic
    from confluent_kafka.schema_registry import SchemaRegistryClient
    from confluent_kafka.schema_registry.avro import AvroDeserializer, AvroSerializer
    from confluent_kafka.serialization import MessageField, SerializationContext
    with Stuntman("stuntman-streams-") as s:
        boot = f"127.0.0.1:{s.KAFKA_PORT}"
        reg = f"http://127.0.0.1:{s.SCHEMA_REGISTRY_PORT}"
        s.call("PUT", "/streams/config", {"advertised_host": "127.0.0.1", "sasl_required": True, "sasl_users": [{"user": "acme-tms", "password": "s3cret"}]})
        sasl = {"bootstrap.servers": boot, "security.protocol": "SASL_PLAINTEXT", "sasl.mechanism": "SCRAM-SHA-512", "sasl.username": "acme-tms", "sasl.password": "s3cret"}
        plain = dict(sasl, **{"sasl.mechanism": "PLAIN"})

        # Admin: a topic with 3 partitions.
        ad = AdminClient(sasl)
        fs = ad.create_topics([NewTopic("shipments.events", num_partitions=3, replication_factor=1)])
        for f in fs.values():
            f.result(10)
        md = ad.list_topics(timeout=10)
        assert len(md.topics["shipments.events"].partitions) == 3, md.topics

        # Avro through the registry.
        sr = SchemaRegistryClient({"url": reg})
        ser = AvroSerializer(sr, SCHEMA)
        des = AvroDeserializer(sr, SCHEMA)
        p = Producer(dict(sasl, **{"linger.ms": 5}))
        sent = []
        for i in range(6):
            v = {"shipment": f"SHP-{1040 + i}", "status": "IN_TRANSIT", "at": 1763190000000 + i, "carrier": "Globex Logistica"}
            p.produce("shipments.events", key=f"SHP-{1040 + i}", value=ser(v, SerializationContext("shipments.events", MessageField.VALUE)), headers={"trace": b"t-1"},
                      on_delivery=lambda e, m: sent.append((e, m.partition(), m.offset())))
        p.flush(15)
        assert len(sent) == 6 and all(e is None for e, _, _ in sent), sent
        subs = s.call("GET", "/streams/schemas")["subjects"]
        assert subs[0]["subject"] == "shipments.events-value", subs

        # A consumer group reads everything and commits.
        c = Consumer(dict(plain, **{"group.id": "acme-tms", "auto.offset.reset": "earliest", "enable.auto.commit": False, "session.timeout.ms": 10000}))
        c.subscribe(["shipments.events"])
        got = []
        end = time.time() + 30
        while len(got) < 6 and time.time() < end:
            m = c.poll(0.5)
            if m is None or m.error():
                continue
            got.append(des(m.value(), SerializationContext(m.topic(), MessageField.VALUE)))
            c.commit(m, asynchronous=False)
        assert sorted(x["shipment"] for x in got) == [f"SHP-{1040 + i}" for i in range(6)], got
        groups = s.call("GET", "/streams/groups")["groups"]
        g = [x for x in groups if x["group"] == "acme-tms"][0]
        assert g["lag"] == 0 and g["state"] == "Stable" and len(g["members"]) == 1, g

        # The partner produces one (Avro, with the subject's schema); the consumer gets it; lag shows before.
        s.call("POST", "/streams/topics/shipments.events/produce", {"key": "SHP-2001", "subject": "shipments.events-value", "value": {"shipment": "SHP-2001", "status": "DELIVERED", "at": 1763190999000, "carrier": None}})
        msg = None
        end = time.time() + 15
        while msg is None and time.time() < end:
            m = c.poll(0.5)
            if m is not None and not m.error():
                msg = des(m.value(), SerializationContext(m.topic(), MessageField.VALUE))
        assert msg == {"shipment": "SHP-2001", "status": "DELIVERED", "at": 1763190999000, "carrier": None}, msg
        shown = s.call("GET", "/streams/topics/shipments.events/messages?limit=1")["messages"][0]
        assert shown["value"]["format"] == "avro" and shown["value"]["value"]["shipment"] == "SHP-2001", shown

        # A rebalance: the member joins again and keeps consuming.
        gen = s.call("GET", "/streams/groups")["groups"][0]["generation"]
        s.call("POST", "/streams/groups/acme-tms/rebalance")
        end = time.time() + 20
        while time.time() < end:
            c.poll(0.5)
            gg = [x for x in s.call("GET", "/streams/groups")["groups"] if x["group"] == "acme-tms"][0]
            if gg["generation"] > gen and gg["state"] == "Stable":
                break
        assert gg["generation"] > gen and gg["state"] == "Stable", gg
        c.close()

        # Wrong password: SASL refused.
        bad = Producer(dict(sasl, **{"sasl.password": "wrong", "message.timeout.ms": 3000}))
        errs = []
        bad.produce("shipments.events", value=b"x", on_delivery=lambda e, m: errs.append(e))
        bad.flush(8)
        assert errs and errs[0] is not None, errs

        # kafka-python against the same broker (no SASL needed once it is off).
        s.call("PUT", "/streams/config", {"sasl_required": False})
        from kafka import KafkaConsumer, KafkaProducer
        kp = KafkaProducer(bootstrap_servers=boot)
        kp.send("invoices", key=b"INV-1", value=b'{"total": 120.5}').get(timeout=10)
        kp.close()
        kc = KafkaConsumer("invoices", bootstrap_servers=boot, group_id="acme-reports", auto_offset_reset="earliest", consumer_timeout_ms=8000)
        vals = [m.value for m in kc]
        kc.close()
        assert vals == [b'{"total": 120.5}'], vals

        # leader_not_available on purpose.
        s.call("PUT", "/streams/config", {"provoke": "leader_not_available"})
        lp = Producer({"bootstrap.servers": boot, "message.timeout.ms": 2500})
        errs = []
        lp.produce("invoices", value=b"y", on_delivery=lambda e, m: errs.append(e))
        lp.flush(6)
        assert errs and errs[0] is not None, errs
        s.call("PUT", "/streams/config", {"provoke": "none"})
        topics = {t["name"]: t for t in s.call("GET", "/streams/topics")["topics"]}
        assert topics["shipments.events"]["messages"] == 7 and topics["invoices"]["messages"] == 1, topics
    print("Streams e2e: ok")


if __name__ == "__main__":
    main()
