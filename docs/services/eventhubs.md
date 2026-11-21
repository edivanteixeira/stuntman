# Azure Event Hubs

Stuntman answers Azure Event Hubs over AMQP 1.0, the way the SDKs speak it in emulator mode
(`UseDevelopmentEmulator=true`), on the same port as [Service Bus](servicebus.md), and over the Kafka protocol through
the [Streams](streams.md) Kafka port. The app sends events (one, a batch, with a partition key or to a partition),
reads partitions from where it wants, uses consumer groups and owner levels, asks for hub and partition properties,
and keeps checkpoints in Blob Storage. Stuntman keeps every event with its partition, sequence number, offset and
enqueued time, the receivers reading each partition now, and the checkpoints the app wrote.

## At a glance

| | |
|---|---|
| **Stands in for** | Azure Event Hubs (AMQP 1.0: `$cbs` SAS tokens, senders, partition receivers with selector filters and epochs, `$management` READ; the Kafka endpoint with SASL PLAIN `$ConnectionString`). Tested with Python `azure-eventhub` in emulator mode and `confluent-kafka`. |
| **Ports** | AMQP `5673` (`STUNTMAN_AMQP_PORT`), AMQPS `5671` (`STUNTMAN_AMQPS_PORT`, Test CA certificate), and `5300` (`STUNTMAN_SERVICEBUS_MGMT_PORT`, which also answers AMQP). Kafka: `9092` (`STUNTMAN_KAFKA_PORT`), plaintext. |
| **Point your app at** | `Endpoint=sb://<namespace>.localhost:5673/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=<any>;EntityPath=<hub>;UseDevelopmentEmulator=true`. Kafka: `bootstrap.servers=localhost:9092`, `security.protocol=SASL_PLAINTEXT`, `sasl.mechanism=PLAIN`, user `$ConnectionString`, password the connection string. |
| **It keeps** | Namespaces (`eventhubs/<namespace>.yaml`: policies, hubs with partitions, consumer groups and retention, the provocations); every event with partition, sequence number, byte offset, enqueued time (environment clock), partition key, properties, body and who sent it (the app over AMQP or Kafka, you, an agent); the receivers reading each partition now with their owner level and position; the checkpoints in a Stuntman storage account. |
| **You can provoke** | The next N sends (and management requests) answered ServerBusy; the next N sends answered QuotaExceeded; slow sends; hubs that answer "not found"; receivers losing their partition (link stolen); every connection of the namespace dropped. Retention on the environment clock. |
| **Screen** | `/queues/eventhubs` |
| **Tools** | `create_eventhub`, `list_eventhubs`, `eventhub_send`, `list_eventhub_events`, `wait_for_eventhub_event`, `set_eventhubs` |

## Connect your app

### Create the hub first

Event Hubs namespaces are separate from Service Bus namespaces. The app cannot create them: make the hub (and its
namespace) with a tool before the app connects.

```text
create_eventhub {"namespace": "acme-hubs", "hub": "orders", "partitions": 4, "consumer_groups": ["billing"], "retention": "7d"}
```

- Namespace names: 3 to 50 lowercase letters, numbers or dashes, starting with a letter. Hub names: letters,
  numbers, dots, dashes or underscores, at most 256; looked up without regard to case.
- `partitions`: 1 to 32, default 4; calling `create_eventhub` again can add partitions (never remove) and consumer
  groups, and change retention. `$Default` is always a consumer group.
- `retention`: a duration, default `24h`.
- A new namespace gets the policy `RootManageSharedAccessKey` with a random key in `secrets.yaml`
  (`eventhubs.<namespace>.RootManageSharedAccessKey`).

If the host of a connection names no Event Hubs namespace, the connection is a [Service Bus](servicebus.md) one: a
host like `acme-hubs.localhost` then **creates a Service Bus namespace** of that name, and the Event Hubs links fail.

### Connection strings

`list_eventhubs` returns the namespace's `connection` and one per hub (with `EntityPath`).

```text
# default environment
Endpoint=sb://acme-hubs.localhost:5673/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=<key>;EntityPath=orders;UseDevelopmentEmulator=true
# environment pr-5462
Endpoint=sb://acme-hubs.pr-5462.localhost:5673/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=<key>;EntityPath=orders;UseDevelopmentEmulator=true
# the official emulator's string: the hub names the namespace (default environment)
Endpoint=sb://localhost:5673;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=SAS_KEY_VALUE;EntityPath=orders;UseDevelopmentEmulator=true
```

| Host | Namespace |
|---|---|
| `acme-hubs.localhost` | `acme-hubs`, default environment |
| `acme-hubs.<env>.localhost`, `acme-hubs.<base domain>` | `acme-hubs` in that environment (see [Environments](../environments.md#how-a-request-finds-its-environment)) |
| `localhost` or an IP address | the default environment's namespace that has a hub of the name in the address path (`EntityPath`) |
| a name with no dot, e.g. `acme-hubs` | `acme-hubs`, default environment |

Without `UseDevelopmentEmulator=true` the SDKs use TLS on `5671`; trust the
[Stuntman Test CA](../configuration.md#the-stuntman-test-ca).

From a container, give the Stuntman service a network alias equal to the namespace (`sb://acme-hubs:5673/`), or use
an IP address with `EntityPath` set.

### Keys

By default any SAS token is accepted. To check them:

```text
set_eventhubs {"namespace": "acme-hubs", "any_key": false}
```

Then `$cbs` accepts only a token signed with a policy's key and not expired (else 401 `InvalidSignature: The token has
an invalid signature.`), and a send or receive link without a valid token is refused `amqp:unauthorized-access`.

### Python (`azure-eventhub`)

```python
from azure.eventhub import EventData, EventHubConsumerClient, EventHubProducerClient

cs = "Endpoint=sb://acme-hubs.localhost:5673/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=any=;EntityPath=orders;UseDevelopmentEmulator=true"
p = EventHubProducerClient.from_connection_string(cs)
batch = p.create_batch(partition_key="ORD-1042")
batch.add(EventData('{"orderId": "ORD-1042", "kind": "OrderPaid"}'))
p.send_batch(batch)

c = EventHubConsumerClient.from_connection_string(cs, consumer_group="billing")
c.receive(on_event=lambda ctx, e: print(e.sequence_number, e.body_as_str()), partition_id="0", starting_position="-1")
```

With `BlobCheckpointStore`, point the checkpoint store at Stuntman's Azure Blob port (see [Storage](storage.md));
`list_eventhubs` then shows the checkpoints.

### .NET (`Azure.Messaging.EventHubs`)

```csharp
var cs = "Endpoint=sb://acme-hubs.localhost:5673/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=any=;EntityPath=orders;UseDevelopmentEmulator=true";
await using var producer = new EventHubProducerClient(cs);
await producer.SendAsync(new[] { new EventData("{\"orderId\":\"ORD-1042\"}") }, new SendEventOptions { PartitionKey = "ORD-1042" });
```

### Kafka clients

```python
from confluent_kafka import Producer

cs = "Endpoint=sb://acme-hubs.localhost:5673/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=any="
p = Producer({"bootstrap.servers": "localhost:9092", "security.protocol": "SASL_PLAINTEXT", "sasl.mechanism": "PLAIN",
              "sasl.username": "$ConnectionString", "sasl.password": cs})
p.produce("orders", key="ORD-1042", value=b'{"orderId": "ORD-1042"}', headers={"kind": "OrderPaid"})
p.flush()
```

The login names the namespace through the connection string's `Endpoint` (same host rules as above). With
`any_key` off, `SharedAccessKeyName` and `SharedAccessKey` must match a policy's key.

## What it supports

### AMQP links

| Address | Role | Notes |
|---|---|---|
| `$cbs` | request/response | `put-token`; the audience can name the namespace |
| `<hub>` | sender | a partition key (`x-opt-partition-key`) hashes to a partition as the service does; no key: round-robin; a batch goes to one partition |
| `<hub>/Partitions/<n>` | sender | straight to that partition |
| `<hub>/ConsumerGroups/<group>/Partitions/<n>` | receiver | starts where the selector filter says; owner level from `com.microsoft:epoch` |
| `$management` | request/response | READ `com.microsoft:eventhub` and `com.microsoft:partition` |

`Partitions` and `ConsumerGroups` are matched without regard to case. A partition id outside the hub's range is
refused `com.microsoft:argument-out-of-range`; an unknown hub `amqp:not-found` (`The messaging entity
'sb://<ns>.servicebus.windows.net/<hub>' could not be found.`); an unknown consumer group `amqp:not-found`.

### Sending

| Feature | Notes |
|---|---|
| One event | body (data sections, AMQP value), properties (`message-id`, `subject`, `content-type`, `correlation-id`…), application properties |
| Batch | message format `0x80013700`: each data section is one encoded event, all appended to one partition |
| Partition key | Bob Jenkins' lookup3 hash of the UTF-8 key, folded to 16 bits, `|hash % partitions|`: the partition the service and the .NET SDK pick |
| Answer | `accepted`, or `rejected` with the provoked or real error |

### Receiving

| Feature | Notes |
|---|---|
| Start position | selector filter `amqp.annotation.x-opt-offset`, `x-opt-sequence-number` or `x-opt-enqueued-time` with `>`, `>=` or `=`; offset `-1` is the start, `@latest` the end; no filter: the start |
| Delivered event | the app's message as sent, with annotations `x-opt-sequence-number`, `x-opt-offset` (string), `x-opt-enqueued-time`, `x-opt-partition-key` |
| Runtime metrics | with the `com.microsoft:enable-receiver-runtime-metric` capability, each delivery carries `last_enqueued_sequence_number`, `last_enqueued_offset`, `last_enqueued_time_utc`, `runtime_info_retrieval_time_utc` |
| Owner level | a receiver with a higher epoch disconnects the ones below it and the ones without (`amqp:link:stolen`); a lower one, or one without while an epoch receiver reads, is refused `amqp:link:stolen` (`Receiver 'nil' with higher epoch of '<n>' already exists…`) |
| Receivers per partition | at most 5 without epoch per partition and consumer group; the sixth gets `amqp:resource-limit-exceeded` (`Exceeded the maximum number of allowed receivers per partition in a consumer group which is 5.`) |
| Flow | link credit and `drain` honoured |

### `$management`

| Request | Answer |
|---|---|
| `type: com.microsoft:eventhub`, `name: <hub>` | `name`, `type`, `created_at`, `partition_count`, `partition_ids` |
| `type: com.microsoft:partition`, `name`, `partition` | `begin_sequence_number`, `last_enqueued_sequence_number` (-1 when empty), `last_enqueued_offset`, `last_enqueued_time_utc`, `is_partition_empty`; 400 for a partition out of range |
| unknown hub | 404 |
| other types | 400 |

A `security_token` application property is accepted as the token.

### Kafka endpoint

| Item | Notes |
|---|---|
| Login | SASL PLAIN, user `$ConnectionString`, password the connection string |
| Topics | the namespace's hubs, with the same partitions |
| Produce | the record goes to the partition the client picked; the key becomes the partition key, headers become properties (as text), the value the body |
| Fetch | offsets are the sequence numbers |
| ListOffsets | earliest (`-2`) is the partition's begin sequence number, latest (`-1`) the next one; a timestamp lookup answers the end |
| Consumer groups, committed offsets | Streams' own (not the AMQP consumer groups) |

An event sent over Kafka reaches AMQP receivers, and the other way round.

### Namespace file

`eventhubs/<namespace>.yaml`:

| Key | Default | Meaning |
|---|---|---|
| `any_key` | `true` | any token |
| `policies` | `[RootManageSharedAccessKey]` | policies with a key each |
| `hubs.<hub>.partitions` | 4 | |
| `hubs.<hub>.consumer_groups` | `[$Default]` | |
| `hubs.<hub>.retention` | `24h` | |
| `provoke` | | `server_busy`, `quota_exceeded`, `slow_sends_ms`, `not_found` |

## How it behaves

- **Sequence numbers** start at 0 per partition and grow by one. **Offsets** are byte offsets: each event adds the
  size of the AMQP message as sent.
- **Enqueued time** is the [environment clock](../clock.md). **Retention** runs on it too: every second, events
  older than the hub's retention are deleted and the partition's begin sequence number moves on. `advance_clock`
  ages events.
- Events are never settled or removed by reading; any number of consumer groups read the same events.
- A **checkpoint** is a blob `<fully qualified namespace>/<hub>/<group>/checkpoint/<partition>` with `offset` and
  `sequencenumber` metadata, in any storage account of the environment: `list_eventhubs` shows them per consumer
  group.
- **Persistence.** Namespaces are YAML files; events are in the environment's SQLite and survive a restart.
  Receivers and connections do not.
- **Who sent it.** Events from the app have `via` `amqp` or `kafka` and the client as `sender`; `eventhub_send`
  records you or the agent, and `wait_for_eventhub_event` ignores those.
- A [Pub/Sub](pubsub.md) import topic whose `azure_event_hubs` names a hub reads its events.

## Provoke failures

All through `set_eventhubs {"namespace": "acme-hubs", …}`. Only what you pass changes; `reset: true` clears the
provocations.

| Failure | Argument | What the app sees | Turn it off |
|---|---|---|---|
| ServerBusy | `server_busy: 2` | the next 2 sends are rejected `com.microsoft:server-busy` (`The request was terminated because the entity is being throttled. Error code : 50002…`); management requests use up the count too (503); the SDKs retry | counts down; `server_busy: 0` |
| Quota exceeded | `quota_exceeded: 1` | the next send is rejected `amqp:resource-limit-exceeded` (`QuotaExceeded: The namespace's throughput units are used up; ingress is refused.`) | counts down |
| Slow sends | `slow_sends: "2s"` | each send is accepted after 2 s | `slow_sends: 0` |
| Hub not found | `not_found: ["orders"]` | links and management requests for `orders` get not found | `not_found: []` |
| Partition stolen | `steal_owner: "orders"` (or `true` for every hub) | every receiver of the hub is detached `amqp:link:stolen` (`New receiver 'nil' with higher epoch…`): `OwnershipLost` in the SDKs | the app reconnects |
| Connections dropped | `drop_connections: true` | every AMQP connection of the namespace is closed with `amqp:connection:forced`; the SDKs reconnect | |
| Keys checked | `any_key: false` | wrong tokens get 401; links without a token are refused | `any_key: true` |
| Old events gone | `create_eventhub {…, "retention": "1h"}` then `advance_clock {"by": "2h"}` | the events are deleted; reading from the start begins later | |

ServerBusy and QuotaExceeded also apply to Kafka produce requests.

## See what the app did

| Need | Tool | Route |
|---|---|---|
| Namespaces, hubs, partitions, consumer groups, receivers now, checkpoints, connection strings, provocations | `list_eventhubs` | `GET /api/eventhubs` |
| Events, newest first | `list_eventhub_events {"namespace", "hub", "partition", "last"}` | `GET /api/eventhubs/{namespace}/{hub}/events` |
| Wait for the app's event | `wait_for_eventhub_event {"namespace", "hub", "where", "text", "partition", "since", "timeout"}` | `GET /api/eventhubs/{namespace}/{hub}/wait` |

- A partition has `id`, `beginSequenceNumber`, `lastEnqueuedSequenceNumber`, `lastEnqueuedOffset`,
  `lastEnqueuedTimeUtc`, `isEmpty`, `count`.
- A consumer group has `readers` (`partition`, `epoch`, `client`, `remote`, `since`, `position` = the last sequence
  number sent) and `checkpoints` (`partition`, `offset`, `sequenceNumber`, `account`, `container`, `at`).
- An event has `partition`, `sequenceNumber`, `offset`, `enqueuedAt`, `partitionKey`, `properties`, `system`
  (`messageId`, `subject`, `contentType`, `correlationId`…), `body` (text) or `bodyBase64`, `sender`, `via`.
- `wait_for_eventhub_event` returns the first event the app sent (`via` `amqp` or `kafka`) since `since` (default
  2 s ago on the environment clock) whose properties or JSON body fields match `where` (`partitionKey` too), whose
  body contains `text`, on `partition` if given. 408 after `timeout` (default 30 s), with the last events seen.

Event Hubs traffic does not write Timeline rows.

## Recipes

1. **Check that the app publishes an event.**
   ```text
   wait_for_eventhub_event {"namespace": "acme-hubs", "hub": "orders", "where": {"kind": "OrderPaid", "orderId": "ORD-1042"}}
   ```

2. **Check the partition key lands where Azure would put it.**
   ```text
   eventhub_send        {"namespace": "acme-hubs", "hub": "orders", "body": {"orderId": "ORD-1042"}, "partition_key": "ORD-1042"}
   list_eventhub_events {"namespace": "acme-hubs", "hub": "orders", "last": 5}      → partition and sequenceNumber
   ```

3. **Feed the app's consumer from another producer.**
   ```text
   eventhub_send {"namespace": "acme-hubs", "hub": "orders", "events": [{"body": {"orderId": "ORD-1"}}, {"body": {"orderId": "ORD-2"}, "properties": {"kind": "OrderPaid"}}], "partition": 2}
   ```

4. **Make the producer survive throttling.**
   ```text
   set_eventhubs {"namespace": "acme-hubs", "server_busy": 3}
   wait_for_eventhub_event {"namespace": "acme-hubs", "hub": "orders"}
   ```

5. **Make the event processor survive losing a partition.**
   ```text
   set_eventhubs  {"namespace": "acme-hubs", "steal_owner": "orders"}
   list_eventhubs                                  → the readers reconnected; checkpoints moved on
   ```

6. **Survive a dropped connection.**
   ```text
   set_eventhubs {"namespace": "acme-hubs", "drop_connections": true}
   ```

7. **Test what the consumer does after retention removed old events.**
   ```text
   create_eventhub {"namespace": "acme-hubs", "hub": "orders", "retention": "1h"}
   advance_clock   {"by": "2h"}
   list_eventhubs                                  → beginSequenceNumber moved on
   ```

## Errors and troubleshooting

| Symptom or error text | Cause | Fix |
|---|---|---|
| `The messaging entity 'sb://<ns>.servicebus.windows.net/<hub>' could not be found.` | no hub of that name, or it is in `not_found` | `create_eventhub`; `set_eventhubs {"not_found": []}` |
| Links fail and a Service Bus namespace `<ns>` appeared on `/queues` | the hub was not created before the app connected, so the host made a Service Bus namespace | `create_eventhub` first; delete the stray Service Bus namespace (`delete_account {"service": "azure-service-bus", "account": "<ns>"}`) |
| `com.microsoft:argument-out-of-range` `The specified partition is invalid…` | partition id outside `0..partitions-1` | use a valid id, or grow the hub |
| `The messaging entity '<ns>:eventhub:<hub>~32766|<group>' could not be found.` | unknown consumer group | `create_eventhub {…, "consumer_groups": ["<group>"]}` |
| `amqp:link:stolen` `Receiver 'nil' with higher epoch of '<n>' already exists…` | a receiver with a higher owner level reads that partition | close it, or use a higher owner level |
| `Exceeded the maximum number of allowed receivers per partition in a consumer group which is 5.` | a sixth receiver without epoch | close receivers, or use another consumer group |
| `Unauthorized access. 'Send' or 'Listen' claim(s) are required…` / 401 `InvalidSignature` | `any_key` off and no valid token | use the key from `list_eventhubs` |
| Kafka: authentication fails | user is not `$ConnectionString`, or the `Endpoint` names no Event Hubs namespace, or the key is wrong with `any_key` off | fix the SASL settings |
| Kafka consumer by timestamp starts at the end | time lookup is not supported | seek by offset (sequence number) |
| Kafka client wants TLS on `9093` | the Kafka endpoint is plaintext on `9092` | `security.protocol=SASL_PLAINTEXT` |

## Not done

- The app cannot create namespaces, hubs or consumer groups (no management REST API); use `create_eventhub`.
- `set_namespace_reachability` does not apply to Event Hubs namespaces; use `set_eventhubs`.
- Throughput units, capture, geo-replication, schema registry, AMQP transactions and idempotent publishing are not
  offered.
- The Kafka endpoint has no time lookup (ListOffsets by timestamp answers the end) and is plaintext on Streams' port,
  not TLS on `9093`.
- Policy rights are not modelled: a valid key of any policy can send and receive.
- No Timeline rows for Event Hubs traffic.

Missing something? Tell the developer what the fix would be and offer a pull request — see [Contributing](../../CONTRIBUTING.md).

## Tools and routes

<!-- tools: eventhubs -->
_Generated by `tools/gen_tool_reference.py` from the engine source; each tool is an MCP tool and the HTTP route shown (under `/api`)._

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_eventhubs` | `GET /api/eventhubs` | — | — | Azure Event Hubs namespaces (AMQP port, the SDKs in emulator mode): each event hub with its partitions (begin and last sequence number, last offset, last enqueued time, how many events it keeps), its consumer groups with the receivers reading each partition now (owner level/epoch, position) and the checkpoints the app wrote with BlobCheckpointStore, retention, the connection string (EntityPath per hub), the Kafka endpoint and what set_eventhubs provokes. |
| `create_eventhub` | `POST /api/eventhubs/{namespace}/hubs` | namespace, hub | partitions, consumer_groups, retention | Creates an event hub (and its namespace when new) with partitions (default 4, can only grow), consumer groups ($Default is always there) and retention (default 24h, on the environment clock). Again on an existing hub: adds partitions and consumer groups, changes retention. |
| `list_eventhub_events` | `GET /api/eventhubs/{namespace}/{hub}/events` | namespace, hub | partition, last | Events of an event hub, newest first, with partition, sequence number, offset, enqueued time (environment clock), partition key, properties, body and who sent it (the app over AMQP or Kafka, you, an agent). |
| `eventhub_send` | `POST /api/eventhubs/{namespace}/{hub}/events` | namespace, hub | body, properties, partition_key, partition, events | Sends events to an event hub as another producer would; the app's receivers get them. One event (body) or a batch (events: [{body, properties, partition_key}]). partition_key is hashed to a partition as the service does; partition picks one. |
| `set_eventhubs` | `PUT /api/eventhubs/{namespace}/settings` | namespace | server_busy, quota_exceeded, slow_sends, not_found, steal_owner, drop_connections, any_key, reset | Event Hubs provocations for a namespace: server_busy (the next N sends get ServerBusy, which the SDKs retry), quota_exceeded (the next N sends get QuotaExceeded), slow_sends (wait before each send is accepted), not_found (hubs that answer 'entity not found' while listed), steal_owner (the receivers of a hub, or all with true, are disconnected as by a higher epoch: OwnershipLost / link stolen), drop_connections (closes every AMQP connection of the namespace now; the SDKs reconnect), any_key. Only what you pass changes; reset=true goes back to normal. |
| `wait_for_eventhub_event` | `GET /api/eventhubs/{namespace}/{hub}/wait` | namespace, hub | where, text, partition, since, timeout | Waits for the first event the app sent (over AMQP or Kafka, not you or an agent) to an event hub that matches: properties or JSON body fields in `where`, text in the body, a partition. 408 after the timeout. |
<!-- /tools -->
