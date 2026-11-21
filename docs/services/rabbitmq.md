# RabbitMQ

Stuntman answers RabbitMQ's AMQP 0-9-1 protocol and the RabbitMQ management HTTP API. The app declares exchanges and
queues, binds them, publishes (with publisher confirms and `mandatory`), consumes with prefetch, acks, nacks and
rejects with its normal client; provisioning scripts and `rabbitmqadmin` create vhosts, users, permissions, policies,
queues, exchanges and bindings over HTTP. Stuntman keeps every vhost as a YAML file, every message with its full
delivery history, every connection that consumes, and every change made through the management API.

RabbitMQ shares one engine, one AMQP port and the same tools with [Azure Service Bus](servicebus.md): the namespace's
kind decides the protocol. The tools use one vocabulary for both: a **vhost is a namespace**, an **exchange is a
topic**, a **binding is a subscription** (named after the queue it points at), and messages live in **queues**.

![Queues: messages with their attempts, dead-letter and consumers](../images/screen-queues.png)

## At a glance

| | |
|---|---|
| **Stands in for** | RabbitMQ 3.13 / 4 (AMQP 0-9-1; the server says `RabbitMQ 3.13.7 (Stuntman)`), its management API (`management_version` 3.13.7), quorum queues and policies with RabbitMQ 4 rules. Tested with .NET `RabbitMQ.Client` 7.2.2, MassTransit's topology (exchange-to-exchange bindings), and `rabbitmqadmin` v2 style calls (password hashes). |
| **Ports** | AMQP `5673` (`STUNTMAN_AMQP_PORT`), AMQPS `5671` (`STUNTMAN_AMQPS_PORT`, a certificate from the Stuntman Test CA). Management API `15672` (`STUNTMAN_RABBITMQ_MGMT_PORT`, plain HTTP). Note: AMQP is `5673`, not RabbitMQ's `5672`. |
| **Point your app at** | `amqp://guest:guest@localhost:5673/<namespace>` (any user and password by default). Management: `http://localhost:15672`, any Basic credentials by default. |
| **It keeps** | Vhosts (`messaging/<namespace>.yaml`: queues with their arguments, exchanges with their type, bindings with their keys, policies, permissions, the default queue type), management users (`messaging/_rabbitmq-users.yaml`, password hashes in `secrets.yaml`), every message with properties, headers, body and history, consumers (connected now and in the last day), messages that reached no queue. |
| **You can provoke** | The next message (or every one) on a queue: ack lost (redelivered), delivered twice, held, dropped, dead-lettered, out of order. Rules on publish or delivery (by queue, subject, header, size, sender, delivery count). Refusals on publish (nack, channel errors). A throughput limit. A vhost that refuses connections, never answers, rejects the login, is blocked, or dies (`down`: TCP drop; `close`: 320 `CONNECTION_FORCED`). A connection closed by name. A vhost deleted under the app. Max-length, delivery limits and dead-letter exchanges through policies. |
| **Screen** | `/queues` (messages), `/queues/subscriptions` (bindings), `/queues/dead-letter`, `/queues/consumers`, `/queues/rules`, `/queues/settings` |
| **Tools** | `list_namespaces`, `create_account`, `get_account`, `add_entity`, `add_subscription`, `publish`, `trace_message_route`, `list_queue_messages`, `get_queue_message`, `wait_for_message`, `set_next_delivery`, `create_rule`, `list_consumers`, `disconnect_consumer`, `set_namespace_reachability`, `set_throughput`, `set_auto_create`, `list_messaging_warnings`, `resubmit` |

## Connect your app

### The vhost is the namespace

The host name does not matter; the vhost picks the namespace and the environment.

| Vhost | Namespace |
|---|---|
| `acme-rabbit` | the RabbitMQ namespace `acme-rabbit`, looked up in the default environment first, then the others. A name that exists nowhere is **created** in the default environment (so the app works on day zero), if it is a valid namespace name. |
| `pr-5462/acme-rabbit` (URI: `pr-5462%2Facme-rabbit`) | namespace `acme-rabbit` of environment `pr-5462`; it must exist |
| `/` (URI: `%2F`, or no path) | the default environment's RabbitMQ namespace named `default`, else its first RabbitMQ namespace by name; `default` is created when there is none |

Namespace (vhost) names: 3 to 50 lowercase letters, numbers or dashes, starting with a letter. A vhost name outside
these rules (`Acme_Bus`) is not created: the connection is closed with `530 NOT_ALLOWED - vhost <name> not found`.
Queue and exchange names: letters, numbers, dots, dashes, underscores or colons, at most 260 characters.

`list_namespaces` marks the namespace that `/` stands for with `defaultVhost: true`, and every messaging tool takes
`/` as its name (`{"namespace": "/"}`).

```text
# the namespace's own connection string (get_account or list_namespaces → connection)
amqp://guest:<password>@localhost:5673/acme-rabbit
# another environment
amqp://guest:<password>@localhost:5673/pr-5462%2Facme-rabbit
# TLS, Test CA trusted
amqps://guest:<password>@acme-rabbit.localhost:5671/acme-rabbit
```

From a container, use the compose service name or `host.docker.internal` as the host:
`amqp://guest:guest@stuntman:5673/acme-rabbit`. The host plays no part in finding the namespace.

### Users and passwords

Three cases, checked in this order:

1. **A user declared through the management API** (`PUT /api/users/<name>`): the password must match (else `403
   ACCESS_REFUSED - Login was refused using authentication mechanism PLAIN…`), and the vhost must have a permission
   for that user (else `530 NOT_ALLOWED - access to vhost '<vhost>' refused for user '<user>'`).
2. **Anyone else, "Accept any key" on** (the default): any user name and password get in.
3. **Anyone else, "Accept any key" off**: the user must be one of the namespace's users (`access.policies`; a new
   namespace has `guest`) with its password. The password is a random value in `secrets.yaml`
   (`messaging.<namespace>.guest`), shown masked by `get_account`; `rotate_policy_key` makes a new one.

```text
update_account {"service": "rabbitmq", "account": "acme-rabbit", "patch": {"access": {"any_key": false}}}
```

Mechanisms: `PLAIN` and `AMQPLAIN`.

### .NET (`RabbitMQ.Client` 7)

```csharp
using RabbitMQ.Client;

var factory = new ConnectionFactory
{
    Uri = new Uri("amqp://guest:guest@localhost:5673/acme-rabbit"),
    // Shown on the Consumers tab and in /api/connections: tells copies of the app apart.
    ClientProvidedName = $"orders-worker pid {Environment.ProcessId} {Environment.CurrentDirectory}"
};
await using var conn = await factory.CreateConnectionAsync();
await using var ch = await conn.CreateChannelAsync(new CreateChannelOptions(publisherConfirmationsEnabled: true, publisherConfirmationTrackingEnabled: true));
await ch.ExchangeDeclareAsync("order-created", ExchangeType.Topic, durable: true, autoDelete: false);
await ch.QueueDeclareAsync("order-created.invoices", durable: true, exclusive: false, autoDelete: false,
    arguments: new Dictionary<string, object?> { ["x-queue-type"] = "quorum", ["x-dead-letter-exchange"] = "dlx" });
await ch.QueueBindAsync("order-created.invoices", "order-created", "#");
await ch.BasicPublishAsync("order-created", "orders.eu", mandatory: true,
    new BasicProperties { Persistent = true, ContentType = "application/json", MessageId = "m-58213" },
    "{\"orderId\":58213}"u8.ToArray());
```

### Python (`pika`)

```python
import pika

conn = pika.BlockingConnection(pika.URLParameters("amqp://guest:guest@localhost:5673/acme-rabbit"))
ch = conn.channel()
ch.queue_declare("notifications", durable=True)
ch.basic_publish("", "notifications", b'{"to": "jane.doe@example.com"}', pika.BasicProperties(content_type="application/json"))
```

### Tell the app's connections apart

The client's `connection_name` property (`ClientProvidedName` in .NET) names the connection on the Consumers tab and
in `/api/connections`. Without it, Stuntman shows the client's `product`, its platform and its port
(`RabbitMQ .NET :53122`).

## What it supports

### Connection and channels

| Method | Notes |
|---|---|
| `connection.start` | product `RabbitMQ`, version `3.13.7 (Stuntman)`, capabilities `publisher_confirms`, `exchange_exchange_bindings`, `basic.nack`, `consumer_cancel_notify`, `connection.blocked`, `authentication_failure_close`, `per_consumer_qos`; mechanisms `PLAIN AMQPLAIN` |
| `connection.tune` | `channel_max` 2047, `frame_max` 131072, heartbeat 60 s (the client's value is used; Stuntman sends heartbeats at half of it) |
| `connection.open` | the vhost picks the namespace (above) |
| `connection.blocked` | sent right after `open-ok` when the vhost is set to `busy` |
| `connection.close` / `close-ok` | both ways; Stuntman closes with 320 `CONNECTION_FORCED` (disconnect, vhost deleted, `close` mode), 403, 530 |
| `channel.open`, `close`, `close-ok`, `flow` | `flow` is echoed; channel errors close the channel with the code, as RabbitMQ |
| `confirm.select` | publisher confirms: one `basic.ack` per publish, after the message is stored |
| `tx.*` | not supported: the channel is closed with 540 `NOT_IMPLEMENTED - Stuntman does not do transactions; use publisher confirms` |

### Exchanges, queues, bindings

| Method | Notes |
|---|---|
| `exchange.declare` | types `topic`, `direct`, `fanout`, `headers` (empty type: `direct`); `passive` (404 when missing); a redeclare with another type closes the channel with 406 `PRECONDITION_FAILED - inequivalent arg 'type'…`. Names starting with `amq.` are answered but not created. |
| `exchange.delete` | `if-unused` (in use = it is the source of a binding): 406 |
| `exchange.bind` / `unbind` | exchange-to-exchange bindings, followed when routing |
| `queue.declare` | `passive` (404 when missing); an empty name makes `amq.gen-…`; arguments are kept; `x-queue-type` (`classic`, `quorum`, `stream`) or the vhost's default type for durable, non-exclusive queues; a redeclare with another `x-queue-type` closes the channel with 406. `declare-ok` returns ready messages and consumers now. `amq.rabbitmq.reply-to` (passive) answers ok. |
| `queue.bind` / `unbind` | one binding may carry several keys; headers bindings keep their arguments; binding to the default exchange is refused (403 `ACCESS_REFUSED - operation not permitted on the default exchange`); a missing queue or exchange is 404 |
| `queue.purge` | removes ready messages; returns how many |
| `queue.delete` | `if-unused` (consumers now), `if-empty` (ready messages): 406 `PRECONDITION_FAILED - queue '<q>' in vhost '<v>' in use` / `not empty`; removes the bindings to it; an exchange of the same name stays |

A queue and an exchange may share a name (MassTransit does this): they are kept apart.

### Routing

| Exchange type | Rule |
|---|---|
| default (`""`) | to the queue named by the routing key; no such queue: dropped (and returned with `mandatory`) |
| `direct` | a binding key equal to the routing key |
| `topic` | words split by `.`; `*` is one word, `#` zero or more |
| `fanout` | every binding |
| `headers` | every binding argument `k=v` must equal the message header (`x-match` `all`) |

Exchange-to-exchange bindings are followed; each queue gets one copy even when reached twice. A message no binding
takes is dropped, as in RabbitMQ: with `mandatory` the publisher gets `basic.return` 312 `NO_ROUTE`.
`list_messaging_warnings` lists it as `unrouted` (exchange, routing key, count, last time, sender) until a binding
takes that key.

### Publishing and consuming

| Method | Notes |
|---|---|
| `basic.publish` | to the default exchange, a declared exchange, or (Direct Reply-to) `amq.rabbitmq.reply-to.<id>`; `mandatory` honoured; a missing exchange closes the channel with 404 `NOT_FOUND - no exchange '<x>' in vhost '<v>'` |
| Properties | `content-type`, `content-encoding`, `headers`, `delivery-mode`, `priority`, `correlation-id`, `reply-to`, `expiration`, `message-id`, `timestamp`, `type`, `user-id`, `app-id` are kept and delivered |
| `basic.qos` | `prefetch_count` per channel (the `global` flag and `prefetch_size` are ignored) |
| `basic.consume` | `no_ack` or manual ack; consumer tag from the client or `amq.ctag-…`; a missing queue closes the channel with 404 |
| `basic.cancel` | stops the consumer |
| `basic.get` | `get-ok` with the remaining count, or `get-empty` |
| `basic.ack` | `multiple` honoured; an unknown delivery tag closes the channel with 406 `PRECONDITION_FAILED - unknown delivery tag <n>` |
| `basic.nack` / `basic.reject` | `requeue=true`: back to the queue, redelivered; `requeue=false`: dead-lettered (to the dead-letter exchange, if any) |
| `basic.recover` / `recover-async` | every unacked message of the channel goes back |
| Deliveries | `redelivered` is set from the second delivery on; exchange and routing key as published |

**Direct Reply-to (RPC).** A channel that consumes `amq.rabbitmq.reply-to` (auto-ack only, else 406
`PRECONDITION_FAILED - reply consumer cannot acknowledge`) and publishes with `reply_to: amq.rabbitmq.reply-to` gets
`reply_to` rewritten to `amq.rabbitmq.reply-to.<id>`. A publish to that key on the default exchange, from any
connection, goes straight to that channel without a queue. When the requester has gone the reply is dropped
(returned with `mandatory`). Asking for a reply without consuming first: 406 `PRECONDITION_FAILED - fast reply
consumer does not exist`. Replies do not pass through a queue: the queue screens and the message history do not show
them.

### Quorum queues, policies and limits (RabbitMQ 4 rules)

- `x-queue-type: quorum`, or the vhost's `default_queue_type`, makes a quorum queue (durable, non-exclusive queues
  only).
- A policy (`/api/policies/<vhost>/<name>`) applies to queues or exchanges whose name matches its `pattern` (a regex)
  and whose kind fits `apply-to` (`all`, `queues`, `classic_queues`, `quorum_queues`, `streams`, `exchanges`); the
  highest `priority` wins.
- Limits from `x-` arguments and from the policy: the **lower** one wins.

| Limit | Source | Effect |
|---|---|---|
| `max-length` | `x-max-length` or policy | with `overflow` `drop-head` (default): the oldest ready message is dropped (reason `maxlen`); with `reject-publish` / `reject-publish-dlx`: the new message is refused (a `basic.nack` with confirms, dropped without) |
| `delivery-limit` | `x-delivery-limit` or policy; quorum queues only; **20** when unset (negative: none) | a message returned to the queue (its channel or connection closed with it unacked) more times than the limit is dropped (reason `delivery_limit`); returns are counted in the `x-delivery-count` header. A nack or reject with requeue does not count. |
| `dead-letter-exchange` | `x-dead-letter-exchange` or policy | where rejected, dropped and over-limit messages go |
| `dead-letter-routing-key` | `x-dead-letter-routing-key` or policy | the routing key for that publish (else the original) |

A dead-lettered message goes to the dead-letter exchange with an `x-death` header (`count`, `reason`, `queue`,
`time`, `exchange`, `routing-keys`) and `x-first-death-reason|queue|exchange` and `x-last-death-…` headers, reasons
`rejected`, `maxlen`, `delivery_limit`. Without a dead-letter exchange the message is discarded (the screen still
keeps it as dead-lettered, with "a real RabbitMQ discards it").

### Management API (`:15672`)

The vhosts of the management API are the RabbitMQ namespaces of the **default environment**. What a script creates
here is what the app finds on AMQP. Errors are JSON `{"error": …, "reason": …}`. A `PUT` answers 201 when it created
the object and 204 when it changed an existing one.

| Route | Methods | Notes |
|---|---|---|
| `/api/overview` | GET | versions, node `stuntman@localhost`, queue and exchange totals |
| `/api/whoami` | GET | the Basic user and its tags (`administrator` for a user nobody declared) |
| `/api/healthchecks/node` | GET | `{"status": "ok"}` |
| `/api/health/checks/alarms`, `/api/health/checks/local-alarms` | GET | 503 with a memory alarm per vhost set to `busy`; else ok |
| `/api/aliveness-test/{vhost}` | GET | ok, or 404 for an unknown vhost |
| `/api/vhosts` | GET | `/` first, then every RabbitMQ namespace |
| `/api/vhosts/{vhost}` | GET, PUT, DELETE | PUT creates the namespace (name rules above; 400 otherwise) or updates it; `default_queue_type` in the body (`classic`, `quorum`, `stream`); a PUT **without** it sets it back to classic, as RabbitMQ does. A declared user who creates a vhost gets full permissions on it. An unknown vhost is 404 on GET; it is not created. DELETE removes the namespace and its messages and closes its connections with 320 `CONNECTION_FORCED - vhost '<name>' is deleted`. `PUT /api/vhosts/%2F` answers 204 (the `/` namespace is made if needed). |
| `/api/vhosts/{vhost}/permissions` | GET | |
| `/api/vhosts/{vhost}/connections` | GET | the vhost's open AMQP connections |
| `/api/users` | GET | declared users |
| `/api/users/{user}` | GET, PUT, DELETE | PUT with `password` or `password_hash` (RabbitMQ's `rabbit_password_hashing_sha256`: base64 of a 4-byte salt and SHA-256 of salt + password) and `tags` (a comma list or an array); a new user without either: 400 `Mandatory fields missing`. GET `guest` answers even when undeclared. DELETE removes the user's permissions too. |
| `/api/users/{user}/permissions` | GET | |
| `/api/permissions` | GET | |
| `/api/permissions/{vhost}/{user}` | GET, PUT, DELETE | `configure`, `write`, `read` patterns (kept, not checked per operation) |
| `/api/policies`, `/api/policies/{vhost}` | GET | |
| `/api/policies/{vhost}/{name}` | GET, PUT, DELETE | `pattern` and `definition` required (400 `[{key_missing,pattern}]`); `apply-to` (default `all`), `priority`; GET returns what was PUT |
| `/api/queues`, `/api/queues/{vhost}` | GET | `?columns=name,messages…` honoured |
| `/api/queues/{vhost}/{name}` | GET, PUT, DELETE | PUT with `durable`, `exclusive`, `arguments`; DELETE with `?if-unused=true`, `?if-empty=true` (400 when not met); a quorum queue refuses both flags (400 `… not supported by quorum queues`), as RabbitMQ 4 |
| `/api/queues/{vhost}/{name}/contents` | DELETE | purges ready messages |
| `/api/exchanges`, `/api/exchanges/{vhost}` | GET | the default exchange (`""`) first |
| `/api/exchanges/{vhost}/{name}` | GET, PUT, DELETE | PUT with `type` (default `direct`); DELETE with `?if-unused=true` |
| `/api/bindings`, `/api/bindings/{vhost}` | GET | the default exchange's binding to each queue included |
| `/api/bindings/{vhost}/e/{exchange}/q/{queue}` | GET, POST | POST with `routing_key` and `arguments`: 201 |
| `/api/bindings/{vhost}/e/{exchange}/e/{destination}` | GET, POST | exchange-to-exchange |
| `/api/bindings/{vhost}/e/{exchange}/q/{queue}/{props}`, `…/e/{exchange}/e/{destination}/{props}` | DELETE | `props` is the routing key, `~` for an empty one |
| `/api/connections` | GET | open AMQP connections of the default environment |
| `/api/connections/{name}` | GET, DELETE | DELETE closes it with 320 `CONNECTION_FORCED - <X-Reason header, else "Closed via management plugin">` |

A queue row has `name`, `vhost`, `durable`, `auto_delete`, `exclusive`, `arguments`, `type`, `state`, `node`,
`policy`, `effective_policy_definition`, `messages`, `messages_ready`, `messages_unacknowledged`, `consumers`. A
connection row has `name` (`<peer> -> 127.0.0.1:5673`), `vhost`, `user`, `user_provided_name`, `peer_host`,
`peer_port`, `protocol`, `channels`, `connected_at`, and `namespace`.

**Access.** Basic credentials of a user nobody declared (or none at all) get in as an administrator. A declared user
must give the right password (else 401 `not_authorized` `Login failed`). Without the `administrator` tag it sees only
the vhosts it has a permission on (404 for the others) and cannot read users or change vhosts or permissions (401
`Not administrator user`); policies need `policymaker`.

**Audit.** Every change through this API (users and passwords, permissions, vhosts, policies, connections closed) is a
row on the [Timeline](../timeline.md) under Messaging: what changed (`user acme-app: password set`, `permission of
acme-app on vhost acme-rabbit set (configure .*, write .*, read .*)`, `vhost / deleted`…), the management user who
did it, and the method, path, client address and user agent. A refused change is a row too (`— refused (401)`).

## How it behaves

- **Declared on first use.** With `auto_create` on (the default), a `queue.declare` or `exchange.declare` creates the
  object, marked with the connection's name. Off: a declare of a missing one closes the channel with 404 `NOT_FOUND -
  no queue '<q>' in vhost '<v>' (Stuntman: “Create queues the app asks for” is off)`.
- **Unacked messages** stay with their channel without a time limit. When the channel or the connection closes
  (including `disconnect_consumer`, `down`, `close`), they go back to the queue and come again with `redelivered`.
  On a quorum queue each return counts towards the delivery limit.
- **Prefetch.** A consumer gets at most `prefetch_count` unacked messages; 0 means no limit.
- **Order.** Ready messages are delivered in publish order. The `priority` property is kept but does not change the
  order.
- **Message TTL.** The `expiration` property (ms) is honoured: an expired message waiting in the queue is removed.
  A queue `ttl` set in the YAML (`entities.<q>.queue.ttl`) applies too.
- **Subject.** Stuntman shows a message's `type` property as its subject, else the exchange name, else the routing
  key. Use it in `wait_for_message` (`where: {"subject": …}`) and rules.
- **Clock.** Times use the machine's clock, not the [environment clock](../clock.md).
- **Persistence.** Vhosts, users and permissions are files; messages and history are in the environment's SQLite and
  survive a restart. Connections do not.
- **Who sent it.** Messages from the app have `via: amqp` and the connection's name as `sender`. Messages from
  `publish` are recorded as sent by you or the agent and `wait_for_message` ignores them.

## Provoke failures

| Failure | How | What the app sees | Turn it off |
|---|---|---|---|
| Ack lost | `set_next_delivery {"namespace": "acme-rabbit", "entity": "notifications", "behavior": "lose_lock"}` | the app's ack of the next message is taken as a requeue: the message comes again, `redelivered` | once (`times: 1` default); `behavior: normal` |
| Delivered twice | `behavior: duplicate` | two copies of the next message | as above |
| Held | `behavior: hold` | the next message arrives 30 s late | as above |
| Dropped | `behavior: drop` | never delivered | as above |
| Dead-lettered | `behavior: dead_letter` | the next message goes to dead-letter on arrival (not forwarded to a dead-letter exchange) | as above |
| Out of order | `behavior: out_of_order` | the next message is held 3 s so the one after overtakes it | as above |
| A rule | `create_rule {"service": "rabbitmq", …}` | see [Rules](#rules) | `set_rule_enabled`, `delete_rule` |
| Publishes refused | `set_throughput {"namespace": "acme-rabbit", "per_second": "throttle_now"}` (or a number) | with confirms: `basic.nack`; without: the channel is closed with 506 `RESOURCE_ERROR - The request was terminated because the namespace … is being throttled…` | `per_second: none` |
| Queue full | a policy or `x-max-length` with `overflow: reject-publish` | with confirms: `basic.nack`; without: the message is silently dropped | remove the limit |
| Vhost refuses | `set_namespace_reachability {"namespace": "acme-rabbit", "mode": "refuse"}` | new connections are closed during the handshake, after `connection.open`, with no reply | `mode: ok` |
| Vhost never answers | `mode: timeout` | `connection.open` gets no answer (10 minutes) | `ok` |
| Login rejected | `mode: 401` | new connections get 403 `ACCESS_REFUSED - Login was refused…` | `ok` |
| Broker blocked | `mode: busy` | new connections get `connection.blocked`; publishes are refused as with `throttle_now`; `/api/health/checks/alarms` answers 503 | `ok` |
| Broker killed | `mode: down` | open connections drop at once (TCP closed, no `connection.close`), new ones are refused, the management API answers 503 for that vhost; unacked deliveries go back to their queues; queued messages stay | `ok` |
| Broker shut down | `mode: close` | open connections get 320 `CONNECTION_FORCED - broker forced connection closure with reason 'shutdown'`, then as `down` | `ok` |
| Connection forced closed | `disconnect_consumer {"namespace": "acme-rabbit", "id": "<consumer id>"}` | the consumer's **whole connection** is closed with 320 `CONNECTION_FORCED - Closed via management plugin (Stuntman: Disconnect it)` | the app reconnects |
| Connection closed with a reason | `DELETE /api/connections/{name}` with `X-Reason: rotating credentials` | 320 `CONNECTION_FORCED - rotating credentials` | |
| Vhost deleted | `DELETE /api/vhosts/{vhost}` | 320 `CONNECTION_FORCED - vhost '<name>' is deleted`; the namespace and its messages are gone | `PUT` it again |
| Delivery limit | a policy `{"pattern": "^orders", "apply-to": "quorum_queues", "definition": {"delivery-limit": 2, "dead-letter-exchange": "dlx"}}` | the third return of a message drops it to `dlx` | delete the policy |
| Missing queue or exchange | `set_auto_create {"namespace": "acme-rabbit", "on": false}` | declares of missing objects get 404 | `on: true` |

`expire_consumer_locks` on a RabbitMQ consumer puts its unacked messages back on the queue while the app still holds
them; its later ack of those delivery tags is ignored and the messages are delivered again.

### Rules

Rules live in `messaging/rules/rabbitmq.yaml`, one file for every RabbitMQ namespace of the environment. The first
enabled rule that matches answers.

| Part | Values |
|---|---|
| `when.op` | `send` (publish), `receive` (delivery), `any` |
| `when.namespace`, `when.entity`, `when.subject` | exact or a `*` pattern; `entity` is the queue, or the exchange for publishes |
| `when.only_when` | `{field, key, op, value}`: `property` (a header equals), `size` (over N KB), `sender` (the connection name), `delivery_count` (at least N), `session` |
| `then.delivery` | `lose_lock` (the ack becomes a requeue), `duplicate`, `hold` (`seconds`, default 30), `reorder`, `dead_letter`, `drop` |
| `then.refuse` (publish only) | `server_busy` (nack with confirms, else channel 506), `quota_exceeded` (nack with confirms, else dropped), `entity_not_found` (channel 404 `NOT_FOUND - …`), `unauthorized` (channel 403 `ACCESS_REFUSED - …`) |
| `how_often` | `{"first": N}`, `{"random": P}`, `"every"` (default) |

```text
create_rule {"service": "rabbitmq", "name": "invoices nacked", "when": {"op": "send", "namespace": "acme-rabbit", "entity": "order-created"}, "then": {"refuse": "server_busy"}, "how_often": {"first": 3}}
create_rule {"service": "rabbitmq", "name": "globex twice", "when": {"op": "receive", "entity": "order-created.*", "only_when": [{"field": "property", "key": "tenant", "value": "globex"}]}, "then": {"delivery": "duplicate"}}
```

## See what the app did

| Need | Tool | Route |
|---|---|---|
| Vhosts, queues, exchanges, bindings, counts, connection strings | `list_namespaces` (`summary: true` for the short view) | `GET /api/messaging/namespaces` |
| Where a message would go | `trace_message_route {"vhost", "exchange", "key", "headers"}` | `GET /api/messaging/route?vhost=…&exchange=…&key=…` |
| Messages of a queue | `list_queue_messages {"namespace", "entity", "state", "q", "last"}` | `GET /api/messaging/{namespace}/{entity}/messages` |
| One message with its history | `get_queue_message {"namespace", "seq"}` | `GET /api/messaging/{namespace}/{entity}/messages/{seq}` |
| Wait for the app's message | `wait_for_message {"namespace", "entity", "where", "timeout", "since"}` | `GET /api/messaging/{namespace}/{entity}/wait` |
| Who consumes | `list_consumers {"namespace", "entity"}`, `get_consumer {"namespace", "id"}` | `GET /api/messaging/{namespace}/{entity}/consumers` |
| Unrouted messages, several consumers on one queue | `list_messaging_warnings` | `GET /api/messaging/warnings` |
| Open connections | | `GET :15672/api/connections` |

`trace_message_route` walks the bindings without publishing:

```text
trace_message_route {"vhost": "acme-rabbit", "exchange": "Acme.Contracts:Routed", "key": "tenant-a"}
→ {"queues": ["acme-routed"], "hops": [
    {"source": "Acme.Contracts:Routed", "sourceType": "direct", "destination": "acme-routed", "destinationType": "exchange", "bindingKeys": ["tenant-a", "tenant-b"], "matches": true},
    {"source": "acme-routed", "sourceType": "fanout", "destination": "acme-routed", "destinationType": "queue", "bindingKeys": [""], "matches": true}]}
```

With `exchange` empty it answers the queue the routing key names. An unknown exchange is 404.

- `state`: `active` (ready), `scheduled`, `locked` (unacked), `dead_letter`, `completed` (acked). A message has
  `seq`, `messageId`, `subject`, `contentType`, `state`, `deliveryCount`, `enqueuedAt`, `lockedBy`, `settledAt`,
  `deadLetterReason`, `sender`, `via`, `properties` (the headers, `x-death` included) and `system` (`exchange`,
  `routingKey`, `correlationId`, `replyTo`, `expiration`, `type`, `userId`, `appId`, `contentEncoding`,
  `deliveryMode`, `priority`, `timestamp`). `get_queue_message` adds `body` and `history`.
- `wait_for_message` returns the first message the app published (`via: amqp`) that matches `where` (message
  fields, headers or system properties), with `body` parsed when JSON; 408 after `timeout` (default 30 s).
- A consumer has `id` (`rmq-…`), `client`, `link` (`<queue>/<consumer tag>`), `mode` (`auto-ack` or `manual ack`),
  `prefetch`, `locks` (unacked now), `doneToday`, `remote`, `protocol` (`amqp091`), `state`.

The [Timeline](../timeline.md) under Messaging has a row for each message the app published, acks folded per
consumer, queue and minute, each dead-letter and each message dropped past its delivery limit, plus the management
API audit rows.

## Recipes

1. **Make the consumer survive a forced disconnect.**
   ```text
   list_consumers {"namespace": "acme-rabbit", "entity": "notifications"}
   disconnect_consumer {"namespace": "acme-rabbit", "id": "rmq-…"}
   list_consumers {"namespace": "acme-rabbit", "entity": "notifications"}   → a new consumer receiving
   ```
   The app gets `connection.close` 320 `CONNECTION_FORCED`; its unacked messages come again with `redelivered`.

2. **Survive a broker crash.**
   ```text
   set_namespace_reachability {"namespace": "acme-rabbit", "mode": "down"}
   set_namespace_reachability {"namespace": "acme-rabbit", "mode": "ok"}
   ```
   `down` is a killed process (no close frame); `close` is a clean shutdown (320 first).

3. **Check that a message reaches the right queue.**
   ```text
   trace_message_route {"vhost": "acme-rabbit", "exchange": "order-created", "key": "orders.eu"}
   list_messaging_warnings                                   → kind unrouted, with exchange and routingKey
   ```

4. **Test a poison message with a quorum queue and a dead-letter exchange.**
   ```text
   PUT :15672/api/policies/acme-rabbit/orders {"pattern": "^orders", "apply-to": "quorum_queues", "priority": 10, "definition": {"delivery-limit": 3, "dead-letter-exchange": "dlx"}}
   list_queue_messages {"namespace": "acme-rabbit", "entity": "orders-dlq"}
   ```
   Each crash of the consumer (channel closed with the message unacked) counts; the fourth return sends it to `dlx`
   with `x-death` reason `delivery_limit`.

5. **Provision like a deploy tool, then connect as the app's user.**
   ```sh
   curl -u guest:guest -X PUT localhost:15672/api/vhosts/acme-rabbit -H 'content-type: application/json' -d '{"default_queue_type":"quorum"}'
   curl -u guest:guest -X PUT localhost:15672/api/users/acme-app -H 'content-type: application/json' -d '{"password":"s3cret-pass","tags":""}'
   curl -u guest:guest -X PUT localhost:15672/api/permissions/acme-rabbit/acme-app -H 'content-type: application/json' -d '{"configure":".*","write":".*","read":".*"}'
   ```
   The app then connects with `amqp://acme-app:s3cret-pass@localhost:5673/acme-rabbit`; a wrong password gets 403.
   To find who changed a password later, filter the Timeline by Messaging and look for `password set`.

6. **Send a message as another system would.**
   ```text
   publish {"namespace": "acme-rabbit", "entity": "order-created", "routing_key": "orders.eu", "body": {"orderId": 58213}, "properties": {"tenant": "globex"}}
   ```
   `entity` is an exchange (routed with `routing_key`) or a queue. A name that is both a queue and an exchange goes to
   the queue.

7. **Assert the app published.**
   ```text
   wait_for_message {"namespace": "acme-rabbit", "entity": "order-created.invoices", "where": {"tenant": "globex"}, "timeout": "20s"}
   ```

## Errors and troubleshooting

| Symptom or error text | Cause | Fix |
|---|---|---|
| Connection refused on `5672` | Stuntman's AMQP port is `5673` | use `5673` (or set `STUNTMAN_AMQP_PORT`) |
| `530 NOT_ALLOWED - vhost <name> not found` | the vhost names another environment's missing namespace, or its name breaks the namespace rules | create it (`create_account`, `PUT /api/vhosts`), or use a valid name |
| `530 NOT_ALLOWED - access to vhost '<v>' refused for user '<u>'` | a user declared in the management API has no permission on that vhost | `PUT /api/permissions/<v>/<u>` |
| `403 ACCESS_REFUSED - Login was refused…` | wrong password for a declared user, "Accept any key" off and wrong password, or the vhost set to `401` | check `/api/users`, `get_account`, `mode: ok` |
| `404 NOT_FOUND - no exchange 'amq.topic' in vhost '<v>'` | the built-in `amq.*` exchanges do not exist here | declare your own exchange |
| `404 NOT_FOUND - no queue '<q>' in vhost '<v>'` on consume | the queue was never declared, or was deleted | declare it before consuming |
| `406 PRECONDITION_FAILED - inequivalent arg 'x-queue-type'…` | the queue exists with another type (the vhost's default type made it quorum) | delete it, or declare with the same type |
| `406 PRECONDITION_FAILED - unknown delivery tag <n>` | ack of a tag the channel does not hold (a second ack, or a tag from another channel) | ack once, on the channel that received it |
| `540 NOT_IMPLEMENTED - Stuntman does not do transactions` | `tx.select` | use publisher confirms |
| `506 RESOURCE_ERROR - …` on publish without confirms | throughput limit, `busy`, or a `server_busy` rule | turn it off; enable confirms to get a nack instead |
| Messages vanish without error | default exchange to a missing queue, or no binding takes the key | `list_messaging_warnings`, `trace_message_route`; publish with `mandatory` |
| A consumer stays silent after its queue was deleted | no `basic.cancel` is sent | recreate the queue; restart the consumer |
| 400 on `PUT /api/vhosts/Acme_Bus` | vhost names follow the namespace rules | 3 to 50 lowercase letters, numbers or dashes |
| 503 from the management API for one vhost | the namespace is set to `down` or `close` | `mode: ok` |
| `RabbitMQ management: port 15672 is taken` in the log | another RabbitMQ runs on the machine | stop it or set `STUNTMAN_RABBITMQ_MGMT_PORT` |

## Not done

- The built-in exchanges `amq.direct`, `amq.topic`, `amq.fanout`, `amq.headers`, `amq.match` are not there:
  declaring them answers ok, publishing or binding to them answers 404.
- Queue arguments kept but not applied: `x-message-ttl`, `x-expires`, `x-max-priority` (priority order),
  `x-single-active-consumer`, `x-max-length-bytes`. Exclusive and auto-delete queues are not deleted when their
  connection or last consumer goes. Stream queues (`x-queue-type: stream`) behave as plain queues (no offsets).
- Headers exchanges match all arguments only (`x-match: any` is ignored).
- Expired messages are removed, not sent to the dead-letter exchange.
- `consumer-timeout` is kept in the policy but not enforced; permission patterns are kept but not checked per
  operation.
- No `basic.cancel` when a queue a consumer reads is deleted; no `connection.unblocked` (a `busy` vhost blocks
  connections opened while it was busy for their whole life).
- Transactions (`tx.select`, `tx.commit`, `tx.rollback`).
- The management API covers only the default environment's vhosts in its lists, users and connections; it has no
  `/api/definitions`, `/api/nodes`, `/api/channels`, `/api/consumers`, nor publish/get through HTTP.
- Messages a consumer held unacked when Stuntman stopped stay unacked after the restart (they are not redelivered).
- Times use the machine's clock, not the environment clock.

Missing something? Tell the developer what the fix would be and offer a pull request — see [Contributing](../../CONTRIBUTING.md).

## Tools and routes

<!-- tools: messaging -->
_Generated by `tools/gen_tool_reference.py` from the engine source; each tool is an MCP tool and the HTTP route shown (under `/api`)._

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_namespaces` | `GET /api/messaging/namespaces` | — | summary | Namespaces with kind (azure-service-bus, rabbitmq), entities (queues, topics and their subscriptions or bindings), counters per state and the connection string. summary=true: only ports, connection strings and counts. |
| `trace_message_route` | `GET /api/messaging/route` | — | namespace, vhost, exchange, key, headers | Where a RabbitMQ message would go, without publishing it: every binding on the way (exchange to exchange, exchange to queue), whether its key matches, and the queues it reaches. |
| `list_messaging_warnings` | `GET /api/messaging/warnings` | — | — | What works here but would break on the real provider, or hides a bug: Service Bus entities the app created on first use (auto create; Azure answers MessagingEntityNotFound), queues or subscriptions that more than one connection receives from (each gets part of the messages), and RabbitMQ messages that reached no queue (unrouted: exchange and routing key). |
| `list_queue_messages` | `GET /api/messaging/{namespace}/{entity}/messages` | namespace, entity | state, q, last | Messages of a queue, topic or subscription, newest first; state = active \| scheduled \| locked \| dead_letter \| completed. |
| `get_queue_message` | `GET /api/messaging/{namespace}/{entity}/messages/{seq}` | namespace, seq | entity | One message: properties, body and the delivery history (published, delivered, abandoned, completed, dead-lettered…). |
| `publish` | `POST /api/messaging/{namespace}/{entity}/messages` | namespace, entity | body, subject, properties, schedule_at, correlation_id, session_id, routing_key, message_id | Publishes a message as a test (recorded as sent by you or the agent, never as the app). Topics fan out to their subscriptions. |
| `resubmit` | `POST /api/messaging/{namespace}/{entity}/resubmit` | namespace | entity, seq, all_dead_letters | Puts dead-lettered messages back on their queue, with a note in their history: one seq, or all_dead_letters. |
| `dead_letter_message` | `POST /api/messaging/{namespace}/{entity}/messages/{seq}/dead-letter` | namespace, seq | entity | Moves a waiting message to dead-letter by hand (“dead-lettered by you”). |
| `delete_queue_message` | `DELETE /api/messaging/{namespace}/{entity}/messages/{seq}` | namespace, seq | entity | Deletes one message and its history. |
| `purge` | `DELETE /api/messaging/{namespace}/{entity}/messages` | namespace, entity | state | Removes messages of an entity (a state, or all). People confirm on the screen; agents do not. |
| `list_consumers` | `GET /api/messaging/{namespace}/{entity}/consumers` | namespace | entity | Connections receiving from this entity: client id, link, receive mode, prefetch, locks held, completed today; disconnected ones of the last day. |
| `get_consumer` | `GET /api/messaging/{namespace}/consumers/{id}` | namespace, id | — | One consumer: connection, the locks it holds now and its last messages. |
| `disconnect_consumer` | `POST /api/messaging/{namespace}/consumers/{id}/disconnect` | namespace, id | — | Forces a consumer off (RabbitMQ: CONNECTION_FORCED; Service Bus: amqp:link:detach-forced). Its locks are released: tests the app's reconnect logic. |
| `expire_consumer_locks` | `POST /api/messaging/{namespace}/consumers/{id}/expire-locks` | namespace, id | — | Expires the locks a consumer holds now: its next complete fails with MessageLockLost and the messages are delivered again. |
| `set_namespace_reachability` | `PUT /api/messaging/{namespace}/reachability` | namespace, mode | — | The whole namespace: ok \| refuse (new connections refused) \| timeout (no answer) \| 401 (key rejected) \| busy (ServerBusy / connection.blocked) \| down (a dead broker: open connections drop without a word, new ones are refused, management answers 503) \| close (the same, but open connections get a forced close first). |
| `add_entity` | `POST /api/services/{name}/accounts/{account}/entities` | namespace, entity | kind, lock, max_delivery, sessions, exchange_type | Adds a queue or a topic to a namespace (or marks one the app created as declared: “Keep it in the YAML”). |
| `remove_entity` | `DELETE /api/services/{name}/accounts/{account}/entities/{entity}` | namespace, name | — | Removes a queue or a topic and its messages. |
| `add_subscription` | `POST /api/services/{name}/accounts/{account}/topics/{topic}/subscriptions` | namespace, topic, subscription | filter, lock, max_delivery | Adds a subscription to a topic (RabbitMQ: binds a queue to the exchange; filter is the binding key). |
| `rotate_policy_key` | `POST /api/services/{name}/accounts/{account}/policies/{policy}/rotate` | namespace, policy | — | Makes a new key for a shared access policy (RabbitMQ: a user's password); senders still using the old one get 401. |
| `set_throughput` | `PUT /api/messaging/{namespace}/throughput` | namespace, per_second | — | Messages a second the namespace takes before ServerBusy; none, a number, or throttle_now (every send ServerBusy). |
| `set_auto_create` | `PUT /api/messaging/{namespace}/auto-create` | namespace, on | — | On: queues and topics the app declares or sends to are created. Off: they answer 404 (test a missing entity). |
<!-- /tools -->
