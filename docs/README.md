# Stuntman documentation

Stuntman stands in for the external providers an app calls (email, SMS, queues, storage, identity, AI models…).
It speaks each provider's protocol, keeps the provider's state, sends the provider's callbacks, and lets you provoke
the provider's failures. Every action is an **MCP tool** and an **HTTP route** with the same name.

**For agents:** this index is also served by every running Stuntman at `GET /docs` (Markdown, the docs of that very
version), and each file below at `GET /docs/<path>`, e.g. `GET /docs/services/rabbitmq.md`. Open the one file for
the service you work with: it lists every endpoint, behaviour, provocation, tool and gap of that service.

Addresses assume the default ports on `localhost`. In another environment, add `/e/<env>` after the port (or use
its host names): see [Environments](environments.md). All ports: [Configuration](configuration.md#ports).

## Services

| Service | Stands in for | File |
|---|---|---|
| HTTP APIs | Any HTTP provider as a plugin (45 in the catalog: Stripe, Twilio, PayPal, Shopify, Slack, Microsoft Graph…), HTTPS with SNI and mTLS, webhooks, idempotency | [services/http-apis.md](services/http-apis.md) |
| AI models | OpenAI, Azure OpenAI, Anthropic, Gemini (chat, streaming, embeddings, token counts) | [services/ai-models.md](services/ai-models.md) |
| SMS | Zenvia API v2 and its status webhooks | [services/sms.md](services/sms.md) |
| WhatsApp | Meta WhatsApp Cloud API: messages, templates, media, webhooks | [services/whatsapp.md](services/whatsapp.md) |
| Email | SMTP (STARTTLS, AUTH) and SendGrid v3 with its Event Webhook | [services/email.md](services/email.md) |
| Mailboxes | IMAP4rev1 and POP3 mailboxes the app reads | [services/mailboxes.md](services/mailboxes.md) |
| Storage | Azure Blob Storage, Amazon S3, Cloudflare R2 | [services/storage.md](services/storage.md) |
| Files | FTP, explicit FTPS, SFTP | [services/files.md](services/files.md) |
| Azure Service Bus | AMQP 1.0 messaging and the administration client | [services/servicebus.md](services/servicebus.md) |
| RabbitMQ | AMQP 0-9-1 and the management API (`rabbitmqadmin`) | [services/rabbitmq.md](services/rabbitmq.md) |
| ActiveMQ | ActiveMQ Classic over OpenWire (NMS) | [services/activemq.md](services/activemq.md) |
| Azure Event Hubs | AMQP 1.0 and the Kafka endpoint, checkpoints | [services/eventhubs.md](services/eventhubs.md) |
| Streams | Apache Kafka and Confluent Schema Registry | [services/streams.md](services/streams.md) |
| Pub/Sub | Google Cloud Pub/Sub (gRPC and REST) | [services/pubsub.md](services/pubsub.md) |
| Kinesis | Amazon Kinesis Data Streams | [services/kinesis.md](services/kinesis.md) |
| Identity | Entra ID / OIDC / OAuth2, SAML 2.0, LDAP, Bot Framework, OAuth client checks | [services/identity.md](services/identity.md) |
| Secrets | Azure Key Vault (secrets, keys, certificates) and managed identity | [services/secrets.md](services/secrets.md) |
| Webhook inboxes | Endpoints the app sends its own webhooks to | [services/webhooks.md](services/webhooks.md) |
| Product events | PostHog, Segment, Mixpanel, GA4 Measurement Protocol | [services/events.md](services/events.md) |
| Telemetry | OpenTelemetry (OTLP traces, logs, metrics), GELF, StatsD | [services/telemetry.md](services/telemetry.md) |

Every service file has the same sections: At a glance · Connect your app · What it supports · How it behaves ·
Provoke failures · See what the app did · Recipes · Errors and troubleshooting · Not done · Tools and routes.

## Guides

| Guide | What it covers |
|---|---|
| [Getting started](getting-started.md) | Run it, point your app, connect an agent |
| [Agents](agents.md) | MCP and HTTP, client-by-client setup, the loop |
| [Environments](environments.md) | Several isolated copies on one Stuntman; host names, `/e/<env>`, hosts file and Test CA for apps outside containers |
| [Failures on cue](failures.md) | Rules, forced answers and every provocation |
| [Timeline and proof](timeline.md) | What the app did, across services; unmatched calls |
| [The clock](clock.md) | Move an environment's time; scheduled webhooks |
| [Plugins](plugins.md) | The plugin YAML: routes, answers, rules, templates, webhooks |
| [OpenAPI](openapi.md) · [SOAP](soap.md) · [WireMock mocks](mocks.md) · [Recording](recording.md) | Other ways to make an HTTP provider |
| [Configuration](configuration.md) | Every setting and port, the data folder, the Test CA |
| [Security](security.md) | What is open, and where not to run it |
| [Tool reference](reference/tools.md) | All tools in one table (each service file has its own) |

## When Stuntman falls short

A behaviour it gets wrong, a missing endpoint or tool, a bug: each service file ends with what is not done. Tell the
developer the fix you would make and offer it as a pull request — see [Contributing](../CONTRIBUTING.md).
