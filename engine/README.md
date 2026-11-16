# Stuntman engine

Stand-in for external providers. Services so far: **e-mail** (E1–E4), **webhook inboxes** (H1–H2), **telemetry** (T1–T4), **environments** (N1–N3), **storage** (S1–S5: Azure Blob and S3) and **HTTP APIs** (A1–A5: plugins) of `../design/duble.figcraft`.

## Run

    cargo build -p stuntman && (cd web && trunk build)
    STUNTMAN_DATA=./data STUNTMAN_PORT=8025 STUNTMAN_SMTP_PORT=2587 STUNTMAN_HOOKS_PORT=8027 STUNTMAN_WEB=./web/dist ./target/debug/stuntman
    python3 tests/seed.py http://localhost:8025 2587          # synthetic e-mail data (optional)
    python3 tests/seed_webhook.py http://localhost:8025 8027  # synthetic webhook data (optional)
    cargo build -p stuntman-telemetry --example otlp_send && python3 tests/seed_telemetry.py http://localhost:8025 4318

- UI: http://localhost:8025 · HTTP API: `/api/…` · MCP (streamable HTTP): `POST /mcp` · tool catalog: `GET /api/tools`
- App side, telemetry: OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 (OTLP/HTTP protobuf or JSON) or gRPC :4317
- App side, webhooks: send to `http://localhost:8027/in/<inbox>` (own port: resets and empty replies are real)
- App side, e-mail: SendGrid `POST http://localhost:8025/sendgrid/v3/mail/send` (any key) or SMTP `localhost:2587` (STARTTLS, any login)
- App side, Azure Blob: connection string `DefaultEndpointsProtocol=http;AccountName=<acct>;AccountKey=<key1>;BlobEndpoint=http://127.0.0.1:10000/<acct>;`
  (Services › Azure Blob copies it; Azure SDKs need an IP here, as with Azurite)
- App side, S3 / R2: endpoint `http://127.0.0.1:9000`, path style, access key and secret from Services › S3
  (`STUNTMAN_BLOB_PORT`, `STUNTMAN_S3_PORT` move the ports)
- App side, HTTP APIs: base URL `http://127.0.0.1:8028/<service>` (or the host `<service>.<env>.stuntman.local:8028`,
  or the provider's real host name pointed at Stuntman). Plugins live in `apis/<service>.yaml`; the catalog is `plugins/`
  (`STUNTMAN_APIS_PORT`, `STUNTMAN_PLUGINS` move them)
- Env: `STUNTMAN_ENV` (the default environment, `stg-a`), `STUNTMAN_HOST` (shown in copy buttons)
- Environments: one process serves all of them. A request finds its environment by host name
  (`*.<env>.stuntman.local`, see `GET /api/environments/{name}/hosts`), by the `/e/<name>/` prefix on any HTTP port
  (`http://localhost:8025/e/pr-5462/mcp`, `…:8027/e/pr-5462/in/<inbox>`, `…:4318/e/pr-5462/v1/traces`), by the
  `x-stuntman-env` gRPC metadata, or falls to the default one. Several environments can share an SMTP port: the
  SMTP login (user `<env>`) names the environment; a copy still moves its SMTP services to free ports. A pipeline: `POST /api/environments {"name":"pr-5462","from":"stg-a","expires":"24h"}`.

## Running in Docker (a shared test environment)

```sh
docker compose up -d --build            # image: server + UI + plugin catalog; data in the stuntman-data volume
STUNTMAN_HOST=stuntman.example.com:8025 BIND=10.0.0.5 docker compose up -d   # reachable on a private address
```

- `STUNTMAN_HOST` is the name apps and people use; base URLs, links and MCP show it.
- `BIND` defaults to 127.0.0.1. Stuntman has **no sign-in yet** (Settings › Sign-in is designed, not built):
  publish it only on a private network, a VPN, or behind a proxy that authenticates (Cloudflare Access,
  oauth2-proxy). Anyone who reaches port 8025 can read every message and change every rule.
- Everything lives in `/data` (environments, YAML, SQLite, the Test CA in `/data/keys`). Back up the volume;
  delete it to start over.
- Build for another architecture: `docker buildx build --platform linux/amd64 -t stuntman .`

Pointing an app at it (setting names are examples):

| Provider | Setting | Value |
|---|---|---|
| Zenvia SMS | `SMS_API_URL` | `http://<host>:8025/e/<env>/zenvia/v2/channels/{0}/messages` |
| WhatsApp (Meta) | Graph base URL | `http://<host>:8025/e/<env>/whatsapp` |
| BirdID | `BIRDID_BASE_URL` | `http://<host>:8028/e/<env>/birdid/v0` |
| ViDaaS | `VIDAAS_BASE_URL` | `http://<host>:8028/e/<env>/vidaas` |
| REST PKI (Lacuna) | `RESTPKI_ENDPOINT` | `http://<host>:8028/e/<env>/restpki/` (the browser's Web PKI stand-in posts to `…/restpki/__webpki/{token}/start` and `/signature`) |
| Service Bus | connection string | `Endpoint=sb://<namespace>.<env base domain>:5673;…` (any key) |
| RabbitMQ | host / vhost | `<host>:5673`, vhost `<env>/<namespace>` |
| Azure Blob | connection string | `BlobEndpoint=http://<host>:10000/<account>;…` |
| Key Vault | vault URL | `https://<vault>.vault.<env base domain>:8444` (trust the Test CA) |
| OpenTelemetry | OTLP endpoint | `http://<host>:4318` (or gRPC 4317) |
| SMTP | host / port | `<host>:2525` |

`/e/<env>` is left out for the default environment. Service Bus and Key Vault find the namespace or vault by
host name: give each environment a base domain under a wildcard DNS that points at the server
(Environments › settings, e.g. `acme.stuntman.example.com` with `*.stuntman.example.com` → the host). The Test CA is at `http://<host>:8025/api/test-ca.pem`.

## Check

    cargo test --workspace --exclude stuntman-web
    cargo build -p stuntman && python3 tests/e2e_email.py    # SMTP+STARTTLS, SendGrid, signed events, inboxes, held, access, MCP
    python3 tests/mcp_api.py http://localhost:8025 2587    # every e-mail tool over MCP and HTTP, against a running server
    cargo build -p stuntman && python3 tests/e2e_webhook.py  # signatures, replay, retries, hang, reset, empty, traps, 410, MCP
    cargo build -p stuntman-telemetry --example otlp_send && python3 tests/e2e_telemetry.py  # OTLP JSON/protobuf/gRPC, trips, logs, metrics, provoke, forward
    cargo build -p stuntman && python3 tests/e2e_storage.py  # az CLI (Microsoft SDK) + aws CLI (botocore): SharedKey/SAS/SigV4, blocks, multipart, links, events, rules, real resets
    (cd tests/dotnet-azure-blob && dotnet run -- "<connection string>")  # common blob operations with Azure.Storage.Blobs
    python3 tests/e2e_blob_hoststyle.py  # the get_account connection string on a named host: azure-storage-blob + the .NET check above
    python3 tests/seed_storage.py http://localhost:8025 10000 9000 8027  # synthetic storage data for the screens
    cargo build -p stuntman && python3 tests/e2e_apis.py  # SERPRO OAuth2 + data-driven answers, forced, billing, rate, fail share, reset/empty, real host, wait, webhooks, unmatched, try, schema, MCP
    python3 tests/seed_apis.py http://localhost:8025 8028  # synthetic API traffic for the screens
    cargo build -p stuntman && python3 tests/e2e_environments.py  # create/clone/folder, isolation per door, pause 503, settings 409, reset, expiry, delete, MCP

## Layout (add a service without touching the others)

    crates/core     shared: SQLite, Tool (= MCP tool + HTTP route), extractors, signed callbacks, live events, YAML files
    crates/email    the e-mail module: config (YAML), smtp, sendgrid, ingest, inboxes + steps, lifecycle, events, tools
    crates/telemetry OTLP receiver (own ports, own data/telemetry.sqlite, FTS5 on logs), traces↔trips, metrics math, tools
    crates/storage  Azure Blob + S3 on their own ports (hyper per connection, so reset/empty are real), auth (SharedKey,
                    SAS, SigV4), gate (reachability, limit, next read, rules), links, events (Event Grid / S3), tools
    crates/apis     HTTP plugin engine: expr (CEL-like subset + {{ }} templates), plugin (YAML model, validation with lines),
                    engine (receive → let → keep → wait → decide → respond → send), store, tools; plugins/ is the catalog
    crates/webhook  webhook inboxes: config (webhooks/inboxes.yaml + ephemeral in SQLite), listener (own port), sign, store, tools
    crates/server   wires modules: one router per environment (/api from tools, /mcp, SSE, UI), dispatch by host or /e/<name>/;
                    environments.rs = N1–N3 (create, clone, pause, reset, extend, delete, expiry sweep)
    web/            Leptos (CSR): shell + one folder per service (web/src/email, web/src/webhook, web/src/telemetry)

A new service is a crate implementing `stuntman_core::Module` (migrations, tools, extra routes, start), one line
in `crates/server/src/main.rs::modules()`, a folder in `web/src/<service>` and its routes/rail entry in `web/src/shell.rs`.

Each environment is a folder `data/environments/<name>/`: `environment.yaml`, `email/services/*.yaml`,
`email/extractors.yaml`, `webhooks/inboxes.yaml`, `telemetry/<name>.yaml`, `secrets.yaml`, and its state in
`duble.sqlite` + `telemetry.sqlite`. An old flat `data/` folder moves into `environments/<STUNTMAN_ENV>/` on start.
The events signing key `data/keys/events-ecdsa.pem` is shared.

## Known limits (ponytail)

- Step timers live in memory: a restart forgets steps still waiting.
- On the shared HTTP port, SendGrid "refused" and "TLS failure" answer 502 (SMTP does both for real).
- T4 “Rule on” means Stuntman answered with provoked failures during the jump (bounces, drops, 4xx/5xx, resets); named scenario rules come with the Scenarios service.
- Environments: the OTLP ports follow the default environment's settings ("refused" there takes them down for all);
  receiver counters (refused, dropped) are per process, not per environment.
- Not built yet (no service to back them): scenarios count/copy, members and keys ("who can use it"), the Stuntman
  Test CA download, and probing whether the callback target answers (N3 "callback target down").
- Storage: metadata names come back lower-case (HTTP headers lose their case on the way in); Azure keeps the case.
- Storage: objects live in SQLite (fine for test files; a 1 GB upload is held in memory once).
- Storage: Service Bus subscriptions wait for the Queues service; user-delegation SAS is accepted as any SAS.
- APIs: expressions are a CEL-like subset (no list macros); time zones are fixed offsets (no DST rules).
- APIs: saving through the screen or tools rewrites the plugin file, so YAML comments are not kept (edit by hand to keep them).
- APIs over HTTPS: port 8443 (`STUNTMAN_APIS_TLS_PORT`) answers any host name with a certificate signed by the Stuntman
  Test CA (download: `GET /api/keyvault/test-ca.pem`); a plugin with `client_certificate.required` gets real mTLS
  (issue an app certificate with `issue_client_certificate`). Session-cookie portals use `auth.session` + a `login` route.
