"""End-to-end check of Pub/Sub with the official clients pointed at PUBSUB_EMULATOR_HOST: topics and
subscriptions made by google-cloud-pubsub, three messages with attributes and an ordering key,
received over StreamingPull and acked; Pull, ModifyAckDeadline and redelivery after the deadline;
a dead-letter topic; a provoked UNAVAILABLE the client retries; NOT_FOUND; the REST publish; what
the tools show. Then the rest of the API with the same clients: SchemaServiceClient (Avro and
Protocol Buffer schemas, revisions, ValidateMessage) and topics that validate what is published;
snapshots and Seek; UpdateTopic and UpdateSubscription; filters; push subscriptions to a local HTTP
receiver (wrapped and unwrapped, OIDC token, retry after a 500); BigQuery and Cloud Storage
subscriptions (checked through the tools); exactly-once ack errors; DetachSubscription; paging. And:
Bigtable subscriptions; import topics (Amazon MSK read from Streams' Kafka, Cloud Storage read from
a storage container, Kinesis fed by pubsub_ingest); JavaScript UDF transforms on topics and
subscriptions; expiration on the environment clock; BigQuery table schemas with a dead-letter;
typed Avro files; proto2 groups; retry-policy backoff on nacks; transient exactly-once ack failures
the client retries; max_outstanding_bytes. The .NET client (Google.Cloud.PubSub.V1) publishes too
when dotnet is installed.

    pip install google-cloud-pubsub fastavro cryptography
    cargo build -p stuntman && python3 tests/e2e_pubsub.py
"""
import base64, http.client, http.server, io, json, os, shutil, subprocess, tempfile, threading, time

from harness import Stuntman, free_port, wait

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

        rest_of_the_api(s, pub, sub)
        the_remaining_items(s, pub, sub)

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


SCHEMA_AVRO = json.dumps({"type": "record", "name": "ShipmentEvent", "namespace": "acme.logistics", "fields": [
    {"name": "shipment", "type": "string"}, {"name": "status", "type": {"type": "enum", "name": "Status", "symbols": ["CREATED", "DELIVERED"]}},
    {"name": "weight_kg", "type": "double"}]})
SCHEMA_PROTO = """syntax = "proto3";
package acme.logistics;
message ShipmentEvent {
  string shipment = 1;
  int32 stops = 2;
  repeated string tags = 3;
}"""


def b64url_json(part):
    return json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4)))


class Receiver:
    """A push endpoint: records each request; answers the statuses queued, then 200."""

    def __init__(self, statuses=()):
        self.got, self.statuses, self.port = [], list(statuses), free_port()
        rec = self

        class H(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                rec.got.append((self.path, {k.lower(): v for k, v in self.headers.items()}, body))
                st = rec.statuses.pop(0) if rec.statuses else 200
                self.send_response(st)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def log_message(self, *a):
                pass
        self.srv = http.server.ThreadingHTTPServer(("127.0.0.1", self.port), H)
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()

    def url(self, path):
        return f"http://127.0.0.1:{self.port}{path}"

    def close(self):
        self.srv.shutdown()
        self.srv.server_close()


def rest_of_the_api(s, pub, sub):
    from google.api_core import exceptions as gx
    from google.cloud.pubsub import SchemaServiceClient
    from google.cloud.pubsub_v1.subscriber._protocol.streaming_pull_manager import _get_ack_errors
    from google.protobuf import duration_pb2, field_mask_pb2, timestamp_pb2
    from google.pubsub_v1.types import Encoding, Schema
    import fastavro

    sc = SchemaServiceClient()
    plain = pubsub_v1_publisher()

    def pull(name, n=10):
        return sub.pull(request={"subscription": name, "max_messages": n, "return_immediately": True}, timeout=10).received_messages

    def pull_ack(name, n=10):
        got = pull(name, n)
        if got:
            sub.acknowledge(request={"subscription": name, "ack_ids": [m.ack_id for m in got]})
        return got

    # ---- Schemas: Avro and Protocol Buffer, revisions, validation.
    avro = sc.create_schema(request={"parent": P, "schema_id": "shipment-event", "schema": {"type_": Schema.Type.AVRO, "definition": SCHEMA_AVRO}})
    assert avro.name == f"{P}/schemas/shipment-event" and avro.revision_id and avro.definition == SCHEMA_AVRO, avro
    proto = sc.create_schema(request={"parent": P, "schema_id": "shipment-proto", "schema": {"type_": Schema.Type.PROTOCOL_BUFFER, "definition": SCHEMA_PROTO}})
    try:
        sc.create_schema(request={"parent": P, "schema_id": "shipment-event", "schema": {"type_": Schema.Type.AVRO, "definition": SCHEMA_AVRO}})
        raise AssertionError("ALREADY_EXISTS expected")
    except gx.AlreadyExists:
        pass
    for bad in [{"type_": Schema.Type.AVRO, "definition": '{"type": "record", "name": "X", "fields": [{"name": "a", "type": "Nope"}]}'},
                {"type_": Schema.Type.PROTOCOL_BUFFER, "definition": "message X { Missing m = 1; }"}]:
        try:
            sc.validate_schema(request={"parent": P, "schema": bad})
            raise AssertionError("an invalid definition must be refused")
        except gx.InvalidArgument:
            pass
    sc.validate_schema(request={"parent": P, "schema": {"type_": Schema.Type.PROTOCOL_BUFFER, "definition": SCHEMA_PROTO}})
    assert sc.get_schema(request={"name": avro.name, "view": "BASIC"}).definition == ""
    assert {x.name for x in sc.list_schemas(request={"parent": P})} == {avro.name, proto.name}
    good = json.dumps({"shipment": "SHP-3001", "status": "CREATED", "weight_kg": 4.2}).encode()
    sc.validate_message(request={"parent": P, "name": avro.name, "message": good, "encoding": Encoding.JSON})
    try:
        sc.validate_message(request={"parent": P, "name": avro.name, "message": b'{"shipment": "SHP-3001", "status": "LOST", "weight_kg": 1}', "encoding": Encoding.JSON})
        raise AssertionError("LOST is not a Status")
    except gx.InvalidArgument:
        pass
    # Protocol Buffer binary: shipment="SHP-3002" (field 1), stops=3 (field 2), tags=["cold"] (field 3).
    pb_msg = b"\x0a\x08SHP-3002\x10\x03\x1a\x04cold"
    sc.validate_message(request={"parent": P, "name": proto.name, "message": pb_msg, "encoding": Encoding.BINARY})
    sc.validate_message(request={"parent": P, "schema": {"type_": Schema.Type.PROTOCOL_BUFFER, "definition": SCHEMA_PROTO}, "message": b'{"shipment": "SHP-1", "stops": 2}', "encoding": Encoding.JSON})
    for bad in [b"\x0a\x20SHP", b"\x08\x05"]:  # truncated; shipment sent as a varint
        try:
            sc.validate_message(request={"parent": P, "name": proto.name, "message": bad, "encoding": Encoding.BINARY})
            raise AssertionError(f"{bad!r} must be refused")
        except gx.InvalidArgument:
            pass
    # Revisions: commit, list, rollback, delete one.
    v2_def = json.dumps({**json.loads(SCHEMA_AVRO), "fields": json.loads(SCHEMA_AVRO)["fields"] + [{"name": "carrier", "type": ["null", "string"], "default": None}]})
    v2 = sc.commit_schema(request={"name": avro.name, "schema": {"type_": Schema.Type.AVRO, "definition": v2_def}})
    assert v2.revision_id != avro.revision_id and sc.get_schema(request={"name": avro.name}).revision_id == v2.revision_id
    assert sc.get_schema(request={"name": f"{avro.name}@{avro.revision_id}"}).definition == SCHEMA_AVRO
    v3 = sc.rollback_schema(request={"name": avro.name, "revision_id": avro.revision_id})
    assert v3.definition == SCHEMA_AVRO and [x.revision_id for x in sc.list_schema_revisions(request={"name": avro.name})] == [v3.revision_id, v2.revision_id, avro.revision_id]
    sc.delete_schema_revision(request={"name": f"{avro.name}@{v3.revision_id}"})
    assert len(list(sc.list_schema_revisions(request={"name": avro.name}))) == 2
    print("ok   schemas: create, validate (Avro JSON, proto binary and JSON), commit, rollback, delete a revision")

    # ---- Topics with a schema validate what is published and stamp googclient_ attributes.
    typed = f"{P}/topics/shipments-typed"
    pub.create_topic(request={"name": typed, "schema_settings": {"schema": avro.name, "encoding": Encoding.JSON}})
    assert pub.get_topic(request={"topic": typed}).schema_settings.schema == avro.name
    sub.create_subscription(request={"name": f"{P}/subscriptions/shipments-typed-app", "topic": typed})
    plain.publish(typed, good).result(10)
    plain.publish(typed, json.dumps({"shipment": "SHP-3003", "status": "DELIVERED", "weight_kg": 1.5, "carrier": "Initech Haulage"}).encode()).result(10)  # revision 2
    try:
        plain.publish(typed, b'{"shipment": "SHP-3004"}').result(10)
        raise AssertionError("INVALID_ARGUMENT expected")
    except gx.InvalidArgument:
        pass
    got = pull_ack(f"{P}/subscriptions/shipments-typed-app")
    a = [dict(m.message.attributes) for m in got]
    assert len(got) == 2 and a[0]["googclient_schemaname"] == avro.name and a[0]["googclient_schemaencoding"] == "JSON", a
    # The newest revision in range that validates: v2 (carrier is optional) for both.
    assert [x["googclient_schemarevisionid"] for x in a] == [v2.revision_id, v2.revision_id], (a, avro.revision_id, v2.revision_id)
    binary = f"{P}/topics/shipments-binary"
    pub.create_topic(request={"name": binary, "schema_settings": {"schema": avro.name, "encoding": Encoding.BINARY, "first_revision_id": avro.revision_id, "last_revision_id": avro.revision_id}})
    buf = io.BytesIO()
    fastavro.schemaless_writer(buf, fastavro.parse_schema(json.loads(SCHEMA_AVRO)), {"shipment": "SHP-3005", "status": "DELIVERED", "weight_kg": 9.75})
    assert plain.publish(binary, buf.getvalue()).result(10)
    try:
        plain.publish(binary, good).result(10)
        raise AssertionError("JSON on a BINARY topic must be refused")
    except gx.InvalidArgument:
        pass
    proto_topic = f"{P}/topics/shipments-proto"
    pub.create_topic(request={"name": proto_topic, "schema_settings": {"schema": proto.name, "encoding": Encoding.BINARY}})
    assert plain.publish(proto_topic, pb_msg).result(10)
    print("ok   topics with schemas: JSON and binary (Avro from fastavro, protobuf) accepted, invalid refused, googclient_ attributes")

    # ---- UpdateTopic and UpdateSubscription with field masks; paging.
    deliveries = f"{P}/topics/deliveries"
    pub.create_topic(request={"name": deliveries, "kms_key_name": "projects/acme-stg/locations/us/keyRings/ring/cryptoKeys/pubsub", "message_storage_policy": {"allowed_persistence_regions": ["us-east1"]}})
    t = pub.update_topic(request={"topic": {"name": deliveries, "labels": {"team": "logistics"}, "message_retention_duration": duration_pb2.Duration(seconds=86400)},
                                  "update_mask": field_mask_pb2.FieldMask(paths=["labels", "message_retention_duration"])})
    t = pub.get_topic(request={"topic": deliveries})
    assert dict(t.labels) == {"team": "logistics"} and t.message_retention_duration.total_seconds() == 86400 and t.kms_key_name.endswith("/pubsub") and list(t.message_storage_policy.allowed_persistence_regions) == ["us-east1"], t
    replay = f"{P}/subscriptions/deliveries-replay"
    sub.create_subscription(request={"name": replay, "topic": deliveries})
    u = sub.update_subscription(request={"subscription": {"name": replay, "ack_deadline_seconds": 45, "labels": {"team": "logistics"}, "retain_acked_messages": True},
                                         "update_mask": field_mask_pb2.FieldMask(paths=["ack_deadline_seconds", "labels", "retain_acked_messages"])})
    assert u.ack_deadline_seconds == 45 and u.retain_acked_messages and dict(sub.get_subscription(request={"subscription": replay}).labels) == {"team": "logistics"}, u
    try:
        sub.update_subscription(request={"subscription": {"name": replay, "filter": 'attributes:x'}, "update_mask": field_mask_pb2.FieldMask(paths=["filter"])})
        raise AssertionError("filter is immutable")
    except gx.InvalidArgument:
        pass
    pages = list(pub.list_topics(request={"project": P, "page_size": 2}).pages)
    assert len(pages) >= 3 and pages[0].next_page_token and sum(len(p.topics) for p in pages) == len(list(pub.list_topics(request={"project": P}))), [len(p.topics) for p in pages]
    print("ok   UpdateTopic (labels, retention; KMS and storage policy kept), UpdateSubscription, paging")

    # ---- Snapshots and Seek.
    for i in range(2):
        plain.publish(deliveries, f"DLV-{i} out for delivery".encode()).result(10)
    assert len(pull_ack(replay)) == 2
    snap = sub.create_snapshot(request={"name": f"{P}/snapshots/before-dlv-2", "subscription": replay, "labels": {"why": "replay"}})
    assert snap.topic == deliveries and snap.expire_time, snap
    plain.publish(deliveries, b"DLV-2 out for delivery").result(10)
    assert [m.message.data for m in pull_ack(replay)] == [b"DLV-2 out for delivery"]
    sub.seek(request={"subscription": replay, "snapshot": snap.name})
    assert [m.message.data for m in pull_ack(replay)] == [b"DLV-2 out for delivery"], "only what came after the snapshot"
    sub.seek(request={"subscription": replay, "time": timestamp_pb2.Timestamp(seconds=int(time.time()) - 60)})
    assert len(pull_ack(replay)) == 3, "retain_acked_messages: a seek to a time replays acked messages"
    assert list(pub.list_topic_snapshots(request={"topic": deliveries})) == [snap.name]
    up = sub.update_snapshot(request={"snapshot": {"name": snap.name, "labels": {"why": "audit"}}, "update_mask": field_mask_pb2.FieldMask(paths=["labels"])})
    assert dict(up.labels) == {"why": "audit"} and [x.name for x in sub.list_snapshots(request={"project": P})] == [snap.name]
    sub.delete_snapshot(request={"snapshot": snap.name})
    try:
        sub.get_snapshot(request={"snapshot": snap.name})
        raise AssertionError("deleted")
    except gx.NotFound:
        pass
    print("ok   snapshots: create, seek to one, list, update, delete; seek to a time replays retained acked messages")

    # ---- Filters.
    east = f"{P}/subscriptions/deliveries-us-east"
    sub.create_subscription(request={"name": east, "topic": deliveries, "filter": 'attributes.region = "us-east" AND NOT attributes:test'})
    try:
        sub.create_subscription(request={"name": f"{P}/subscriptions/bad-filter", "topic": deliveries, "filter": "attributes.region = us-east"})
        raise AssertionError("invalid filter")
    except gx.InvalidArgument:
        pass
    for region, test in [("us-east", None), ("us-west", None), ("us-east", "1")]:
        attrs = {"region": region, **({"test": test} if test else {})}
        plain.publish(deliveries, f"{region} {test}".encode(), **attrs).result(10)
    assert [m.message.data for m in pull_ack(east)] == [b"us-east None"]
    assert sub.get_subscription(request={"subscription": east}).filter.startswith("attributes.region")
    print("ok   filter: attributes.k = \"v\" AND NOT attributes:k; invalid filter refused")

    # ---- Push: wrapped with an OIDC token (signed by Stuntman's identity provider), a 500 retried.
    s.call("PUT", "/identity/providers/acme-google", {"tenant": "acme-stg", "flavour": "google"})
    rcv = Receiver([500])
    try:
        push = f"{P}/subscriptions/deliveries-push"
        sub.create_subscription(request={"name": push, "topic": deliveries, "push_config": {"push_endpoint": rcv.url("/pubsub/push"),
            "oidc_token": {"service_account_email": "push-invoker@acme-stg.iam.gserviceaccount.com", "audience": "https://shipments.acme-stg.example.com"}}})
        mid = plain.publish(deliveries, b"DLV-3 delivered", region="us-east").result(10)
        wait(lambda: len(rcv.got) >= 2, "a push, a 500, and the retry")
        path, h, body = rcv.got[1]
        v = json.loads(body)
        assert path == "/pubsub/push" and v["subscription"] == push and base64.b64decode(v["message"]["data"]) == b"DLV-3 delivered", v
        assert v["message"]["messageId"] == v["message"]["message_id"] == mid and v["message"]["attributes"] == {"region": "us-east"} and v["message"]["publishTime"].endswith("Z"), v
        assert rcv.got[0][2] == body, "the retry carries the same message"
        token = h["authorization"].split(" ", 1)[1]
        head, claims = b64url_json(token.split(".")[0]), b64url_json(token.split(".")[1])
        assert claims["aud"] == "https://shipments.acme-stg.example.com" and claims["email"] == "push-invoker@acme-stg.iam.gserviceaccount.com" and claims["email_verified"], claims
        jwks = json.loads(urllib_get(f"http://127.0.0.1:{s.PORT}/login/acme-stg/discovery/v2.0/keys"))
        key = next(k for k in jwks["keys"] if k["kid"] == head["kid"])
        from cryptography.hazmat.primitives import hashes
        from cryptography.hazmat.primitives.asymmetric import padding, rsa
        n, e = (int.from_bytes(base64.urlsafe_b64decode(key[x] + "=" * (-len(key[x]) % 4)), "big") for x in ("n", "e"))
        rsa.RSAPublicNumbers(e, n).public_key().verify(base64.urlsafe_b64decode(token.split(".")[2] + "=" * (-len(token.split(".")[2]) % 4)),
                                                         ".".join(token.split(".")[:2]).encode(), padding.PKCS1v15(), hashes.SHA256())
        wait(lambda: s.call("GET", "/pubsub/messages?subscription=deliveries-push")["subscription"]["acked"] == 1, "push acked on 200")
        # Unwrapped, with metadata in headers.
        raw = f"{P}/subscriptions/deliveries-raw"
        sub.create_subscription(request={"name": raw, "topic": deliveries, "push_config": {"push_endpoint": rcv.url("/raw"), "no_wrapper": {"write_metadata": True}}})
        mid = plain.publish(deliveries, b"DLV-4 delivered", region="us-west").result(10)
        got = wait(lambda: [g for g in rcv.got if g[0] == "/raw"], "the unwrapped push")
        _, h, body = got[0]
        assert body == b"DLV-4 delivered" and h["x-goog-pubsub-message-id"] == mid and h["x-goog-pubsub-subscription-name"] == raw and h["region"] == "us-west", h
        # ModifyPushConfig to nothing: a pull subscription again.
        sub.modify_push_config(request={"subscription": raw, "push_config": {}})
        assert not sub.get_subscription(request={"subscription": raw}).push_config.push_endpoint
        plain.publish(deliveries, b"DLV-5 delivered", region="us-west").result(10)
        assert [m.message.data for m in pull_ack(raw)] == [b"DLV-5 delivered"]
    finally:
        rcv.close()
    print("ok   push: wrapped body, OIDC token verified with the JWKS, 500 retried then acked; unwrapped with metadata headers; ModifyPushConfig")

    # ---- BigQuery: rows with the topic schema's columns and metadata.
    bq = f"{P}/subscriptions/shipments-typed-bq"
    sub.create_subscription(request={"name": bq, "topic": typed, "bigquery_config": {"table": "acme-stg.logistics.shipment_events", "use_topic_schema": True, "write_metadata": True}})
    mid = plain.publish(typed, json.dumps({"shipment": "SHP-3006", "status": "DELIVERED", "weight_kg": 3.25}).encode(), source="warehouse").result(10)
    rows = wait(lambda: s.call("GET", "/pubsub/bigquery?table=acme-stg.logistics.shipment_events")["rows"], "a BigQuery row")
    r = rows[0]["row"]
    assert (r["shipment"], r["status"], r["weight_kg"], r["message_id"], r["subscription_name"]) == ("SHP-3006", "DELIVERED", 3.25, mid, bq), r
    assert json.loads(r["attributes"])["source"] == "warehouse" and r["publish_time"].endswith("Z"), r
    print("ok   BigQuery subscription: a row per message, topic schema columns, write_metadata")

    # ---- Cloud Storage: Avro files of 2 messages, into a Stuntman storage account with that bucket.
    s.call("POST", "/services/s3/accounts", {"account": "acme-stg-archive", "containers": ["acme-stg-shipments-archive"]})
    gcs = f"{P}/subscriptions/deliveries-archive"
    sub.create_subscription(request={"name": gcs, "topic": deliveries, "cloud_storage_config": {"bucket": "acme-stg-shipments-archive", "filename_prefix": "shipments/",
        "filename_suffix": ".avro", "max_messages": 2, "avro_config": {"write_metadata": True}}})
    ids = [plain.publish(deliveries, f"DLV-{i} archived".encode(), region="eu-west").result(10) for i in (6, 7)]
    files = wait(lambda: s.call("GET", "/pubsub/storage?bucket=acme-stg-shipments-archive")["files"], "a Cloud Storage file")
    f = files[0]
    recs = list(fastavro.reader(io.BytesIO(base64.b64decode(f["content"]))))
    assert f["object"].startswith("shipments/") and f["object"].endswith(".avro") and f["messages"] == 2 and f["storedIn"] == "acme-stg-archive/acme-stg-shipments-archive", f
    assert [x["data"] for x in recs] == [b"DLV-6 archived", b"DLV-7 archived"] and [x["message_id"] for x in recs] == ids and recs[0]["attributes"] == {"region": "eu-west"}, recs
    obj = s.call("GET", f"/storage/acme-stg-archive/acme-stg-shipments-archive/objects/{f['object']}")
    assert obj, obj
    print("ok   Cloud Storage subscription: an Avro file of 2 messages (read by fastavro), also in the storage account")

    # ---- Exactly-once: an expired ack id fails with the ErrorInfo the clients read.
    eo = f"{P}/subscriptions/deliveries-exactly-once"
    sub.create_subscription(request={"name": eo, "topic": deliveries, "enable_exactly_once_delivery": True})
    plain.publish(deliveries, b"DLV-8 delivered").result(10)
    first = pull(eo)[0]
    sub.modify_ack_deadline(request={"subscription": eo, "ack_ids": [first.ack_id], "ack_deadline_seconds": 0})
    again = pull(eo)[0]
    try:
        sub.acknowledge(request={"subscription": eo, "ack_ids": [first.ack_id]})
        raise AssertionError("an expired ack id must fail under exactly-once")
    except gx.InvalidArgument as e:
        assert _get_ack_errors(e) == {first.ack_id: "PERMANENT_FAILURE_INVALID_ACK_ID"}, _get_ack_errors(e)
    sub.acknowledge(request={"subscription": eo, "ack_ids": [again.ack_id]})
    # Streaming: ack_with_response resolves; acking after a nack elsewhere fails with INVALID_ACK_ID.
    from google.cloud.pubsub_v1.subscriber.exceptions import AcknowledgeError, AcknowledgeStatus
    results, lock, nacked = [], threading.Lock(), []

    def on(m):
        if m.data == b"DLV-10 stale" and not nacked:
            nacked.append(m.ack_id)
            sub.modify_ack_deadline(request={"subscription": eo, "ack_ids": [m.ack_id], "ack_deadline_seconds": 0})
        try:
            r = m.ack_with_response().result(timeout=10)
        except AcknowledgeError as e:
            r = e.error_code
        with lock:
            results.append((m.data, r))
    fut = sub.subscribe(eo, on)
    plain.publish(deliveries, b"DLV-9 fresh").result(10)
    plain.publish(deliveries, b"DLV-10 stale").result(10)
    wait(lambda: len(results) >= 3, "exactly-once acks over the stream", 30)
    fut.cancel()
    try:
        fut.result(5)
    except Exception:
        pass
    assert (b"DLV-9 fresh", AcknowledgeStatus.SUCCESS) in results and (b"DLV-10 stale", AcknowledgeStatus.INVALID_ACK_ID) in results and (b"DLV-10 stale", AcknowledgeStatus.SUCCESS) in results, results
    print("ok   exactly-once: invalid ack id in ErrorInfo (unary), INVALID_ACK_ID then SUCCESS over StreamingPull")

    # ---- DetachSubscription.
    pub.detach_subscription(request={"subscription": east})
    assert sub.get_subscription(request={"subscription": east}).detached
    try:
        pull(east)
        raise AssertionError("a detached subscription cannot be pulled")
    except gx.FailedPrecondition:
        pass
    assert east not in list(pub.list_topic_subscriptions(request={"topic": deliveries}))
    view = {x["name"]: x for t in s.call("GET", "/pubsub/topics")["topics"] for x in t["subscriptions"]}
    assert (view[push]["type"], view[bq]["type"], view[gcs]["type"], view[east]["detached"]) == ("push", "bigquery", "cloud_storage", True), view
    print("ok   DetachSubscription; list_pubsub_topics shows each subscription's type")


UDF_TOPIC = """function redact(message, metadata) {
  const d = JSON.parse(message.data);
  if (d.status === "TEST") return null;
  delete d.email;
  message.data = JSON.stringify(d);
  message.attributes["redacted"] = "true";
  return message;
}"""
UDF_SUB = """function stamp(message, metadata) {
  if (message.data.indexOf("BROKEN") >= 0) throw new Error("cannot read " + metadata.message_id);
  message.attributes["seen_by"] = "stamp";
  return message;
}"""


def the_remaining_items(s, pub, sub):
    from google.api_core import exceptions as gx
    from google.cloud.pubsub import SchemaServiceClient
    from google.cloud.pubsub_v1 import types
    from google.cloud.pubsub_v1.subscriber.exceptions import AcknowledgeStatus
    from google.protobuf import duration_pb2
    from google.pubsub_v1.types import Encoding, Schema
    import fastavro, queue

    plain = pubsub_v1_publisher()

    def pull(name, n=10):
        return sub.pull(request={"subscription": name, "max_messages": n, "return_immediately": True}, timeout=10).received_messages

    def pull_ack(name, n=10):
        got = pull(name, n)
        if got:
            sub.acknowledge(request={"subscription": name, "ack_ids": [m.ack_id for m in got]})
        return got

    def subs_view():
        return {x["name"]: x for t in s.call("GET", "/pubsub/topics")["topics"] for x in t["subscriptions"]}

    # ---- Bigtable subscriptions: default row key and a delimited key from JSON fields.
    orders = f"{P}/topics/orders"
    pub.create_topic(request={"name": orders})
    bt, btk = f"{P}/subscriptions/orders-bigtable", f"{P}/subscriptions/orders-bigtable-keyed"
    table = "projects/acme-stg/instances/ops/tables/orders"
    sub.create_subscription(request={"name": bt, "topic": orders, "bigtable_config": {"table": table, "write_metadata": True}})
    sub.create_subscription(request={"name": btk, "topic": orders, "bigtable_config": {"table": table + "-by-customer",
        "column_family_mapping": {"delimited_key": {"key_fields": ["customer", "order"], "delimiter": b"#"}}}})
    mid = plain.publish(orders, b'{"customer": "C-77", "order": "ORD-5001", "total": 18.5}', channel="web").result(10)
    rows = wait(lambda: s.call("GET", f"/pubsub/bigtable?table={table}")["rows"], "a Bigtable row")
    r = rows[0]
    assert r["rowKey"].startswith(bt + "#") and r["rowKey"].endswith("#" + mid) and r["cells"]["data"][""].startswith('{"customer"'), r
    assert r["cells"]["pubsub_metadata"]["message_id"] == mid and json.loads(r["cells"]["pubsub_metadata"]["attributes"]) == {"channel": "web"}, r
    keyed = wait(lambda: s.call("GET", f"/pubsub/bigtable?table={table}-by-customer")["rows"], "a keyed Bigtable row")
    assert keyed[0]["rowKey"] == "C-77#ORD-5001" and "pubsub_metadata" not in keyed[0]["cells"], keyed
    assert subs_view()[bt]["type"] == "bigtable"
    print("ok   Bigtable subscription: row key subscription#salt#id, data and pubsub_metadata families; delimited key")

    # ---- Import topics: MSK from Streams' Kafka, Cloud Storage from a storage container, Kinesis fed by the tool.
    for i in range(2):
        s.call("POST", "/streams/topics/partner-orders/produce", {"key": f"C-{i}", "value": {"order": f"ORD-60{i}"}, "headers": {"source": "partner"}, "partition": 0})
    msk = f"{P}/topics/orders-from-msk"
    t = pub.create_topic(request={"name": msk, "ingestion_data_source_settings": {"aws_msk": {"cluster_arn": "arn:aws:kafka:us-east-1:111122223333:cluster/acme/1", "topic": "partner-orders",
        "aws_role_arn": "arn:aws:iam::111122223333:role/pubsub", "gcp_service_account": "ingest@acme-stg.iam.gserviceaccount.com"}}})
    assert t.ingestion_data_source_settings.aws_msk.state == 1, t
    sub.create_subscription(request={"name": f"{msk}-app".replace("/topics/", "/subscriptions/"), "topic": msk})
    got = wait(lambda: pull_ack(f"{msk}-app".replace("/topics/", "/subscriptions/")) or None, "MSK records ingested")
    assert sorted(json.loads(m.message.data)["order"] for m in got) == ["ORD-600", "ORD-601"] and {m.message.ordering_key for m in got} == {"C-0", "C-1"} and got[0].message.attributes["source"] == "partner", got
    s.call("POST", "/streams/topics/partner-orders/produce", {"key": "C-2", "value": {"order": "ORD-602"}, "partition": 0})
    got = wait(lambda: pull_ack(f"{msk}-app".replace("/topics/", "/subscriptions/")) or None, "a later MSK record")
    assert [json.loads(m.message.data)["order"] for m in got] == ["ORD-602"], got
    gone = f"{P}/topics/orders-from-missing"
    assert pub.create_topic(request={"name": gone, "ingestion_data_source_settings": {"confluent_cloud": {"bootstrap_server": "pkc-1.us-east-1.aws.confluent.cloud:9092", "cluster_id": "lkc-1", "topic": "nope", "identity_pool_id": "pool-1"}}}).ingestion_data_source_settings.confluent_cloud.state == 6
    s.call("POST", "/services/s3/accounts", {"account": "acme-stg-inbound", "containers": ["acme-stg-inbound-files"]})
    gcs_t = f"{P}/topics/orders-from-files"
    pub.create_topic(request={"name": gcs_t, "ingestion_data_source_settings": {"cloud_storage": {"bucket": "acme-stg-inbound-files", "text_format": {"delimiter": "\n"}, "match_glob": "**.txt"}}})
    sub.create_subscription(request={"name": f"{P}/subscriptions/orders-from-files-app", "topic": gcs_t})
    s.call("PUT", "/storage/acme-stg-inbound/acme-stg-inbound-files/objects/daily/2026-11-15.txt", {"content": "ORD-7001 placed\nORD-7002 placed\n"})
    s.call("PUT", "/storage/acme-stg-inbound/acme-stg-inbound-files/objects/daily/ignored.csv", {"content": "ORD-7003 placed\n"})
    got = wait(lambda: pull_ack(f"{P}/subscriptions/orders-from-files-app") or None, "Cloud Storage lines ingested")
    assert sorted(m.message.data for m in got) == [b"ORD-7001 placed", b"ORD-7002 placed"], got
    kin = f"{P}/topics/orders-from-kinesis"
    pub.create_topic(request={"name": kin, "ingestion_data_source_settings": {"aws_kinesis": {"stream_arn": "arn:aws:kinesis:us-east-1:111122223333:stream/orders",
        "consumer_arn": "arn:aws:kinesis:us-east-1:111122223333:stream/orders/consumer/pubsub:1", "aws_role_arn": "arn:aws:iam::111122223333:role/pubsub", "gcp_service_account": "ingest@acme-stg.iam.gserviceaccount.com"}}})
    sub.create_subscription(request={"name": f"{P}/subscriptions/orders-from-kinesis-app", "topic": kin})
    r = s.call("POST", "/pubsub/topics/orders-from-kinesis/ingest", {"records": [{"data": {"order": "ORD-8001"}, "partition_key": "C-9"}]})
    assert r["source"]["kind"] == "aws_kinesis" and r["source"]["state"] == "ACTIVE" and len(r["messageIds"]) == 1, r
    m = pull_ack(f"{P}/subscriptions/orders-from-kinesis-app")[0].message
    assert json.loads(m.data) == {"order": "ORD-8001"} and m.ordering_key == "C-9", m
    view = {t["name"]: t for t in s.call("GET", "/pubsub/topics")["topics"]}
    assert view[msk]["ingestion"]["state"] == "ACTIVE" and view[gcs_t]["ingestion"]["from"].startswith("gs://acme-stg-inbound-files"), view[msk]
    print("ok   import topics: MSK from the Kafka topic (key → ordering key, headers), Cloud Storage lines (glob), Kinesis via pubsub_ingest; states")

    # ---- JavaScript UDF transforms: on the topic (redact, drop TEST) and the subscription (stamp, fail).
    udf_t = f"{P}/topics/orders-redacted"
    try:
        pub.create_topic(request={"name": udf_t, "message_transforms": [{"javascript_udf": {"function_name": "redact", "code": "function redact(m { return m }"}}]})
        raise AssertionError("JavaScript that does not compile is refused")
    except gx.InvalidArgument:
        pass
    pub.create_topic(request={"name": udf_t, "message_transforms": [{"javascript_udf": {"function_name": "redact", "code": UDF_TOPIC}}]})
    udf_s = f"{P}/subscriptions/orders-redacted-app"
    sub.create_subscription(request={"name": udf_s, "topic": udf_t, "message_transforms": [{"javascript_udf": {"function_name": "stamp", "code": UDF_SUB}}],
                                     "retry_policy": {"minimum_backoff": duration_pb2.Duration(seconds=10), "maximum_backoff": duration_pb2.Duration(seconds=20)}})
    ids = [plain.publish(udf_t, json.dumps(d).encode()).result(10) for d in ({"order": "ORD-9001", "email": "jane@example.com"}, {"order": "ORD-9002", "status": "TEST"}, {"order": "BROKEN"})]
    assert all(ids) and len(set(ids)) == 3, "a filtered message still gets an id"
    got = wait(lambda: pull_ack(udf_s) or None, "transformed messages")
    assert [json.loads(m.message.data) for m in got] == [{"order": "ORD-9001"}] and dict(got[0].message.attributes) == {"redacted": "true", "seen_by": "stamp"}, got
    err = wait(lambda: subs_view()[udf_s]["lastError"], "the subscription transform's error")
    assert "cannot read" in err["error"], err
    try:
        pub.create_topic(request={"name": f"{P}/topics/ai", "message_transforms": [{"ai_inference": {"endpoint": "projects/acme-stg/locations/us-central1/endpoints/1"}}]})
        raise AssertionError("AI inference is refused")
    except gx.InvalidArgument:
        pass
    print("ok   transforms: topic UDF redacts and filters (id kept), subscription UDF stamps; a throwing UDF nacks with lastError")

    # ---- Expiration on the environment clock.
    idle = f"{P}/subscriptions/orders-idle"
    sub.create_subscription(request={"name": idle, "topic": orders, "expiration_policy": {"ttl": duration_pb2.Duration(seconds=86400)}})
    never = f"{P}/subscriptions/orders-forever"
    sub.create_subscription(request={"name": never, "topic": orders, "expiration_policy": {}})
    s.call("POST", "/clock/advance", {"by": "2d"})
    try:
        wait(lambda: idle not in subs_view(), "the idle subscription expiring")
    finally:
        s.call("POST", "/clock/advance", {"reset": True})
    try:
        sub.get_subscription(request={"subscription": idle})
        raise AssertionError("expired")
    except gx.NotFound:
        pass
    assert sub.get_subscription(request={"subscription": never}).name == never and subs_view()[never]["expiresAt"] is None
    print("ok   expiration policy: idle a day past its ttl on the environment clock → deleted; ttl unset → never")

    # ---- BigQuery table schema: use_table_schema, drop_unknown_fields; a row that does not fit is dead-lettered.
    s.call("PUT", "/pubsub/bigquery/tables/acme-stg.sales.orders", {"schema": [{"name": "order", "type": "STRING", "mode": "REQUIRED"}, {"name": "total", "type": "FLOAT"}]})
    dlq = f"{P}/topics/orders-bq-dlq"
    pub.create_topic(request={"name": dlq})
    bqs = f"{P}/subscriptions/orders-bq-table"
    sub.create_subscription(request={"name": bqs, "topic": orders, "bigquery_config": {"table": "acme-stg.sales.orders", "use_table_schema": True, "drop_unknown_fields": True},
        "dead_letter_policy": {"dead_letter_topic": dlq, "max_delivery_attempts": 5}, "retry_policy": {"minimum_backoff": duration_pb2.Duration(nanos=100_000_000), "maximum_backoff": duration_pb2.Duration(nanos=200_000_000)}})
    plain.publish(orders, b'{"order": "ORD-5100", "total": 7.25, "coupon": "FALL"}').result(10)
    plain.publish(orders, b'{"total": "a lot"}').result(10)
    rows = wait(lambda: s.call("GET", "/pubsub/bigquery?table=acme-stg.sales.orders")["rows"], "the fitting row")
    assert [x["row"] for x in rows] == [{"order": "ORD-5100", "total": 7.25}], rows
    dead = wait(lambda: s.call("GET", "/pubsub/messages?topic=orders-bq-dlq")["messages"], "the misfit row dead-lettered", 30)
    assert dead[0]["data"]["value"] == {"total": "a lot"} and "order" in subs_view()[bqs]["lastError"]["error"], dead
    print("ok   BigQuery table schema: columns mapped, unknown dropped, a misfit retried then dead-lettered")

    # ---- Cloud Storage Avro files with use_topic_schema: typed records.
    typed = f"{P}/topics/shipments-typed"
    sub.create_subscription(request={"name": f"{P}/subscriptions/shipments-typed-avro", "topic": typed, "cloud_storage_config": {"bucket": "acme-stg-typed-archive",
        "max_messages": 1, "avro_config": {"use_topic_schema": True, "write_metadata": True}}})
    plain.publish(typed, json.dumps({"shipment": "SHP-3010", "status": "CREATED", "weight_kg": 2.0}).encode()).result(10)
    f = wait(lambda: s.call("GET", "/pubsub/storage?bucket=acme-stg-typed-archive")["files"], "a typed Avro file")[0]
    rec = list(fastavro.reader(io.BytesIO(base64.b64decode(f["content"]))))[0]
    assert rec["data"]["shipment"] == "SHP-3010" and rec["data"]["status"] == "CREATED" and rec["data"]["weight_kg"] == 2.0 and rec["message_id"], rec
    print("ok   Cloud Storage Avro with use_topic_schema: data is a typed record (read by fastavro)")

    # ---- Proto2: groups and required fields; unknown fields accepted.
    sc = SchemaServiceClient()
    p2 = 'syntax = "proto2";\nmessage Manifest {\n  required string shipment = 1;\n  repeated group Stop = 2 { required string city = 3; optional int32 parcels = 4; }\n}'
    sc.create_schema(request={"parent": P, "schema_id": "manifest-proto2", "schema": {"type_": Schema.Type.PROTOCOL_BUFFER, "definition": p2}})
    ok = b"\x0a\x05SHP-9\x13\x1a\x0bSpringfield\x20\x02\x14\x48\x07"
    sc.validate_message(request={"parent": P, "name": f"{P}/schemas/manifest-proto2", "message": ok, "encoding": Encoding.BINARY})
    try:
        sc.validate_message(request={"parent": P, "name": f"{P}/schemas/manifest-proto2", "message": b"\x13\x1a\x01x\x14", "encoding": Encoding.BINARY})
        raise AssertionError("required shipment is missing")
    except gx.InvalidArgument:
        pass
    print("ok   proto2: a repeated group and an unknown field accepted; a missing required field refused")

    # ---- Retry policy paces nacked pull messages.
    paced = f"{P}/subscriptions/orders-paced"
    sub.create_subscription(request={"name": paced, "topic": orders, "retry_policy": {"minimum_backoff": duration_pb2.Duration(seconds=2), "maximum_backoff": duration_pb2.Duration(seconds=4)}})
    plain.publish(orders, b'{"order": "ORD-5200"}').result(10)
    m = pull(paced)[0]
    sub.modify_ack_deadline(request={"subscription": paced, "ack_ids": [m.ack_id], "ack_deadline_seconds": 0})
    t0 = time.time()
    assert not pull(paced), "the nack waits its minimum backoff"
    again = wait(lambda: pull(paced), "the redelivery after the backoff", 10)
    assert again[0].message.message_id == m.message.message_id and time.time() - t0 >= 1.8, time.time() - t0
    sub.acknowledge(request={"subscription": paced, "ack_ids": [again[0].ack_id]})
    print(f"ok   retry policy: a nacked message came back after {time.time() - t0:.1f}s (minimum 2 s)")

    # ---- Exactly-once transient failures: the client retries until the ack lands.
    eo = f"{P}/subscriptions/orders-exactly-once"
    sub.create_subscription(request={"name": eo, "topic": orders, "enable_exactly_once_delivery": True})
    plain.publish(orders, b'{"order": "ORD-5300"}').result(10)
    s.call("PUT", "/pubsub/config", {"provoke": "ack_transient", "times": 3})
    results = []

    def on(m):
        results.append(m.ack_with_response().result(timeout=60))
    fut = sub.subscribe(eo, on)
    try:
        wait(lambda: results, "the ack after transient failures", 60)
    finally:
        fut.cancel()
        try:
            fut.result(5)
        except Exception:
            pass
    assert results[0] == AcknowledgeStatus.SUCCESS and s.call("GET", "/pubsub/topics")["provoke"] is None, results
    assert s.call("GET", "/pubsub/messages?subscription=orders-exactly-once")["subscription"]["acked"] == 1
    print("ok   exactly-once: 3 TRANSIENT_FAILURE_UNORDERED_ACK_ID answers, the client retried, SUCCESS")

    # ---- max_outstanding_bytes on a raw StreamingPull.
    fc_t = f"{P}/topics/flow"
    pub.create_topic(request={"name": fc_t})
    fc = f"{P}/subscriptions/flow-bytes"
    sub.create_subscription(request={"name": fc, "topic": fc_t})
    for i in range(3):
        plain.publish(fc_t, f"PAYLOAD-{i}-".encode() + b"x" * 50).result(10)
    q = queue.Queue()
    q.put(types.StreamingPullRequest(subscription=fc, stream_ack_deadline_seconds=60, max_outstanding_bytes=100))

    def reqs():
        while True:
            r = q.get()
            if r is None:
                return
            yield r
    stream = sub.api.streaming_pull(requests=reqs())
    first = next(stream)
    assert len(first.received_messages) == 1, first
    nxt = []
    th = threading.Thread(target=lambda: nxt.append(next(stream)), daemon=True)
    th.start()
    th.join(1.5)
    assert not nxt, "60 bytes out of 100: the next 60 waits"
    q.put(types.StreamingPullRequest(ack_ids=[first.received_messages[0].ack_id]))
    th.join(10)
    assert nxt and len(nxt[0].received_messages) == 1, nxt
    q.put(None)
    stream.cancel()
    print("ok   StreamingPull max_outstanding_bytes: one 60-byte message at a time under 100 bytes")


def urllib_get(url):
    import urllib.request
    with urllib.request.urlopen(url, timeout=10) as r:
        return r.read()


def pubsub_v1_publisher():
    from google.cloud import pubsub_v1
    return pubsub_v1.PublisherClient()


if __name__ == "__main__":
    main()
