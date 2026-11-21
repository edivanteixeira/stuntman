# Kinesis

Stuntman stands in for Amazon Kinesis Data Streams. Your app's AWS SDK points its Kinesis endpoint at Stuntman and
creates streams, puts records, reads them with shard iterators or enhanced fan-out, and reshards, with any
credentials. Stuntman keeps the streams, their shards (with parent and child shards after resharding) and every
record, enforces the real per-shard throughput limits, and lets you make puts and reads fail.

## At a glance

| | |
|---|---|
| **Stands in for** | Amazon Kinesis Data Streams, API version `Kinesis_20131202`: AWS JSON 1.1 and the CBOR (`application/x-amz-cbor-1.1`) the AWS SDK for Java sends by default. Tested with boto3, the AWS SDK for Java v2 (`KinesisClient` with `endpointOverride`, CBOR) and the AWS SDK for JavaScript v3 (`@aws-sdk/client-kinesis`) |
| **Ports** | `4567` plain HTTP, HTTP/1.1 and HTTP/2 (`STUNTMAN_KINESIS_PORT`). No TLS variant |
| **Point your app at** | `AWS_ENDPOINT_URL_KINESIS=http://localhost:4567` (or the client's `endpoint_url` / `endpoint` / `endpointOverride` / `ServiceURL`), any access key and secret, any region |
| **It keeps** | streams (status, mode, retention, tags, encryption type and key id, shard-level metrics), shards (hash key range, sequence number range, parent and adjacent parent), every record (partition key, shard, sequence number, arrival time, who put it), enhanced fan-out consumers. In the environment's SQLite. The provocation, the limits switch and the transition time in `kinesis/config.yaml` |
| **You can provoke** | `ProvisionedThroughputExceededException` on puts or gets · `ResourceNotFoundException` · `ExpiredIteratorException` · `KMSThrottlingException` / `KMSDisabledException` (encrypted streams) · slow puts · PutRecords partial failures · a reshard now · the real throughput limits (on by default) |
| **Screen** | `/kinesis` (**Kinesis** in the menu) |
| **Tools** | `list_kinesis_streams`, `create_kinesis_stream`, `list_kinesis_records`, `kinesis_put`, `wait_for_kinesis_record`, `set_kinesis` |

## Connect your app

### Hosts

| App runs | Endpoint |
|---|---|
| On the host, Stuntman on the host or in Docker with the port published | `http://localhost:4567` |
| In a container of the same compose project | `http://stuntman:4567` (the service name) |
| In a container, Stuntman on the host | `http://host.docker.internal:4567` |

### Credentials and region

SigV4 is accepted with any key; the signature is not checked. The region comes from the SigV4 credential scope
(`us-east-1` when there is none) and goes into the stream ARN: `arn:aws:kinesis:<region>:000000000000:stream/<name>`.
The account is always `000000000000`.

### SDK snippets

```python
import boto3
k = boto3.client("kinesis", endpoint_url="http://localhost:4567", region_name="us-east-1",
                 aws_access_key_id="AKIAEXAMPLE", aws_secret_access_key="any")
```

```java
KinesisClient k = KinesisClient.builder()
    .endpointOverride(URI.create("http://localhost:4567"))
    .region(Region.US_EAST_1)
    .credentialsProvider(StaticCredentialsProvider.create(AwsBasicCredentials.create("AKIAEXAMPLE", "any")))
    .build();
```

```js
import { KinesisClient } from "@aws-sdk/client-kinesis";
const k = new KinesisClient({ endpoint: "http://localhost:4567", region: "us-east-1",
  credentials: { accessKeyId: "AKIAEXAMPLE", secretAccessKey: "any" } });
```

With SDKs that read `AWS_ENDPOINT_URL_KINESIS` (or `AWS_ENDPOINT_URL`), set it and change no code. The Java SDK's
default CBOR protocol works; you do not need `aws.cborEnabled=false`.

### Environments

The port routes like Stuntman's HTTP ports: a `/e/<env>` path prefix (`http://localhost:4567/e/stg-b`) or a host
name of that environment picks it; otherwise the host's environment, which is the default one for `localhost`. A
paused environment answers 503 `ServiceUnavailable`. See [Environments](../environments.md).

## What it supports

Every operation is a `POST` with `X-Amz-Target: Kinesis_20131202.<Operation>`. An unknown target answers
`UnknownOperationException`. A body that is not JSON (or CBOR) answers `SerializationException`; bodies over 12 MiB
answer `SerializationException` `body too large`. A stream may be named by `StreamName` or `StreamARN`.

### Streams

| Operation | Notes |
|---|---|
| CreateStream | `ShardCount` 1–10,000 for provisioned mode; `StreamModeDetails.StreamMode=ON_DEMAND` gives 4 shards. With neither, the stream is on-demand. Shards split the hash key space evenly. Name: `[a-zA-Z0-9_.-]+`, at most 128 characters. `Tags` kept. An existing name answers `ResourceInUseException` |
| DeleteStream | refused with `ResourceInUseException` while consumers are registered, unless `EnforceConsumerDeletion` |
| DescribeStream | `StreamStatus`, `StreamModeDetails`, `RetentionPeriodHours`, `StreamCreationTimestamp`, `EnhancedMonitoring`, `EncryptionType`, `KeyId`; `Shards` paged with `Limit` (default 100) and `ExclusiveStartShardId`, `HasMoreShards` |
| DescribeStreamSummary | the same without shards, plus `OpenShardCount` and `ConsumerCount` |
| ListStreams | `Limit` (default 100), `ExclusiveStartStreamName` or `NextToken`; `StreamNames`, `StreamSummaries`, `HasMoreStreams` |
| ListShards | `MaxResults` (default 1000), `NextToken`, `ExclusiveStartShardId`, `ShardFilter` types `AT_LATEST` (open shards), `AFTER_SHARD_ID`, `AT_TIMESTAMP`, `FROM_TIMESTAMP`, `AT_TRIM_HORIZON` (all) |
| IncreaseStreamRetentionPeriod, DecreaseStreamRetentionPeriod | 24–8,760 hours; increase cannot shorten and decrease cannot lengthen (`InvalidArgumentException`, Kinesis's messages) |
| AddTagsToStream, RemoveTagsFromStream, ListTagsForStream | at most 50 tags |
| TagResource, UntagResource, ListTagsForResource | the same tags, by `ResourceARN` |
| UpdateStreamMode | `PROVISIONED` ↔ `ON_DEMAND`; the shards stay as they are; the same mode answers `InvalidArgumentException` |
| StartStreamEncryption, StopStreamEncryption | `EncryptionType` must be `KMS`; `KeyId` a key ID (UUID or `mrk-…`), key ARN, alias name (`alias/aws/kinesis`) or alias ARN. Records are not encrypted |
| EnableEnhancedMonitoring, DisableEnhancedMonitoring | `ShardLevelMetrics`: `ALL` or `IncomingBytes`, `IncomingRecords`, `OutgoingBytes`, `OutgoingRecords`, `WriteProvisionedThroughputExceeded`, `ReadProvisionedThroughputExceeded`, `IteratorAgeMilliseconds`. Kept and returned (`CurrentShardLevelMetrics`, `DesiredShardLevelMetrics`); no metric is emitted |
| DescribeLimits | `ShardLimit` 500, `OpenShardCount`, `OnDemandStreamCount`, `OnDemandStreamCountLimit` 50 |

### Records

| Operation | Notes |
|---|---|
| PutRecord | `PartitionKey` 1–256 characters; `Data` base64, at most 1 MiB; `ExplicitHashKey` a decimal in [0, 2^128−1]. Answers `ShardId`, `SequenceNumber`, `EncryptionType` |
| PutRecords | 1–500 records, at most 5 MiB in all (`InvalidArgumentException` `Records size exceeds 5 MB limit`). Each record answers `ShardId` and `SequenceNumber`, or `ErrorCode` and `ErrorMessage`; `FailedRecordCount` |
| GetShardIterator | `TRIM_HORIZON`, `LATEST`, `AT_SEQUENCE_NUMBER`, `AFTER_SEQUENCE_NUMBER` (`StartingSequenceNumber` from this stream), `AT_TIMESTAMP` (`Timestamp` in seconds) |
| GetRecords | `Limit` 1–10,000 (default 10,000), at most 10 MiB (always at least one record). Answers `Records` (`SequenceNumber`, `ApproximateArrivalTimestamp`, `Data`, `PartitionKey`, and `EncryptionType: KMS` on encrypted streams), `MillisBehindLatest`, `NextShardIterator`; on a closed shard read to its end, no `NextShardIterator` and `ChildShards` |

**Shard choice.** The MD5 of the partition key, read as a big-endian 128-bit number (or `ExplicitHashKey`), picks the
open shard whose hash key range holds it.

**Sequence numbers.** `49` followed by 54 digits, increasing across the whole stream. A sequence number that did not
come from this stream answers `InvalidArgumentException`.

**Iterators** expire 5 minutes after they were issued, on the environment clock: `ExpiredIteratorException`
`Iterator expired. The iterator was created at time … while right now it is …`.

### Resharding

| Operation | Notes |
|---|---|
| SplitShard | `ShardToSplit`, `NewStartingHashKey` strictly inside the shard's range. The parent closes; two children start after it |
| MergeShards | `ShardToMerge` and `AdjacentShardToMerge` must be open and adjacent. One child with `ParentShardId` and `AdjacentParentShardId` |
| UpdateShardCount | `TargetShardCount` 1–10,000, `UNIFORM_SCALING`: every open shard closes; the new ones split the key space evenly, each a child of the old shards it overlaps (the first two). Not for on-demand streams (`ValidationException`) |

A split or merge closes the parent at the stream's last sequence number. Reading the parent to its end gives no
`NextShardIterator` and lists its `ChildShards`; new records go to the children. A shard already split or merged
answers `ResourceInUseException`.

### Enhanced fan-out

| Operation | Notes |
|---|---|
| RegisterStreamConsumer | `ConsumerName`; ARN `<stream ARN>/consumer/<name>:<epoch seconds>`; status `ACTIVE` at once. A name in use answers `ResourceInUseException` |
| ListStreamConsumers, DescribeStreamConsumer | by stream, or by `ConsumerARN` |
| DeregisterStreamConsumer | by `ConsumerARN`, or stream + `ConsumerName` |
| SubscribeToShard | `ConsumerARN`, `ShardId`, `StartingPosition` (the iterator types). Answers an `application/vnd.amazon.eventstream` stream over HTTP/1.1 or HTTP/2: an `initial-response`, then `SubscribeToShardEvent`s (`Records`, `ContinuationSequenceNumber`, `MillisBehindLatest`, `ChildShards` when the shard ends). An event goes out when records arrive and at least every 5 s. The subscription ends after 5 minutes, or when a closed shard is read to its end |

## How it behaves

### Stream lifecycle

| After | Status | For |
|---|---|---|
| CreateStream | `CREATING` | `transition_ms` |
| SplitShard, MergeShards, UpdateShardCount, UpdateStreamMode, Start/StopStreamEncryption, the `reshard` provocation | `UPDATING` | `transition_ms` |
| DeleteStream | `DELETING`, then gone | `transition_ms` |

`transition_ms` is 1,000 ms by default, on the wall clock, so SDK waiters such as boto3's `stream_exists` and
`stream_not_exists` work. `set_kinesis {"transition_ms": 0}` makes every change take effect at once (maximum
600,000).

| Call | While `CREATING` or `DELETING` | While `UPDATING` |
|---|---|---|
| PutRecord, PutRecords, GetShardIterator, GetRecords, SubscribeToShard | `ResourceNotFoundException` | work |
| DeleteStream, resharding, mode, encryption, retention, tags, monitoring, RegisterStreamConsumer | `ResourceInUseException` `Stream … not ACTIVE, instead in state …` | `ResourceInUseException` |
| Describe*, List* | work | work |

Resharding takes effect at once: the new shards exist while the stream is `UPDATING`.

### Throughput limits

On by default for the app's calls, on the wall clock, counted in 1-second windows:

| Mode | Writes | Reads |
|---|---|---|
| Provisioned | per shard: 1,000 records/s, 1 MiB/s (data + partition key). The first record of a second always fits | per shard: 5 GetRecords/s, 2 MiB/s. A read over 2 MiB is paid back over the next seconds, so a 10 MiB GetRecords throttles the shard's reads for about 5 s |
| On-demand | per stream: 4,000 records/s, 4 MiB/s | per shard, as provisioned |

Over the limit, PutRecord and GetRecords answer `ProvisionedThroughputExceededException`
`Rate exceeded for shard <shard> in stream <stream> under account 000000000000.` (the SDKs retry). PutRecords fails
only the records over the limit. `set_kinesis {"limits": "off"}` turns the limits off. Your own tool calls never meet
the limits.

### Retention and the clock

Records older than the retention period (24 hours by default) are not read and are deleted on the next put. Arrival
times, retention and iterator expiry use the environment clock, so [`advance_clock`](../clock.md) ages records and
expires iterators. Transitions and throughput limits use the wall clock.

### Persistence

Streams, shards, records and consumers are in SQLite and survive a restart. The throughput counters are in memory.

### Errors

Errors carry `__type` and `message` in the body and the `x-amzn-errortype` header, in JSON or CBOR to match the
request. `InternalFailure` answers 500, `ServiceUnavailable` 503, the rest 400.

## Provoke failures

`set_kinesis` sets one provocation at a time. `times` limits it to the next N matching calls (0 = until cleared);
`stream` limits it to one stream; `none` clears it. Provocations and limits apply only to the app's calls, never to
the tools.

| Failure | How | What the app sees | Turn it off |
|---|---|---|---|
| Write throttling | `set_kinesis {"provoke": "throughput_puts", "times": 2, "stream": "shipments"}` | PutRecord and PutRecords answer `ProvisionedThroughputExceededException`; the SDKs retry | runs out, or `provoke: none` |
| Read throttling | `set_kinesis {"provoke": "throughput_gets"}` | GetRecords answers `ProvisionedThroughputExceededException` | `provoke: none` |
| Stream not found | `set_kinesis {"provoke": "not_found", "stream": "shipments"}` | every call naming the stream (except CreateStream and ListStreams) answers `ResourceNotFoundException` `Stream shipments under account 000000000000 not found.` | `provoke: none` |
| Expired iterator | `set_kinesis {"provoke": "expired_iterator", "times": 1}` | GetRecords answers `ExpiredIteratorException`; the consumer must get a new iterator | runs out |
| KMS throttling | `set_kinesis {"provoke": "kms_throttling"}` | puts and GetRecords on encrypted streams answer `KMSThrottlingException` | `provoke: none` |
| KMS key disabled | `set_kinesis {"provoke": "kms_disabled"}` | puts to encrypted streams answer `KMSDisabledException` | `provoke: none` |
| Slow puts | `set_kinesis {"provoke": "slow_puts", "delay_ms": 5000}` | each put waits (3,000 ms by default) | `provoke: none` |
| Partial failures | `set_kinesis {"provoke": "partial_failures"}` | PutRecords fails every second record (`ErrorCode` `ProvisionedThroughputExceededException`, `FailedRecordCount` > 0) | `provoke: none` |
| Reshard now | `set_kinesis {"provoke": "reshard", "stream": "shipments"}` | the widest open shard splits in two at its middle; the stream is `UPDATING`; consumers reading the parent reach its end and get `ChildShards` | — (one-off) |
| Real limits | on by default; burst over 1,000 records/s per shard | `ProvisionedThroughputExceededException` | `set_kinesis {"limits": "off"}` |
| Iterator really expires | `advance_clock {"by": "6m"}` | the next GetRecords with an old iterator answers `ExpiredIteratorException` | `advance_clock {"reset": true}` |

KMS provocations only reach encrypted streams; on other streams the call goes through and the provocation is not
counted down.

## See what the app did

| Need | Tool | Route | Fields that matter |
|---|---|---|---|
| Streams and shards | `list_kinesis_streams` | `GET /api/kinesis/streams` | `streams[].status`, `mode`, `encryption`, `keyId`, `metrics`, `retentionHours`, `records`, `openShards`, `shards[]` (`id`, `parent`, `adjacent`, `startingHashKey`, `endingHashKey`, `share` %, `startingSequenceNumber`, `endingSequenceNumber`, `open`, `records`), `consumers`, `tags`; `connect`, `limits`, `transitionMs`, `provoke` |
| Records | `list_kinesis_records {"stream": "shipments", "shard": "shardId-000000000000", "limit": 50, "since": 0}` | `GET /api/kinesis/streams/{stream}/records` | `shard`, `sequenceNumber`, `partitionKey`, `data` (`{format: json/text, value}` or `{format: binary, base64}`), `bytes`, `at`, `producer` (`app`, `you`, or the tool caller) |
| Wait for the app's record | `wait_for_kinesis_record {"stream": "shipments", "contains": "SHP-1042", "partition_key": "SHP-1042"}` | `GET /api/kinesis/streams/{stream}/wait` | the first record the app put (producer `app`) matching. Looks back 15 s on the environment clock (`since` in ms epoch), waits `timeout` (30 s), then 408 with `seen` |

Kinesis calls appear among the live clients (by the SDK's user agent) and count in `usage` (kind `kinesis`). They are
not written as [Timeline](../timeline.md) rows.

A [Pub/Sub](pubsub.md) import topic whose `aws_kinesis.stream_arn` names one of these streams reads it from
TRIM_HORIZON (partition key → ordering key).

## Recipes

### 1. Feed the app's consumer

1. `create_kinesis_stream {"name": "shipments", "shards": 2}` (or let the app create it).
2. `kinesis_put {"stream": "shipments", "data": {"shipment": "SHP-1042", "status": "DELIVERED"}, "partition_key": "SHP-1042"}`.
3. Check the app processed it (its own state, or its logs).

### 2. Assert what the app produced

1. Run the app's action.
2. `wait_for_kinesis_record {"stream": "shipments", "contains": "SHP-1042", "timeout": "20s"}`.
3. Check `partitionKey`, `shard` and `data.value`.

### 3. Make the producer handle throttling

1. `set_kinesis {"provoke": "throughput_puts", "times": 2, "stream": "shipments"}`.
2. Run the app's action. The SDK retries; the record lands once (`list_kinesis_records`).
3. For batches: `set_kinesis {"provoke": "partial_failures"}`; check the app re-sends only the failed records
   (`FailedRecordCount`), then `set_kinesis {"provoke": "none"}`.

### 4. Make the consumer follow a reshard

1. Start the app's consumer on `shipments`.
2. `set_kinesis {"provoke": "reshard", "stream": "shipments"}`.
3. `kinesis_put` a few records with different partition keys.
4. `list_kinesis_streams`: the parent is closed (`open: false`), two children are open; check the consumer read the
   records from the children.

### 5. Recover from an expired iterator

1. `set_kinesis {"provoke": "expired_iterator", "times": 1}`.
2. Check the consumer gets a new iterator (from its last sequence number) and keeps reading without gaps.

### 6. Fast tests

1. `set_kinesis {"limits": "off", "transition_ms": 0}` so streams are `ACTIVE` at once and bursts are not throttled.

### 7. Records older than the retention

1. `kinesis_put` a record.
2. `advance_clock {"by": "25h"}`.
3. A consumer starting at `TRIM_HORIZON` does not get it. `advance_clock {"reset": true}`.

## Errors and troubleshooting

| Symptom or error text | Cause | Fix |
|---|---|---|
| `UnknownOperationException Unknown X-Amz-Target …` | the client is not speaking `Kinesis_20131202`, or the call is not offered | check the endpoint is the Kinesis port |
| `ResourceNotFoundException` right after CreateStream | the stream is `CREATING` (1 s) | use the SDK's waiter, or `set_kinesis {"transition_ms": 0}` |
| `ResourceInUseException … not ACTIVE, instead in state UPDATING` | a change right after another | wait for `ACTIVE` |
| `ResourceInUseException … registered consumer(s)` on DeleteStream | consumers registered | deregister them, or pass `EnforceConsumerDeletion` |
| `ProvisionedThroughputExceededException` without any provocation | the real limits | spread the load over shards, or `set_kinesis {"limits": "off"}` |
| `ValidationException … partitionKey … length between 1 and 256` | empty or long partition key | send a key of 1–256 characters |
| `InvalidArgumentException StartingSequenceNumber … did not come from this stream` | a sequence number from another stream or another Stuntman | take it from this stream |
| `ExpiredIteratorException` | the iterator is over 5 minutes old (environment clock) | get a new iterator |
| `ServiceUnavailable … is paused in Stuntman` | the environment is paused | resume it |
| Calls reach the wrong environment | no `/e/<env>` prefix or host name | put the prefix in the endpoint |

## Not done

- On-demand streams do not scale with their traffic: the write limit stays at 4 MiB/s or 4,000 records/s, and a mode
  can be switched any number of times a day.
- The limits are counted in 1-second windows per shard, without the burst credit Kinesis sometimes allows.
- Records are not encrypted, and no KMS key is checked to exist.
- No CloudWatch metric is emitted.
- Resharding takes effect at once (the new shards exist while the stream is `UPDATING`).
- UpdateMaxRecordSize (10 MiB records), resource policies (`PutResourcePolicy` and the rest), consumers'
  `CREATING`/`DELETING` states and the account-level shard quota are not offered.
- The account is always `000000000000`; SigV4 signatures are not verified.
- No TLS endpoint.
- No Timeline rows for Kinesis.

Missing something? Tell the developer what the fix would be and offer a pull request — see [Contributing](../../CONTRIBUTING.md).

## Tools and routes

<!-- tools: kinesis -->
_Generated by `tools/gen_tool_reference.py` from the engine source; each tool is an MCP tool and the HTTP route shown (under `/api`)._

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_kinesis_streams` | `GET /api/kinesis/streams` | — | — | The Kinesis streams of this environment: status (CREATING, UPDATING, ACTIVE, DELETING), mode, encryption (KMS key), shard-level metrics, retention, shards (hash key range, sequence range, parent shards, open or closed, records), enhanced fan-out consumers, tags; how to connect; whether the throughput limits are enforced; the provocation. |
| `create_kinesis_stream` | `PUT /api/kinesis/streams/{name}` | name | shards, mode | Creates a stream: shards = N for provisioned mode (hash key space split evenly), or mode = on_demand (4 shards). It is CREATING for transition_ms (1 s), then ACTIVE. |
| `list_kinesis_records` | `GET /api/kinesis/streams/{stream}/records` | stream | shard, limit, since | The newest records of a stream (or one shard): shard, sequence number, partition key, data (JSON or text), who put it, when. |
| `kinesis_put` | `POST /api/kinesis/streams/{stream}/records` | stream, data | partition_key, explicit_hash_key | Puts a record as another producer would: data (JSON or text), partition_key (MD5 of it picks the shard), explicit_hash_key to choose the shard. The app's consumers read it. |
| `wait_for_kinesis_record` | `GET /api/kinesis/streams/{stream}/wait` | stream | contains, partition_key, since, timeout | Waits for the first record the app put (not you or an agent) on a stream that matches: text in the data, partition key. Looks back 15 s by default; 408 after the timeout. |
| `set_kinesis` | `PUT /api/kinesis/config` | — | provoke, times, stream, delay_ms, limits, transition_ms | The Kinesis endpoint's settings and provocations. limits = on \| off: the real throughput limits (per provisioned shard 1 MiB/s or 1,000 records/s in, 2 MiB/s and 5 GetRecords/s out; on-demand streams 4 MiB/s or 4,000 records/s in) answer ProvisionedThroughputExceededException; on by default. transition_ms: how long CREATING, UPDATING and DELETING last (1000; 0 = at once). provoke: throughput_puts, throughput_gets (ProvisionedThroughputExceededException), not_found (ResourceNotFoundException), expired_iterator, kms_throttling (KMSThrottlingException), kms_disabled (KMSDisabledException on puts; both KMS ones only on encrypted streams), slow_puts (delay_ms), partial_failures (PutRecords fails every second record); times = for the next N calls (0 = until cleared), stream = only that stream; none clears. reshard splits the stream's widest open shard now. |
<!-- /tools -->
