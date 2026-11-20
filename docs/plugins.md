# Plugins

A provider is a plugin. A plugin is one YAML file: the routes, the state they keep, the answers they give, the
callbacks they send and the sentence each call leaves on the timeline. Someone who knows the provider writes it
once; everyone else installs it.

<p align="center"><img src="images/plugin-anatomy.png" alt="Anatomy of a plugin: service, routes, keep, decide, answers" width="100%"></p>

Plugins power the **HTTP APIs** line. SMS, WhatsApp, email, mailboxes, storage, files, messaging, streams,
identity, secrets, AI models, product events, webhooks and telemetry are built into the engine. You do not have to
write a plugin by hand: one can be made from the provider's [OpenAPI spec](openapi.md), from its [WSDL](soap.md)
(SOAP), from [WireMock mappings](mocks.md), or by [recording the real provider](recording.md).

## A complete example

`plugins/recaptcha.yaml`, as it ships:

```yaml
# Google reCAPTCHA siteverify (v2 and v3). A token used twice answers timeout-or-duplicate.
service:
  name: recaptcha
  title: reCAPTCHA
  origin: official
  description: Bot check (siteverify)
  base_url: https://www.google.com
  real_host: www.google.com
routes:
  - route: POST /recaptcha/api/siteverify
    receive:
      body: [secret, response]
      missing: missing_input
    keep:
      state: tokens
      key: body.response
    decide:
      - when: body.response in state.tokens
        answer: duplicate
      - when: body.response.startsWith('bad')
        answer: invalid_token
      - when: body.response.startsWith('bot')
        answer: low_score
      - answer: human
answers:
  - name: human
    sentence: "Verified a token from {{ body.response | default('the form') }}: score 0.9"
    body: '{"success": true, "score": 0.9, "action": "submit", "challenge_ts": "{{ now() }}", "hostname": "stg-a.stuntman.local"}'
  - name: duplicate
    sentence: "Token used twice: timeout-or-duplicate"
    body: '{"success": false, "error-codes": ["timeout-or-duplicate"]}'
  # … low_score, invalid_token, missing_input
```

A test drives each answer by the token it sends: `bad…` is invalid, `bot…` scores 0.1, the same token twice is a
duplicate.

## Format

### `service`

| Key | Meaning |
|---|---|
| `name` | lowercase, digits and dashes; the URL segment: `:8028/<name>` |
| `title`, `description` | shown in the UI |
| `origin` | `official`, `community` or `yours` (default) |
| `region` | optional, for regional providers |
| `base_url`, `real_host` | the provider's real address; `real_host` lets you resolve the real name to Stuntman. It takes one name or a list (`real_host: [a.example.com, b.example.com]`): the service answers each of them, on :8028 by `Host` and on :8443 by SNI |
| `auth` | `oauth` (client credentials: `client_id`, `client_secret`, `any_client`, `token_ttl`), `basic` (`users`, `any_user`, `challenge`), `session` (`cookie`, `ttl`, `users`), `client_certificate.required` (mTLS) and `client_certificate.icp` (the certificate must carry a CNPJ, as ICP-Brasil e-CNPJ certificates do; see [SOAP › XML signatures](soap.md#xml-signatures-and-icp-brasil-certificates)) |
| `billing` | `enabled`, `per_call`, `warn_above`: a counter, never real money |
| `limits` | `latency` (e.g. `300ms..600ms`), `rate`, `fail_share` |
| `cors`, `ignore_case` | request handling |
| `webhooks` | how the `send` steps sign: `sign`, `header`, `secret`, `tolerance`, `provoke` ([Webhook signing](#webhook-signing)) |
| `idempotency` | `header` (default `Idempotency-Key`) and `ttl` (default `24h`) ([Idempotency keys](#idempotency-keys)) |

### `routes`

Each route reads top to bottom: **receive → let → keep → wait → decide → respond → send**.

| Key | Meaning |
|---|---|
| `route` | `METHOD /path`; `{name}` segments are path parameters, a last `{*}` segment takes the rest as `path.rest`; `ANY /path` takes every method; `METHOD ~regex` matches the whole path with a regular expression, and its named groups are path parameters ([Regex routes](#regex-routes)) |
| `receive.auth` | `none`, `bearer` (a token from this service's token route), `basic`, `session` |
| `receive.body` | required body fields (dotted paths); a missing one answers 400, or the answer named by `missing` |
| `receive.schema`, `receive.invalid` | the body is checked against this schema of `spec.schemas`; a body that does not match it gets the answer named by `invalid`, or 400 naming the field (see [OpenAPI](openapi.md)) |
| `operation` | the spec's `operationId`, kept for reference |
| `let` | named values computed first, e.g. `person: lookup('people', 'id', body.document_id)`; one that names another is computed after it, whatever the order in the file |
| `keep` | `state` (a collection) and `key` (an expression): what the provider remembers between calls. Only an accepted call (an answer below 400) is saved; a refused one leaves the state as it was |
| `wait` | "answer `processing` 3 times, then `done`": counts calls per key |
| `decide` | a list of `when` → `answer`; the first match wins; the last item usually has no `when`. `scenario` moves the service to that scenario after this answer ([Scenarios](#scenarios)) |
| `respond.latency` | e.g. `300ms..600ms` |
| `send` | callbacks after answering: `url` (a template), `method`, `body`, `delay` (on the [environment's clock](clock.md)), `secret` (HMAC-SHA256 into `header`; may be a template), `sign` (overrides `service.webhooks.sign`), `event` (the event type), `headers` (names and values are templates; a name that renders empty is left out; the name `"*"` takes `Name: value` lines, one header each, e.g. the headers a subscriber chose), `when` (an expression: sent only when it is true), `template` (`handlebars`: url, body and header values are WireMock templates reading `originalRequest` and `originalResponse`), `operation` (a SOAP callback) |
| `fail` | a generic failure on this route only: `reset`, `empty`, `timeout`, `401`, `403`, `500`, `503`, `html`, `503_html` |
| `schema` | response fields and their types; answers are checked against it |
| `billable`, `token`, `login`, `grant`, `step` | billing, the OAuth2 token route, the session login route, people grants (authorization code with PKCE), push approvals polled by the app |
| `graphql` | the GraphQL operation name, or first field of the query, this route answers ([GraphQL](#graphql)) |
| `fallthrough` | when no rule matches, the next route with a matching path is tried, as WireMock does across mappings |
| `tool`, `description`, `input_schema` | the route is a tool of the service's MCP server ([MCP server](#mcp-server)) |

### `answers`

| Key | Meaning |
|---|---|
| `name` | what `decide` and `force_answer` refer to |
| `status` | default 200 |
| `headers`, `body` | the body is a template, JSON unless the headers say otherwise |
| `raw` | send exactly as typed, not checked against the schema (malformed on purpose) |
| `file` | a file in `apis/<service>.files/` sent as the body, as is (a PDF, an image); recordings write these |
| `sentence` | the timeline sentence, a template |
| `note` | what this answer means, shown under its name |
| `latency` | wait before this answer (`800ms`), instead of the route's latency |
| `stream`, `stream_format` | send the answer in parts instead of `body`: `sse` (default), `ndjson` or `chunks` ([Streamed answers](#streamed-answers)) |
| `template` | `handlebars`: the body is a WireMock response template ([WireMock mocks](mocks.md#response-templates)) |
| `fault` | a SOAP fault instead of a response ([SOAP](soap.md)) |
| `rpc_error` | for an MCP tool: a JSON-RPC error code (`-32602`) instead of a tool result; the body is its message |

### `mcp`

A top-level block that makes the service an MCP server too: `path` (default `/mcp`), `name`, `version`,
`instructions`, `auth` (empty: open; `bearer`: a token from this service's token route) and `sse` (answer as
`text/event-stream`). See [MCP server](#mcp-server).

### `spec` and `record`

`spec` is set on a service made from an OpenAPI spec: the kept file, where it came from, `validate` and the
`schemas` the routes check bodies against ([OpenAPI](openapi.md)). `record` is set while the service records the
real provider: `upstream`, `mode`, its time and call limits and extra fields to `mask` ([Recording](recording.md)).

### `data` and `pages`

`data` holds tables the answers read (people, flagged documents, selfies…). `pages` are screens Stuntman draws for
the provider, such as an authorize page or a push approval, each with fields, buttons and checks.

## Expressions

Conditions and templates use a small, side-effect-free language (a CEL-like subset).

| | |
|---|---|
| **Read** | `body.x` (JSON, form fields, `multipart/form-data` fields, or the JSON part of a `multipart/related` upload), `files` (the uploaded parts: `field`, `filename`, `size`, `contentType`), `query.x`, `path.x`, `headers.x`, `raw` (the body as text; a `Content-Encoding: gzip` body arrives decompressed), `state.<collection>`, `data.<table>`, the `let` names (`person.x`), `caller`, `client_cert` and `client_cnpj` (mTLS), `scenario`, `request_path` (the path inside the service); on GraphQL routes `variables` and `operation`; in `send`, `answer` |
| **Operators** | `==`, `!=`, `<`, `>`, `<=`, `>=`, `+`, `-`, `*`, `/`, `%`, `&&`, `\|\|`, `!`, `in`, `cond ? a : b`, `x[0]`, `x['key']`; lists `[1, 2]` and maps `{'n': 2}` |
| **Methods** | `.startsWith()`, `.endsWith()`, `.contains()`, `.matches()`, `.size()`, `.upper()`, `.lower()`, `.digits()`, `.mask()`, `.sha256()`, `.base64url()`, `.cpf_ok()`, `.string()`, `.int()` |
| **Functions** | `size`, `exists` (or `has`), `default`, `string`, `int`, `double`, `upper`, `lower`, `trim`, `replace(s, from, to)`, `split(s, sep)`, `join(list, sep)`, `substring(s, start, end)`, `digits`, `mask`, `cpf_ok`, `sha256`, `base64`, `base64url`, `uuid`, `random_id`, `now`, `now_ms`, `matches(s, regex)`, `xpath(xml, expr)` (the values an XPath finds; `xpath(xml, expr, 'equalTo' | 'contains' | 'matches', v)`: whether one matches), `json`, `cookie(header, name)`, `lookup(table, field, value)`, `iso(ms, zone)` (a kept time as ISO 8601), `crc16(s)` (CRC16/CCITT, the end of a Pix BR Code), `gzip_base64(text)` and `gunzip_base64(b64)` (documents sent gzipped in base64), `parse_json(text)`, `hmac(alg, key, message, enc)` (`sha256` or `sha1`; a key written `hex:…` is hex bytes; `hex` or `base64` out), `jwt(claims, key)` and `jwks(key)` (an RS256 JWT signed with the plugin's own key of that name, and the JWKS that verifies it) |
| **Template filters** | `{{ value \| default('x') }}`, `mask`, `json`, `round(2)`, `tz('America/New_York')` (a short list of zones, fixed offsets), and any one-argument function (`\| trim`, `\| upper`) |

Templates use `{{ }}` inside answer bodies, sentences, callback URLs, headers and secrets. `split` and `join`
default to a comma; `substring` counts characters, and a negative start counts from the end. `now()` and `now_ms()`
read the [environment's clock](clock.md).

```text
split(replace('X-Key:abc', 'X-', ''), ':')[1]        → "abc"
substring('2026-11-15', 0, 4)                         → "2026"
join(split('a b', ' '), '+')                          → "a+b"
```

## Regex routes

A path that starts with `~` is a regular expression over the whole path (after `/<service>`). Named groups are
path parameters. Imported WireMock mappings keep their `urlPathPattern` this way.

```yaml
routes:
  - route: GET ~/v2/positions/(?<plate>[A-Z]{3}[0-9][A-Z0-9][0-9]{2})
    decide:
      - answer: position
answers:
  - name: position
    body: '{"plate": "{{ path.plate }}", "at": "{{ now() }}"}'
```

Routes are tried in the order of the file, so put a broad pattern after the specific routes it would also
match. `service.ignore_case` makes the regex case-insensitive.

## GraphQL

Several routes can share `POST /graphql`; `graphql:` picks the one that answers, by the request's `operationName`
or by the first field of the query. The scope gets `variables` and `operation`. A query no route takes gets 404.

```yaml
routes:
  - route: POST /graphql
    graphql: CreateCharge
    decide:
      - when: variables.amount > 1000
        answer: declined
      - answer: charge
answers:
  - name: charge
    body: '{"data": {"createCharge": {"id": "ch_9", "amount": {{ variables.amount }}}}}'
  - name: declined
    body: '{"errors": [{"message": "card declined"}]}'
```

## Streamed answers

An answer with `stream` is sent in parts, each one `after` the previous one. `stream_format` is `sse` (the default:
`event:` and `data:` lines), `ndjson` (one line per part) or `chunks` (plain text). `data` is a template.

```yaml
answers:
  - name: charge_events
    stream:
      - event: charge.pending
        data: '{"id": "ch_1"}'
      - after: 300ms
        event: charge.succeeded
        data: '{"status": "succeeded"}'
      - after: 100ms
        data: '[DONE]'
```

`force_answer` with `cut` sends half the parts and breaks the connection; `slow` waits 2 s more before each part.

## MCP server

With a top-level `mcp:` block, the service is also an MCP server: JSON-RPC 2.0 over streamable HTTP on
`:8028/<service>/mcp` (protocol `2025-06-18`): `initialize`, `ping`, `tools/list`, `tools/call` and notifications.
Its tools are the routes that have `tool:`. The arguments are the `body`, so a tool decides like any route.

```yaml
mcp:
  name: example-docs
  instructions: Search the Example docs.
routes:
  - tool: search_docs
    description: Search the docs by words.
    input_schema:
      type: object
      required: [query]
      properties:
        query: {type: string}
    decide:
      - when: body.query.contains('refund')
        answer: refund_pages
      - answer: no_results
answers:
  - name: refund_pages
    body: 'Refunds take 5 days.'
  - name: no_results
    status: 404
    body: 'Nothing found.'
```

The answer's body is the tool result's text. A status of 400 or more gives `isError: true`; arguments that break
`input_schema` do too. `rpc_error: -32602` answers a JSON-RPC error instead. `force_answer` with `session_expired`
answers the next request with 404, so the client must start a new session. `set_mcp_tool` adds or changes a tool;
the web UI shows them on the service's **Tools** tab, and calls appear as `MCP <tool>`.

## Webhook signing

`service.webhooks` sets how the `send` steps sign, in the provider's own format:

| `sign` | Headers |
|---|---|
| `hex` (default) | `X-Signature`: HMAC-SHA256 of the body, hex |
| `hex_ts` | `X-Webhook-Timestamp`, and `X-Webhook-Signature`: HMAC-SHA256 of `{timestamp}.{body}` |
| `stripe` | `Stripe-Signature: t=…,v1=…` |
| `svix` | `svix-id`, `svix-timestamp`, `svix-signature: v1,<base64>`; the secret is `whsec_<base64>` |
| `github` | `X-Hub-Signature-256: sha256=<hex>` |
| `cloudevents` | `ce-specversion`, `ce-type` (the send's `event`), `ce-source`, `ce-id`, `ce-time`; an HMAC into `header` when both `secret` and `header` are set |
| `twilio` | `X-Twilio-Signature`: HMAC-SHA1 of the callback URL followed by the sorted form parameters (name + value), base64 |
| `slack` | `X-Slack-Request-Timestamp`, and `X-Slack-Signature: v0=<hex>`: HMAC-SHA256 of `v0:{timestamp}:{body}` |
| `shopify` | `X-Shopify-Hmac-Sha256`: HMAC-SHA256 of the body, base64 |
| `hubspot` | `X-HubSpot-Request-Timestamp` (ms), and `X-HubSpot-Signature-v3`: HMAC-SHA256 of method + URL + body + timestamp, base64 |

`header` renames the signature header, `secret` is a literal or `secret://name`, and `tolerance` (default 300 s)
is how old a timestamp the app should accept. `provoke` breaks every send while it is set: `wrong_signature`,
`old_timestamp` (past the tolerance), `no_signature`, `twice` (each webhook sent two times) and `out_of_order` (the
first webhook of a route waits 2 s, so the next one arrives first). A send's own `sign` overrides the service's. A send that sets its own `Content-Type` header (a form-encoded callback) replaces the JSON default.

```yaml
service:
  name: example-payments
  webhooks:
    sign: stripe
    secret: secret://example-payments-webhook
```

## Idempotency keys

With `service.idempotency`, a `POST`, `PUT`, `PATCH` or `DELETE` that repeats an `Idempotency-Key` gets the first
answer again, with `idempotent-replayed: true`, and sends no webhook. The same key with another body gets 409
`idempotency_error`. Keys are kept for `ttl` (default `24h`).

## Scenarios

A service is in one scenario at a time; it starts in `Started`. Rules read it as `scenario`, a `decide` item with
`scenario:` moves the service after its answer, and `set_scenario` moves it by hand and `get_scenario` (`GET /api/plugins/{service}/scenario`) reads it. Imported WireMock scenarios
work this way.

```yaml
decide:
  - when: scenario == 'gps_down'
    answer: gps_down
  - when: body.kind == 'panic'
    answer: alert
    scenario: gps_down
  - answer: alert
```

## WireMock templates

An answer with `template: handlebars` renders its body as a WireMock response template (`request.path`,
`request.headers.X`, `{{now format='yyyy-MM-dd'}}`, `{{randomInt lower=1 upper=9}}`, `jsonPath`, `xPath`…). See
[WireMock mocks](mocks.md#response-templates) for the helpers.

## Edit in the UI, keep the YAML

![The route editor with the YAML beside it](images/screen-route-editor.png)

The web UI edits the same file. Every change is validated before it is saved, and the YAML beside the editor shows
the exact lines that changed (on a window narrower than 1440 px the editor takes that room; Settings shows the whole
file). Saving from the web UI or a tool rewrites the file, so YAML comments are not kept; edit
by hand when comments matter. A save from the web UI or a tool is used by the next call; a file edited by hand is
read again within a second.

## Install

| From | Tools |
|---|---|
| The catalog | `list_plugins`, `get_catalog_plugin`, `install_plugin`, `install_collection` |
| A folder, a `.zip` or a git URL | `import_plugin` (`path=` inside it, `ref=` branch or tag), then `install_import` |
| Scratch | `install_plugin` with `plugin=empty`, then `put_plugin` |

`validate_plugin` returns schema and expression errors **with line numbers** before anything is saved.
`put_plugin` refuses to overwrite a version that changed since you read it (409).

## Safe by default

A plugin cannot reach the network or the disk unless it declares it. Expressions cannot call out. When an escape
hatch for custom code arrives, it will run sandboxed.

## Contributing a plugin

The catalog lives in [`engine/plugins/`](../engine/plugins). A good plugin:

1. Mirrors the provider's real paths, status codes, error bodies and headers.
2. Names every answer after what it means (`irregular_document`, not `answer3`) and writes a `note`.
3. Writes sentences a person can read on the timeline.
4. Uses synthetic data only (`@example.com`, `acme`, `globex`).
5. Passes `validate_plugin`.

See [CONTRIBUTING.md](../CONTRIBUTING.md).
