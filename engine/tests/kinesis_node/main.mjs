// Kinesis through the AWS SDK for JavaScript v3: create, wait for ACTIVE, put, PutRecords, read.
//   node main.mjs http://127.0.0.1:4567
import assert from "node:assert/strict";
import { KinesisClient, CreateStreamCommand, DescribeStreamCommand, PutRecordCommand, PutRecordsCommand, GetShardIteratorCommand, GetRecordsCommand, waitUntilStreamExists } from "@aws-sdk/client-kinesis";

const k = new KinesisClient({ endpoint: process.argv[2], region: "us-east-1", credentials: { accessKeyId: "AKIAEXAMPLE", secretAccessKey: "any" } });
const StreamName = "node-orders";
const enc = (o) => new TextEncoder().encode(JSON.stringify(o));

await k.send(new CreateStreamCommand({ StreamName, ShardCount: 2 }));
await waitUntilStreamExists({ client: k, maxWaitTime: 20, minDelay: 1, maxDelay: 1 }, { StreamName });
const d = (await k.send(new DescribeStreamCommand({ StreamName }))).StreamDescription;
assert.equal(d.StreamStatus, "ACTIVE");
assert.equal(d.Shards.length, 2);
const p = await k.send(new PutRecordCommand({ StreamName, PartitionKey: "ORD-5001", Data: enc({ order: "ORD-5001", total: 42.5 }) }));
console.log("put", p.ShardId, p.SequenceNumber);
const pr = await k.send(new PutRecordsCommand({ StreamName, Records: [2, 3, 4].map((i) => ({ PartitionKey: "ORD-5001", Data: enc({ order: `ORD-500${i}` }) })) }));
assert.equal(pr.FailedRecordCount, 0);
assert.ok(pr.Records.every((r) => r.ShardId === p.ShardId), "same partition key, same shard");
const { ShardIterator } = await k.send(new GetShardIteratorCommand({ StreamName, ShardId: p.ShardId, ShardIteratorType: "TRIM_HORIZON" }));
const r = await k.send(new GetRecordsCommand({ ShardIterator }));
const got = r.Records.map((x) => JSON.parse(new TextDecoder().decode(x.Data)).order);
assert.deepEqual(got, ["ORD-5001", "ORD-5002", "ORD-5003", "ORD-5004"]);
assert.ok(r.Records[0].ApproximateArrivalTimestamp instanceof Date);
console.log("node ok", got.length, "records");
