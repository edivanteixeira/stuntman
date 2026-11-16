# Configuration

Stuntman is configured with environment variables and with YAML files inside its data folder. Everything has a
default; most setups change nothing.

## Environment variables

| Variable | Default | What it sets |
|---|---|---|
| `STUNTMAN_DATA` | `data` (`/data` in Docker) | where environments, YAML, SQLite and keys live |
| `STUNTMAN_ENV` | `staging` | the default environment |
| `STUNTMAN_HOST` | `localhost:<STUNTMAN_PORT>` | the name apps and people use; shown in base URLs, links and MCP |
| `STUNTMAN_AUTH_ISSUER` | none (sign-in off) | the OIDC provider people sign in with, e.g. `https://login.microsoftonline.com/<tenant>/v2.0`; turns sign-in on ([Environments › Sign-in](environments.md#sign-in-the-central-mcp-and-tokens)) |
| `STUNTMAN_AUTH_CLIENT_ID`, `STUNTMAN_AUTH_CLIENT_SECRET` | none | the app registered at that provider; its redirect URI is `<public url>/auth/callback` |
| `STUNTMAN_AUTH_ALLOWED_DOMAINS`, `STUNTMAN_AUTH_ALLOWED_EMAILS` | none | who may sign in (comma separated); checked on every request |
| `STUNTMAN_ADMIN_KEY` | none | `Authorization: Bearer <key>` on any route, for a pipeline that makes and deletes environments |
| `STUNTMAN_PUBLIC_URL` | none | one public address for a shared Stuntman (`https://stuntman.example.com`): the root lists the environments, each one lives under `/e/<name>/`, and identity names `<url>/e/<name>` ([Environments](environments.md#a-shared-stuntman-one-public-address)) |
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
| `STUNTMAN_SERVICEBUS_MGMT_PORT` | `5300` | Service Bus management (what `ServiceBusAdministrationClient` calls), plain HTTP and HTTPS, plus AMQP; the official emulator's port |
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
| `5300` | Service Bus management over HTTP or HTTPS, and AMQP ([Providers › Messaging](providers.md#management-administration-client)) |
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

TLS endpoints (Key Vault, HTTP plugins on `:8443`, AMQPS, the HTTPS side of `:5300`, FTPS) use certificates signed by
the Stuntman Test CA, created on first start in `data/keys/test-ca.pem`. Download it in the format your platform
takes (none of them holds the private key):

| URL | Format | For |
|---|---|---|
| `http://localhost:8025/api/test-ca.pem` | PEM | Node, Python, Go, curl, Linux system store, `SSL_CERT_FILE` |
| `http://localhost:8025/api/test-ca.crt` (same as `.der`) | DER | macOS Keychain, Windows certificate store |
| `http://localhost:8025/api/test-ca.p12` (same as `.pfx`) | PKCS#12, CA only, password `changeit` | Java truststore, certificate-only PFX |

The `get_test_ca` tool returns the same URLs, the file path and the options below.

Trust it **for the app's test process** whenever the runtime allows it; that changes nothing else on the machine:

| Runtime | Per process (prefer) |
|---|---|
| Node | `NODE_EXTRA_CA_CERTS=stuntman-test-ca.pem` |
| Python | `REQUESTS_CA_BUNDLE=stuntman-test-ca.pem` (requests, the Azure SDKs) and `SSL_CERT_FILE=stuntman-test-ca.pem` (`ssl`, httpx, aiohttp); code that passes `certifi.where()` explicitly needs `verify=` / `cafile=` pointed at the file instead |
| Java | `-Djavax.net.ssl.trustStore=stuntman-test-ca.p12 -Djavax.net.ssl.trustStoreType=PKCS12 -Djavax.net.ssl.trustStorePassword=changeit` (replaces the JVM's default truststore for that run) |
| Go | Linux: `SSL_CERT_FILE=stuntman-test-ca.pem`. macOS and Windows use the OS verifier and ignore it: add the PEM to `tls.Config.RootCAs` in test config, or the OS store below |
| curl | `curl --cacert stuntman-test-ca.pem` or `CURL_CA_BUNDLE=stuntman-test-ca.pem` |
| .NET on Linux | `SSL_CERT_FILE=stuntman-test-ca.pem` (OpenSSL reads it) |
| .NET on macOS and Windows | `SSL_CERT_FILE` is ignored. Validate against the CA in test configuration (below), or use the OS store |

.NET on macOS and Windows, in the test configuration only:

```csharp
var ca = X509Certificate2.CreateFromPem(File.ReadAllText("stuntman-test-ca.pem"));
var handler = new SocketsHttpHandler();
handler.SslOptions.RemoteCertificateValidationCallback = (_, cert, _, errors) =>
{
    if (errors == SslPolicyErrors.None) return true;
    using var chain = new X509Chain();
    chain.ChainPolicy.TrustMode = X509ChainTrustMode.CustomRootTrust;
    chain.ChainPolicy.CustomTrustStore.Add(ca);
    chain.ChainPolicy.RevocationMode = X509RevocationMode.NoCheck;
    return cert is X509Certificate2 c && chain.Build(c);
};
// HttpClient: new HttpClient(handler). Azure SDK clients (Key Vault, Blob, admin clients):
var options = new SecretClientOptions { Transport = new HttpClientTransport(handler) };
// Service Bus / Event Hubs over AMQPS: ServiceBusClientOptions.CertificateValidationCallback (same check).
```

Or trust it in the OS store, which every process of that user then trusts. **It changes the developer's machine:
ask them first** (coding agents: never do this silently), and never on a machine that also talks to real providers.

| OS | Trust (current user) | Undo |
|---|---|---|
| macOS | `security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db stuntman-test-ca.crt` (asks for the password or Touch ID) | `security delete-certificate -c "<CA name>" ~/Library/Keychains/login.keychain-db` |
| Windows | `certutil -user -addstore Root stuntman-test-ca.crt`, or PowerShell `Import-Certificate -FilePath stuntman-test-ca.crt -CertStoreLocation Cert:\CurrentUser\Root` (Windows asks to confirm) | `certutil -user -delstore Root "<CA name>"` |
| Linux, containers | `cp stuntman-test-ca.pem /usr/local/share/ca-certificates/stuntman-test-ca.crt && update-ca-certificates` (Debian, Ubuntu; as root) | remove the file, `update-ca-certificates --fresh` |

`<CA name>` is the certificate's common name; `get_test_ca` returns it as `name` and fills in the undo commands.

STARTTLS on SMTP, IMAP and POP3 uses a self-signed certificate for `localhost` instead, made at each start: turn
certificate checks off in the app's mail client for those.
