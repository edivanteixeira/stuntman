# Streams (Kafka and Schema Registry)

Stuntman stands in for an Apache Kafka cluster and a Confluent Schema Registry. Your app produces and consumes with
its normal Kafka client (librdkafka / Confluent.Kafka, the Java client, kafka-python) against a one-node broker, and
reads Avro schemas from a registry that answers the Confluent REST API. Stuntman keeps the topics with every record,
the consumer groups with their committed offsets and lag, and the schema subjects with their versions. You can
produce records as a partner would, force a rebalance, and make the broker or the registry fail.

## At a glance

| | |
|---|---|
| **Stands in for** | Apache Kafka (one broker, non-flexible request versions only) and Confluent Schema Registry (REST, Avro). Tested with `confluent-kafka` (librdkafka) including `SchemaRegistryClient`, `AvroSerializer` and `AvroDeserializer`, and with `kafka-python` |
| **Ports** | `9092` Kafka, plaintext (`STUNTMAN_KAFKA_PORT`); `8081` Schema Registry, plain HTTP (`STUNTMAN_SCHEMA_REGISTRY_PORT`). No TLS variant |
| **Point your app at** | `bootstrap.servers=localhost:9092`; `schema.registry.url=http://localhost:8081` |
| **It keeps** | topics with their partitions and records (key, value, headers, timestamp, who produced it); committed offsets per group, topic and partition; schemas and subjects with versions. All in the environment's SQLite. Broker settings and the provocation in `streams/config.yaml` |
| **You can provoke** | `leader_not_available` (Metadata, Produce and Fetch answer `LEADER_NOT_AVAILABLE`) · `sasl_refused` (every SASL login fails) · `slow_fetch` (each Fetch waits 3 s) · `duplicate` (each produced record is stored twice) · `registry_down` (schema registration answers 503) · a consumer-group rebalance (`rebalance`) · a record with an unknown schema id (`produce` with `bad_schema`) |
| **Screen** | `/streams` (**Streams** in the menu) |
| **Tools** | `list_topics`, `create_topic`, `delete_topic`, `list_stream_messages`, `produce`, `list_consumer_groups`, `rebalance`, `set_streams`, `list_schemas`, `register_schema` |

## Connect your app

### Hosts and the advertised host

A Kafka client connects to `bootstrap.servers`, asks for Metadata, then connects to the host the broker
advertises. Stuntman advertises one broker, node id `0`, at `<advertised_host>:<Kafka port>`.

- `advertised_host` starts as `STUNTMAN_KAFKA_ADVERTISED_HOST`, else `localhost`.
- Change it per environment with `set_streams {"advertised_host": "stuntman"}`. It is saved in `streams/config.yaml`.
  Once that file holds a value (any `set_streams` call writes the whole file), the environment variable no longer
  applies to that environment.

| App runs | `bootstrap.servers` | `advertised_host` must be |
|---|---|---|
| On the host, Stuntman on the host or in Docker with the ports published | `localhost:9092` | `localhost` (the default) |
| In a container of the same compose project | `stuntman:9092` (the service name) | `stuntman` (set `STUNTMAN_KAFKA_ADVERTISED_HOST=stuntman` in compose, or `set_streams`) |
| In a container, Stuntman on the host | `host.docker.internal:9092` | `host.docker.internal` |

If the advertised host is wrong, the client connects to the bootstrap server, then fails or hangs on the second
connection. `list_topics` returns `bootstrap` (the advertised host and port) so you can check.

The Schema Registry needs no advertised host: use the same host name your app uses for the broker, port `8081`.

### Environments

The Kafka port and the registry port serve one environment: the default one. There is no `/e/<env>` prefix, no
host-name routing and no metadata header on these ports (see [Environments](../environments.md)). The tools and the
HTTP routes do work per environment (`/e/<env>/api/streams/...`), but the app's Kafka traffic always reaches the
default environment.

### Authentication

SASL is off by default: every client connects without credentials. Turn it on with `set_streams`:

```text
set_streams {"sasl_required": true, "sasl_users": [{"user": "acme-tms", "password": "s3cret"}]}
```

| Setting | Effect |
|---|---|
| `sasl_required: false` (default) | SASL is offered but not needed. A client that logs in anyway is checked as below |
| `sasl_required: true` | every request except ApiVersions, SaslHandshake and SaslAuthenticate needs a login first. A request before login closes the connection |
| `sasl_users` empty | PLAIN accepts any user name with any non-empty password except `wrong` |
| `sasl_users` set | only those users, with their passwords. A password may be a literal, `secret://<name>` or `env:<VAR>` |

Mechanisms offered: `PLAIN`, `SCRAM-SHA-256`, `SCRAM-SHA-512` (4096 iterations). Use
`security.protocol=SASL_PLAINTEXT`; there is no `SASL_SSL`.

For SCRAM, list the user in `sasl_users`. Without users, Stuntman computes its server signature from an empty
password, so a client that checks the server's final message rejects the login.

A client that logs in with PLAIN as user `$ConnectionString` is an Azure Event Hubs Kafka client: see
[Event Hubs over Kafka](#event-hubs-over-kafka).

### librdkafka / confluent-kafka (Python)

```python
from confluent_kafka import Producer, Consumer
from confluent_kafka.schema_registry import SchemaRegistryClient
from confluent_kafka.schema_registry.avro import AvroSerializer

conf = {"bootstrap.servers": "localhost:9092"}
# With SASL:
conf |= {"security.protocol": "SASL_PLAINTEXT", "sasl.mechanism": "SCRAM-SHA-512",
         "sasl.username": "acme-tms", "sasl.password": "s3cret"}

producer = Producer(conf)
consumer = Consumer(conf | {"group.id": "acme-tms", "auto.offset.reset": "earliest"})
registry = SchemaRegistryClient({"url": "http://localhost:8081"})
```

### kafka-python

```python
from kafka import KafkaProducer, KafkaConsumer
p = KafkaProducer(bootstrap_servers="localhost:9092")
c = KafkaConsumer("invoices", bootstrap_servers="localhost:9092", group_id="acme-reports", auto_offset_reset="earliest")
```

### Java, .NET (Confluent.Kafka) and others

Set the same properties: `bootstrap.servers`, and for SASL `security.protocol=SASL_PLAINTEXT`, `sasl.mechanism`
and the JAAS config or user name and password. Turn compression off or use gzip (see below).

## What it supports

### Kafka API

The broker answers these API keys and versions (what ApiVersions offers). Every offered version is a non-flexible
one. A request with an API key not in this table closes the connection.

| API (key) | Versions | Notes |
|---|---|---|
| ApiVersions (18) | 0–2 | v3 and later get error 35 (`UNSUPPORTED_VERSION`) in the v0 shape; clients then fall back |
| Metadata (3) | 1–8 | one broker (node `0`, rack none), cluster id `stuntman-kafka`, controller `0`. Every partition has leader `0`, replicas `[0]`, ISR `[0]`. Asking for a topic that does not exist creates it when `auto_create` is on and the request allows it (v4+ `allow_auto_topic_creation`); otherwise error 3 (`UNKNOWN_TOPIC_OR_PARTITION`). An empty or null topic list lists every topic |
| Produce (0) | 3–8 | record batches (magic 2) with no compression or gzip. `acks=0` gets no answer. The transactional id and the timeout are read and ignored. Answers the base offset of each batch; log append time `-1` |
| Fetch (1) | 4–11 | long poll: waits up to `max_wait_ms`, at most 5 s, for data on any asked partition. Returns one uncompressed batch per partition, at most 500 records, cut at the partition's `max_bytes` and the request's `max_bytes`. `isolation_level` is read and ignored. Answers high watermark, last stable offset (= high watermark) and log start offset |
| ListOffsets (2) | 1–5 | timestamp `-1` (latest) answers the high watermark; `-2` (earliest) the log start; any other timestamp the first offset whose record timestamp is at or after it, else the high watermark |
| FindCoordinator (10) | 0–2 | always node `0` at the advertised host |
| JoinGroup (11) | 0–5 | see [Consumer groups](#consumer-groups) |
| SyncGroup (14) | 0–3 | the leader's assignments are handed to each member |
| Heartbeat (12) | 0–3 | `REBALANCE_IN_PROGRESS` (27) while a rebalance runs, `ILLEGAL_GENERATION` (22), `UNKNOWN_MEMBER_ID` (25) |
| LeaveGroup (13) | 0–3 | v3 takes a list of members |
| OffsetCommit (8) | 2–7 | stores the offset and metadata. Generation and member id are not checked |
| OffsetFetch (9) | 1–5 | a null topic list returns every committed offset of the group. A partition with no commit answers `-1` |
| InitProducerId (22) | 0–1 | answers a producer id and epoch 0, so idempotent producers start. Nothing is deduplicated |
| CreateTopics (19) | 0–4 | `num_partitions` ≤ 0 uses `default_partitions`. Replication factor, replica assignments and configs are read and ignored. An existing topic answers error 36 (`TOPIC_ALREADY_EXISTS`) |
| SaslHandshake (17) | 0–1 | an unknown mechanism answers error 33 (`UNSUPPORTED_SASL_MECHANISM`) with the list |
| SaslAuthenticate (36) | 0–1 | PLAIN in one step, SCRAM in two. A failure answers error 58 (`SASL_AUTHENTICATION_FAILED`). v1 returns a session lifetime of 1 hour |

### Records

| Feature | Behaviour |
|---|---|
| Key, value, headers | kept as bytes, as sent. Null keys and null values are kept |
| Timestamp | the record's own timestamp; when it is 0 or missing, the time Stuntman stored it (wall clock) |
| Compression | none and gzip are read. snappy, lz4 and zstd answer error 76 (`UNSUPPORTED_COMPRESSION_TYPE`) with the text `compression snappy is not supported (use none or gzip)` |
| A batch Stuntman cannot read | error 2 (`CORRUPT_MESSAGE`) |
| Offsets | per partition, from 0, one after another. The base offset of each produced batch is the next free offset |
| Retention | none: records stay until you delete the topic. The log start offset stays 0 |
| Who produced it | the client id of the connection, or `you` for the `produce` tool. `list_stream_messages` shows it as `producer` |

### Consumer groups

| Step | Behaviour |
|---|---|
| JoinGroup | the first join (or a join while the group is stable) starts a rebalance round. Members get 3.5 s to join again, 0.3 s when the joiner is alone. When every member has joined (or the time is up) the round closes: members that did not join are removed, the generation goes up, the first member (by id) is the leader. Only the leader gets the member list with their protocol metadata |
| Member ids | an empty member id gets `<client id>-<uuid>` |
| Protocol | the first protocol of the first member. Assignors run in the client, as in Kafka |
| SyncGroup | the leader's assignments end the rebalance (state `Stable`). Other members wait up to 10 s for them, then get `REBALANCE_IN_PROGRESS` |
| Session timeout | a member with no heartbeat for `max(session.timeout.ms, 6 s)` is removed and a rebalance starts. Checked every 2 s |
| `rebalance` tool | the group goes to `PreparingRebalance`; members get `REBALANCE_IN_PROGRESS` (27) on their next heartbeat and join again |
| Committed offsets | in SQLite, kept across restarts. Lag = high watermark − committed offset, per partition |
| Group membership | in memory: after a restart, groups are empty until members join again |

States shown: `Empty`, `PreparingRebalance`, `CompletingRebalance`, `Stable`.

### Schema Registry (REST on `:8081`)

Responses use `content-type: application/vnd.schemaregistry.v1+json`. Errors are `{"error_code", "message"}`.

| Route | Behaviour |
|---|---|
| `GET /` | `{}` |
| `GET /subjects` | the subject names that have a version not deleted |
| `GET /subjects/{subject}/versions` | the version numbers; unknown subject: 404 `40401` |
| `POST /subjects/{subject}/versions` | registers `{"schema", "schemaType"}` (`AVRO` by default; `JSON`, `PROTOBUF` accepted). Answers `{"id"}`. The same schema text (JSON whitespace ignored) keeps its id across subjects; registering it again under the same subject returns the existing version. An Avro schema that is not JSON (and not a quoted primitive such as `"string"`) answers 422 `42201` `Invalid schema`. `references` are ignored |
| `GET /subjects/{subject}/versions/{version}` | `{"subject", "version", "id", "schema", "schemaType"}`; `latest` and `-1` work. Unknown version: 404 `40402`; unknown subject: 404 `40401` |
| `GET /subjects/{subject}/versions/{version}/schema` | the schema text only |
| `POST /subjects/{subject}` | looks a schema up in a subject: `{"subject", "version", "id", "schema"}`, or 404 `40403` `Schema not found` |
| `DELETE /subjects/{subject}` | soft delete: answers the versions deleted. The schema ids stay readable, so consumers can still decode old records. Registering again continues the version numbers |
| `GET /schemas/ids/{id}` | `{"schema"}` (plus `schemaType` when not Avro); unknown id: 404 `40403` |
| `GET /schemas/ids/{id}/versions` | `[{"subject", "version"}]` |
| `POST /compatibility/subjects/{subject}/versions/{version}` | always `{"is_compatible": true}` |
| `GET /config`, `GET /config/{subject}` | always `{"compatibilityLevel": "BACKWARD"}` |
| `PUT /config`, `PUT /config/{subject}` | echoes `{"compatibility"}`; nothing is stored |

### Avro in Stuntman

Stuntman has its own Avro codec for the tools. It reads and writes primitives, records, enums, arrays, maps, unions
(a JSON value or the Avro JSON wrapper `{"string": "x"}`) and fixed. Logical types are read as their base type.
Values use Confluent framing: byte `0`, the schema id (4 bytes, big-endian), the Avro body.

### Event Hubs over Kafka

A Kafka client configured for Azure Event Hubs reaches Stuntman's [Event Hubs](eventhubs.md) through this port:
SASL PLAIN, user `$ConnectionString`, the namespace's connection string as the password. After that login, the
client sees the namespace's hubs as topics (same partitions; Event Hubs sequence numbers are the Kafka offsets; the
partition key is the record key; properties are headers). Consumer groups and their offsets are Streams'. A
connection string that names no namespace, or has a wrong key, fails the login. A throttled send answers error 89
(`THROTTLING_QUOTA_EXCEEDED`). ListOffsets by timestamp answers the end of the partition.

## How it behaves

- **Topic creation.** A topic is made when a client asks for one that does not exist (Metadata or Produce),
  unless `auto_create` is off. It gets `default_partitions` (1 unless changed). `create_topic`, `produce` and
  CreateTopics also make topics. The partition count never changes.
- **Partitioning.** Clients pick the partition themselves; the broker stores the batch where the client sent it.
  The `produce` tool picks its own: an explicit `partition`, else a 31-multiplier hash of the key's bytes modulo the
  partition count, else partition 0. This is not the partitioner your client uses, so pass `partition` when a test
  needs a record on a given partition.
- **Unknown partition.** Produce to a partition index ≥ the count answers error 3.
- **Fetch errors.** An offset past the high watermark answers error 1 (`OFFSET_OUT_OF_RANGE`); the client then
  resets by its `auto.offset.reset`. An unknown topic answers error 3.
- **Ordering.** Per partition, by offset, as produced.
- **Clock.** Record timestamps and lag use the wall clock. [`advance_clock`](../clock.md) does not affect Streams.
- **Persistence.** Topics, records, committed offsets and schemas survive a restart. Group membership does not.
- **What differs from Kafka.** One broker, no replication, no retention or compaction, no transactions, no ACLs,
  no quotas, no topic configs. The Schema Registry checks no compatibility.

## Provoke failures

| Failure | How | What the app sees | Turn it off |
|---|---|---|---|
| Leader not available | `set_streams {"provoke": "leader_not_available"}` | Metadata lists each partition with error 5 and leader `-1`; Produce and Fetch answer error 5. Clients retry, then time out (`message.timeout.ms`) | `set_streams {"provoke": "none"}` |
| SASL refused | `set_streams {"provoke": "sasl_refused"}` | every SaslAuthenticate answers error 58 `Authentication failed: Invalid username or password (provoked in Stuntman)` | `provoke: none` |
| Wrong password | log in with password `wrong` (no users listed) or a wrong one (users listed) | error 58 | use the right password |
| Slow fetch | `set_streams {"provoke": "slow_fetch"}` | each Fetch answers after 3 s more | `provoke: none` |
| Duplicates | `set_streams {"provoke": "duplicate"}` | each produced batch is stored twice: consumers read every record twice, at consecutive offsets | `provoke: none` |
| Registry down | `set_streams {"provoke": "registry_down"}` | `POST /subjects/{s}/versions` answers 503 `50003`. Reads (`GET`) still work, so a serializer with a cached schema id keeps working | `provoke: none` |
| Rebalance | `rebalance {"group": "acme-tms"}` | members get `REBALANCE_IN_PROGRESS` on the next heartbeat, the client revokes and reassigns partitions, the generation goes up | ends by itself when members join again |
| A record the schema rejects | `produce {"topic": "shipments", "bad_schema": true}` | the value starts with byte 0 and schema id 99999, which is not in the registry, so the deserializer fails | none needed |
| Member dies | stop sending heartbeats (kill the consumer) | after the session timeout, the others rebalance | — |

Only one provocation is on at a time. It lasts until you clear it; there is no `times` count.

## See what the app did

| Need | Tool | Route | Fields that matter |
|---|---|---|---|
| Topics, record counts, how to connect | `list_topics` | `GET /api/streams/topics` | `topics[].name`, `partitions`, `messages`, `offsets[].start`, `offsets[].highWatermark`; `bootstrap`, `schemaRegistry`, `sasl`, `provoke` |
| Records of a topic | `list_stream_messages {"topic": "shipments", "partition": 0, "limit": 50}` | `GET /api/streams/topics/{topic}/messages` | `partition`, `offset`, `at`, `key`, `headers` (`[name, hex]` pairs), `producer`, `valueBytes`, `value` |
| Groups and lag | `list_consumer_groups` | `GET /api/streams/groups` | `group`, `state`, `generation`, `members[].client`, `members[].host`, `members[].leader`, `lag`, `partitions[].committed`, `partitions[].highWatermark` |
| Schemas | `list_schemas` | `GET /api/streams/schemas` | `subjects[].subject`, `versions`, `latestId` |

`value` in `list_stream_messages` is decoded:

| Value | `value` |
|---|---|
| Confluent-framed, Avro schema in the registry | `{"schemaId", "format": "avro", "value": {...}}` |
| Confluent-framed, does not match the schema | `{"schemaId", "format": "avro", "error": "does not match schema …", "hex"}` |
| Confluent-framed, JSON or Protobuf schema | `{"schemaId", "format": "json"/"protobuf", "value"}` (the bytes after the header as JSON, else text) |
| Confluent-framed, unknown id | `{"schemaId", "error": "schema N is not in the registry", "hex"}` |
| JSON | `{"format": "json", "value"}` |
| UTF-8 text | `{"format": "text", "value"}` |
| Anything else | `{"format": "binary", "hex"}` |

There is no `wait_for_*` tool for Streams: poll `list_stream_messages` or `list_consumer_groups`. Streams calls are
counted in `usage` (kind `streams`), and the client id appears among the live clients. They are not written to the
[Timeline](../timeline.md).

## Recipes

### 1. Consume what the partner produces (Avro)

1. Register the partner's schema:
   ```text
   register_schema {"subject": "shipments-value", "schema": "{\"type\": \"record\", \"name\": \"Shipment\", \"fields\": [{\"name\": \"id\", \"type\": \"string\"}, {\"name\": \"status\", \"type\": \"string\"}]}"}
   ```
2. Produce as the partner:
   ```text
   produce {"topic": "shipments", "key": "SH-1042", "value": {"id": "SH-1042", "status": "DELIVERED"}, "subject": "shipments-value", "headers": {"source": "globex"}}
   ```
3. Start the app's consumer (group `acme-tms`).
4. Check it consumed and committed: `list_consumer_groups`, group `acme-tms`, `lag` is `0`.

### 2. Check what the app produced

1. Run the app's action.
2. `list_stream_messages {"topic": "invoices", "limit": 10}`.
3. Assert on `key`, `headers` and `value.value` (decoded Avro or JSON). `producer` is the app's client id.

### 3. Make the consumer survive a rebalance

1. Start the consumer and let it reach `Stable` (`list_consumer_groups`).
2. Note `generation`.
3. `rebalance {"group": "acme-tms"}`.
4. Produce a record (`produce`) and check the consumer still processes it.
5. `list_consumer_groups`: `generation` is higher, `state` is `Stable`, `lag` is `0`.

### 4. Make the producer handle a broker outage

1. `set_streams {"provoke": "leader_not_available"}`.
2. Run the app's action. Its producer retries and then reports a delivery error after `message.timeout.ms`.
3. Check the app's error handling (retry queue, outbox, alert).
4. `set_streams {"provoke": "none"}` and check the app sends what it held.

### 5. Test an idempotent consumer

1. `set_streams {"provoke": "duplicate"}`.
2. Run the app's action (it produces) or `produce` yourself.
3. `list_stream_messages`: each record appears twice, at consecutive offsets.
4. Check the consumer applied the effect once.
5. `set_streams {"provoke": "none"}`.

### 6. Turn on SASL/SCRAM like production

1. `set_streams {"sasl_required": true, "sasl_users": [{"user": "acme-tms", "password": "secret://kafka-password"}]}`.
2. Configure the app with `security.protocol=SASL_PLAINTEXT`, `sasl.mechanism=SCRAM-SHA-512`, user and password.
3. Check a wrong password fails: the client logs `SASL authentication failed`.

### 7. A poison message

1. `produce {"topic": "shipments", "key": "SH-9999", "bad_schema": true}`.
2. Check the app's consumer skips it or sends it to its dead-letter topic, and keeps consuming.

## Errors and troubleshooting

| Symptom or error text | Cause | Fix |
|---|---|---|
| Client connects, then `Connection refused` / hangs on `localhost:9092` from a container | the broker advertises `localhost` | `set_streams {"advertised_host": "stuntman"}` or `STUNTMAN_KAFKA_ADVERTISED_HOST` |
| `UNSUPPORTED_COMPRESSION_TYPE` / `compression lz4 is not supported (use none or gzip)` | the producer compresses with snappy, lz4 or zstd | `compression.type=none` or `gzip` |
| Connection closed right after connect, SASL on | the client sent a request before logging in | configure `security.protocol=SASL_PLAINTEXT` and a mechanism |
| `SASL_AUTHENTICATION_FAILED` with SCRAM and no users listed | see [Authentication](#authentication) | add the user to `sasl_users` |
| `UNKNOWN_TOPIC_OR_PARTITION` | `auto_create` is off, or the client disabled auto creation | `create_topic` first |
| `TOPIC_ALREADY_EXISTS` from the admin client | the topic exists | ignore it, or `delete_topic` first |
| Admin `delete_topics`, `describe_configs`, `list_groups`, transactions: connection closed | that API key is not offered | use the tools (`delete_topic`, `list_consumer_groups`); avoid transactions |
| Consumer reads nothing after a restart of Stuntman | groups are in memory; offsets are kept | the consumer joins again by itself; check `auto.offset.reset` if it never committed |
| Kafka traffic of another environment shows nothing | the Kafka port serves only the default environment | run that app against the default environment |
| `40401 Subject 'x' not found` | the serializer looks up a subject that was never registered | `register_schema`, or let the serializer auto-register |
| Registry `503 50003` | `registry_down` is on | `set_streams {"provoke": "none"}` |

## Not done

- One broker; no replication, no ISR changes, no controller moves.
- Flexible (compact) request versions are not offered; clients fall back to older versions.
- snappy, lz4 and zstd compression.
- Transactions (AddPartitionsToTxn, EndTxn and the rest), exactly-once semantics, producer deduplication.
- DeleteTopics, CreatePartitions, DescribeConfigs, AlterConfigs, DescribeGroups, ListGroups, DeleteGroups,
  DeleteRecords, OffsetDelete and the ACL APIs: an unoffered API key closes the connection.
- Topic configs, retention and compaction.
- TLS (`SSL`, `SASL_SSL`), OAUTHBEARER, GSSAPI.
- Environment routing on the Kafka and registry ports.
- Schema Registry: compatibility checks (always compatible), stored compatibility config, schema references,
  deleting one version, permanent deletes, modes, contexts, `/schemas/types`.
- Consumer group membership does not survive a restart.
- No `wait_for_*` tool and no Timeline rows for Streams.

Missing something? Tell the developer what the fix would be and offer a pull request — see [Contributing](../../CONTRIBUTING.md).

## Tools and routes

<!-- tools: streams -->
_Generated by `tools/gen_tool_reference.py` from the engine source; each tool is an MCP tool and the HTTP route shown (under `/api`)._

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_topics` | `GET /api/streams/topics` | — | — | The Kafka topics of this environment with their partitions, messages and high watermarks; how to connect. |
| `create_topic` | `PUT /api/streams/topics/{name}` | name | partitions | Creates a topic with N partitions. |
| `delete_topic` | `DELETE /api/streams/topics/{name}` | name | — | Deletes a topic and its messages. |
| `list_stream_messages` | `GET /api/streams/topics/{topic}/messages` | topic | partition, limit | The newest messages of a topic: partition, offset, key, headers and the value decoded (Avro with its registry schema, JSON or text). |
| `produce` | `POST /api/streams/topics/{topic}/produce` | topic | key, value, headers, partition, subject, schema_id, bad_schema | Produces a message as the partner would: key, value (JSON or text), headers, partition; with subject (or schema_id) the value is Avro-encoded with that registry schema in Confluent framing. bad_schema=true sends one the schema rejects (an unknown schema id). |
| `list_consumer_groups` | `GET /api/streams/groups` | — | — | Consumer groups: state, generation, members (client, host, assigned partitions) and the lag of each partition (committed offset vs high watermark). |
| `rebalance` | `POST /api/streams/groups/{group}/rebalance` | group | — | Starts a rebalance of a consumer group: members get REBALANCE_IN_PROGRESS on their next heartbeat and must join again. |
| `set_streams` | `PUT /api/streams/config` | — | default_partitions, auto_create, advertised_host, sasl_required, sasl_users, provoke | Broker settings: default_partitions, auto_create, advertised_host, sasl_required, sasl_users [{user, password}], and a provocation: leader_not_available, sasl_refused, slow_fetch, duplicate, registry_down (none clears). |
| `list_schemas` | `GET /api/streams/schemas` | — | — | The Schema Registry's subjects with their versions and ids. |
| `register_schema` | `POST /api/streams/schemas` | subject, schema | schema_type | Registers a schema under a subject (Avro by default; JSON or PROTOBUF as schema_type). The same schema keeps its id. |
<!-- /tools -->
