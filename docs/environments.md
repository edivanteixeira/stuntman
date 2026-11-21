# Environments

One Stuntman serves many environments. Each has its own plugins, state, rules, inboxes, base URLs, callback target
and MCP endpoint. Pause one and its state is kept; clone one for a pull request and let it expire.

![Environments: STG-A, STG-B (paused), STG-C and pr-5451, cloned by a pipeline, expiring in 22 h](images/screen-environments.png)

## One per branch

<p align="center"><img src="images/env-lifecycle.png" alt="The life of pr-5462: cloned from stg-a, the pipeline points the app, an agent tests the branch, paused for review, expires by itself" width="100%"></p>

```sh
curl -s -X POST localhost:8025/api/environments -H 'content-type: application/json' \
     -d '{"name": "pr-5462", "from": "stg-a", "expires": "24h"}'
```

| Argument | Meaning |
|---|---|
| `name` | lowercase, digits and dashes, 1 to 40 characters |
| `from` | an environment to copy, a template (`templates/<name>`), `"empty"`, or a folder |
| `vars` | the `${…}` values of a template or folder, e.g. `{"domain": "preview.acme.example"}` |
| `copy` | what the copy takes: services, state, inboxes, secrets… (default: everything but state) |
| `expires` | e.g. `24h`, `72h`; the environment deletes itself afterwards |
| `callbacks` | where provider callbacks go for this environment |
| `base_domain` | apps reach every service by name under it, e.g. `pr-5462.stuntman.svc.local` |
| `public_url` | where browsers open it, e.g. `https://stuntman.example.com/e/pr-5462` |
| `allowed_email_domains` | real e-mail domains identity accepts for people (synthetic only by default) |

The answer lists the new environment's addresses: its MCP endpoint, its API, its host names.

| Action | Tool | Route |
|---|---|---|
| List / read | `list_environments`, `get_environment` | `GET /api/environments[/{name}]` |
| Create or clone | `create_environment` | `POST /api/environments` |
| Change callbacks, lifetime, base domain, public URL, e-mail domains | `set_environment` | `PUT /api/environments/{name}` |
| Push the expiry (default 24 h) | `extend_environment` | `POST /api/environments/{name}/extend` `{"by": "24h"}` |
| Pause or resume | `pause_environment` | `POST /api/environments/{name}/pause` |
| What the app sets (connection strings, URLs, `.env`) | `get_connections` | `GET /api/environments/{name}/connections[?base=…]` |
| One app's variables, filled in (see below) | `get_app_env` | `GET /api/environments/{name}/apps/{app}`, or `GET /api/env/{app}` as `KEY=VALUE` lines |
| Templates and their values | `list_environment_templates` | `GET /api/environments/templates` |
| Delete, with everything in it | `delete_environment` | `DELETE /api/environments/{name}` |

`PUT /api/environments/{name}` with `{"expires": "72h"}` sets the expiry to 72 hours from now; `extend` adds to the
current expiry.

A **paused** environment answers 503 on every service, stores nothing, and keeps its state.

### The apps of an environment

`apps` in `environment.yaml` (or `set_environment` with `apps`) lists the apps that use the environment and the
variables each one sets. A value can name a connection setting (`${QUEUES_CONNECTION}`, any key of
`get_connections`) or a secret (`${secret:name}`, `${secret:vault/name}`):

```yaml
apps:
  api:
    BUS: ${QUEUES_CONNECTION}
    KEYVAULT_URL: ${KEYVAULT_URL}
    IDENTITY_ENDPOINT: http://localhost:8025/msi/token
    IDENTITY_HEADER: api-mi
  worker:
    BUS: ${QUEUES_CONNECTION}
    SIGNING_PASSWORD: ${secret:acme-kv/signing-password}
    IDENTITY_ENDPOINT: http://localhost:8025/msi/token
    IDENTITY_HEADER: worker-mi
    SSL_CERT_FILE: /tmp/stuntman-ca-bundle.pem
```

`curl -s http://localhost:8025/api/env/worker > .env` gives the app its lines; `/e/<name>/api/env/worker` reads another
environment. A name that is not known stays as written and is listed under `missing` in `get_app_env`. Secret reads go
to the vault's access log as `app <name>`. `/api/env/{app}` needs sign-in like the rest of `/api` when it is on. The
apps come with the environment when it is cloned or made from a template.

## How a request finds its environment

<p align="center"><img src="images/env-routing.png" alt="By host name, by path prefix, by gRPC metadata" width="100%"></p>

| By | Example |
|---|---|
| Host name | `*.pr-5462.stuntman.local` (see `GET /api/environments/{name}/hosts`) |
| Path prefix, on any HTTP port | `http://localhost:8025/e/pr-5462/mcp`, `:8027/e/pr-5462/in/<inbox>`, `:4318/e/pr-5462/v1/traces` |
| gRPC metadata | `x-stuntman-env: pr-5462` |

Anything else falls to the default environment (`STUNTMAN_ENV`, `staging` unless set).

**Apps outside a container.** A process that runs straight on the machine (not in compose, where `extra_hosts`
does it) needs the host names in `/etc/hosts` and the Test CA trusted:

```sh
curl -s localhost:8025/api/environments/pr-5462/hosts | jq -r .hosts | sudo tee -a /etc/hosts
sudo sed -i.bak '/# stuntman:pr-5462/d' /etc/hosts        # undo: every line carries that tag
curl -s localhost:8025/api/apis/test-ca | jq .runtimes   # trust commands per runtime and OS, with the undo
```

**SMTP.** Several environments can listen on the same SMTP port. On a port only one environment uses, nothing
changes. On a shared port the SMTP login names the environment: user `<env>` (or `<env>/<service>`), any password,
or one of the service's keys when keys are checked; a STARTTLS server name under the environment's base domain also
picks it. Without a login the server answers `530` and lists the environments on the port. A template or a folder
keeps its SMTP port (the environments share it); a copy of an environment still moves its SMTP services to free ports.

Service Bus and Key Vault find the namespace or vault by host name. Give each environment a **base domain** under a
wildcard DNS name that points at the server, e.g. `acme.stuntman.example.com` with `*.stuntman.example.com` → the
host.

An environment with a **base domain of its own** (anything but the automatic `<name>.stuntman.local`) gets addresses
by name everywhere: the Service Bus connection string names `sb://<namespace>.<base domain>`, a webhook inbox is
`http://<base domain>:8027/in/<inbox>`, SendGrid, Zenvia and OTLP use the base domain without a prefix. A
`.localhost` base domain needs no DNS at all.

## Templates

A template is a folder in `templates/` on the server (`/data/templates/<name>` in Docker), laid out like an
environment's folder. Any text in it may hold `${…}` values; `${name}` is always the new environment's name and the
others come in `vars`. A value with no `vars` entry refuses the creation (400, with each value and the files that use
it); nothing is made. The template's `environment.yaml` may set `base_domain`, `public_url`, `callbacks`,
`real_hosts` and `allowed_email_domains`, with values in them.

```sh
curl -s -X POST localhost:8025/api/environments -H 'content-type: application/json' -d '{
  "name": "pr-5462", "from": "acme-preview", "vars": {"domain": "preview.acme.example"}, "expires": "72h" }'
```

## A shared Stuntman, one public address

Set `STUNTMAN_PUBLIC_URL=https://stuntman.example.com` and put one reverse proxy (an Ingress) in front of the main
port. The root lists the environments; everything of one environment lives under `/e/<name>/`: its screens (reload
any page and the same page opens), `/e/<name>/api`, `/e/<name>/mcp` and its identity pages `/e/<name>/login/<tenant>`.

Identity names the address a browser uses. Its issuer, discovery, authorize, token, keys and logout URLs are, in
order: the environment's `public_url`; else `STUNTMAN_PUBLIC_URL/e/<name>`; else the address the call came by
(`X-Forwarded-Proto` and `X-Forwarded-Host` first, then `Host`, plus `/e/<name>` when it came by the prefix). With a
public address set, a token a browser got there is valid for the app that reaches Stuntman inside the network.

A single-page app (MSAL) reads discovery and keys and redeems its code from the browser: those routes answer CORS.
The token endpoint lets a browser in only for an app of type `spa`, and only from the origin of one of its redirect
URIs; any other origin gets `AADSTS9002326`, as Entra answers.

People with real e-mail addresses (the app's database is a copy of staging and only knows them) need their domain in
the environment's `allowed_email_domains`; every other address stays synthetic only.

Azure Blob is on its own port; a SAS link the app hands to a browser also works through the main port, at
`<public url>/blob/<account>/<container>/<blob>?<sas>`. A link made in Stuntman shows it as `publicUrl`.

`GET /api/environments/{name}/connections` lists what the app sets (`?base=http://stuntman.svc` gives the
path-prefix form where a protocol takes it; Blob, Service Bus, Key Vault and SMTP keep host names). `env` in the
answer is the same as `.env` lines.

## Sign-in, the central MCP and tokens

Off unless `STUNTMAN_AUTH_ISSUER` is set. Register an app at the company's OIDC provider with the redirect URI
`<public url>/auth/callback`, then:

```sh
STUNTMAN_PUBLIC_URL=https://stuntman.example.com
STUNTMAN_AUTH_ISSUER=https://login.microsoftonline.com/<tenant>/v2.0
STUNTMAN_AUTH_CLIENT_ID=<app id>
STUNTMAN_AUTH_CLIENT_SECRET=<from a secret>
STUNTMAN_AUTH_ALLOWED_DOMAINS=example.com          # and/or STUNTMAN_AUTH_ALLOWED_EMAILS=a@x.com,b@x.com
STUNTMAN_ADMIN_KEY=<from a secret>
```

| Route | Who | Needs |
|---|---|---|
| Screens (`/`, `/e/<name>/…`) | a person in a browser | a session: the company's sign-in (code flow with PKCE), then a signed cookie for 12 hours |
| Control API (`/api`, `/e/<name>/api`) | a pipeline, an agent, the screen | a session, the admin key, an agent's token, or the environment's token |
| Central MCP (`/mcp`) | a person's agent | OAuth: the client registers, the person signs in and allows it (access 8 h, refresh 30 days) |
| One environment's MCP (`/e/<name>/mcp`) | automation | that environment's token (`Authorization: Bearer stm_env_…`), or any of the above |
| Simulated identity (`/e/<name>/login/…`), provider routes, the Test CA | the app under test and its browser | nothing, as before |

The rule is by route, not by network: a call from inside the cluster to the control API needs a token too. Someone
taken off the list loses their session and every token at their next request.

**The central MCP** is one address for everyone: `claude mcp add --transport http stuntman https://stuntman.example.com/mcp`.
The first use opens the browser (sign-in, then "Allow"). Each call works in one environment: the `environment` argument
(every tool has it), else the session's (`use_environment`), else the person's (the **Use in MCP** button, or
`PUT /api/users/{email}/current-environment` with the admin key). There is no silent default: without one the tool
answers with the list. Every answer starts with `[name]`. `create_environment` and `delete_environment` need the
admin key there: people make environments in the pipeline.

**An environment's token** (`stm_env_…`) comes back from `create_environment` and opens only `/e/<name>/api` and
`/e/<name>/mcp`; a person sees it in the environment's settings, `rotate_environment_token` replaces it, and it goes
away with the environment. **Settings › MCP access** lists people's agents and revokes them.

## Delete and expiry

`DELETE /api/environments/{name}` takes everything at once: the folder (YAML, `duble.sqlite`, `telemetry.sqlite`,
identity keys, vault certificates, secrets), its SMTP listeners and its host names; afterwards the environment
answers 404 and the name is free for a new one, which starts clean. Expiry (checked every 30 s) deletes the same way.

## Upgrades and what is in /data

A new version opens every environment in `/data` as it was, with its state. Database changes are migrations that
only add tables and columns, and new `environment.yaml` keys are optional: an older file reads as before. Pin the
image tag (`edivanteixeira/stuntman:0.11.15`) and read the release notes before you move it.

## Sizing

Measured on the 0.9.0 image (arm64, Docker Desktop): six environments, each receiving OpenTelemetry traces from its
app (about 550 spans per second in all) for a minute, used 25–33 MiB of memory and 3.5 % of one core on average
(5.8 % at most); `/data` held 46 MB afterwards. Telemetry is the heaviest steady load; a 512 MiB limit leaves ample
room for six environments.

## On disk

Each environment is a folder you can read, diff and back up:

```text
data/environments/<name>/
  environment.yaml        lifetime, callbacks, base domain, public URL, real hosts, real e-mail domains
  apis/<service>.yaml     HTTP API plugins
  email/services/*.yaml   email services
  email/extractors.yaml
  webhooks/inboxes.yaml   kept webhook inboxes
  storage/rules/*.yaml    storage rules
  telemetry/<name>.yaml
  secrets.yaml
  duble.sqlite            state and history
  telemetry.sqlite        spans, logs, metrics
```

<p align="center"><img src="images/screen-new-environment.png" alt="New environment: a copy of STG-A, empty, or from files; what comes along; lifetime and callbacks" width="100%"></p>
