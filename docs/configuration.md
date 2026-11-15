# Configuration

Stuntman is configured with environment variables and with YAML files inside its data folder. Everything has a
default; most setups change nothing.

## Environment variables

| Variable | Default | What it sets |
|---|---|---|
| `STUNTMAN_DATA` | `data` (`/data` in Docker) | where environments, YAML, SQLite and keys live |
| `STUNTMAN_ENV` | `staging` | the default environment |
| `STUNTMAN_HOST` | `localhost:<STUNTMAN_PORT>` | the name apps and people use; shown in base URLs, links and MCP |
| `STUNTMAN_WEB` | `web/dist` (`/app/web` in Docker) | the built screen |
| `STUNTMAN_PLUGINS` | `plugins` next to the engine (`/app/plugins` in Docker) | the plugin catalog |
| `STUNTMAN_PORT` | `8025` | screen, `/api`, `/mcp`, SMS, WhatsApp, SendGrid, identity, AI models, product events, GELF over HTTP |
| `STUNTMAN_SMTP_PORT` | `2525` | SMTP |
| `STUNTMAN_HOOKS_PORT` | `8027` | webhook inboxes |
| `STUNTMAN_APIS_PORT` | `8028` | HTTP API plugins |
| `STUNTMAN_APIS_TLS_PORT` | `8443` | HTTP API plugins over TLS (any host name, Test CA) |
| `STUNTMAN_KEYVAULT_PORT` | `8444` | Key Vault (TLS) |
| `STUNTMAN_AMQP_PORT` | `5673` | AMQP (Service Bus, Event Hubs, RabbitMQ) |
| `STUNTMAN_AMQPS_PORT` | `5671` | AMQP over TLS |
| `STUNTMAN_BLOB_PORT` | `10000` | Azure Blob |
| `STUNTMAN_S3_PORT` | `9000` | Amazon S3 |
| `STUNTMAN_OTLP_HTTP_PORT` | `4318` | OpenTelemetry over HTTP |
| `STUNTMAN_OTLP_GRPC_PORT` | `4317` | OpenTelemetry over gRPC |
| `STUNTMAN_GELF_PORT` | `12201` | GELF over UDP |
| `STUNTMAN_STATSD_PORT` | `8125` | StatsD over UDP |
| `STUNTMAN_LDAP_PORT` | `3389` | LDAP directory |
| `STUNTMAN_FTP_PORT` | `2121` | FTP and explicit FTPS |
| `STUNTMAN_FTP_PASSIVE_PORTS` | any free port (`30000-30009` in Docker) | the range FTP passive data connections use |
| `STUNTMAN_FTP_PASSIVE_HOST` | the address the client connected to (`127.0.0.1` in Docker) | the IP address passive replies (PASV) give; a host name is ignored |
| `STUNTMAN_SFTP_PORT` | `2222` | SFTP |
| `STUNTMAN_IMAP_PORT` | `1143` | IMAP mailboxes |
| `STUNTMAN_POP3_PORT` | `1110` | POP3 mailboxes |
| `STUNTMAN_KAFKA_PORT` | `9092` | Kafka broker |
| `STUNTMAN_KAFKA_ADVERTISED_HOST` | `localhost` | the host the broker tells clients to connect to (set it when the app runs in another container) |
| `STUNTMAN_SCHEMA_REGISTRY_PORT` | `8081` | Schema Registry |
| `STUNTMAN_PUBSUB_PORT` | `8085` | Google Cloud Pub/Sub emulator (gRPC, and the REST publish) |
| `STUNTMAN_KINESIS_PORT` | `4567` | Amazon Kinesis Data Streams (apps set `AWS_ENDPOINT_URL_KINESIS=http://localhost:4567`) |
| `STUNTMAN_RABBITMQ_MGMT_PORT` | `15672` | RabbitMQ management HTTP API |
| `STUNTMAN_ACTIVEMQ_PORT` | `61616` | ActiveMQ (OpenWire) broker |

The project was first called Dublê: `DUBLE_*` variables still work when the `STUNTMAN_*` one is unset.

## Ports

| Port | What answers |
|---|---|
| `8025` | screen, HTTP API (`/api`), MCP (`/mcp`), SMS, WhatsApp, SendGrid, identity (OIDC, SAML, Bot Framework), managed identity, AI models, product events, GELF over HTTP (`/gelf`) |
| `2525` | SMTP (STARTTLS) |
| `8027` | webhook inboxes (`/in/<inbox>`) |
| `8028` / `8443` | HTTP API plugins / over TLS |
| `8444` | Key Vault (TLS) |
| `5673` / `5671` | AMQP / over TLS |
| `10000` | Azure Blob |
| `9000` | Amazon S3 |
| `4318` / `4317` | OTLP HTTP / gRPC |
| `12201/udp` / `8125/udp` | GELF / StatsD |
| `3389` | LDAP |
| `2121` (+ `30000-30009`) | FTP and FTPS (passive data range in Docker) |
| `2222` | SFTP |
| `1143` / `1110` | IMAP / POP3 |
| `9092` / `8081` | Kafka / Schema Registry |
| `8085` | Google Cloud Pub/Sub (gRPC; REST publish) |
| `4567` | Amazon Kinesis Data Streams |
| `15672` | RabbitMQ management API |
| `61616` | ActiveMQ (OpenWire) |

LDAP, FTP, SFTP, IMAP, POP3, Kafka, the Schema Registry, Pub/Sub, Kinesis, the RabbitMQ management API, ActiveMQ, GELF and StatsD have no host
name to tell environments apart, so they serve the default environment (GELF and StatsD can name another one; see
[Providers › Telemetry](providers.md#telemetry)).

## Docker

```sh
cd engine
docker compose up -d --build
```

`docker-compose.yml` publishes every port on `127.0.0.1`. For a shared test environment on a private network:

```sh
STUNTMAN_HOST=stuntman.example.internal:8025 BIND=10.0.0.5 docker compose up -d
```

- `STUNTMAN_HOST` is the name apps and people use; base URLs, links and MCP show it.
- `BIND` is the address the ports listen on. Read [Security](security.md) before changing it.
- Everything lives in the `/data` volume. Back it up to keep it; delete it to start over.
- Build for another architecture: `docker buildx build --platform linux/amd64 -t stuntman .`

## Data folder

```text
data/
  environments/<name>/   one folder per environment (see Environments › On disk)
  keys/                  the events signing key and the Stuntman Test CA
```

The YAML files are the source of truth; the web UI and the tools edit them. You can keep a folder of environments
in git and start Stuntman from it.

## The Stuntman Test CA

TLS endpoints (Key Vault, HTTP plugins on `:8443`, AMQPS, FTPS) use certificates signed by the Stuntman Test CA, created
on first start. Trust it on the machine or container that runs your app:

```sh
curl -s http://localhost:8025/api/test-ca.pem -o stuntman-test-ca.pem
```

Never trust it anywhere that also talks to real providers.

STARTTLS on SMTP, IMAP and POP3 uses a self-signed certificate for `localhost` instead, made at each start: turn
certificate checks off in the app's mail client for those.
