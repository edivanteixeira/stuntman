# ActiveMQ (OpenWire)

Stuntman is an ActiveMQ "Classic" broker over OpenWire, the binary protocol of Apache.NMS.ActiveMQ and the Java
`activemq-client`. The app connects, opens sessions, produces and consumes on queues and topics, acks in every JMS
mode, uses transactions (local and XA), durable subscriptions, selectors, the scheduler and the dead-letter queue as on
a real broker. Stuntman keeps every destination and every message with its state and redelivery count, the
connections with their wire format, the consumers connected now, durable subscriptions and transactions.

ActiveMQ has its own port and its own store; it does not share the Service Bus / RabbitMQ namespaces.

## At a glance

| | |
|---|---|
| **Stands in for** | ActiveMQ Classic 5.x (the broker announces `ActiveMQ` `5.18.6`), OpenWire version 10, loose or tight encoding, with or without the marshal cache (what the client asks for). Tested with .NET `Apache.NMS.ActiveMQ` 2.3.0. The code also handles what the Java `activemq-client` sends (XA, tight encoding, zlib-compressed bodies, blob messages). |
| **Ports** | OpenWire `61616` (`STUNTMAN_ACTIVEMQ_PORT`), plain TCP. No TLS (`ssl://`) variant. |
| **Point your app at** | NMS: `activemq:tcp://localhost:61616`. Java: `tcp://localhost:61616`. Any user and password until you set users. |
| **It keeps** | Queues and topics (the dead-letter queues too), every message with its kind, properties, headers, body, state (`scheduled`, `pending`, `delivered`, `acked`, `dead_letter`, `expired`, `rolled_back`, `discarded`) and redelivery count, the connections logged in now (user, client id, wire format), the consumers connected now (selector, prefetch, durable subscription, noLocal, exclusive, priority), durable subscriptions (online or offline, with what they keep), local and XA transactions. |
| **You can provoke** | Every login refused; a wrong password; the connection dropped after N dispatched messages; a delay before each dispatch; the next N acks lost (redelivered); the next N commits failed and rolled back; the redelivery limit and per-destination DLQs; MessageDelivered / MessageConsumed advisories; scheduled and expiring messages on the environment clock. |
| **Screen** | `/queues/activemq` |
| **Tools** | `list_activemq_destinations`, `list_activemq_messages`, `activemq_send`, `set_activemq`, `cancel_activemq_scheduled`, `wait_for_activemq_ack` |

## Connect your app

One port serves every environment: the **default environment** answers. Destinations are `queue://<name>` and
`topic://<name>` in the tools; a bare name is a queue.

### .NET (`Apache.NMS.ActiveMQ`)

```csharp
using Apache.NMS;
using Apache.NMS.ActiveMQ;

var factory = new ConnectionFactory("activemq:tcp://localhost:61616");
using var conn = factory.CreateConnection("acme", "s3cret");
conn.ClientId = "positions-worker";          // needed for durable subscriptions
conn.Start();
using var session = conn.CreateSession(AcknowledgementMode.ClientAcknowledge);
using var consumer = session.CreateConsumer(session.GetQueue("positions"), "region = 'north'");
var msg = consumer.Receive(TimeSpan.FromSeconds(5)) as ITextMessage;   // null after 5 s
msg?.Acknowledge();
```

### Java (`activemq-client`)

```java
var factory = new ActiveMQConnectionFactory("acme", "s3cret", "tcp://localhost:61616");
try (var conn = factory.createConnection()) {
    conn.start();
    var session = conn.createSession(false, Session.AUTO_ACKNOWLEDGE);
    session.createProducer(session.createQueue("positions")).send(session.createTextMessage("{\"truck\":\"TRK-100\"}"));
}
```

URI options that only the client reads work as usual: `?jms.prefetchPolicy.all=0`, `queue://orders?consumer.exclusive=true`,
`consumer.priority=9`, `jms.redeliveryPolicy.maximumRedeliveries=…`.

### From a container

The host does not matter: `activemq:tcp://stuntman:61616` (compose service name) or `host.docker.internal:61616`.
Map the port: `-p 127.0.0.1:61616:61616` (the line is in `docker-compose.yml`).

### Users

```text
set_activemq {"users": {"acme": "s3cret", "globex": "pw-2"}}     # only these log in
set_activemq {"users": "acme:s3cret,globex:pw-2"}                 # the same
set_activemq {"users": {}}                                         # anyone logs in again
```

With no users set, any user name and password log in. A wrong one gets `java.lang.SecurityException`
`User name [<user>] or password is invalid.`

## What it supports

### Wire format and connection

| Item | Notes |
|---|---|
| Negotiation | Stuntman offers OpenWire 10 with `TightEncodingEnabled`, `CacheEnabled` (cache size 1024), size prefix on, stack traces off, `MaxInactivityDuration` 30 s; the lower version and the AND of the flags win. A client announcing a version below 10 is not served. |
| `BrokerInfo` | broker id `ID:stuntman-1`, URL `tcp://localhost:61616`, name `stuntman` |
| `ConnectionInfo` | user, password, client id; anything before it gets `javax.jms.IllegalStateException` `Send a ConnectionInfo first` |
| `KeepAliveInfo` | answered |
| `ShutdownInfo`, `RemoveInfo` | close connections, sessions, producers, consumers; what a consumer did not ack goes back |
| Unknown command | `java.io.IOException` `Stuntman's ActiveMQ does not implement OpenWire command type <n>` |

### Messages

| Feature | Notes |
|---|---|
| Kinds | `Message`, `TextMessage`, `BytesMessage`, `MapMessage`, `ObjectMessage`, `StreamMessage`, `BlobMessage` (URL and MIME type kept and delivered as they are; Stuntman does not fetch or store the blob) |
| Headers | `JMSCorrelationID`, `JMSType`, `JMSReplyTo`, `JMSXGroupID`, `JMSXGroupSeq`, `JMSXUserID`, `JMSDeliveryMode` (persistent), `JMSPriority`, `JMSTimestamp`, `JMSExpiration` |
| Properties | every primitive type, kept and delivered |
| Compressed bodies | inflated on arrival (zlib from Java, gzip from NMS 1.x), kept and dispatched as plain text or bytes |
| Producer flow | a `ProducerAck` per async send when the producer has a `producerWindowSize` |
| Temporary destinations | `temp-queue://`, `temp-topic://` addresses work as destinations |

### Consumers and acks

| Feature | Notes |
|---|---|
| Prefetch | as the client sets it; prefetch 0 uses `MessagePull`, so `Receive(timeout)` returns null on time |
| Ack modes | auto, client, individual and transacted acks; delivered acks free prefetch room; standard and individual acks settle; poison acks dead-letter; expired acks expire |
| Queue browsers | copies of what is pending (selector applied), then the end-of-browse null dispatch |
| Selectors | the JMS SQL-92 subset: `=`, `<>`, `<`, `>`, `<=`, `>=`, `AND`, `OR`, `NOT`, `[NOT] IN`, `[NOT] LIKE … ESCAPE`, `[NOT] BETWEEN`, `IS [NOT] NULL`, arithmetic, string, number and boolean literals; header fields `JMSType`, `JMSCorrelationID`, `JMSPriority`, `JMSDeliveryMode`, `JMSTimestamp`, `JMSExpiration`, `JMSMessageID`, `JMSRedelivered`, `JMSXDeliveryCount`, `JMSXGroupID`, `JMSXGroupSeq`, `JMSXUserID`, and properties. An invalid one: `javax.jms.InvalidSelectorException` `Invalid selector '<s>': …` |
| `noLocal` | a consumer does not get what its own connection sent |
| Exclusive consumers | `consumer.exclusive=true` on a queue: the first takes the queue; the next takes over when it leaves |
| Consumer priority | `consumer.priority=9`: higher-priority consumers get queue messages first while they have prefetch room |
| Dispatch order | `JMSPriority` (9 first), then send order |
| Message groups | `JMSXGroupID` sticks to one consumer until it leaves or a message with `JMSXGroupSeq` `-1` closes the group |
| Topic wildcards | `*` one segment, `>` the rest |
| Composite destinations | a consumer on `a,b` gets nothing (the clients' own `TempQueue,TempTopic` advisory watch is accepted and idle) |

### Topics and durable subscriptions

- A plain topic subscriber gets a copy only while connected; when it leaves, its unread copies are `discarded`. A
  message to a topic with no subscriber is kept as `discarded`.
- A consumer with a subscription name and a client id is durable: it keeps its copies while offline and gets them
  when it comes back. A different topic or selector replaces the subscription, as ActiveMQ does. `Unsubscribe`
  deletes it.
- Errors: `You cannot create a durable subscriber without specifying a unique clientID on a Connection`,
  `Durable consumer is in use for client: <c> and subscriptionName: <n>`, `No durable subscription exists for
  clientID: <c> and subscriptionName: <n>`.

### Transactions

- **Local.** A transacted session's sends stay invisible and its acks take no effect until the commit. A rollback
  marks the sends `rolled_back`; the client redelivers what it had received, with the redelivery count up.
- **XA** (Java `XAResource`): start, end, prepare (`XA_RDONLY` when nothing was done in it), one-phase and two-phase
  commit, rollback, forget, and recover (lists prepared transactions from any connection). A two-phase commit of an
  unprepared transaction: `Cannot do 2 phase commit if the transaction has not been prepared`.
- A connection that goes away rolls back what it left open, except what was prepared.
- Errors are `javax.jms.TransactionRolledBackException` (local) or `javax.transaction.xa.XAException` (XA):
  `Transaction '<id>' has not been started.`, `Transaction '<id>' is already prepared.`

### Dead letters

As ActiveMQ does, a message goes to the dead-letter queue when:

- the client poisons it (its redelivery policy gave up),
- it was redelivered more than `max_redeliveries` times (default 6, `-1` never): cause `Delivery[<n>] exceeds
  redelivery policy limit: maximumRedeliveries = <max>`,
- it is persistent and its `JMSExpiration` passed while it waited (a non-persistent one is only dropped).

The DLQ is `queue://ActiveMQ.DLQ`, or with `individual_dlq` `queue://ActiveMQ.DLQ.Queue.<name>` /
`queue://ActiveMQ.DLQ.Topic.<name>`. The copy carries the `dlqDeliveryFailureCause` property and its original
destination; the original keeps its state (`dead_letter` or `expired`) and points at the copy (`movedTo`). The app
consumes the DLQ like any queue; a message already on a DLQ stays there.

### Scheduler

| Property | Effect |
|---|---|
| `AMQ_SCHEDULED_DELAY` | ms before delivery: `scheduled` until then |
| `AMQ_SCHEDULED_PERIOD` + `AMQ_SCHEDULED_REPEAT` | repeat N: N+1 deliveries, `PERIOD` ms apart |
| `AMQ_SCHEDULED_CRON` | 5 fields, UTC; each firing delivers once plus its repeats; runs until cancelled |

Times run on the [environment clock](../clock.md): `advance_clock` brings scheduled messages (and expirations)
forward. The app manages jobs over JMS by sending to `topic://ActiveMQ.Scheduler.Management` with
`AMQ_SCHEDULER_ACTION`:

| Action | Effect |
|---|---|
| `BROWSE` | each scheduled message goes to the message's `JMSReplyTo`, with `scheduledJobId` (needs a `JMSReplyTo`) |
| `REMOVE` | the job `scheduledJobId` (required) |
| `REMOVEALL` | every job |

`BROWSE` and `REMOVEALL` take `ACTION_START_TIME` / `ACTION_END_TIME` (next delivery time, ms). A job's id is its
message id. An invalid cron is refused (`AMQ_SCHEDULED_CRON: …`, or `… never fires`).

### Advisories

On `ActiveMQ.Advisory.*` topics, with the real payloads as the message's data structure (type `Advisory`), made only
while someone listens. Advisory topics stay out of the destination list.

| Topic | Payload |
|---|---|
| `ActiveMQ.Advisory.Connection` | `ConnectionInfo` (without the password), `RemoveInfo` |
| `ActiveMQ.Advisory.Consumer.Queue.<name>` / `.Topic.<name>` | `ConsumerInfo`, `RemoveInfo`; property `consumerCount` |
| `ActiveMQ.Advisory.Producer.Queue.<name>` / `.Topic.<name>` | `ProducerInfo`, `RemoveInfo`; property `producerCount` |
| `ActiveMQ.Advisory.Queue`, `ActiveMQ.Advisory.Topic` | `DestinationInfo`, added or removed |
| `ActiveMQ.Advisory.Expired.*`, `ActiveMQ.Advisory.MessageDLQd.*` | the message without its body, `orignalMessageId` |
| `ActiveMQ.Advisory.MessageDelivered.*`, `.MessageConsumed.*` | only with `advisory_delivered` / `advisory_consumed` on |

## How it behaves

- A consumer that leaves gives back what it did not ack: it goes back to `pending`; what the app had already seen
  comes again as redelivered (the redelivery count goes up, and past `max_redeliveries` it is dead-lettered).
- Destinations are created when a producer, a consumer or a message names them.
- Persistence: messages, destinations, durable subscriptions, transactions and settings are in the environment's
  SQLite and survive a restart. Connected consumers and message-group owners are in memory.
- Scheduled times and expirations use the environment clock; `enqueuedAt`, `deliveredAt`, `ackedAt` are wall-clock
  times.
- `set_activemq` settings are kept per environment (only the default environment is reachable on the port).

## Provoke failures

| Failure | How | What the app sees | Turn it off |
|---|---|---|---|
| Every login fails | `set_activemq {"refuse_login": true}` | `java.lang.SecurityException` `User name [<u>] or password is invalid.` on connect | `refuse_login: false` |
| Wrong password | `set_activemq {"users": {"acme": "s3cret"}}` | other users or passwords: the same SecurityException | `users: {}` |
| Connection drops | `set_activemq {"drop_after": 3}` | each connection is closed after it was sent 3 messages (every connection, until reset) | `drop_after: 0` |
| Slow broker | `set_activemq {"slow_dispatch": "2s"}` | a 2 s wait before each dispatch | `slow_dispatch: 0` |
| Acks lost | `set_activemq {"redeliver": 2}` | the next 2 acked messages come again with a higher redelivery count | counts down to 0 |
| Commits fail | `set_activemq {"fail_commits": 1}` | the next commit fails with `TransactionRolledBackException` (XA: `XAException`) `Commit failed (set_activemq fail_commits): the transaction was rolled back` and rolls back | counts down to 0 |
| Poison messages sooner | `set_activemq {"max_redeliveries": 2}` | the third redelivery goes to the DLQ | `max_redeliveries: 6` |
| One DLQ per destination | `set_activemq {"individual_dlq": true}` | dead letters land in `ActiveMQ.DLQ.Queue.<name>` | `false` |
| A message expires | `activemq_send {"destination": "queue://positions", "text": "x", "ttl": "30s"}` then `advance_clock` | nobody gets it; a persistent one appears on the DLQ | |
| A partner message arrives | `activemq_send {…}` | a normal message, dispatched to the app's consumers | |
| Everything back | `set_activemq {"reset": true}` | defaults | |

## See what the app did

| Need | Tool | Route |
|---|---|---|
| Destinations with counts per state, consumers now, durable subscriptions, transactions, connections, settings | `list_activemq_destinations` | `GET /api/activemq/destinations` |
| Messages, newest first | `list_activemq_messages {"destination", "state", "last"}` | `GET /api/activemq/messages` |
| Wait until the app acked | `wait_for_activemq_ack {"destination", "seq" \| "count", "timeout"}` | `GET /api/activemq/wait-ack` |

- `list_activemq_destinations` returns `destinations` (each with `name`, `kind`, `counts`, `consumers` with `id`,
  `prefetch`, `remote`, `user`, `since`, `selector`, `durable`, `noLocal`, `exclusive`, `priority`, `wire`; topics add
  `subscriptions` with `clientId`, `name`, `selector`, `lastSeen`, `pending`, `active`), `transactions` (`kind`,
  `state`, `sends`, `acks`), `connections` (`remote`, `user`, `clientId`, `wire`: `loose`, `loose+cache`, `tight`,
  `tight+cache`), `url`, `port`, `openwireVersion`, `bindError`, `settings`.
- A message has `seq`, `destination`, `messageId`, `kind`, `properties`, `headers`, `state`, `consumer`,
  `redeliveryCount`, `enqueuedAt`, `deliveredAt`, `ackedAt`, `sender`, `via` (`openwire` for the app), `text` (text
  messages) or `bodyBase64`, and when they apply `subscriber`, `transaction`, `deliverAt`, `expiresAt`, `schedule`,
  `movedTo` (`seq`, `destination`). A DLQ copy names its `originalDestination` in `headers`.
- `wait_for_activemq_ack` waits for one message (`seq`, acked or dead-lettered), at least `count` acked messages, or
  (neither) everything sent to the destination settled: nothing `scheduled`, `pending` or `delivered` and at least one
  `acked`. It returns the counts and the last 50 messages; 408 `<destination>: not acked yet` after `timeout`
  (default 30 s).

ActiveMQ traffic does not write Timeline rows; read it with these tools or on `/queues/activemq`.

## Recipes

1. **Feed the app a partner message and check it was processed.**
   ```text
   activemq_send          {"destination": "queue://positions", "text": "{\"truck\": \"TRK-100\"}", "properties": {"region": "north"}}
   wait_for_activemq_ack  {"destination": "queue://positions"}
   ```

2. **Test the poison-message path.**
   ```text
   set_activemq           {"max_redeliveries": 2, "individual_dlq": true}
   activemq_send          {"destination": "queue://orders", "text": "{\"orderId\": \"bad\"}"}
   list_activemq_messages {"destination": "queue://ActiveMQ.DLQ.Queue.orders"}
   ```
   The original shows `dead_letter` with `movedTo`; the copy has `dlqDeliveryFailureCause`.

3. **Make the consumer survive a dropped connection.**
   ```text
   set_activemq {"drop_after": 5}
   activemq_send {"destination": "queue://positions", "text": "p", "count": 20}
   wait_for_activemq_ack {"destination": "queue://positions", "count": 20, "timeout": "60s"}
   set_activemq {"drop_after": 0}
   ```

4. **Test the commit-retry path of a transacted session.**
   ```text
   set_activemq {"fail_commits": 1}
   list_activemq_destinations                  → transactions: the rolled-back one, then the committed retry
   ```

5. **Scheduled and recurring messages on the environment clock.**
   ```text
   activemq_send {"destination": "queue://positions", "text": "later", "delay": "5s", "priority": 9, "ttl": "1m"}
   activemq_send {"destination": "queue://reports", "text": "nightly", "cron": "0 2 * * *"}
   advance_clock {"by": "1d"}
   cancel_activemq_scheduled {"destination": "queue://reports"}
   ```
   `activemq_send` also takes `period`, `repeat`, `correlation_id`, `type`, `reply_to`, `group_id`, `persistent`
   (default true), `kind` (`text`, `bytes` with `body_base64`, `blob` with `blob_url` and `mime_type`) and `count`
   (up to 1000 copies).

6. **Check a durable subscriber keeps messages while offline.**
   ```text
   list_activemq_destinations                                        → the subscription, active: false
   activemq_send {"destination": "topic://prices", "text": "42.10"}
   list_activemq_destinations                                        → pending: 1 on that subscription
   ```

## Errors and troubleshooting

| Symptom or error text | Cause | Fix |
|---|---|---|
| `User name [<u>] or password is invalid.` | users are set and this one is not, or `refuse_login` | `set_activemq {"users": {…}}` or `refuse_login: false` |
| Connection refused on `61616` | the port was taken at start (`bindError` in `list_activemq_destinations`), or not published from the container | free the port, set `STUNTMAN_ACTIVEMQ_PORT`, map `61616` |
| `ssl://` connection fails | no TLS on this port | use `tcp://` |
| `Send a ConnectionInfo first` | a command before the connection was set up | let the client connect normally |
| `Invalid selector '<s>': …` | a selector outside the supported subset | simplify it |
| `You cannot create a durable subscriber without specifying a unique clientID…` | durable subscriber without `ClientId` | set the connection's client id |
| `Durable consumer is in use for client: …` | a second consumer on the same durable subscription | close the first |
| `Stuntman's ActiveMQ does not implement OpenWire command type <n>` | a command Stuntman lacks | see [Not done](#not-done) |
| `AMQ_SCHEDULER_ACTION BROWSE needs a JMSReplyTo` / `REMOVE needs scheduledJobId` | incomplete scheduler management message | add the header or property |
| Messages wait in a topic as `discarded` | nobody was subscribed when they were sent | subscribe first, or use a durable subscription |
| The app in another environment sees nothing | the port only serves the default environment | point that environment's app at the default one, or run the test there |

## Not done

- No TLS port (`ssl://`, `nio+ssl://`), no STOMP, AMQP or MQTT transports of ActiveMQ.
- One port for every environment: only the default environment answers.
- The advisories a broker network or memory limits produce: `TempQueue`/`TempTopic`, `SlowConsumer`,
  `FastProducer`, `FULL`, `MasterBroker`, `NetworkBridge`. A consumer on a composite destination gets nothing.
- Blob messages: the broker does not fetch or store the blob.
- No broker-side redelivery delay, no memory or store limits (producer flow control only answers `ProducerAck`).
- No JMX, web console or REST API of ActiveMQ.

Missing something? Tell the developer what the fix would be and offer a pull request — see [Contributing](../../CONTRIBUTING.md).

## Tools and routes

<!-- tools: activemq -->
_Generated by `tools/gen_tool_reference.py` from the engine source; each tool is an MCP tool and the HTTP route shown (under `/api`)._

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_activemq_destinations` | `GET /api/activemq/destinations` | — | — | ActiveMQ (OpenWire, port 61616) queues and topics with their messages per state (scheduled, pending, delivered, acked, dead_letter, expired, rolled_back, discarded), the consumers connected now (with their selector and durable subscription), each topic's durable subscriptions (online or offline, with what they keep), the open and recent transactions (local and XA, prepared ones included), the connections logged in now with their negotiated wire format (loose or tight, with or without the marshal cache), the broker URL and what set_activemq changed. |
| `list_activemq_messages` | `GET /api/activemq/messages` | — | destination, state, last | ActiveMQ messages, newest first, with their state: scheduled (AMQ_SCHEDULED_DELAY not over), pending (waiting), delivered (dispatched, not acked yet), acked, dead_letter (poison ack), expired (JMSExpiration passed), rolled_back (sent in a transaction that rolled back), discarded (topic copy whose subscriber left). A message still inside an open transaction carries its id; a repeating or cron one its schedule; one moved to the dead-letter queue (expired, or poisoned past max_redeliveries) points at its copy there (movedTo), and the copy names its originalDestination. |
| `activemq_send` | `POST /api/activemq/messages` | destination | text, kind, blob_url, mime_type, body_base64, properties, correlation_id, type, reply_to, group_id, persistent, priority, count, delay, ttl, period, repeat, cron | Puts a message on an ActiveMQ queue (or topic) as the partner system would; it is dispatched to the app's consumers. Recorded as sent by you or the agent. |
| `set_activemq` | `PUT /api/activemq/settings` | — | users, refuse_login, drop_after, slow_dispatch, redeliver, fail_commits, max_redeliveries, individual_dlq, advisory_delivered, advisory_consumed, reset | ActiveMQ broker behaviour: users (name → password; empty: anyone logs in), refuse_login (every login fails with a SecurityException), drop_after (close the connection after N dispatched messages), slow_dispatch (wait before each dispatch), redeliver (the next N acks are lost: those messages come again with a higher redelivery count), fail_commits (the next N transaction commits fail with a TransactionRolledBackException, or an XAException for XA, and roll back), max_redeliveries (past it a message goes to the dead-letter queue; default 6), individual_dlq (one DLQ per destination instead of ActiveMQ.DLQ), advisory_delivered / advisory_consumed (the MessageDelivered / MessageConsumed advisory topics, off by default as in ActiveMQ). Only what you pass changes; reset=true goes back to defaults. |
| `cancel_activemq_scheduled` | `POST /api/activemq/scheduled/cancel` | — | seq, destination | Cancels scheduled ActiveMQ messages (AMQ_SCHEDULED_DELAY, _PERIOD/_REPEAT, _CRON) before they go out: one (seq) or all of a destination. They are kept as discarded. |
| `wait_for_activemq_ack` | `GET /api/activemq/wait-ack` | destination | seq, count, timeout | Waits until the app acked: one message (seq), at least `count` messages of the destination, or (neither) everything sent to it: nothing pending or delivered and at least one acked. 408 after the timeout. |
<!-- /tools -->
