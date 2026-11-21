# Pub/Sub

Stuntman stands in for Google Cloud Pub/Sub the way the official emulator does: plaintext gRPC on one port, no
authentication, reached with `PUBSUB_EMULATOR_HOST`. It answers the `google.pubsub.v1` Publisher, Subscriber and
SchemaService APIs, so the official clients publish, pull, stream, ack and manage schemas unchanged. Beyond the
emulator, it delivers push, BigQuery, Cloud Storage and Bigtable subscriptions, runs JavaScript, AI inference and
compression transforms, reads import topics from other Stuntman services, and lets you make Publish and acks fail.
Stuntman keeps every message and each subscription's state of it.

## At a glance

| | |
|---|---|
| **Stands in for** | Google Cloud Pub/Sub, `google.pubsub.v1` (generated from the official googleapis protos). Tested with the Python client `google-cloud-pubsub` (`PublisherClient`, `SubscriberClient`, `SchemaServiceClient`) and the .NET client `Google.Cloud.PubSub.V1` (`PublisherClientBuilder` with `EmulatorDetection.EmulatorOrProduction`) |
| **Ports** | `8085` gRPC (HTTP/2, plaintext) and the REST publish (HTTP/1.1) on the same port (`STUNTMAN_PUBSUB_PORT`). No TLS variant |
| **Point your app at** | `PUBSUB_EMULATOR_HOST=localhost:8085`. Any project id. No credentials |
| **It keeps** | topics (labels, schema settings, retention, ingestion source and its state, transforms, storage policy, KMS key name); subscriptions of five kinds (pull, push, BigQuery, Cloud Storage, Bigtable) with all their settings; every message with data, attributes, ordering key, publish time and publisher; each subscription's delivery of it (pending, outstanding, acked, dead-lettered, attempts); schemas with revisions; snapshots; BigQuery and Bigtable table schemas; the rows and files those subscriptions wrote. In the environment's SQLite; the provocation and the default project in `pubsub/config.yaml` |
| **You can provoke** | Publish answers `UNAVAILABLE`, `DEADLINE_EXCEEDED`, `RESOURCE_EXHAUSTED`, `NOT_FOUND`, or `FAILED_PRECONDITION` (KMS key disabled) · slow Publish · every message delivered again after its first ack · exactly-once acks fail with a transient error · push endpoints, BigQuery rows, UDFs and AI calls that fail are retried and dead-lettered |
| **Screen** | `/pubsub` (**Pub/Sub** in the menu) |
| **Tools** | `list_pubsub_topics`, `list_pubsub_messages`, `pubsub_publish`, `create_pubsub_subscription`, `set_pubsub_topic`, `set_pubsub_schema`, `pubsub_ingest`, `wait_for_pubsub_message`, `set_pubsub`, `set_pubsub_bigquery_table`, `set_pubsub_bigtable_table`, `list_pubsub_bigquery_rows`, `list_pubsub_bigtable_rows`, `list_pubsub_storage_files` |

## Connect your app

### Host

| App runs | `PUBSUB_EMULATOR_HOST` |
|---|---|
| On the host, Stuntman on the host or in Docker with the port published | `localhost:8085` |
| In a container of the same compose project | `stuntman:8085` (the service name) |
| In a container, Stuntman on the host | `host.docker.internal:8085` |

### Clients

The official clients switch to plaintext and skip credentials when `PUBSUB_EMULATOR_HOST` is set.

```python
import os
os.environ["PUBSUB_EMULATOR_HOST"] = "localhost:8085"
from google.cloud import pubsub_v1
publisher = pubsub_v1.PublisherClient()
subscriber = pubsub_v1.SubscriberClient()
publisher.create_topic(name="projects/acme-stg/topics/shipments")
```

```csharp
// .NET: the builder only honours PUBSUB_EMULATOR_HOST with EmulatorDetection.
var publisher = await new PublisherClientBuilder
{
    TopicName = TopicName.FromProjectTopic("acme-stg", "shipments"),
    EmulatorDetection = EmulatorDetection.EmulatorOrProduction,
}.BuildAsync();
```

Go, Java and Node clients read `PUBSUB_EMULATOR_HOST` the same way.

### REST publish

`POST /v1/projects/{project}/topics/{topic}:publish` on the same port, body
`{"messages": [{"data": "<base64>", "attributes": {...}, "orderingKey": "..."}]}`, answers `{"messageIds": [...]}`.
Only `:publish` is offered over REST; other REST paths answer 404 `Only :publish is offered over REST; use gRPC for
the rest.`

### Environments

One port serves every environment. A request goes to the environment that the `x-stuntman-env` gRPC metadata (or,
for the REST publish, the `x-stuntman-env` header) names; without it, to the default environment. An unknown name
answers `NOT_FOUND` `No Stuntman environment <name>`. A paused environment answers `UNAVAILABLE`. See
[Environments](../environments.md).

### Names

Resources use full names: `projects/<project>/topics/<topic>`, `projects/<project>/subscriptions/<sub>`,
`projects/<project>/snapshots/<snap>`, `projects/<project>/schemas/<schema>`. Any other shape answers
`INVALID_ARGUMENT` `Invalid [topics] name: (name=...)`.

The tools also take short names (`shipments`): an existing topic or subscription of that name when exactly one
matches, else one in `project` (an argument) or the configured project (`set_pubsub {"project": "acme-stg"}`;
`acme-stg` by default).

## What it supports

### Publisher

| RPC | Notes |
|---|---|
| CreateTopic | validates the name, `kms_key_name` (`projects/*/locations/*/keyRings/*/cryptoKeys/*`), `message_storage_policy` (region names like `us-central1`; `enforce_in_transit` needs at least one region), `message_retention_duration` (10 min to 31 days), `schema_settings` (the schema and revisions must exist; encoding defaults to JSON), `message_transforms`, `ingestion_data_source_settings`. An existing topic answers `ALREADY_EXISTS` |
| GetTopic | the topic as stored |
| UpdateTopic | the `update_mask` must be set. Fields: `labels`, `message_storage_policy`, `kms_key_name`, `schema_settings`, `message_retention_duration`, `message_transforms`, `ingestion_data_source_settings`. `name` or any other field answers `INVALID_ARGUMENT` |
| Publish | see [Publishing](#publishing) |
| ListTopics | by project; pages |
| ListTopicSubscriptions | names of the topic's subscriptions that are not detached; pages |
| ListTopicSnapshots | names; pages |
| DeleteTopic | the subscriptions stay, with topic `_deleted-topic_`; they get nothing more |
| DetachSubscription | the subscription keeps its name, gets nothing more, drops what it held; Pull and StreamingPull answer `FAILED_PRECONDITION` `Subscription … is detached.` |

### Subscriber

| RPC | Notes |
|---|---|
| CreateSubscription | see [Subscription settings](#subscription-settings). An existing name answers `ALREADY_EXISTS`. The topic must exist |
| GetSubscription | fills defaults as Pub/Sub shows them: retention 7 days, expiration 31 days, state ACTIVE |
| UpdateSubscription | mask fields: `ack_deadline_seconds`, `push_config`, `bigquery_config`, `cloud_storage_config` (setting one clears the others), `labels`, `retain_acked_messages`, `message_retention_duration`, `expiration_policy`, `dead_letter_policy`, `retry_policy`, `enable_exactly_once_delivery`, `message_transforms`. `filter`, `enable_message_ordering`, `topic`, `name`, `detached` answer `INVALID_ARGUMENT` `The field '…' of a subscription cannot be updated.` |
| ListSubscriptions | by project; pages |
| DeleteSubscription | removes it and its deliveries |
| ModifyPushConfig | sets or clears the push config (an empty endpoint makes it a pull subscription) |
| Pull | `max_messages` (0 = 100). Waits up to 1.5 s for a message unless `return_immediately`. Push subscriptions can be pulled too; BigQuery, Cloud Storage and Bigtable ones answer `FAILED_PRECONDITION` `… cannot be pulled.` |
| StreamingPull | the stream the Python, .NET, Go and Java subscribers use. First request: subscription, `stream_ack_deadline_seconds` (10–600, else the subscription's), `max_outstanding_messages` (0 = 1000), `max_outstanding_bytes` (0 = no limit; one message always goes when none is out). Later requests carry acks, modacks and heartbeats. Each response carries `subscription_properties` (exactly-once, ordering). The stream ends with `FAILED_PRECONDITION` when the subscription is detached and `NOT_FOUND` when it is deleted |
| Acknowledge | see [Acks](#acks-modacks-and-redelivery) |
| ModifyAckDeadline | seconds capped at 600; 0 is a nack |
| CreateSnapshot | of a subscription; an empty name gets `snapshot-<8 hex>`. A live snapshot of that name answers `ALREADY_EXISTS` |
| GetSnapshot, ListSnapshots, DeleteSnapshot | expired snapshots are not listed |
| UpdateSnapshot | mask fields `labels`, `expire_time` |
| Seek | to a `time` or a `snapshot` of the same topic (else `FAILED_PRECONDITION`) |

### SchemaService

| RPC | Notes |
|---|---|
| CreateSchema | `parent` + `schema_id`, or the schema's name. Types `AVRO` and `PROTOCOL_BUFFER`. The definition is checked; a name in use answers `ALREADY_EXISTS` |
| GetSchema | by name or `name@revision`; `view` BASIC leaves out the definition |
| ListSchemas | newest revision of each; BASIC by default; pages |
| ListSchemaRevisions | newest first; pages |
| CommitSchema | a new revision; the type cannot change |
| RollbackSchema | a new revision with an older revision's definition |
| DeleteSchemaRevision | `name@revision`; the only revision answers `FAILED_PRECONDITION` |
| DeleteSchema | topics that used it get schema `_deleted-schema_`; their publishes fail `INVALID_ARGUMENT` |
| ValidateSchema | checks a definition |
| ValidateMessage | by schema name or an inline schema; `encoding` JSON or BINARY is required |

Revision ids are 8 hex characters.

### Paging

Every List takes `page_size` (0 = 100, at most 1000) and `page_token` (an offset Stuntman hands out). A token it did not
hand out answers `INVALID_ARGUMENT` `Invalid page token`.

### Schemas

| Type | Supported |
|---|---|
| Avro | JSON and binary encodings: primitives, records, enums, arrays, maps, unions, fixed; named types; duplicate fields and unions inside unions are refused. Binary validation refuses bytes left after the datum |
| Protocol Buffers | the `.proto` text is parsed: proto2 and proto3, `package`, messages, nested messages and enums, `oneof`, `map`, `repeated` and packed fields, proto2 groups and `required`. The first top-level message is the root. Binary and proto3 JSON are validated. Unknown fields in binary (groups too) are skipped and accepted; a missing proto2 `required` field is refused; JSON refuses unknown fields. A definition with `import` is refused (`Imports are not supported in Pub/Sub schema definitions`), so well-known types are not available |

A topic with `schema_settings` validates every message against the revisions in its range
(`first_revision_id`..`last_revision_id`), newest first. A message none accepts fails the whole Publish with
`INVALID_ARGUMENT` `Invalid data in message: Message failed schema validation. <why>`. Accepted messages get the
attributes `googclient_schemaname`, `googclient_schemaencoding` (`JSON` or `BINARY`) and `googclient_schemarevisionid`.

### Publishing

In order:

1. The provocation set with `set_pubsub`, if it applies to this topic (see [Provoke failures](#provoke-failures)).
2. The topic must exist, else `NOT_FOUND` `Resource not found (resource=<topic>).`
3. An empty request answers `INVALID_ARGUMENT` `The request contains no messages.`; a message with neither data nor
   attributes, `INVALID_ARGUMENT` `Some messages contain neither data nor attributes`.
4. The topic's transforms run on each message (see [Transforms](#transforms)).
5. The topic's schema checks each message.
6. Each message goes to every subscription of the topic that exists now, is not detached, and whose filter matches.

Message ids are decimal integers, one sequence per environment. Publish time is the wall clock. Who published is
kept as `app:<first word of x-goog-api-client or user-agent>` (for example `app:gl-python/3.12.1`), `you` for the
tools, `dead-letter`, or `ingestion:<kind>`.

### Filters

`attributes.k = "v"`, `attributes.k != "v"`, `attributes:k` (has the key), `hasPrefix(attributes.k, "x")`, `NOT`
(or `-`), `AND`, `OR` and parentheses. An invalid filter answers `INVALID_ARGUMENT` `Invalid filter: …` at creation.
A message the filter does not match is never given to that subscription (no delivery row; it does not count
anywhere).

### Subscription settings

| Setting | Default and bounds |
|---|---|
| `ack_deadline_seconds` | 10; clamped to 10–600 |
| `enable_message_ordering` | off |
| `filter` | none |
| `enable_exactly_once_delivery` | off; not allowed on push (`Exactly-once delivery is not supported on push subscriptions.`) |
| `dead_letter_policy` | `max_delivery_attempts` 5 when unset; clamped to 5–100 |
| `retry_policy` | `minimum_backoff` 10 s and `maximum_backoff` 600 s when unset inside the policy; `0 ≤ min ≤ max ≤ 600 s` |
| `message_retention_duration` | 7 days; 10 min to 31 days |
| `retain_acked_messages` | off |
| `expiration_policy.ttl` | 31 days when the policy is unset; at least 1 day; an empty ttl never expires |
| kind | one of push, BigQuery, Cloud Storage or Bigtable, or none (pull). More than one answers `INVALID_ARGUMENT` |
| push endpoint | must start with `http://` or `https://` |
| BigQuery table | `project.dataset.table` (or `project:dataset.table`); `use_topic_schema` and `use_table_schema` are exclusive; `use_topic_schema` needs a topic with a schema |
| Cloud Storage bucket | the name without `gs://`; `max_duration` 5 min when unset; text output when unset |
| Bigtable table | `projects/<p>/instances/<i>/tables/<t>`; a column family mapping needs a key definition; `delimited_key` needs `key_fields` |

### Acks, modacks and redelivery

| Event | Behaviour |
|---|---|
| Delivery | a message goes out with an ack id (`<message id>-<attempt>-<8 hex>`) and the ack deadline. `delivery_attempt` is filled only when the subscription has a dead-letter policy, as Pub/Sub does |
| Ack | marks it acked. An ack of an id already acked succeeds. Unknown or expired ids are ignored (without exactly-once) |
| Nack (ModifyAckDeadline 0) | without a retry policy, the message is pending again at once; with one, it waits its backoff |
| Ack deadline passes | the message is delivered again (on a pull subscription with a retry policy, after the backoff counted from the deadline) |
| Retry backoff | `minimum_backoff × 2^(attempt−1)`, capped at `maximum_backoff`. Pull without a retry policy: none. Push, BigQuery, Cloud Storage and Bigtable without one: 100 ms doubling to 60 s |
| Ordering | with `enable_message_ordering`, a key with a message outstanding waits; other keys and messages without a key flow |
| Dead letter | when a message has been delivered `max_delivery_attempts` times, the next delivery moves it to the dead-letter topic instead, with attributes `CloudPubSubDeadLetterSourceSubscription` (the short name) and `CloudPubSubDeadLetterSourceDeliveryCount`. If the dead-letter topic does not exist, the message is marked dead and dropped |
| Retention | messages older than the subscription's retention (wall clock) are not delivered |

**Exactly-once delivery.** An ack or modack of an id whose deadline passed (or that a later delivery replaced) fails.
When every id in the request fails, the answer is `INVALID_ARGUMENT` with a `google.rpc.ErrorInfo` (reason
`EXACTLY_ONCE_ACKID_FAILURE`, domain `pubsub.googleapis.com`, metadata mapping each failed id to
`PERMANENT_FAILURE_INVALID_ACK_ID`). When only some fail, the code is `ABORTED` with the same details, so the clients
fail just those and send the others again. On StreamingPull, every ack and modack is confirmed on the stream
(`acknowledge_confirmation` and `modify_ack_deadline_confirmation` with `ack_ids`, `invalid_ack_ids`,
`temporary_failed_ack_ids`). The Python client's `ack_with_response()` reads both.

### Snapshots and Seek

- A snapshot keeps what was unacked in the subscription and the topic's newest message id at that moment. It
  expires 7 days after its oldest unacked message was published (or 7 days after creation when nothing was unacked).
- Seek to a snapshot: what was unacked then, and everything the topic got since, is unacked again; the rest acked. A
  subscription created later on the same topic can seek to it too.
- Seek to a time: messages published from then on are unacked again; older ones are acked. Acked messages replay
  only with `retain_acked_messages` or topic retention. Topic retention also reaches messages published before the
  subscription existed. Nothing older than the retention window comes back. Dead-lettered messages stay dead.

### Push subscriptions

| Part | Behaviour |
|---|---|
| Request | `POST` to the endpoint, `User-Agent: APIs-Google; (+https://developers.google.com/webmasters/APIs-Google.html)`, timeout = the ack deadline |
| Wrapped body (default) | `{"message": {"data" (base64), "attributes", "messageId", "message_id", "publishTime", "publish_time", "orderingKey"}, "subscription", "deliveryAttempt"}`; empty data, empty attributes and an empty ordering key are left out; `deliveryAttempt` only with a dead-letter policy |
| Unwrapped (`no_wrapper`) | the data as the body. With `write_metadata`: headers `x-goog-pubsub-subscription-name`, `x-goog-pubsub-message-id`, `x-goog-pubsub-publish-time`, `x-goog-pubsub-delivery-attempt` (with a dead-letter policy), `x-goog-pubsub-ordering-key` (when set), and each attribute as a header |
| OIDC token | `Authorization: Bearer <JWT>` with `aud` (the audience, else the endpoint), `email` (the service account), `email_verified: true`, `azp` and `sub` `stuntman-pubsub-push`, 1 hour validity. Signed RS256 by the environment's [identity provider](identity.md) (a `google` flavour one first, else the first); its JWKS verifies it. With no provider, an unsigned token (`alg: none`, `iss: https://stuntman.invalid/pubsub-test-token`) that a real verifier refuses |
| Answer | 2xx acks. Anything else, or no answer, retries after the backoff; `lastError` shows `push to <endpoint> answered <status>` |
| Flow | at most 100 messages outstanding per subscription; with ordering, one message per key at a time |

### BigQuery subscriptions

A row per message, in a table Stuntman keeps (no BigQuery is called).

| Setting | Row |
|---|---|
| none | `data` (the data as text, base64 when not UTF-8) |
| `use_topic_schema` | the message decoded with the topic's schema revision it was published under; its fields as columns |
| `use_table_schema` | the message's JSON object fields as columns; needs a table schema from `set_pubsub_bigquery_table` |
| `write_metadata` | adds `subscription_name`, `message_id`, `publish_time` (RFC 3339), `attributes` (JSON text) |
| `drop_unknown_fields` | with a table schema and `use_table_schema` or `use_topic_schema`: fields that are not columns are dropped |

With a table schema (columns `{name, type, mode}`: `STRING`, `BYTES`, `INTEGER`/`INT64`, `FLOAT`/`FLOAT64`,
`NUMERIC`, `BIGNUMERIC`, `BOOLEAN`/`BOOL`, `TIMESTAMP`, `DATE`, `DATETIME`, `TIME`, `GEOGRAPHY`, `JSON`,
`RECORD`/`STRUCT`; modes `NULLABLE`, `REQUIRED`, `REPEATED`), a row that does not fit (unknown field, missing
`REQUIRED` column, wrong type, `REPEATED` not an array) is not written. The message comes back after the backoff and,
with a dead-letter policy, moves to the dead-letter topic. `lastError` says why.

### Cloud Storage subscriptions

Messages are batched into a file when the backlog reaches `max_messages`, `max_bytes`, or its oldest message is
`max_duration` old (5 min by default). A file stops at `max_bytes`; the rest waits for the next file.

| Part | Behaviour |
|---|---|
| Object name | `<filename_prefix><date time>_<uuid><filename_suffix>`; the date time follows `filename_datetime_format` (`YYYY`, `MM`, `DD`, `hh`, `mm`, `ss`; default `YYYY-MM-DD/hh_mm_ssZ`), UTC |
| Text | each message's data followed by a newline |
| Avro (`avro_config`) | an Avro object container file (codec null), record `PubsubMessage` with `data` (bytes); with `write_metadata`, `subscription_name`, `message_id`, `publish_time` (timestamp-micros) and `attributes` (map) first. With `use_topic_schema` (Avro topic schema only), `data` is a record of the topic's schema; one schema revision per file |
| Where | kept by Stuntman (`list_pubsub_storage_files`). When a [Storage](storage.md) account has a container named like the bucket, the file is also written there (by `pubsub`) |

### Bigtable subscriptions

A row per message, in a table Stuntman keeps.

| Part | Behaviour |
|---|---|
| Row key | `<subscription>#<salt>#<message id>` (salt: two hex digits of a hash of the id). With `column_family_mapping.delimited_key`: the message's JSON `key_fields` joined by the delimiter. With `row_key_schema`: the table's row key schema from `set_pubsub_bigtable_table` |
| Row key schema | fields of type `STRING` (UTF-8), `INT64` (8 bytes big-endian; sign bit flipped under ordered code, so keys sort), `BYTES` (base64, else the text); encodings `singleton` (one field), `delimited_bytes` (delimiter, default `#`), `ordered_code_bytes`. Non-printable bytes show as `\xNN` |
| Cells | family `data`, qualifier `""`: the data. With `write_metadata`, family `pubsub_metadata`: `subscription_name`, `message_id`, `publish_time`, `attributes`, `ordering_key` |
| Missing key field | the message stays in the backlog, retried; `lastError` says which field |

### Transforms

`message_transforms` on a topic or a subscription run in order; disabled ones are skipped.

| Transform | Behaviour |
|---|---|
| `javascript_udf` | QuickJS, `function <function_name>(message, metadata)`. `message` is `{data: string, attributes: {k: string}}`; `metadata` is `{message_id, publish_time, ordering_key}` (on a topic, `message_id` is empty). Return the message (changed or not) or `null`. Each call gets a fresh runtime, 500 ms and 32 MB. The code must compile and define the function at creation |
| `ai_inference` | endpoint `projects/<p>/locations/<l>/publishers/<publisher>/models/<model>` or `…/endpoints/<id>`. Answered in process by Stuntman's [AI models](ai-models.md) simulator, with its rules, forced failures and call log. The data must be a JSON request; `unstructured_inference.parameters` are merged under it (the message wins) and the model fills `model`. A body with `contents` goes to Gemini `generateContent`; an `anthropic` publisher to Messages; anything else to Chat Completions (`openai`). The data becomes `{"original_message": …, "model_output": …}` |
| `compression` | `ZLIB` or `ZSTD`; compress or decompress the data |

On a topic (before the schema check): `null` drops the message (the publisher still gets an id; nothing is kept); an
error fails the whole Publish with `INVALID_ARGUMENT` `Message transformation failed: …`. An AI inference 429 answers
`RESOURCE_EXHAUSTED` and a 5xx `UNAVAILABLE` (clients retry both).

On a subscription: `null` acks without delivering; an error nacks (back after the backoff; `lastError`). Past
`max_delivery_attempts`, the original message goes to the dead-letter topic with the extra attribute
`CloudPubSubDeadLetterSourceSMTErrorMessage`.

### Import topics

A topic with `ingestion_data_source_settings` (set by CreateTopic/UpdateTopic or `set_pubsub_topic`) reads its source
when Stuntman has it, polling about every 250 ms. Ingested messages go through the topic's transforms and schema.

| Source | Reads | State when missing |
|---|---|---|
| Cloud Storage | the objects of the [Storage](storage.md) container named like the bucket; `match_glob` (`*`, `**`, `?`), `minimum_object_create_time`; each object version once. Text split at the delimiter (newline by default); Avro records as their JSON; Pub/Sub Avro records as data and attributes with `original_message_ordering_key`. Objects compressed with gzip, zstandard, bzip2 or xz are read decompressed (known by their first bytes); Avro codecs null, deflate, snappy, zstandard, bzip2, xz | `BUCKET_NOT_FOUND` |
| Amazon MSK, Confluent Cloud | the [Streams](streams.md) (Kafka) topic of that name, from the earliest offset, then following it. Value → data, key → ordering key, headers → attributes | `TOPIC_NOT_FOUND` |
| Amazon Kinesis | the [Kinesis](kinesis.md) stream `stream_arn` names, from TRIM_HORIZON. Partition key → ordering key | `STREAM_NOT_FOUND` |
| Azure Event Hubs | the [event hub](eventhubs.md) `namespace` and `event_hub` name, from the start of each partition. Body → data, partition key → ordering key, properties → attributes | `NAMESPACE_NOT_FOUND`, `EVENT_HUB_NOT_FOUND` |

The state is `ACTIVE` when the source exists. Google documents no attribute mapping for Kafka, Kinesis or Event
Hubs; the mappings above are Stuntman's. Every source can also be fed with `pubsub_ingest`.

### KMS key and storage policy

The key name and region names are checked as Pub/Sub checks them, but nothing is encrypted or moved.

## How it behaves

- **New subscriptions** get only messages published after they exist (unless they seek with topic retention).
- **Deleted topic**: subscriptions stay, on `_deleted-topic_`, get nothing more, can still be pulled for what they
  hold.
- **Clock.** Publish times, ack deadlines, backoff and retention use the wall clock. The expiration policy (idle =
  no pull, ack or successful push/write) uses the environment clock, so [`advance_clock`](../clock.md) expires idle
  subscriptions.
- **Delivery loop.** Push, BigQuery, Cloud Storage, Bigtable delivery, expiry and ingestion run in one loop for every
  environment, woken when messages arrive and at least every 250 ms. Paused environments are skipped.
- **Persistence.** Everything is in SQLite and survives a restart.
- **Tool publishes are not provoked.** `pubsub_publish` and `pubsub_ingest` skip `set_pubsub` provocations; only the
  app's Publish (gRPC or REST) meets them.

## Provoke failures

`set_pubsub` sets one provocation at a time. `times` limits it to the next N Publish calls (N ack/modack requests for
`ack_transient`); 0 keeps it until cleared. `topic` limits it to one topic (short or full name). `none` clears it.

| Failure | How | What the app sees | Turn it off |
|---|---|---|---|
| Service unavailable | `set_pubsub {"provoke": "unavailable", "times": 2}` | Publish `UNAVAILABLE` `The service is currently unavailable. (provoked by Stuntman)`; clients retry | runs out, or `provoke: none` |
| Deadline exceeded | `set_pubsub {"provoke": "deadline_exceeded"}` | `DEADLINE_EXCEEDED` | `provoke: none` |
| Quota | `set_pubsub {"provoke": "resource_exhausted"}` | `RESOURCE_EXHAUSTED` `Quota exceeded for quota metric 'Publish requests' (provoked by Stuntman)` | `provoke: none` |
| Topic gone | `set_pubsub {"provoke": "not_found", "topic": "shipments"}` | `NOT_FOUND` `Resource not found (resource=shipments). (provoked by Stuntman)` | `provoke: none` |
| Slow publish | `set_pubsub {"provoke": "slow_publish", "delay_ms": "5s"}` | Publish answers after the delay (3 s by default) | `provoke: none` |
| Duplicate delivery | `set_pubsub {"provoke": "duplicate"}` | a message's first ack does not stick: it is delivered again | `provoke: none` |
| Transient exactly-once ack failure | `set_pubsub {"provoke": "ack_transient", "times": 1}` on an exactly-once subscription | acks and modacks fail with `TRANSIENT_FAILURE_UNORDERED_ACK_ID` (in the ErrorInfo, or `temporary_failed_ack_ids` on the stream); nothing is applied; clients retry | runs out, or `provoke: none` |
| KMS key disabled | `set_pubsub {"provoke": "kms_disabled"}` | Publish to a topic with `kms_key_name` answers `FAILED_PRECONDITION` `The Cloud KMS key … is disabled or its permission was revoked. (provoked by Stuntman)`; topics without a key publish normally | `provoke: none` |
| Push endpoint fails | your endpoint answers non-2xx | redelivery with backoff, then dead-letter | fix the endpoint |
| BigQuery row refused | `set_pubsub_bigquery_table` with a schema the messages do not fit | retried, then dead-lettered; `lastError` | change the table schema |
| UDF throws | a transform that throws | topic: Publish fails `INVALID_ARGUMENT`; subscription: nack and retry | change the transform |
| AI model fails | `force_ai_failure` on provider `openai`, `gemini` or `anthropic` (see [AI models](ai-models.md)) | topic: `RESOURCE_EXHAUSTED` (429), `UNAVAILABLE` (5xx), else `INVALID_ARGUMENT`; subscription: retried, then dead-lettered | the forced failure runs out |
| Redelivery after the deadline | do not ack within `ack_deadline_seconds` | the message comes again | — |

## See what the app did

| Need | Tool | Route | Fields that matter |
|---|---|---|---|
| Everything | `list_pubsub_topics` | `GET /api/pubsub/topics` | `topics[].subscriptions[]`: `type`, `filter`, `pending`, `outstanding`, `acked`, `deadLettered`, `unacked`, `lastError`, `lastActiveAt`, `expiresAt`, `delivery`; `topics[].ingestion.state`; `detachedSubscriptions`, `snapshots`, `schemas`, `bigqueryTables`, `bigtableTables`, `connect`, `provoke` |
| Messages of a topic | `list_pubsub_messages {"topic": "shipments"}` | `GET /api/pubsub/messages?topic=…` | `messageId`, `publishedAt`, `publisher`, `orderingKey`, `attributes`, `data` (`{format: json/text/…, value}`), `subscriptions[].state` (`pending`, `outstanding`, `expired`, `acked`, `dead`), `deliveryAttempts`, `ackedAt` |
| One subscription | `list_pubsub_messages {"subscription": "shipments-billing"}` | same, `?subscription=…` | the subscription view plus each delivery |
| Wait for the app's publish | `wait_for_pubsub_message {"topic": "shipments", "contains": "SHP-1041", "attributes": {"source": "billing"}}` | `GET /api/pubsub/topics/{topic}/wait` | the first message the app published (publisher `app:…`) matching text in the data, attributes and ordering key. Looks back 15 s (`since` in ms epoch), waits `timeout` (30 s), then 408 with `seen` |
| BigQuery rows | `list_pubsub_bigquery_rows` | `GET /api/pubsub/bigquery` | `subscription`, `table`, `row`, `writtenAt` |
| Bigtable rows | `list_pubsub_bigtable_rows` | `GET /api/pubsub/bigtable` | `rowKey`, `cells` |
| Cloud Storage files | `list_pubsub_storage_files` | `GET /api/pubsub/storage` | `bucket`, `object`, `messages`, `format`, `content` (text, or base64 for Avro), `storedIn` |

Pub/Sub calls are counted in `usage` (kind `pubsub`), and the app's client shows among the live clients. They are not
written as [Timeline](../timeline.md) rows.

## Recipes

### 1. Feed the app's subscriber

1. `create_pubsub_subscription {"subscription": "shipments-billing", "topic": "shipments"}` (or let the app create it).
2. `pubsub_publish {"topic": "shipments", "data": {"shipment": "SHP-1041"}, "attributes": {"source": "partner"}, "ordering_key": "SHP-1041"}`.
3. `list_pubsub_messages {"subscription": "shipments-billing"}`: `state` becomes `acked` when the app acks.

### 2. Assert what the app published

1. Run the app's action.
2. `wait_for_pubsub_message {"topic": "shipments", "contains": "SHP-1041", "timeout": "20s"}`.
3. Check `attributes`, `orderingKey` and `data.value`.

### 3. Make the publisher survive outages

1. `set_pubsub {"provoke": "unavailable", "times": 2, "topic": "shipments"}`.
2. Run the app's action. The client retries; the third attempt succeeds.
3. `list_pubsub_messages {"topic": "shipments"}`: the message is there once.
4. For a failure the client does not retry: `set_pubsub {"provoke": "not_found"}`, check the app's error path, then
   `set_pubsub {"provoke": "none"}`.

### 4. Dead-letter a poison message

1. `create_pubsub_subscription {"subscription": "shipments-billing", "topic": "shipments", "dead_letter_topic": "shipments-dlq", "max_delivery_attempts": 5}`.
2. `create_pubsub_subscription {"subscription": "shipments-dlq-audit", "topic": "shipments-dlq"}` (so the dead-letter
   topic's messages are kept somewhere you can read).
3. Publish a message the app nacks or fails on.
4. After 5 deliveries: `list_pubsub_messages {"topic": "shipments-dlq"}` shows it with
   `CloudPubSubDeadLetterSourceSubscription` and `CloudPubSubDeadLetterSourceDeliveryCount`.

### 5. Test a push endpoint with OIDC

1. Make sure the environment has an identity provider (see [Identity](identity.md)).
2. `create_pubsub_subscription {"subscription": "shipments-push", "topic": "shipments", "push_endpoint": "http://host.docker.internal:5000/pubsub/push", "oidc_service_account": "push-invoker@acme-stg.iam.gserviceaccount.com", "oidc_audience": "https://api.acme.example"}`.
3. `pubsub_publish {"topic": "shipments", "data": {"shipment": "SHP-1042"}}`.
4. `list_pubsub_topics`: `lastError` is empty and `acked` grew when your endpoint answered 2xx.

### 6. Enforce a schema

1. `set_pubsub_schema {"schema": "shipment-event", "type": "avro", "definition": {"type": "record", "name": "Shipment", "fields": [{"name": "shipment", "type": "string"}]}, "topic": "shipments", "encoding": "JSON"}`.
2. Publish `{"shipment": "SHP-1"}` from the app: accepted, with `googclient_schema*` attributes.
3. Publish `{"id": 1}`: the app gets `INVALID_ARGUMENT` `Message failed schema validation`.

### 7. Expire an idle subscription

1. `create_pubsub_subscription {"subscription": "nightly", "topic": "shipments", "expiration_ttl": "1d"}`.
2. `advance_clock {"by": "2d"}`.
3. `list_pubsub_topics`: `nightly` is gone. `advance_clock {"reset": true}`.

### 8. An import topic reading Kafka

1. `create_topic {"name": "shipments.events"}` ([Streams](streams.md)).
2. `set_pubsub_topic {"topic": "shipments-import", "ingestion": {"kind": "aws_msk", "cluster_arn": "arn:aws:kafka:us-east-1:000000000000:cluster/acme/1", "topic": "shipments.events"}}`.
   `list_pubsub_topics` shows `ingestion.state` `ACTIVE`.
3. `create_pubsub_subscription {"subscription": "import-check", "topic": "shipments-import"}`.
4. `produce {"topic": "shipments.events", "key": "SHP-1", "value": {"shipment": "SHP-1"}, "headers": {"source": "globex"}}`.
5. `list_pubsub_messages {"subscription": "import-check"}`: `orderingKey` is `SHP-1`, `attributes.source` is
   `globex`, `publisher` is `ingestion:aws_msk`.

## Errors and troubleshooting

| Symptom or error text | Cause | Fix |
|---|---|---|
| The client calls Google, asks for credentials | `PUBSUB_EMULATOR_HOST` not set in the app's process (in .NET, no `EmulatorDetection`) | set it; in .NET use `EmulatorDetection.EmulatorOrProduction` |
| `NOT_FOUND Resource not found (resource=…)` | the topic or subscription does not exist in this environment | create it, or send `x-stuntman-env` for the right environment |
| `NOT_FOUND No Stuntman environment …` | the `x-stuntman-env` value names no environment | fix the name |
| `UNAVAILABLE … is paused in Stuntman` | the environment is paused | resume it |
| `INVALID_ARGUMENT Invalid [topics] name` | a short name sent over gRPC | use `projects/<p>/topics/<t>` |
| `ALREADY_EXISTS` | the topic, subscription, schema or snapshot exists | ignore it, or delete first |
| `FAILED_PRECONDITION … cannot be pulled.` | pulling a BigQuery, Cloud Storage or Bigtable subscription | read with `list_pubsub_bigquery_rows`, `list_pubsub_storage_files`, `list_pubsub_bigtable_rows` |
| `FAILED_PRECONDITION … is detached.` | DetachSubscription was called | make a new subscription |
| The subscriber gets nothing that was published earlier | subscriptions get only later messages | create the subscription first, or Seek with topic retention |
| Messages vanish into the dead-letter topic | it has no subscription | add one to the dead-letter topic |
| `INVALID_ARGUMENT The field 'filter' of a subscription cannot be updated.` | as in Pub/Sub | recreate the subscription |
| Push endpoint never called | `localhost` inside a container means the container | use an address Stuntman can reach |
| `wait_for_pubsub_message` 408 though the message is there | it was published by a tool (`you`), or more than 15 s before | publish from the app, or pass `since` |
| Avro/proto schema refused: `Imports are not supported…` | the definition uses `import` | inline the types |

## Not done

- TLS and authentication on the port (the official emulator has none either).
- REST beyond `:publish`.
- AI inference has no 60-second timeout to hit: the simulator answers in process (force `slow` for a slow model).
- KMS encryption and message storage regions are checked, not applied.
- Pub/Sub's minimums on Cloud Storage `max_messages` and `max_duration` are not enforced, so tests can use small
  batches.
- Real BigQuery, Bigtable and Cloud Storage are never called; the rows and files stay in Stuntman (files also go to a
  Stuntman storage container of the bucket's name).
- Schema `import`s and well-known protobuf types.
- No Timeline rows for Pub/Sub.

Missing something? Tell the developer what the fix would be and offer a pull request — see [Contributing](../../CONTRIBUTING.md).

## Tools and routes

<!-- tools: pubsub -->
_Generated by `tools/gen_tool_reference.py` from the engine source; each tool is an MCP tool and the HTTP route shown (under `/api`)._

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_pubsub_topics` | `GET /api/pubsub/topics` | — | — | The Pub/Sub topics of this environment (schema, retention, labels) with their subscriptions (pull, push, BigQuery or Cloud Storage; filter, ack deadline, ordering, exactly-once, dead-letter) and how many messages each has pending, outstanding, acked and dead-lettered; snapshots, schemas and how apps connect. |
| `list_pubsub_messages` | `GET /api/pubsub/messages` | — | topic, subscription, project, limit | The newest messages of a topic with their state in each subscription (pending, outstanding, expired, acked, dead), or of one subscription: data (JSON or text), attributes, ordering key, delivery attempts, who published. |
| `pubsub_publish` | `POST /api/pubsub/topics/{topic}/publish` | topic | data, attributes, ordering_key, project | Publishes a message to a topic as another service would (the app's subscribers receive it): data (JSON or text), attributes, ordering_key. Makes the topic when it does not exist. A topic with a schema validates the data, as Pub/Sub does. |
| `create_pubsub_subscription` | `PUT /api/pubsub/subscriptions/{subscription}` | subscription, topic | filter, ack_deadline_seconds, ordering, exactly_once, retain_acked, dead_letter_topic, max_delivery_attempts, push_endpoint, unwrapped, write_metadata, oidc_service_account, oidc_audience, bigquery_table, use_topic_schema, bucket, filename_prefix, filename_suffix, max_messages, max_duration, avro, bigtable_table, expiration_ttl, min_backoff, max_backoff, udf_code, udf_function, labels, project | Creates a subscription on a topic (the topic too when missing): pull by default; push (push_endpoint, unwrapped, write_metadata, oidc_service_account), BigQuery (bigquery_table, use_topic_schema, write_metadata), Cloud Storage (bucket, filename_prefix, max_messages, max_duration, avro) or Bigtable (bigtable_table); filter, JavaScript UDF, ack deadline, message ordering, exactly-once, expiration ttl, retry backoff, a dead-letter topic with max delivery attempts. |
| `set_pubsub_schema` | `PUT /api/pubsub/schemas/{schema}` | schema, definition | type, topic, encoding, project | Creates a Pub/Sub schema (Avro or Protocol Buffer) or commits a new revision of it; with topic (and encoding JSON or BINARY), the topic validates every publish against it. |
| `list_pubsub_bigquery_rows` | `GET /api/pubsub/bigquery` | — | table, subscription, limit | The rows BigQuery subscriptions wrote (Stuntman keeps the tables): one per message, the topic schema's fields with use_topic_schema, else data; subscription_name, message_id, publish_time and attributes with write_metadata. |
| `list_pubsub_storage_files` | `GET /api/pubsub/storage` | — | bucket, subscription, limit | The files Cloud Storage subscriptions wrote: bucket, object name, messages, format (text or Avro) and content; storedIn names the Stuntman storage account when one has that bucket. |
| `list_pubsub_bigtable_rows` | `GET /api/pubsub/bigtable` | — | table, subscription, limit | The rows Bigtable subscriptions wrote (Stuntman keeps the tables): row key subscription#salt#message_id (or the delimited key), the data in family data (qualifier ""), and with write_metadata the pubsub_metadata family (subscription_name, message_id, publish_time, attributes, ordering_key). |
| `set_pubsub_bigtable_table` | `PUT /api/pubsub/bigtable/tables/{table}` | table, row_key_schema | — | Defines a Bigtable table's row key schema for Bigtable subscriptions with row_key_schema: fields [{name, type STRING \| INT64 \| BYTES}] taken from the message's JSON, encoding singleton, delimited_bytes (delimiter, default #) or ordered_code_bytes. |
| `set_pubsub_bigquery_table` | `PUT /api/pubsub/bigquery/tables/{table}` | table, schema | — | Defines a BigQuery table's schema for BigQuery subscriptions: columns [{name, type, mode}] (STRING, BYTES, INTEGER, FLOAT, NUMERIC, BOOLEAN, TIMESTAMP, DATE, JSON, RECORD; NULLABLE, REQUIRED, REPEATED). With it, use_table_schema maps the message's JSON fields to columns, drop_unknown_fields drops extras, and a row that does not fit is not written (retried, then dead-lettered). |
| `set_pubsub_topic` | `PUT /api/pubsub/topics/{topic}` | topic | ingestion, udf_code, udf_function, project | Sets a topic's ingestion source (kind aws_kinesis, cloud_storage, azure_event_hubs, aws_msk, confluent_cloud with that source's fields) and/or a JavaScript UDF transform (udf_code, udf_function), making the topic when missing. Cloud Storage reads a Stuntman storage container of the bucket's name; MSK and Confluent read the Streams (Kafka) topic; Kinesis reads the Stuntman Kinesis stream its stream_arn names, from TRIM_HORIZON. |
| `pubsub_ingest` | `POST /api/pubsub/topics/{topic}/ingest` | topic, records | project | Feeds records into an import topic as its source would: records [{data, key, attributes}] — Kafka (MSK, Confluent): value → data, key → ordering key, headers → attributes; Kinesis: partition key → ordering key; Event Hubs: partition key → ordering key, properties → attributes; Cloud Storage: each record one text line. They go through the topic's transforms and schema. |
| `wait_for_pubsub_message` | `GET /api/pubsub/topics/{topic}/wait` | topic | contains, attributes, ordering_key, since, timeout, project | Waits for the first message the app published (not you or an agent) on a topic that matches: text in the data, attributes, ordering key. Looks back 15 s by default; 408 after the timeout. |
| `set_pubsub` | `PUT /api/pubsub/config` | — | project, provoke, times, topic, delay_ms | Pub/Sub settings: project (for short names) and a provocation on Publish: unavailable, deadline_exceeded, resource_exhausted, not_found, slow_publish (delay_ms), duplicate (each message delivered again after its first ack), ack_transient (exactly-once acks fail with a transient error the client retries), kms_disabled (publishes to topics with a KMS key fail with FAILED_PRECONDITION); times = for the next N publishes (0 = until cleared), topic = only that topic; none clears. |
<!-- /tools -->
