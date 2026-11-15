# Stuntman — setup guide for coding agents

You are a coding agent (Claude Code, Codex, Gemini CLI, OpenCode, Cursor, GitHub Copilot or similar). A developer
pasted a short prompt that sent you here. Your job: **set Stuntman up for the project in the current working
directory, end to end, and leave the developer with a working staging environment that never calls a real provider.**

Stuntman stands in for the external providers an app calls (SMS, WhatsApp, email and mailboxes, storage, file
servers, queues (Service Bus, RabbitMQ, ActiveMQ), Azure Event Hubs, Kafka, Google Cloud Pub/Sub, identity, secrets, AI models, product analytics, webhooks, telemetry and any REST, GraphQL or
SOAP API through YAML plugins). It answers like the provider, keeps state,
sends callbacks and fails on cue. Every action is an MCP tool and an HTTP route.

- Site: https://edivanteixeira.github.io/stuntman/
- Source and full docs: https://github.com/edivanteixeira/stuntman
- Tool reference (every tool, route and argument): https://github.com/edivanteixeira/stuntman/blob/main/docs/reference/tools.md

Follow the eight steps below **in order**. The developer should never need to know Docker, ports or tool names:
you take care of all of it, explain what you are doing in plain words and ask only when a decision is theirs.

---

## Ground rules

1. **Talk in the developer's language.** Step 1 asks for it; use it for every message until the end.
2. **Be visual and brief.** Show progress as `Step N of 8 — <name>` with a short checklist. Use tables for
   findings. No walls of text: one short paragraph per step, then the result.
3. **Never touch production.** Change only local, development or staging configuration. If you cannot tell which
   file is production, ask.
4. **Show before you write.** Before changing any file, show the exact change (a diff or a table) and wait for a yes.
5. **No real secrets.** Stuntman accepts any key, token or password for the providers it stands in for. Write
   placeholder values such as `stuntman-any-key`. Never print or copy real credentials you find.
6. **Do not commit or push.** Leave the changes in the working tree and say so at the end.
7. **Stop on surprises.** If a step fails twice, explain what happened, offer the next best option and ask.

---

## Step 1 of 8 — Language

Ask exactly one question, with numbered options, and wait:

> **Stuntman setup** · Which language should we talk in?
> 1. English  2. Português  3. Español  4. Another (tell me which)

Then greet the developer in that language with this plan (translated), as a checklist:

```text
Here is what I will do — about five minutes, and I will ask before changing any file:
☐ 1  Language
☐ 2  Start Stuntman on this machine
☐ 3  Connect myself to it
☐ 4  Find every provider this project calls
☐ 5  Set Stuntman up for each one
☐ 6  Point your local / staging configuration at Stuntman (I will show you first)
☐ 7  Prove nothing still reaches a real provider
☐ 8  Hand you everything: links, what changed, what to try next
```

Update and reprint the checklist (☑ done, ▶ now, ☐ next) at the start of each step.

## Step 2 of 8 — Start Stuntman

1. Check whether it is already running: `curl -s -o /dev/null -w "%{http_code}" http://localhost:8025/api/env`.
   `200` means yes: skip to Step 3.
2. Check Docker: `docker info`. If Docker is missing or stopped, tell the developer in one sentence what to do
   (install or start Docker Desktop / OrbStack / Colima) and wait.
3. Start Stuntman (all ports bound to this machine only):

```sh
docker run -d --name stuntman --restart unless-stopped \
  -p 127.0.0.1:8025:8025 -p 127.0.0.1:2525:2525 -p 127.0.0.1:8027:8027 -p 127.0.0.1:8028:8028 \
  -p 127.0.0.1:8443:8443 -p 127.0.0.1:8444:8444 -p 127.0.0.1:5673:5673 -p 127.0.0.1:5671:5671 \
  -p 127.0.0.1:10000:10000 -p 127.0.0.1:9000:9000 -p 127.0.0.1:4317:4317 -p 127.0.0.1:4318:4318 \
  -p 127.0.0.1:3389:3389 -p 127.0.0.1:2121:2121 -p 127.0.0.1:30000-30009:30000-30009 -p 127.0.0.1:2222:2222 \
  -p 127.0.0.1:1143:1143 -p 127.0.0.1:1110:1110 -p 127.0.0.1:9092:9092 -p 127.0.0.1:8081:8081 \
  -p 127.0.0.1:8085:8085 -p 127.0.0.1:4567:4567 -p 127.0.0.1:15672:15672 -p 127.0.0.1:61616:61616 \
  -p 127.0.0.1:12201:12201/udp -p 127.0.0.1:8125:8125/udp \
  -v stuntman-data:/data edivanteixeira/stuntman
```

   The second half of the ports (LDAP, FTP and its passive range, SFTP, IMAP, POP3, Kafka, Schema Registry,
   Pub/Sub, Kinesis, RabbitMQ management, ActiveMQ, GELF, StatsD) is only needed when the project uses those lines; leave them in when unsure.

4. Wait until `http://localhost:8025/api/env` answers `200` (poll every 2 s, up to 60 s).
5. If a container named `stuntman` already exists but is stopped, run `docker start stuntman` instead.
   If a port is taken, tell the developer which one and which app holds it (`lsof -i :<port>`), and ask.

Tell the developer: *"Stuntman is running. Its web UI is at http://localhost:8025 — you can open it any time to
watch what your app does."*

## Step 3 of 8 — Connect yourself

Prefer MCP; fall back to HTTP. MCP runs over streamable HTTP at `http://localhost:8025/mcp`, with no key.

| Agent | How to add the server |
|---|---|
| Claude Code | `claude mcp add --transport http stuntman http://localhost:8025/mcp` |
| Codex | `codex mcp add stuntman --url http://localhost:8025/mcp` |
| Gemini CLI | `mcpServers.stuntman.httpUrl = "http://localhost:8025/mcp"` in `.gemini/settings.json` |
| OpenCode | `mcp.stuntman = { "type": "remote", "url": "http://localhost:8025/mcp" }` in `opencode.json` |
| Cursor | `mcpServers.stuntman.url = "http://localhost:8025/mcp"` in `.cursor/mcp.json` |
| GitHub Copilot (VS Code) | `servers.stuntman = { "type": "http", "url": "http://localhost:8025/mcp" }` in `.vscode/mcp.json` |

If your client needs a restart to load a new MCP server, **do not stop the setup**: use the HTTP API for the rest of
this session. Every tool is a route under `/api`; `GET http://localhost:8025/api/tools` lists each tool with its
`http` route and parameters. Send JSON with `content-type: application/json`, and pass `"by": "<your name>"` so the
timeline names you.

Check the connection by calling `list_environments`. The default environment is `stg-a`.

## Step 4 of 8 — Find every provider this project calls

Scan the repository (source, config files, `.env*` examples, infrastructure files, dependency manifests). Look for
hosts, SDK packages and setting names. Do not print secret values you come across.

| Stuntman line | Signals to look for |
|---|---|
| **Email (SMTP)** | `SMTP_HOST`, `smtp.*`, MailKit, nodemailer, smtplib, `spring.mail`, ActionMailer `smtp_settings` |
| **Email (SendGrid)** | `api.sendgrid.com`, `@sendgrid/mail`, `sendgrid` packages, `SENDGRID_API_KEY` |
| **Mailboxes the app reads** | `IMAP_HOST`, `imap.*`, `pop.*`, MailKit `ImapClient`/`Pop3Client`, imaplib, poplib, `imapflow`, XOAUTH2 settings |
| **SMS** | `api.zenvia.com`, Zenvia SDKs (Stuntman speaks Zenvia API v2) |
| **WhatsApp** | `graph.facebook.com`, WhatsApp Cloud API, phone number ID / WABA settings |
| **Azure Blob** | `blob.core.windows.net`, `Azure.Storage.Blobs`, `@azure/storage-blob`, `azure-storage-blob`, storage connection strings |
| **Amazon S3 / R2** | `s3.amazonaws.com`, `AWS_ENDPOINT_URL`, aws-sdk S3 clients, `boto3.client("s3")`, R2 endpoints |
| **Azure Service Bus** | `servicebus.windows.net`, `Azure.Messaging.ServiceBus`, `@azure/service-bus` |
| **Azure Event Hubs** | `azure-eventhub` (Python), `Azure.Messaging.EventHubs` (.NET, with `.Processor`), `@azure/event-hubs`, `azure-messaging-eventhubs` (Java), `servicebus.windows.net` connection strings with `EntityPath=`, `EventHubProducerClient` / `EventHubConsumerClient` / `EventProcessorClient`, `BlobCheckpointStore`; Kafka clients with `sasl.username=$ConnectionString` or `:9093` |
| **RabbitMQ** | `amqp://`, `RabbitMQ.Client`, `amqplib`, `pika`, `bunny`; the management API (`:15672`, MassTransit) |
| **ActiveMQ** | `tcp://…:61616`, `activemq:tcp://…`, `failover:(tcp://…)` broker URLs, `Apache.NMS.ActiveMQ`, `activemq-client` / `activemq-all`, `spring.activemq.*`, `ConnectionFactory` with OpenWire, `BrokerURL` settings; an `ActiveMQ.DLQ` or `AMQ_SCHEDULED_*` in the code (Stuntman speaks OpenWire; STOMP/AMQP-only ActiveMQ clients are not covered) |
| **Google Cloud Pub/Sub** | `PUBSUB_EMULATOR_HOST`, `google-cloud-pubsub`, `@google-cloud/pubsub`, `Google.Cloud.PubSub.V1`, `cloud.google.com/go/pubsub`, `spring-cloud-gcp-starter-pubsub`, `pubsub.googleapis.com`, `projects/<p>/topics/<t>` names |
| **Amazon Kinesis** | `AWS_ENDPOINT_URL_KINESIS`, `boto3.client("kinesis")`, `@aws-sdk/client-kinesis`, `AWSSDK.Kinesis`, `software.amazon.awssdk:kinesis`, `aws-sdk-go-v2/service/kinesis`, KCL (`amazon-kinesis-client`), `kinesis.<region>.amazonaws.com`, `PutRecord`/`GetShardIterator` calls, stream ARNs `arn:aws:kinesis:…:stream/<name>` |
| **Kafka** | `bootstrap.servers`, `Confluent.Kafka`, `kafkajs`, `kafka-python`, `confluent-kafka`, `schema.registry.url` |
| **File servers** | `ftp://`, `sftp://`, FluentFTP, SSH.NET, paramiko, `ssh2-sftp-client`, EDI drop folders |
| **Identity** | `login.microsoftonline.com` (Entra ID), `accounts.google.com`, `*.okta.com`, OIDC `Authority` / `issuer` settings; SAML metadata URLs and ACS settings; `ldap://`, `DirectoryServices`, `ldap3`; Bot Framework / Teams bots (`MicrosoftAppId`) |
| **Secrets** | `vault.azure.net`, `Azure.Security.KeyVault.*`, `DefaultAzureCredential`, managed identity |
| **Telemetry** | `OTEL_EXPORTER_OTLP_ENDPOINT`, OpenTelemetry exporters; GELF / Graylog sinks; StatsD / DogStatsD clients |
| **AI models** | `api.openai.com`, `*.openai.azure.com`, `api.anthropic.com`, `generativelanguage.googleapis.com`, `openrouter.ai`, `api.deepseek.com`; `openai`, `anthropic`, `@google/genai` SDKs |
| **Product analytics** | PostHog, Segment, Mixpanel, GA4 Measurement Protocol (`google-analytics.com/mp/collect`) |
| **Webhook inboxes** | URLs the app **sends** webhooks to (partner callbacks, notification targets) |
| **HTTP API plugins** | reCAPTCHA, Turnstile, Stripe, Resend, OneSignal, Microsoft Graph, Google Drive, Google Maps, Calendly, lemlist, Azure DevOps, New Relic and the others in `list_plugins`, and **any other external base URL**; a WireMock `mappings/` folder in the repository |

Call `list_plugins` to see which HTTP providers already have a plugin in the catalog.

Show the developer one table, in their language:

```text
Provider            Found in                          Stuntman line        How
SendGrid            src/mail/sender.ts, .env.example   Email               built in
Azure Blob          appsettings.Development.json        Storage              built in
Acme KYC API        src/kyc/client.py (KYC_BASE_URL)    HTTP API plugin      new plugin (I will write it)
Stripe              src/billing/*.ts                    HTTP API plugin      recorder first, then a plugin
```

Ask: *"Did I miss anything, or is there a provider you want to leave out?"* and wait.

## Step 5 of 8 — Set Stuntman up for each provider

Use the default environment (`stg-a`) unless the developer asks for another one. For each provider:

| Line | What to do |
|---|---|
| Email, SMS, WhatsApp, Webhook inboxes, Telemetry, AI models, Product events | Built in; nothing to install. Create webhook inboxes with `create_webhook_inbox` when the app sends its own webhooks. For AI models, add a few rules with `set_ai_rule` so the app's prompts get useful answers; for analytics, write the expected events as a plan with `set_event_plan`. |
| Mailboxes | `set_mailbox` for each address the app reads; `put_mail` to drop a test message. |
| File servers | `set_file_server` with the users and folders the app uses; `put_file` for files a partner would drop. |
| ActiveMQ | Usually nothing: queues and topics appear when the app uses them. `set_activemq {"users": {...}}` when the app logs in with a fixed user (with no users, any login works); `activemq_send` for messages another system would send. |
| Azure Event Hubs | `create_eventhub {"namespace": "<ns>", "hub": "<hub>", "partitions": 4, "consumer_groups": [...]}` for every hub the app uses (hubs are not created on first use); `eventhub_send` for events another producer would send. |
| Google Cloud Pub/Sub | `set_pubsub {"project": "<the app's project id>"}`; `create_pubsub_subscription` (it makes the topic too) for what the app expects to exist, or let the app create them; `set_pubsub_schema` for schema-bound topics. |
| Kafka | `create_topic` for the topics the app uses (or let clients create them); `register_schema` for Avro subjects. |
| Azure Blob / S3 | `create_account` (service `azure-blob` or `s3`) with the account and containers the app uses; `add_container` for more. |
| Service Bus / RabbitMQ | Create the namespace and the queues or topics the app uses (see the Messaging tools in the reference). |
| Identity | `set_provider` with flavor `entra-v2`, `google`, `okta` or `oidc` (plus `saml`, `ldap` or `password_grant` when the app uses them); add the app registration and a few synthetic users. |
| Secrets | Create the vault and the secret names the app reads (values are synthetic). |
| A provider in the catalog | `install_plugin` (or `install_collection`). |
| A provider the repository already mocks with WireMock | `import_mocks` with the folder that holds `mappings/` (try `dry: true` first and read the gaps). Guide: https://github.com/edivanteixeira/stuntman/blob/main/docs/mocks.md |
| A provider with an OpenAPI or Swagger spec (an `openapi.yaml`, `swagger.json` or a spec URL in the repository or the provider's docs) | `import_spec` with the spec's URL, a file path or its text. Routes, answers and body checks come from it; add rules with `set_route` and check them with `try_route`. Point the app at `http://localhost:8028/<service>`. Guide: https://github.com/edivanteixeira/stuntman/blob/main/docs/openapi.md |
| A SOAP provider (the app sends XML envelopes, or the repository has a `.wsdl` file or a WSDL URL) | `import_wsdl` with the WSDL's URL, a file path or its text. Operations, types and faults come from it; add rules with `set_operation` and check them with `try_operation`. Point the app at `http://localhost:8028/<service><endpoint>`. Guide: https://github.com/edivanteixeira/stuntman/blob/main/docs/soap.md |
| Any other HTTP API | Write a plugin: read one from the catalog with `get_plugin` as a model, describe the routes the app calls (paths, bodies, the provider's real status codes and errors, a clear `sentence` per answer), save it with `put_plugin` and check it with `validate_plugin`. If you do not know the provider's API well and it has a sandbox the user can reach, ask the user for the sandbox URL and a sandbox key (never a production one), then `start_recording` with that URL, run the app's flows, `stop_recording` and `make_plugin`. Guide: https://github.com/edivanteixeira/stuntman/blob/main/docs/recording.md. With no sandbox, install a recorder (`install_plugin` with `plugin=empty, service=<name>`), run the app and build the plugin from what it sent. |

Plugin format and expressions: https://github.com/edivanteixeira/stuntman/blob/main/docs/plugins.md

Use synthetic data only (`@example.com`, `acme`, `globex`, made-up people).

## Step 6 of 8 — Point the project at Stuntman

Change **base URLs only**; the app keeps its code and SDKs.

| Provider | Value |
|---|---|
| SMTP | host `localhost`, port `2525`, STARTTLS, any user and password |
| SendGrid | base URL `http://localhost:8025/sendgrid`, any API key |
| SMS (Zenvia API v2) | `http://localhost:8025/zenvia/v2/channels/{0}/messages` |
| WhatsApp (Graph) | `http://localhost:8025/whatsapp` |
| HTTP API plugins | `http://localhost:8028/<service>` (or `https://localhost:8443/<service>` with the Test CA) |
| Azure Blob | `DefaultEndpointsProtocol=http;AccountName=<account>;AccountKey=<key from get_account>;BlobEndpoint=http://127.0.0.1:10000/<account>;` (Azure SDKs need `127.0.0.1`, not `localhost`) |
| S3 / R2 | endpoint `http://127.0.0.1:9000`, path-style addressing, keys from `get_account` |
| Service Bus | `Endpoint=sb://<namespace>.<base domain>:5673;SharedAccessKeyName=any;SharedAccessKey=any` — Service Bus and Key Vault find the namespace or vault by host name: set the environment's base domain first (`set_environment`) and make it resolve to this machine; see the [environments guide](https://github.com/edivanteixeira/stuntman/blob/main/docs/environments.md#how-a-request-finds-its-environment) |
| RabbitMQ | `amqp://any:any@localhost:5673/stg-a%2F<namespace>` (the vhost is `<env>/<namespace>`, URL-encoded) |
| Key Vault | `https://<vault>.vault.<base domain>:8444`, trusting the Test CA from `http://localhost:8025/api/test-ca.pem` |
| Identity | the authority returned by `list_providers` |
| OpenTelemetry | `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318` |
| GELF / StatsD | `localhost:12201` (UDP) / `localhost:8125` (UDP) |
| The app's own webhooks | `http://localhost:8027/in/<inbox>` |
| OpenAI / Azure OpenAI | `http://localhost:8025/openai/v1` / `http://localhost:8025/azure-openai`, any key |
| Anthropic / Gemini | `http://localhost:8025/anthropic` / `http://localhost:8025/gemini`, any key |
| OpenRouter / DeepSeek | `http://localhost:8025/openrouter` / `http://localhost:8025/deepseek`, any key |
| PostHog, Segment, Mixpanel, GA4 | `http://localhost:8025/posthog`, `/segment`, `/mixpanel`, `/ga4` |
| IMAP / POP3 | `localhost:1143` / `localhost:1110`, login = the mailbox address (STARTTLS uses a self-signed certificate) |
| FTP / FTPS / SFTP | `localhost:2121` / `localhost:2222`, users from `set_file_server` |
| Kafka / Schema Registry | `bootstrap.servers=localhost:9092`, `schema.registry.url=http://localhost:8081` |
| RabbitMQ management API | `http://localhost:15672` |
| ActiveMQ (OpenWire) | `tcp://localhost:61616` (Java, Spring `spring.activemq.broker-url`) / `activemq:tcp://localhost:61616` (Apache.NMS.ActiveMQ); keep a `failover:(…)` wrapper if the app has one, with only this URL inside |
| Azure Event Hubs | `Endpoint=sb://<namespace>.localhost:5673/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=<key>;EntityPath=<hub>;UseDevelopmentEmulator=true` (`list_eventhubs` gives it); Kafka clients: `localhost:9092`, `SASL_PLAINTEXT`, `PLAIN`, user `$ConnectionString`, password that connection string |
| Google Cloud Pub/Sub | `PUBSUB_EMULATOR_HOST=localhost:8085` (no credentials needed; in .NET also `EmulatorDetection = EmulatorDetection.EmulatorOrProduction`) |
| Amazon Kinesis | `AWS_ENDPOINT_URL_KINESIS=http://localhost:4567` (or the client's endpoint override), any access key and secret, any region |
| SAML 2.0 | IdP metadata `http://localhost:8025/login/<tenant>/saml/metadata` |
| LDAP | `ldap://localhost:3389`, base DN from `set_provider` |

How to write it, in this order of preference:

1. A file the project already uses for local or staging settings (for example `.env.local`, `.env.development`,
   `appsettings.Development.json`, `config/development.*`, a `docker-compose.override.yml`).
2. Otherwise a new `.env.stuntman` file, plus one line telling the developer how to load it.

**Show the full change first and wait for a yes.** If a production file is the only place a URL lives, do not edit
it: explain and propose an environment-variable override instead.

## Step 7 of 8 — Prove it

1. Run the app's own smoke path if one exists (a test, a seed script, a health check), or ask the developer to use
   the app for a minute.
2. Read what arrived: `list_api_calls`, `list_messages`, `list_sms`, `list_whatsapp_messages`, `list_objects`,
   `list_deliveries`, `list_traces`, `list_ai_calls`, `list_product_events`, `list_transfers`,
   `list_mailbox_messages`, `list_stream_messages`, `list_pubsub_messages`, `list_activemq_messages` — whichever lines apply.
3. Check for leaks: `real_provider_calls` (needs telemetry) and `list_unmatched`. Anything listed still goes
   to a real provider or has no route: fix it or tell the developer.
4. Show one small provocation, then undo it. For example: `force_sms_failure {"answer": "500", "count": 1}`,
   or a storage rule with `create_rule` and `how_often: {"first": 1}`, then show on the timeline how the app reacted.

Report the results as a table with ✓ / ✗ per provider.

## Step 8 of 8 — Welcome

Finish with one message in the developer's language, shaped like this:

```text
════════════════════════════════════════════════════════════
  Welcome to Stuntman — <project name> is ready
════════════════════════════════════════════════════════════

  Web UI            http://localhost:8025          watch every call here
  MCP               http://localhost:8025/mcp      already connected to me
  HTTP API          http://localhost:8025/api      GET /api/tools lists everything

  Your providers, now answered by Stuntman
  ✓ SendGrid        http://localhost:8025/sendgrid
  ✓ Azure Blob      http://127.0.0.1:10000/acme-docs
  ✓ Acme KYC        http://localhost:8028/acme-kyc     (new plugin: plugins/acme-kyc.yaml)
  ✓ Leaks           none — no call reached a real provider

  What changed      .env.development (5 lines) — not committed
  To stop / start   docker stop stuntman · docker start stuntman

  Try next — just ask me:
  • "Make the next email bounce and show me what the app does."
  • "Fail the first two uploads with a 503 and check that we retry."
  • "Test sign-up end to end with MFA and fix what breaks."
════════════════════════════════════════════════════════════
```

Keep the frame, translate the words, fill in the real values and list only what you actually set up.

---

## Troubleshooting

| Symptom | What to do |
|---|---|
| `docker: command not found` | Docker is not installed. Ask the developer to install Docker Desktop, OrbStack or Colima, then continue. |
| Port already in use | `lsof -i :<port>`; ask whether to stop that process or map Stuntman to another host port (`-p 127.0.0.1:18025:8025`) and use that port in every URL. |
| Azure SDK rejects the blob endpoint | Use `http` and `127.0.0.1` (not `localhost`), exactly as in the table. |
| Kafka clients connect, then time out | The broker advertises `localhost`. When the app runs in another container, start Stuntman with `-e STUNTMAN_KAFKA_ADVERTISED_HOST=<name the app uses>`. |
| Pub/Sub client calls the real Google API or asks for credentials | `PUBSUB_EMULATOR_HOST` is not set in the app's process (or, in .NET, `EmulatorDetection` is missing). The project id must match `set_pubsub`'s `project` for short names in tools. |
| ActiveMQ client fails with a SecurityException | `set_activemq` has users and the app logs in with another one, or `refuse_login` is on: `set_activemq {"reset": true}`. |
| FTP listings hang | Passive mode needs ports `30000-30009` published as well as `2121`. |
| TLS errors (Key Vault, `:8443`, AMQPS, FTPS) | Trust the Stuntman Test CA (`/api/test-ca.pem`) for the app's process only — never system-wide on a machine that talks to real providers. |
| A call shows up in `list_unmatched` | The plugin has no route for it: add the route with `put_plugin`. |
| The MCP server does not appear | Continue over HTTP (`/api`); tell the developer to restart their agent later to get MCP. |

## Safety, again

Stuntman has **no sign-in yet**. Keep every port on `127.0.0.1` (as in Step 2). Never expose it beyond this machine,
never point production at it and never put real customer data into it.
