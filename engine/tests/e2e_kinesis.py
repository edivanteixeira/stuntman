"""End-to-end check of Kinesis Data Streams (A21) with boto3 pointed at Stuntman's endpoint: streams
in provisioned and on-demand mode, puts by partition key (MD5 into the shard ranges) and every
iterator type, GetRecords paging with MillisBehindLatest, PutRecords with a provoked partial failure,
a provoked ProvisionedThroughputExceededException the SDK retries (and one it gives up on),
not-found, expired and KMS errors, resharding (split, merge, shard count, the provoked one) with
child shards, retention on the environment clock, tags, enhanced fan-out consumers with
SubscribeToShard, the CBOR protocol of the Java SDK, the tools. Last: a Pub/Sub import topic reading
a Kinesis stream (partition key → ordering key), ACTIVE and STREAM_NOT_FOUND.

    pip install boto3 cbor2 google-cloud-pubsub
    cargo build -p stuntman && python3 tests/e2e_kinesis.py
"""
import base64, datetime, http.client, json, os, threading, time

from harness import Stuntman, wait


def main():
    import boto3
    from botocore.config import Config
    from botocore.exceptions import ClientError
    with Stuntman("stuntman-kinesis-") as s:
        url = f"http://127.0.0.1:{s.KINESIS_PORT}"
        mk = lambda **cfg: boto3.client("kinesis", endpoint_url=url, region_name="us-east-1", aws_access_key_id="AKIAEXAMPLE", aws_secret_access_key="any",
                                        config=Config(**cfg) if cfg else None)
        k = mk(retries={"mode": "standard", "max_attempts": 4})

        def err(f, **kw):
            try:
                f(**kw)
            except ClientError as e:
                return e.response["Error"]["Code"]
            raise AssertionError(f"{f.__name__} did not fail")

        # ---- Streams.
        k.create_stream(StreamName="shipments", ShardCount=2)
        k.create_stream(StreamName="positions", StreamModeDetails={"StreamMode": "ON_DEMAND"})
        k.get_waiter("stream_exists").wait(StreamName="shipments", WaiterConfig={"Delay": 1, "MaxAttempts": 5})
        assert err(k.create_stream, StreamName="shipments", ShardCount=1) == "ResourceInUseException"
        d = k.describe_stream(StreamName="shipments")["StreamDescription"]
        assert d["StreamStatus"] == "ACTIVE" and len(d["Shards"]) == 2 and d["StreamARN"] == "arn:aws:kinesis:us-east-1:000000000000:stream/shipments", d
        assert d["Shards"][1]["HashKeyRange"]["StartingHashKey"] == str(2 ** 127) and d["Shards"][1]["HashKeyRange"]["EndingHashKey"] == str(2 ** 128 - 1), d["Shards"]
        sm = k.describe_stream_summary(StreamARN=d["StreamARN"])["StreamDescriptionSummary"]
        assert sm["OpenShardCount"] == 2 and sm["StreamModeDetails"]["StreamMode"] == "PROVISIONED", sm
        assert k.describe_stream_summary(StreamName="positions")["StreamDescriptionSummary"]["OpenShardCount"] == 4
        assert k.list_streams()["StreamNames"] == ["positions", "shipments"]
        assert len(k.list_shards(StreamName="positions", MaxResults=3)["Shards"]) == 3
        assert err(k.describe_stream, StreamName="nope") == "ResourceNotFoundException"
        print("ok   streams: provisioned and on-demand, describe, summary, list, shards with their hash key ranges")

        # ---- Puts and iterators.
        import hashlib
        h = int(hashlib.md5(b"SHP-1042").hexdigest(), 16)
        want = "shardId-000000000001" if h >= 2 ** 127 else "shardId-000000000000"
        t0 = datetime.datetime.now(datetime.timezone.utc)
        puts = [k.put_record(StreamName="shipments", PartitionKey="SHP-1042", Data=json.dumps({"shipment": "SHP-1042", "status": st}).encode()) for st in ["CREATED", "IN_TRANSIT", "DELIVERED"]]
        assert all(p["ShardId"] == want for p in puts), (puts, want)
        assert puts[0]["SequenceNumber"] < puts[1]["SequenceNumber"] < puts[2]["SequenceNumber"]
        it = k.get_shard_iterator(StreamName="shipments", ShardId=want, ShardIteratorType="TRIM_HORIZON")["ShardIterator"]
        r = k.get_records(ShardIterator=it, Limit=1)
        assert json.loads(r["Records"][0]["Data"])["status"] == "CREATED" and r["Records"][0]["PartitionKey"] == "SHP-1042" and r["MillisBehindLatest"] >= 0, r
        assert isinstance(r["Records"][0]["ApproximateArrivalTimestamp"], datetime.datetime)
        r = k.get_records(ShardIterator=r["NextShardIterator"])
        assert [json.loads(x["Data"])["status"] for x in r["Records"]] == ["IN_TRANSIT", "DELIVERED"] and r["MillisBehindLatest"] == 0, r
        r = k.get_records(ShardIterator=r["NextShardIterator"])
        assert r["Records"] == [] and r["NextShardIterator"], r
        at = lambda typ, **kw: k.get_records(ShardIterator=k.get_shard_iterator(StreamName="shipments", ShardId=want, ShardIteratorType=typ, **kw)["ShardIterator"])["Records"]
        assert at("AT_SEQUENCE_NUMBER", StartingSequenceNumber=puts[1]["SequenceNumber"])[0]["SequenceNumber"] == puts[1]["SequenceNumber"]
        assert at("AFTER_SEQUENCE_NUMBER", StartingSequenceNumber=puts[1]["SequenceNumber"])[0]["SequenceNumber"] == puts[2]["SequenceNumber"]
        assert len(at("AT_TIMESTAMP", Timestamp=t0 - datetime.timedelta(seconds=5))) == 3
        latest = k.get_shard_iterator(StreamName="shipments", ShardId=want, ShardIteratorType="LATEST")["ShardIterator"]
        assert k.get_records(ShardIterator=latest)["Records"] == []
        k.put_record(StreamName="shipments", PartitionKey="SHP-1042", Data=b"after latest")
        assert [x["Data"] for x in k.get_records(ShardIterator=latest)["Records"]] == [b"after latest"]
        # ExplicitHashKey picks the shard.
        assert k.put_record(StreamName="shipments", PartitionKey="x", ExplicitHashKey="0", Data=b"low")["ShardId"] == "shardId-000000000000"
        assert err(k.get_records, ShardIterator="bm9wZQ==") == "InvalidArgumentException"
        print("ok   puts: MD5 of the partition key picks the shard; TRIM_HORIZON, AT/AFTER_SEQUENCE_NUMBER, AT_TIMESTAMP, LATEST; paging and MillisBehindLatest")

        # ---- PutRecords, and its provoked partial failure.
        recs = [{"PartitionKey": f"SHP-{2000 + i}", "Data": f'{{"shipment": "SHP-{2000 + i}"}}'.encode()} for i in range(4)]
        r = k.put_records(StreamName="shipments", Records=recs)
        assert r["FailedRecordCount"] == 0 and all("SequenceNumber" in x for x in r["Records"]), r
        s.call("PUT", "/kinesis/config", {"provoke": "partial_failures", "times": 1, "stream": "shipments"})
        r = k.put_records(StreamName="shipments", Records=recs)
        assert r["FailedRecordCount"] == 2 and r["Records"][1]["ErrorCode"] == "ProvisionedThroughputExceededException" and "SequenceNumber" in r["Records"][0], r
        retry = [recs[i] for i, x in enumerate(r["Records"]) if "ErrorCode" in x]
        assert k.put_records(StreamName="shipments", Records=retry)["FailedRecordCount"] == 0
        print("ok   PutRecords: 4 in one call; provoked partial failure (every second record), the failed ones put again")

        # ---- Throughput: the SDK retries 2 provoked errors; with fewer retries it gives up.
        calls = []
        k.meta.events.register("before-send.kinesis.PutRecord", lambda **kw: calls.append(1))
        s.call("PUT", "/kinesis/config", {"provoke": "throughput_puts", "times": 2})
        p = k.put_record(StreamName="shipments", PartitionKey="SHP-3000", Data=b"retried")
        assert len(calls) == 3 and p["SequenceNumber"], calls
        assert s.call("GET", "/kinesis/streams")["provoke"] is None
        stingy = mk(retries={"mode": "standard", "max_attempts": 2})
        s.call("PUT", "/kinesis/config", {"provoke": "throughput_gets", "times": 0})
        it = stingy.get_shard_iterator(StreamName="shipments", ShardId=want, ShardIteratorType="TRIM_HORIZON")["ShardIterator"]
        assert err(stingy.get_records, ShardIterator=it) == "ProvisionedThroughputExceededException"
        for kind, code in [("not_found", "ResourceNotFoundException"), ("expired_iterator", "ExpiredIteratorException"), ("kms_throttling", "KMSThrottlingException")]:
            s.call("PUT", "/kinesis/config", {"provoke": kind, "stream": "shipments"})
            assert err(stingy.get_records, ShardIterator=it) == code, kind
        assert err(stingy.put_record, StreamName="shipments", PartitionKey="a", Data=b"x") == "KMSThrottlingException"
        assert stingy.put_record(StreamName="positions", PartitionKey="a", Data=b"x")["SequenceNumber"]  # only shipments is provoked
        s.call("PUT", "/kinesis/config", {"provoke": "slow_puts", "delay_ms": 1200, "times": 1})
        t = time.time(); k.put_record(StreamName="shipments", PartitionKey="SHP-3001", Data=b"slow"); assert time.time() - t >= 1.1
        s.call("PUT", "/kinesis/config", {"provoke": "none"})
        print("ok   provoked: throughput retried by the SDK (3 attempts), given up with 2; not found, expired iterator, KMS throttling (one stream only); slow put")

        # ---- Resharding: split, read the parent to its end, merge, shard count, the provoked one.
        sh0 = k.describe_stream(StreamName="shipments")["StreamDescription"]["Shards"][0]
        k.split_shard(StreamName="shipments", ShardToSplit=sh0["ShardId"], NewStartingHashKey=str(2 ** 126))
        shards = k.list_shards(StreamName="shipments")["Shards"]
        kids = [x for x in shards if x.get("ParentShardId") == sh0["ShardId"]]
        assert len(kids) == 2 and kids[0]["HashKeyRange"]["EndingHashKey"] == str(2 ** 126 - 1) and kids[1]["HashKeyRange"]["StartingHashKey"] == str(2 ** 126), kids
        assert "EndingSequenceNumber" in [x for x in shards if x["ShardId"] == sh0["ShardId"]][0]["SequenceNumberRange"]
        it = k.get_shard_iterator(StreamName="shipments", ShardId=sh0["ShardId"], ShardIteratorType="TRIM_HORIZON")["ShardIterator"]
        r = k.get_records(ShardIterator=it)
        assert "NextShardIterator" not in r and sorted(c["ShardId"] for c in r["ChildShards"]) == sorted(x["ShardId"] for x in kids), r
        assert k.put_record(StreamName="shipments", PartitionKey="x", ExplicitHashKey="0", Data=b"to a child")["ShardId"] == kids[0]["ShardId"]
        assert err(k.split_shard, StreamName="shipments", ShardToSplit=sh0["ShardId"], NewStartingHashKey="5") == "ResourceInUseException"
        k.merge_shards(StreamName="shipments", ShardToMerge=kids[0]["ShardId"], AdjacentShardToMerge=kids[1]["ShardId"])
        merged = [x for x in k.list_shards(StreamName="shipments", ShardFilter={"Type": "AT_LATEST"})["Shards"] if x.get("AdjacentParentShardId") == kids[1]["ShardId"]]
        assert merged and merged[0]["ParentShardId"] == kids[0]["ShardId"], merged
        u = k.update_shard_count(StreamName="shipments", TargetShardCount=3, ScalingType="UNIFORM_SCALING")
        assert u["CurrentShardCount"] == 2 and u["TargetShardCount"] == 3
        assert k.describe_stream_summary(StreamName="shipments")["StreamDescriptionSummary"]["OpenShardCount"] == 3
        r = s.call("PUT", "/kinesis/config", {"provoke": "reshard", "stream": "shipments"})
        assert r["split"] and k.describe_stream_summary(StreamName="shipments")["StreamDescriptionSummary"]["OpenShardCount"] == 4, r
        print("ok   resharding: split (parent closes, ChildShards at its end, new puts go to children), merge, UpdateShardCount, provoked reshard")

        # ---- Tags, retention on the environment clock.
        k.add_tags_to_stream(StreamName="shipments", Tags={"team": "logistics", "env": "stg"})
        assert {t["Key"]: t["Value"] for t in k.list_tags_for_stream(StreamName="shipments")["Tags"]} == {"team": "logistics", "env": "stg"}
        k.increase_stream_retention_period(StreamName="shipments", RetentionPeriodHours=48)
        assert err(k.increase_stream_retention_period, StreamName="shipments", RetentionPeriodHours=30) == "InvalidArgumentException"
        assert k.describe_stream(StreamName="shipments")["StreamDescription"]["RetentionPeriodHours"] == 48
        print("ok   tags; retention increased to 48 h")

        # ---- The tools: records as people read them, a put as another producer, waiting for the app's.
        view = {x["name"]: x for x in s.call("GET", "/kinesis/streams")["streams"]}
        assert view["shipments"]["records"] > 10 and len(view["shipments"]["shards"]) >= 8 and view["shipments"]["tags"]["team"] == "logistics", view["shipments"]
        rows = s.call("GET", "/kinesis/streams/shipments/records?limit=5")["records"]
        assert rows[0]["producer"] == "app" and rows[0]["sequenceNumber"].startswith("49"), rows
        p = s.call("POST", "/kinesis/streams/shipments/records", {"data": {"shipment": "SHP-4000", "status": "DELIVERED"}, "partition_key": "SHP-4000"})
        it = k.get_shard_iterator(StreamName="shipments", ShardId=p["shardId"], ShardIteratorType="AT_SEQUENCE_NUMBER", StartingSequenceNumber=p["sequenceNumber"])["ShardIterator"]
        assert json.loads(k.get_records(ShardIterator=it)["Records"][0]["Data"])["shipment"] == "SHP-4000"
        threading.Timer(0.5, lambda: k.put_record(StreamName="shipments", PartitionKey="SHP-4001", Data=b'{"shipment": "SHP-4001"}')).start()
        w = s.call("GET", "/kinesis/streams/shipments/wait?contains=SHP-4001&timeout=10s")
        assert w["partitionKey"] == "SHP-4001" and w["data"]["value"]["shipment"] == "SHP-4001", w
        s.call("PUT", "/kinesis/streams/invoices", {"shards": 1})
        assert "invoices" in k.list_streams()["StreamNames"]
        print("ok   tools: list_kinesis_streams, list_kinesis_records, kinesis_put (read by the SDK), wait_for_kinesis_record, create_kinesis_stream")

        # ---- Enhanced fan-out: a consumer and SubscribeToShard.
        arn = k.describe_stream_summary(StreamName="invoices")["StreamDescriptionSummary"]["StreamARN"]
        c = k.register_stream_consumer(StreamARN=arn, ConsumerName="billing")["Consumer"]
        assert c["ConsumerStatus"] == "ACTIVE" and c["ConsumerARN"].startswith(arn + "/consumer/billing:"), c
        assert [x["ConsumerName"] for x in k.list_stream_consumers(StreamARN=arn)["Consumers"]] == ["billing"]
        assert k.describe_stream_consumer(ConsumerARN=c["ConsumerARN"])["ConsumerDescription"]["StreamARN"] == arn
        k.put_record(StreamName="invoices", PartitionKey="INV-1", Data=b'{"invoice": "INV-1"}')
        threading.Timer(0.8, lambda: k.put_record(StreamName="invoices", PartitionKey="INV-2", Data=b'{"invoice": "INV-2"}')).start()
        sub = k.subscribe_to_shard(ConsumerARN=c["ConsumerARN"], ShardId="shardId-000000000000", StartingPosition={"Type": "TRIM_HORIZON"})
        got = []
        for ev in sub["EventStream"]:
            if "SubscribeToShardEvent" in ev:
                e = ev["SubscribeToShardEvent"]
                got += [json.loads(x["Data"])["invoice"] for x in e["Records"]]
                assert e["ContinuationSequenceNumber"].startswith("49")
            if len(got) >= 2:
                break
        assert got == ["INV-1", "INV-2"], got
        assert err(k.delete_stream, StreamName="invoices") == "ResourceInUseException"
        k.deregister_stream_consumer(ConsumerARN=c["ConsumerARN"])
        assert k.list_stream_consumers(StreamARN=arn)["Consumers"] == []
        print("ok   enhanced fan-out: RegisterStreamConsumer, List, Describe, SubscribeToShard event stream (records as they arrive), Deregister")

        # ---- CBOR, as the Java SDK speaks by default.
        import cbor2
        def raw(op, body):
            cn = http.client.HTTPConnection("127.0.0.1", s.KINESIS_PORT, timeout=10)
            cn.request("POST", "/", cbor2.dumps(body), {"X-Amz-Target": f"Kinesis_20131202.{op}", "Content-Type": "application/x-amz-cbor-1.1",
                                                        "Authorization": "AWS4-HMAC-SHA256 Credential=AKIA/20261115/eu-west-1/kinesis/aws4_request, SignedHeaders=host, Signature=00"})
            r = cn.getresponse()
            return r.status, r.getheader("content-type"), cbor2.loads(r.read())
        st, ct, v = raw("PutRecord", {"StreamName": "positions", "PartitionKey": "POS-1", "Data": b"\x00\x01binary"})
        assert st == 200 and ct == "application/x-amz-cbor-1.1" and v["SequenceNumber"].startswith("49"), (st, v)
        st, _, it = raw("GetShardIterator", {"StreamName": "positions", "ShardId": v["ShardId"], "ShardIteratorType": "AT_SEQUENCE_NUMBER", "StartingSequenceNumber": v["SequenceNumber"]})
        st, _, r = raw("GetRecords", {"ShardIterator": it["ShardIterator"]})
        rec = r["Records"][0]
        assert rec["Data"] == b"\x00\x01binary" and isinstance(rec["ApproximateArrivalTimestamp"], int) and rec["ApproximateArrivalTimestamp"] > 1_700_000_000_000, rec
        st, _, e = raw("DescribeStream", {"StreamName": "nope"})
        assert st == 400 and e["__type"] == "ResourceNotFoundException", e
        st, _, v = raw("CreateStream", {"StreamName": "eu-stream", "ShardCount": 1})
        assert k.describe_stream_summary(StreamName="eu-stream")["StreamDescriptionSummary"]["StreamARN"].startswith("arn:aws:kinesis:eu-west-1:")
        print("ok   CBOR (application/x-amz-cbor-1.1): byte-string data, millisecond timestamps, errors; region from the SigV4 scope")

        # ---- Pub/Sub import topic reading a Kinesis stream.
        os.environ["PUBSUB_EMULATOR_HOST"] = f"127.0.0.1:{s.PUBSUB_PORT}"
        from google.cloud import pubsub_v1
        pub, subc = pubsub_v1.PublisherClient(), pubsub_v1.SubscriberClient()
        k.create_stream(StreamName="orders", ShardCount=2)
        for i in range(3):
            k.put_record(StreamName="orders", PartitionKey=f"C-{i}", Data=json.dumps({"order": f"ORD-90{i}"}).encode())
        P = "projects/acme-stg"
        ing = lambda name: {"aws_kinesis": {"stream_arn": f"arn:aws:kinesis:us-east-1:111122223333:stream/{name}", "consumer_arn": f"arn:aws:kinesis:us-east-1:111122223333:stream/{name}/consumer/pubsub:1",
                                            "aws_role_arn": "arn:aws:iam::111122223333:role/pubsub", "gcp_service_account": "ingest@acme-stg.iam.gserviceaccount.com"}}
        subc.create_subscription(request={"name": f"{P}/subscriptions/orders-app", "topic": pub.create_topic(request={"name": f"{P}/topics/orders-from-kinesis", "ingestion_data_source_settings": ing("orders")}).name})
        pub.create_topic(request={"name": f"{P}/topics/from-missing", "ingestion_data_source_settings": ing("missing")})

        def pull():
            r = subc.pull(request={"subscription": f"{P}/subscriptions/orders-app", "max_messages": 10}, timeout=5)
            if r.received_messages:
                subc.acknowledge(request={"subscription": f"{P}/subscriptions/orders-app", "ack_ids": [m.ack_id for m in r.received_messages]})
            return list(r.received_messages)
        got = []
        wait(lambda: got.extend(pull()) or len(got) >= 3, "Kinesis records ingested", 20)
        assert sorted(json.loads(m.message.data)["order"] for m in got) == ["ORD-900", "ORD-901", "ORD-902"] and {m.message.ordering_key for m in got} == {"C-0", "C-1", "C-2"}, got
        k.put_record(StreamName="orders", PartitionKey="C-7", Data=b'{"order": "ORD-907"}')
        later = wait(lambda: pull() or None, "a later Kinesis record", 20)
        assert [json.loads(m.message.data)["order"] for m in later] == ["ORD-907"] and later[0].message.ordering_key == "C-7", later
        states = {t["name"].rsplit("/", 1)[1]: t["ingestion"]["state"] for t in s.call("GET", "/pubsub/topics")["topics"] if t.get("ingestion")}
        assert states["orders-from-kinesis"] == "ACTIVE" and states["from-missing"] == "STREAM_NOT_FOUND", states
        assert pub.get_topic(request={"topic": f"{P}/topics/from-missing"}).ingestion_data_source_settings.aws_kinesis.state == 4
        # pubsub_ingest still feeds an import topic.
        r = s.call("POST", "/pubsub/topics/from-missing/ingest", {"records": [{"data": {"order": "ORD-8001"}, "partition_key": "C-9"}]})
        assert len(r["messageIds"]) == 1, r
        print("ok   Pub/Sub import topic: reads the Kinesis stream from TRIM_HORIZON, partition key → ordering key, later records too; ACTIVE / STREAM_NOT_FOUND")

        # ---- Retention and iterator expiry follow the environment clock (last: it moves the clock).
        it = k.get_shard_iterator(StreamName="positions", ShardId="shardId-000000000000", ShardIteratorType="TRIM_HORIZON")["ShardIterator"]
        s.call("POST", "/clock/advance", {"by": "6m"})
        assert err(k.get_records, ShardIterator=it) == "ExpiredIteratorException"
        s.call("POST", "/clock/advance", {"by": "25h"})
        for sh in k.list_shards(StreamName="positions")["Shards"]:
            it = k.get_shard_iterator(StreamName="positions", ShardId=sh["ShardId"], ShardIteratorType="TRIM_HORIZON")["ShardIterator"]
            assert k.get_records(ShardIterator=it)["Records"] == [], sh
        assert any(k.get_records(ShardIterator=k.get_shard_iterator(StreamName="shipments", ShardId=x["ShardId"], ShardIteratorType="TRIM_HORIZON")["ShardIterator"])["Records"]
                   for x in k.list_shards(StreamName="shipments")["Shards"]), "48 h retention keeps shipments"
        s.call("POST", "/clock/advance", {"reset": True})
        k.delete_stream(StreamName="positions")
        assert "positions" not in k.list_streams()["StreamNames"]
        print("ok   clock: an iterator expires after 5 min, records after the retention (24 h gone, 48 h kept); DeleteStream")


if __name__ == "__main__":
    main()
    print("e2e_kinesis: all passed")
