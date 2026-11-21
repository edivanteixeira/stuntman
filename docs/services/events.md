# Product events

Stuntman receives the product analytics your app sends to PostHog, Segment, Mixpanel and the Google Analytics 4
Measurement Protocol. It answers as each collector does, so the SDKs are satisfied, and keeps every event with its
user, its properties and the personal data found in them. A tracking plan lists the events a flow must send;
`check_events` compares what arrived with the plan, so a test or a CI run can fail when the app stops tracking
something.

## At a glance

| | |
|---|---|
| **Stands in for** | PostHog capture API (`capture`, `batch`, `e`, `i/v0/e`, `decide`, `flags`), Segment HTTP Tracking API (`/v1/track`, `identify`, `page`, `screen`, `group`, `alias`, `batch`), Mixpanel ingestion (`track`, `import`, `engage`), GA4 Measurement Protocol (`/mp/collect`, `/debug/mp/collect`). Tested with the Python `posthog` SDK (`sync_mode=True`) and raw HTTP |
| **Ports** | `8025` (`STUNTMAN_PORT`); `8445` is the same over TLS with the Test CA (`STUNTMAN_HTTPS_PORT`) |
| **Point your app at** | PostHog host `http://localhost:8025/posthog`, Segment host `http://localhost:8025/segment`, Mixpanel API host `http://localhost:8025/mixpanel`, GA4 `http://localhost:8025/ga4/mp/collect` |
| **It keeps** | Every event: collector, kind, name, user, properties, personal data found, caller, time sent. Tracking plans in `events/plans.yaml` |
| **You can provoke** | The collector's next answers (`force_events_failure`): `400`, `413`, `429` with `Retry-After`, `slow` (3 s), `drop_half` (half the events lost, success anyway) |
| **Screen** | `/events` (events), `/events/plan` (plans), `/events/people` (users) |
| **Tools** | `list_product_events`, `event_people`, `wait_for_event`, `check_events`, `list_event_plans`, `set_event_plan`, `delete_event_plan`, `force_events_failure` |

## Connect your app

Point the SDK's host (or API base URL) at the collector's prefix. Keys are not checked: any project key, write key,
token, `measurement_id` or `api_secret` works and none is stored.

```python
# PostHog (Python), as the tests run it
import posthog
ph = posthog.Posthog("phc_test", host="http://localhost:8025/posthog", sync_mode=True)
ph.capture("signup_completed", distinct_id="u-7f3a", properties={"plan": "pro"})
```

```js
// posthog-js
posthog.init("phc_test", { api_host: "http://localhost:8025/posthog" })

// Segment analytics-node
new Analytics({ writeKey: "wk_test", host: "http://localhost:8025/segment" })
```

```text
# GA4 Measurement Protocol
POST http://localhost:8025/ga4/mp/collect?measurement_id=G-TEST&api_secret=x
{"client_id": "c.1", "events": [{"name": "purchase", "params": {"value": 10}}]}
```

| App runs | Host |
|---|---|
| On the host | `http://localhost:8025/<collector>` |
| In Docker, Stuntman on the host | `http://host.docker.internal:8025/<collector>` |
| In the same compose file | `http://stuntman:8025/<collector>` |
| Another environment | `http://localhost:8025/e/<env>/<collector>`, or a host name of that environment ([Environments](../environments.md)) |
| HTTPS only | `https://localhost:8445/<collector>` (trust the Test CA) |

## What it supports

Every path under `/posthog/`, `/segment/`, `/mixpanel/` and `/ga4/` is accepted, with any method. The body is
decoded the way the SDKs send it:

- JSON;
- gzip (a `Content-Encoding: gzip` header, `?compression=gzip` or `gzip-js` in the query, or a body that starts
  with the gzip magic bytes);
- form-encoded `data=<base64 JSON>` (standard or URL-safe base64, as posthog-js legacy and Mixpanel send);
- a bare base64 JSON body.

A body that is none of these still gets the success answer. PostHog and GA4 then keep no event; Segment and
Mixpanel keep one event with an empty name.

### PostHog (`/posthog/...`)

| Path | What happens |
|---|---|
| `decide`, `flags` (any suffix, e.g. `/decide/?v=3`) | `{"featureFlags": {}, "flags": {}, "featureFlagPayloads": {}, "errorsWhileComputingFlags": false, "sessionRecording": false, "supportedCompression": ["gzip", "gzip-js"]}`. No flags are ever on |
| Any other path (`capture/`, `batch/`, `e/`, `i/v0/e/`…) | Events read from a list, from `{"batch": [...]}`, or from one object with `event`. Answer `{"status": 1}` |

Per event: `name` = `event`; `user` = `distinct_id` (or `properties.distinct_id`); properties = `properties`,
plus `$set` when present. Kind: `$identify` → `identify`, `$pageview` → `page`, `$groupidentify` → `group`, any
other → `track`. `timestamp` is kept as sent.

### Segment (`/segment/...`)

| Body | What happens |
|---|---|
| `{"batch": [...]}` (e.g. `/segment/v1/batch`) | Each item by its `type` |
| One message (`/segment/v1/track`, `identify`, `page`, `screen`, `group`, `alias`) | Its `type`, or the last path segment when `type` is missing |

| Type | Event name | Properties kept |
|---|---|---|
| `track` | `event` | `properties` |
| `identify` | `identify` | `traits` |
| `page`, `screen` | `page` or `page: <name>` (`screen: <name>`) | `properties` |
| `group` | `group` | `{groupId, traits}` |
| `alias` | `alias` | `{previousId}` |

`user` = `userId`, else `anonymousId`. Answer `{"success": true}`.

### Mixpanel (`/mixpanel/...`)

| Path | What happens |
|---|---|
| `engage...` | Each object is an `identify` event named `$engage`; `user` = `$distinct_id`; the whole object is kept as properties |
| Any other (`track`, `import`) | Each object (or the list) is a `track` event: `name` = `event`, `user` = `properties.distinct_id` |

Answer: the text `1`, or `{"status": 1, "error": null}` with `?verbose=1`.

### GA4 Measurement Protocol (`/ga4/...`)

| Path | What happens |
|---|---|
| `/ga4/mp/collect` (any path not starting with `debug`) | Each item of `events` is a `track` event: `name`, `params` as properties. `user_properties`, when present, is one more `identify` event named `user_properties`. `user` = `user_id`, else `client_id`. Answer `204 No Content` |
| `/ga4/debug/mp/collect` | Nothing is stored and no forced failure applies. Answer `{"validationMessages": [...]}` |

The validation server's checks:

| Check | `validationCode` |
|---|---|
| No `client_id` and no `app_instance_id` | `VALUE_REQUIRED` (`fieldPath: client_id`) |
| More than 25 events | `VALUE_OUT_OF_BOUNDS` (`fieldPath: events`) |
| Event name not a letter followed by up to 39 letters, digits or underscores | `NAME_INVALID` |
| Reserved name (`first_visit`, `session_start`, `user_engagement`, `app_install`, `error`, … 20 names) or a `ga_`, `google_`, `firebase_` prefix | `NAME_RESERVED` |
| More than 25 params on an event | `VALUE_OUT_OF_BOUNDS` (`fieldPath: events[i].params`) |

### Personal data

Every string and number in the properties is scanned. A hit is listed in the event's `pii` as `<path>: e-mail`,
`<path>: CPF` or `<path>: card number`:

| Kind | Rule |
|---|---|
| E-mail | An address like `name@domain.tld` |
| CPF | `000.000.000-00` or 11 digits in that shape |
| Card number | 13 to 19 digits (spaces or dashes allowed) that pass the Luhn check |

Keys that start with `$` (SDK metadata) are skipped unless their path contains `email`. A user id with `@` in it
adds `user id: e-mail`.

### Tracking plans

A plan is a name and a list of expected events, kept in `events/plans.yaml`:

```yaml
plans:
  - name: signup
    events:
      - event: signup_completed
      - event: workspace_created
        props: [plan, owner.email]
        min: 1
```

| Key | Meaning |
|---|---|
| `event` | The event name, exact |
| `props` | Properties every arrived event of that name must have, not null. Dotted paths reach nested keys |
| `min` | At least this many must arrive (default 1) |

`set_event_plan` takes `events` as a list of names or `{event, props, min}` objects.

## How it behaves

- Events are stored in the order they came. Lists are newest first.
- The user of an event is the collector's user field as sent; Stuntman does not merge anonymous and identified
  users.
- No retention limit: events stay until the environment is deleted. They survive restarts.
- Times use the real clock, not the [environment's clock](../clock.md).
- Product events are not on the [Timeline](../timeline.md); they are on the Events screen and in the tools.
- `caller` is the first word of the `User-Agent` (`posthog-python`, `analytics-node`), or `app`.

### check_events

```text
check_events {"plan": "signup", "within": "10m"}
→ {"ok": false,
   "events": [{"event": "signup_completed", "arrived": 1, "min": 1, "state": "ok", "missingProps": []},
              {"event": "workspace_created", "arrived": 0, "min": 1, "state": "missing", "missingProps": []}],
   "missing": ["workspace_created"], "pii": [...], "plan": "signup", "since": 1794540000000}
```

| `state` | Meaning |
|---|---|
| `ok` | At least `min` arrived, and every one has every listed property |
| `missing` | Fewer than `min` arrived |
| `props_missing` | Enough arrived, but some lack a property (named in `missingProps`) |

`ok` is true only when every expected event is `ok`. `pii` lists each event that carried personal data: `{event,
user, found}`. The window is `since` (ms), or `within` (a duration); with neither, every event ever kept counts.
`source` and `user` narrow it to one collector or one user. Up to 10 000 events are compared.

## Provoke failures

`force_events_failure` applies to the next `count` requests (default 1) to one collector (`source`) or any
collector (no `source`). A request is one HTTP call, however many events it carries.

| Failure | How | What the app sees | Events kept |
|---|---|---|---|
| Batch refused | `force_events_failure {"kind": "400", "source": "posthog"}` | `400 {"type": "validation_error", "detail": "Invalid batch (forced in Stuntman)"}` | None |
| Too large | `force_events_failure {"kind": "413"}` | `413 {"error": "Payload too large (forced in Stuntman)"}` | None |
| Rate limited | `force_events_failure {"kind": "429", "count": 3}` | `429 {"error": "Rate limited (forced in Stuntman)"}`, `Retry-After: 2` | None |
| Slow collector | `force_events_failure {"kind": "slow"}` | The normal answer after 3 s | All |
| Silent loss | `force_events_failure {"kind": "drop_half", "source": "segment"}` | The normal success answer | The first half of the request's events |
| Stop | `force_events_failure {"kind": "none"}` (with `source` to clear one collector) | Normal answers | |

The forced failure is the same shape for every collector. GA4 `/debug/` requests never take a forced failure.

## See what the app did

| Need | Tool | HTTP |
|---|---|---|
| Events, newest first | `list_product_events {"source": "posthog", "name": "signup_completed", "user": "u-7f3a", "since": <ms>, "limit": 200}` | `GET /api/events?name=signup_completed` |
| Users and their counts | `event_people {"since": <ms>}` | `GET /api/events/people` |
| Wait for one event | `wait_for_event {"name": "signup_completed", "props": {"plan": "pro"}, "timeout": "30s", "within": "10s"}` | `GET /api/events/wait?name=signup_completed` |
| Compare with a plan | `check_events {"plan": "signup", "within": "10m"}` | `POST /api/events/check` |
| Plans and the failure kinds | `list_event_plans` | `GET /api/events/plans` |

Event fields: `id`, `at` (ms), `source` (`posthog`, `segment`, `mixpanel`, `ga4`), `kind` (`track`, `identify`,
`page`, `screen`, `group`, `alias`), `name`, `user`, `props`, `pii`, `caller`.

`event_people` returns `{user, events, last, sources}` for each user (up to 500), most recent first. Events with no
user are not listed there.

`wait_for_event` looks at events from `within` ago (default: only events that arrive after the call) and polls
every 200 ms until `timeout` (default 30 s); then it answers `408 No “<name>” event yet`. `props` compares
top-level properties for equality (`{"plan": "pro"}`); nested paths are not read there.

## Recipes

1. **Fail CI when the signup flow stops tracking.**

   ```text
   set_event_plan {"name": "signup", "events": ["signup_completed", {"event": "workspace_created", "props": ["plan"]}]}
   # run the end-to-end signup test
   check_events {"plan": "signup", "within": "10m"}   → assert ok == true
   ```

   Over HTTP: `POST /api/events/check` with `{"plan": "signup", "within": "10m"}`; fail the job when `ok` is false.

2. **Wait for an event after a click in a browser test.**

   ```text
   wait_for_event {"name": "checkout_started", "source": "posthog", "props": {"currency": "USD"}, "within": "5s", "timeout": "20s"}
   ```

3. **Check that no personal data leaks into analytics.**

   ```text
   check_events {"events": [], "within": "1h"}   → pii == []
   ```

   An empty `events` list compares nothing and still returns `pii` for the window. Each hit names the property
   path inside the properties, e.g. `email: e-mail` or `customer.card: card number`.

4. **Prove the app retries a rate-limited batch.**

   ```text
   force_events_failure {"kind": "429", "source": "segment", "count": 2}
   # trigger the events
   wait_for_event {"name": "Order Completed", "source": "segment", "timeout": "60s"}
   ```

   The event arrives only if the SDK retried after the two `429`s.

5. **Show silent loss.** `force_events_failure {"kind": "drop_half", "source": "posthog"}`, send a batch of 10,
   then `list_product_events {"source": "posthog", "limit": 10}`: 5 arrived, and the SDK saw success.

6. **Validate GA4 events before sending them.** Post the payload to `/ga4/debug/mp/collect` and assert
   `validationMessages` is empty.

## Errors and troubleshooting

| Symptom or error text | Cause | Fix |
|---|---|---|
| Events never show up, the SDK reports success | Wrong host (the SDK still points at the real collector), or the body is not one Stuntman decodes | Check the host; `list_product_events` |
| `404` from the SDK | The host lacks the collector prefix (`/posthog`, `/segment`, `/mixpanel`, `/ga4`) | Add it to the host or base URL |
| PostHog flags are always off | `decide` and `flags` answer no flags | Not supported; stub flags in the app's test config |
| `check_events` → `400 plan or events` | Neither `plan` nor `events` given | Pass one |
| `check_events` → `404 No plan “x”` | Unknown plan name | `list_event_plans` |
| `check_events` counts old events | No `within` or `since` | Pass `within: "10m"` |
| `force_events_failure` → `400 kind: 400 \| 413 \| 429 \| slow \| drop_half \| none` | Unknown kind | Use one of those |
| `force_events_failure` → `400 source: posthog \| segment \| mixpanel \| ga4` | Unknown collector | Use one of those |
| `wait_for_event` → `408` | Not arrived within `timeout`, or arrived before the call and `within` was not set | Add `within`; raise `timeout` |
| Segment `page` events have odd names | Names are `page: <name>` | Match `page: Home`, not `Home` |

## Not done

- PostHog feature flags, session recordings, surveys and the PostHog query API are not served (`decide` and
  `flags` always answer no flags).
- Amplitude, RudderStack, Heap, Snowplow and the GA4 gtag/`/g/collect` browser endpoint are not supported.
- Keys are never checked: an app with a wrong key still succeeds.
- No identity merging: `alias` and `$identify` are stored as events, not applied to earlier events.
- Mixpanel `engage` operations (`$set`, `$append`, …) are stored as sent, not applied to a profile.
- The GA4 validation covers the checks listed above only (no parameter value or user property checks).
- Forced failures have one generic shape, not each collector's own error body.
- Events are not on the Timeline.

Missing something? Tell the developer what the fix would be and offer a pull request — see [Contributing](../../CONTRIBUTING.md).

## Tools and routes

<!-- tools: events -->
_Generated by `tools/gen_tool_reference.py` from the engine source; each tool is an MCP tool and the HTTP route shown (under `/api`)._

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_product_events` | `GET /api/events` | — | source, name, user, since, limit | Product events the app sent (PostHog, Segment, Mixpanel, GA4), newest first, with the user, the properties and any personal data found in them. |
| `event_people` | `GET /api/events/people` | — | since | The users events arrived for, with counts. |
| `wait_for_event` | `GET /api/events/wait` | name | source, user, props, timeout, within | Waits until the app sends an event (by name; optionally a property value). Returns it. |
| `check_events` | `POST /api/events/check` | — | plan, events, since, within, source, user | Checks what the app sent against a plan (by name) or a list of events: each expected event, how many arrived and the properties some lacked. ok=false names the missing ones, so a test or CI run can fail on it. |
| `list_event_plans` | `GET /api/events/plans` | — | — | The plans: which events (and properties) each flow must send. |
| `set_event_plan` | `PUT /api/events/plans` | name, events | — | Adds or replaces a plan: a name and its events (names, or {event, props, min}). |
| `delete_event_plan` | `DELETE /api/events/plans/{name}` | name | — | Removes a plan. |
| `force_events_failure` | `POST /api/events/force` | kind | source, count | The collector's next answers fail on purpose (count): 400, 413, 429, slow (3 s), drop_half (half the events lost, 200 anyway). kind=none clears. |
<!-- /tools -->
