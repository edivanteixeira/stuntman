"""End-to-end check of Pub/Sub with the official clients pointed at PUBSUB_EMULATOR_HOST: topics and
subscriptions made by google-cloud-pubsub, three messages with attributes and an ordering key,
received over StreamingPull and acked; Pull, ModifyAckDeadline and redelivery after the deadline;
a dead-letter topic; a provoked UNAVAILABLE the client retries; NOT_FOUND; the REST publish; what
the tools show. Then the rest of the API with the same clients: SchemaServiceClient (Avro and
Protocol Buffer schemas, revisions, ValidateMessage) and topics that validate what is published;
snapshots and Seek; UpdateTopic and UpdateSubscription; filters; push subscriptions to a local HTTP
receiver (wrapped and unwrapped, OIDC token, retry after a 500); BigQuery and Cloud Storage
subscriptions (checked through the tools); exactly-once ack errors; DetachSubscription; paging. The
.NET client (Google.Cloud.PubSub.V1) publishes too when dotnet is installed.

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


def urllib_get(url):
    import urllib.request
    with urllib.request.urlopen(url, timeout=10) as r:
        return r.read()


def pubsub_v1_publisher():
    from google.cloud import pubsub_v1
    return pubsub_v1.PublisherClient()


if __name__ == "__main__":
    main()
