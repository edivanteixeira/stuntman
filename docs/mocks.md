# WireMock mocks

If you already keep WireMock mappings for a provider, Stuntman turns them into plugins. You keep the paths, the
matchers, the answers, the delays and the scenarios, and you get what a plugin adds: the timeline, rules you can
read, failures on cue and an MCP tool for every action.

A folder of WSDL files works the same way: each one becomes a SOAP service ([SOAP services](soap.md)).

## Import a folder

The folder holds `mappings/*.json` and, for answers that read a file, `__files/`. It must be on the machine that runs
Stuntman.

**From an agent (MCP):**

```text
import_mocks(source="/mocks/partners", dry=true)
→ 7 mappings, 2 services: acme-tracking (scenarios auth-failure, network), globex-tolls
import_mocks(source="/mocks/partners")
```

The repository's [`engine/tests/wiremock`](../engine/tests/wiremock) folder is a small synthetic example with
those two services.

**Over HTTP:**

```sh
curl -X POST http://localhost:8025/api/plugins/import -H 'Content-Type: application/json' \
  -d '{"source": "/mocks/partners"}'
```

| Argument | What it does |
|---|---|
| `source` | the folder |
| `kind` | `wiremock` or `wsdl`; found from the folder when left out |
| `group_by` | `folder` (default: one service per subfolder of `mappings/`), `host` (one per `request.host`) or `one` |
| `name` | with `group_by=one`, the service's name |
| `dry` | only report what it would make |
| `replace` | overwrite services that exist (otherwise they are skipped) |

The result lists, per service, the mappings it read, the routes and answers it made, the faults, the scenarios, the
files it copied and the **gaps**: matchers it could not turn into a rule. Read the gaps before you trust the
service.

## What becomes what

| WireMock | In the plugin |
|---|---|
| `url`, `urlPath`, `urlPathTemplate` | the route's path (`{name}` segments; query parameters in `url` become conditions) |
| `urlPattern`, `urlPathPattern` | a [regex route](plugins.md#regex-routes) (`GET ~/v2/positions/[A-Z0-9]+`), or a plain path when the pattern has no regex in it; the query part of a `urlPattern` is reported as a gap |
| no URL at all | a route that takes every path (`~.*`) |
| `method` | the route's method; `ANY` becomes a `GET` and a `POST` route |
| `queryParameters`, `headers` | conditions in `decide`: `equalTo`, `contains`, `doesNotContain`, `matches`, `doesNotMatch`, `absent` |
| `host` | only used by `group_by=host` |
| `basicAuthCredentials` | a condition on the `Authorization` header (`Basic` with that user and password) |
| `cookies` | conditions on one cookie each: `cookie(headers['cookie'], 'sid') == 's1'` |
| `bodyPatterns` | conditions on the body: `equalTo`, `contains`, `matches` and their negations, `equalToJson` (one condition per field, nested objects included; lists compare whole and in order), `matchesJsonPath` (a path, or a `$[?(@.x == 'y')]` filter), `matchesXPath` (the forms mocks use: `//x`, `//x[text()='y']`, `//*[local-name()='x' and text()='y']`) |
| each mapping | one rule in the route's `decide` and one named answer; rules follow the mappings' `priority` |
| `status`, `headers`, `body`, `jsonBody`, `base64Body`, `bodyFileName` | the answer; a binary file is copied to `apis/<service>.files/` |
| `fixedDelayMilliseconds`, `delayDistribution.median` | the answer's `latency` |
| `fault` | `EMPTY_RESPONSE` becomes `empty`, the others `reset` |
| `scenarioName`, `requiredScenarioState`, `newScenarioState` | [scenarios](plugins.md#scenarios): rules with `scenario == '…'` and a `scenario:` move after the answer |
| a body with `{{ }}` or the `response-template` transformer | an answer with `template: handlebars` |
| `postServeActions` / `serveEventListeners` `webhook` | a `send` on the route that fires only after that mapping's answer (`when: answer == '…'`), with its method, URL, headers, body and delay; templates stay WireMock templates (`template: handlebars`) reading `originalRequest` and `originalResponse` |
| no mapping matched | the answer `not_matched`: 404 "Request was not matched", as WireMock answers |

Imported routes have `fallthrough: true`: when none of a route's rules match, the next route whose path matches is
tried, as WireMock tries the next mapping.

## Scenarios

A service is in one scenario at a time, `Started` at first. Mappings that need a state become rules that check
`scenario`; a `newScenarioState` moves the service after the answer. Move it by hand to test a state directly:

```text
set_scenario(service="acme-tracking", scenario="auth-failure")
set_scenario(service="acme-tracking")            → back to Started
```

## Response templates

An answer with `template: handlebars` renders its body like WireMock's response-template transformer. The template
reads `request`:

| Field | Value |
|---|---|
| `request.url` | path and query |
| `request.path`, `request.pathSegments.[n]` | the path inside the service, and its segments |
| `request.query.<name>` | a query parameter |
| `request.headers.<name>` | a header, whatever its case |
| `request.body` | the body as text |
| `request.method`, `request.baseUrl`, `request.host` | the rest of the request |

Helpers:

| Helper | Example |
|---|---|
| `now` | `{{now}}`, `{{now format='yyyy-MM-dd'}}`, `{{now offset='3 days' timezone='America/New_York'}}`, `format='epoch'` or `'unix'` |
| `randomValue` | `{{randomValue length=8 type='ALPHANUMERIC' uppercase=true}}`; types `ALPHANUMERIC`, `ALPHABETIC`, `NUMERIC`, `HEXADECIMAL`, `ALPHANUMERIC_AND_SYMBOLS`, `UUID` |
| `randomInt` | `{{randomInt lower=1 upper=9}}` |
| `math`, `numberFormat` | `{{math 2 '*' 3}}` (`+ - * / %`) |
| `jsonPath` | `{{jsonPath request.body '$.order.id'}}` |
| `xPath` | `{{xPath request.body '//protocol/text()'}}`, `//x/@attr`, `count(//x)` |
| `regexExtract` | `{{regexExtract request.body '[A-Z]{3}[0-9]{4}'}}` |
| `parseJson`, `assign` | `{{#parseJson request.body 'order'}}{{/parseJson}}{{order.id}}`; `{{#assign 'name'}}…{{/assign}}` |
| strings | `trim`, `replace`, `contains`, `upper`, `lower`, `base64` (`decode=true` decodes), `size`, `toJson` |
| logic | `eq`, `ne` (`neq`), `lt`, `gt`, `lte`, `gte`, `and`, `or`, `not`, and Handlebars' own `if`, `unless`, `each`, `with` |

`now` reads the [environment's clock](clock.md), so a moved clock moves the dates in the answers too. You can use
`template: handlebars` in any plugin, not only imported ones; plugin templates otherwise use
[Stuntman's expressions](plugins.md#expressions).

## Limits

- Matchers with no equivalent are reported as gaps and left out of the rule: `equalToXml`, `binaryEqualTo`,
  `matchesJsonPath` with a matcher object, `multipartPatterns`, and query or header matchers other than the ones
  listed above.
- Nothing is dropped quietly: any request field, response field or serve action the import does not convert
  (`customMatcher`, `chunkedDribbleDelay`, `proxyBaseUrl`, `transformerParameters`, an action other than
  `webhook`…) is listed in the import's gaps, file by file. `equalToJson` with `ignoreArrayOrder` is reported too:
  lists are compared in order. `pathParameters` of a `urlPathTemplate` match any value.
- `host`, `port` and `scheme` are not conditions: the app reaches Stuntman, not the real host.
- `ANY` becomes `GET` and `POST` only.
- Templates: `jsonPath` takes plain paths (`$.a.b`, `$.a[0]`, `$['a']`), not filters or wildcards; `xPath` takes the
  forms listed above, not full XPath. Helpers not listed here (`pickRandom`, `dateFormat`, `parseDate`,
  `formData`, `request.cookies`…) are not available.
- Delays from `delayDistribution` use the median only; `chunkedDribbleDelay` is not read.
- Proxying (`proxyBaseUrl`) is not imported: use [Recording](recording.md).
- Request journals and the WireMock admin API (`/__admin`) are not served.
