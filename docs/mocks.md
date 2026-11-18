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
| `method` | the route's method; `ANY` becomes an `ANY` route, which takes every method |
| `queryParameters`, `headers` | conditions in `decide`: `equalTo`, `contains`, `doesNotContain`, `matches`, `doesNotMatch`, `absent`; `matches` is a whole-value match, as Java's `String.matches` (`^(?:re)$`) |
| `host` | only used by `group_by=host` |
| `basicAuthCredentials` | a condition on the `Authorization` header (`Basic` with that user and password) |
| `cookies` | conditions on one cookie each: `cookie(headers['cookie'], 'sid') == 's1'` |
| `bodyPatterns` | conditions on the body: `equalTo`, `contains`, `matches` and their negations, `equalToJson` (one condition per field, nested objects included; lists compare whole and in order), `matchesJsonPath` (a path, or a `$[?(@.x == 'y')]` filter), `matchesXPath`, evaluated as XPath on the body: `/` and `//` steps, `*` or a name (any namespace), predicates joined by `and` (`local-name()`, `name()`, `text()`, `.`, a child, `@attr`, `contains(…)`, `starts-with(…)`, a position), a last `text()` or `@attr`; with an `equalTo`, `contains` or `matches` sub-matcher one of the values must match. Other XPath is reported as a gap |
| each mapping | one rule and one named answer. As in WireMock, a request gets the best `priority` among every mapping that matches it (any path, any method), the one read last on a tie: each route holds every rule that can match its requests, in that order, including a pattern mapping's (`/acme/.*`) on the plain routes it covers. Rules copied onto another pattern route check `request_path` |
| `status`, `headers`, `body`, `jsonBody`, `base64Body`, `bodyFileName` | the answer; a binary file is copied to `apis/<service>.files/` |
| `fixedDelayMilliseconds`, `delayDistribution.median` | the answer's `latency` |
| `fault` | `EMPTY_RESPONSE` becomes `empty`, the others `reset` |
| `scenarioName`, `requiredScenarioState`, `newScenarioState` | [scenarios](plugins.md#scenarios): rules with `scenario == '…'` and a `scenario:` move after the answer |
| a body or a header with `{{ }}`, or the `response-template` transformer | an answer with `template: handlebars` (its headers render as templates too) |
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

An answer with `template: handlebars` renders its body and its headers like WireMock's response-template transformer. The template
reads `request`:

| Field | Value |
|---|---|
| `request.url` | path and query, the query as the app sent it (`?h=a%26b`) |
| `request.path`, `request.pathSegments.[n]` | the path inside the service, and its segments (see the limits) |
| `request.query.<name>` | a query parameter: a list, as in WireMock. `{{request.query.c}}` prints the first value, `{{request.query.l.[1]}}` the second, `{{#each request.query.l}}` reads them all |
| `request.headers.<name>` | a header, whatever its case; a list too (`request.headers.X-Id.[0]`) |
| `request.body` | the body as text |
| `request.method` | the method |
| `request.scheme`, `request.host`, `request.port`, `request.baseUrl` | as received: host and port from the `Host` header (80 or 443 when it has no port), `baseUrl` is `scheme://host[:port]` without a default port or a trailing slash |

Helpers:

| Helper | Example |
|---|---|
| `now` | `{{now}}`, `{{now format='yyyy-MM-dd'}}`, `{{now offset='3 days' timezone='America/New_York'}}`, `format='epoch'` or `'unix'` |
| `randomValue` | `{{randomValue length=8 type='ALPHANUMERIC' uppercase=true}}`; types `ALPHANUMERIC`, `ALPHABETIC`, `NUMERIC`, `HEXADECIMAL`, `ALPHANUMERIC_AND_SYMBOLS`, `UUID` |
| `randomInt` | `{{randomInt lower=1 upper=9}}` |
| `math`, `numberFormat` | `{{math 2 '*' 3}}` (`+ - * / %`), decimal arithmetic as WireMock's BigDecimal: `0.1 + 0.2` is `0.3`, `10 / 4` is `3` (division keeps the left operand's decimals, rounded half up), `1.5 * 2` is `3.0` |
| `jsonPath` | `{{jsonPath request.body '$.order.id'}}` |
| `xPath` | `{{xPath request.body '//protocol/text()'}}`, `//x/@attr`, `count(//x)` |
| `regexExtract` | `{{regexExtract request.body '[A-Z]{3}[0-9]{4}'}}` |
| `parseJson`, `assign` | `{{#parseJson request.body 'order'}}{{/parseJson}}{{order.id}}`; `{{#assign 'name'}}…{{/assign}}`. These variables (and `regexExtract`'s and `formData`'s) belong to the whole template: set or read them inside `each`, `with` or `if`; a field of the current item with the same name comes first. parseJson keeps a decimal's text (`-26.30`); text that is not JSON is a template error |
| `formData` | `{{formData request.body 'f'}}{{f.k}}`: a urlencoded body as the variable `f`, each field a list; `urlDecode=true` decodes the values |
| `matches`, `range` | `{{#if (matches request.url '.*/v2/.*')}}`: the whole value matches the regex (the block form renders nothing when it does not); `{{#each (range 1 3) as \|i\|}}{{i}}{{/each}}` |
| strings | `trim`, `replace`, `contains`, `substring` (`{{substring 'abcdef' 1 3}}` → `bc`), `upper`, `lower`, `base64` (`decode=true` decodes), `size`, `toJson` |
| logic | `eq`, `ne` (`neq`), `lt`, `gt`, `lte`, `gte`, `and`, `or`, `not`, and Handlebars' own `if`, `unless`, `each`, `with`, `lookup` |

As in WireMock, the comparisons and `contains` also work as blocks (`{{#eq request.query.c.[0] '1'}}one{{else}}other{{/eq}}`),
and `{{ }}` output is not HTML-escaped: a value with `&` or `"` goes into a JSON body as it is. `toJson`, and a
`jsonPath` that finds an object or a list, print JSON as WireMock does (`{\n  "a" : 1\n}`, keys in their order).
Whitespace follows Handlebars.java: lines with only a block tag are kept, and `~` does not trim the text before the
first tag or after the last one. `randomValue` is lower case unless `uppercase=true`. A template that fails answers 500. A comparison takes the
list as a list: `{{#eq request.query.c '1'}}` is false in WireMock and here; compare `request.query.c.[0]`.

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
- Mapping files are read in path order; WireMock's own order for equal priorities across files may differ.
- When nothing matches, the 404 body is `Request was not matched`, without WireMock's closest-stub report.
- Templates: `jsonPath` takes plain paths (`$.a.b`, `$.a[0]`, `$['a']`), not filters or wildcards; `xPath` takes the
  forms listed above, not full XPath. Helpers not listed here (`pickRandom`, `dateFormat`, `parseDate`, `array`,
  `request.cookies`…) are not available. WireMock has no `split` helper either.
- `request.path` is the path inside the service: on `:8028/<service>/…` it does not include the `/<service>` prefix,
  so it matches the mapping's `url` as WireMock saw it. A bare `{{x}}` whose value is a list prints the first item
  (WireMock prints `request.pathSegments` whole).
- Delays from `delayDistribution` use the median only; `chunkedDribbleDelay` is not read.
- Proxying (`proxyBaseUrl`) is not imported: use [Recording](recording.md).
- Request journals and the WireMock admin API (`/__admin`) are not served.
