# Configuration

Stuntman is configured with environment variables and with YAML files inside its data folder. Everything has a
default; most setups change nothing.

## Environment variables

| Variable | Default | What it sets |
|---|---|---|
| `STUNTMAN_DATA` | `data` (`/data` in Docker) | where environments, YAML, SQLite and keys live |
| `STUNTMAN_ENV` | `stg-a` | the default environment |
| `STUNTMAN_HOST` | `localhost:<STUNTMAN_PORT>` | the name apps and people use; shown in base URLs, links and MCP |
| `STUNTMAN_WEB` | `web/dist` (`/app/web` in Docker) | the built screen |
| `STUNTMAN_PLUGINS` | `plugins` next to the engine (`/app/plugins` in Docker) | the plugin catalog |
| `STUNTMAN_PORT` | `8025` | screen, `/api`, `/mcp`, SMS, WhatsApp, SendGrid, identity |
| `STUNTMAN_SMTP_PORT` | `2525` | SMTP |
| `STUNTMAN_HOOKS_PORT` | `8027` | webhook inboxes |
| `STUNTMAN_APIS_PORT` | `8028` | HTTP API plugins |
| `STUNTMAN_APIS_TLS_PORT` | `8443` | HTTP API plugins over TLS (any host name, Test CA) |
| `STUNTMAN_KEYVAULT_PORT` | `8444` | Key Vault (TLS) |
| `STUNTMAN_AMQP_PORT` | `5673` | AMQP (Service Bus, RabbitMQ) |
| `STUNTMAN_AMQPS_PORT` | `5671` | AMQP over TLS |
| `STUNTMAN_BLOB_PORT` | `10000` | Azure Blob |
| `STUNTMAN_S3_PORT` | `9000` | Amazon S3 |
| `STUNTMAN_OTLP_HTTP_PORT` | `4318` | OpenTelemetry over HTTP |
| `STUNTMAN_OTLP_GRPC_PORT` | `4317` | OpenTelemetry over gRPC |

The project was first called Dublê: `DUBLE_*` variables still work when the `STUNTMAN_*` one is unset.

## Ports

| Port | What answers |
|---|---|
| `8025` | screen, HTTP API (`/api`), MCP (`/mcp`), SMS, WhatsApp, SendGrid, identity, managed identity |
| `2525` | SMTP (STARTTLS) |
| `8027` | webhook inboxes (`/in/<inbox>`) |
| `8028` / `8443` | HTTP API plugins / over TLS |
| `8444` | Key Vault (TLS) |
| `5673` / `5671` | AMQP / over TLS |
| `10000` | Azure Blob |
| `9000` | Amazon S3 |
| `4318` / `4317` | OTLP HTTP / gRPC |

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

The YAML files are the source of truth; the screen and the tools edit them. You can keep a folder of environments
in git and start Stuntman from it.

## The Stuntman Test CA

TLS endpoints (Key Vault, HTTP plugins on `:8443`, AMQPS) use certificates signed by the Stuntman Test CA, created
on first start. Trust it on the machine or container that runs your app:

```sh
curl -s http://localhost:8025/api/test-ca.pem -o stuntman-test-ca.pem
```

Never trust it anywhere that also talks to real providers.
