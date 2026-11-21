# SMS (Zenvia API v2)

Stuntman stands in for the Zenvia API v2 SMS channel. The app sends to Stuntman as to `api.zenvia.com`, with text or
template contents and its `X-API-TOKEN`. Stuntman answers like Zenvia, then plays the carrier and the person: the
message goes `SENT` → `DELIVERED` (or `NOT_DELIVERED` with a reason), the person may text back, and `STOP` opts the
number out. Every change reaches the app as Zenvia's own `MESSAGE_STATUS` and `MESSAGE` webhooks. Stuntman keeps
every message both ways (with encoding, segments and status history), the opt-out list and every callback with the
app's answer.

## At a glance

| | |
|---|---|
| **Stands in for** | Zenvia API v2, `POST /v2/channels/sms/messages`, and its `MESSAGE_STATUS` / `MESSAGE` subscription webhooks. Tested with plain HTTP clients sending Zenvia's JSON |
| **Ports** | the main HTTP port `8025` (`STUNTMAN_PORT`); `8445` is the same over TLS with the Test CA (`STUNTMAN_HTTPS_PORT`) |
| **Point your app at** | API URL `http://localhost:8025/zenvia/v2/channels/{0}/messages` (Zenvia base URL `http://localhost:8025/zenvia`); token: any non-empty `X-API-TOKEN` |
| **It keeps** | Messages both ways (`out` the app's sends and refusals, `in` the person's replies): number, sender, text, template and fields, encoding, segments, status history with each callback's answer, trace id, caller, `externalId`. The opt-out list. Every callback sent (URL, status, time, attempt, body). `sms/service.yaml` and `sms/recipients.yaml` |
| **You can provoke** | `force_sms_failure`: `500`, `400`, `reset`, `empty`, `timeout` for the next N sends. Per number (recipient steps): `NOT_DELIVERED` with a reason, never delivered, a reply. The person texts back (`reply_sms_as_user`), `STOP` / `START`. Any status by hand (`set_sms_status`), a duplicate callback (`resend_sms_status`). Strict token, strict senders, strict templates, throughput, max segments |
| **Screen** | `/sms` (messages), `/sms/recipients`, `/sms/service` |
| **Tools** | `list_sms`, `get_sms`, `wait_for_sms`, `reply_sms_as_user`, `set_sms_status`, `resend_sms_status`, `list_sms_callbacks`, `retry_sms_callbacks`, `list_sms_opt_outs`, `sms_opt_out`, `sms_opt_in`, `force_sms_failure`, `list_sms_recipients`, `set_sms_recipient`, `remove_sms_recipient`, `get_sms_service`, `set_sms_service` |

## Connect your app

Change the base URL only:

| App runs | API URL |
|---|---|
| On the host | `http://localhost:8025/zenvia/v2/channels/{0}/messages` |
| In a container, Stuntman on the host | `http://host.docker.internal:8025/zenvia/v2/channels/{0}/messages` |
| In the same compose project | `http://stuntman:8025/zenvia/v2/channels/{0}/messages` |
| Another environment | `http://localhost:8025/e/<env>/zenvia/v2/channels/{0}/messages`, or the environment's host name ([Environments](../environments.md)) |

`{0}` is the channel; the app fills in `sms`. `get_sms_service` returns `baseUrl` and `apiUrl` for the current
environment.

```sh
curl -s http://localhost:8025/zenvia/v2/channels/sms/messages \
  -H 'X-API-TOKEN: any' -H 'Content-Type: application/json' \
  -d '{"from": "Acme", "to": "14155550142", "externalId": "order-58213",
       "contents": [{"type": "text", "text": "Acme: your code is 482913"}]}'
# {"id": "6f1c…", "from": "Acme", "to": "14155550142", "direction": "OUT", "channel": "sms",
#  "contents": [...], "timestamp": "2026-11-21T14:02:11.784Z", "externalId": "order-58213"}
```

**Token.** Any non-empty `X-API-TOKEN` works while `access.any_token` is `true` (the default). With
`set_sms_service {"any_token": false}` only the value of `secret://zenvia-token` works: a random value written once
into the environment's `secrets.yaml` (`<data>/environments/<env>/secrets.yaml`, key `zenvia-token`). A Key Vault
created afterwards imports it as a secret ([Secrets](secrets.md)).

**Callbacks.** Tell Stuntman where your webhook is:

```text
set_sms_service {"callbacks": {"status": "http://host.docker.internal:3000/hooks/zenvia", "inbound": "http://host.docker.internal:3000/hooks/zenvia"}}
```

`status` gets `MESSAGE_STATUS`, `inbound` gets `MESSAGE` (the person's replies). Empty: the environment's default
callback (`set_environment` `callbacks`). Both empty: nothing is sent and the history notes `no callback URL set`.

## What it supports

### `POST /zenvia/v2/channels/{channel}/messages`

| Field | Honoured |
|---|---|
| `X-API-TOKEN` header | required (see above) |
| `from` | required; with `strict_senders`, must be one of `senders[].id` |
| `to` | required; a string or a number; 10 to 15 digits, no letters (`+`, spaces and dashes are dropped) |
| `contents[0]` | the first content only; later ones are ignored |
| `contents[0].type: "text"`, `text` | the text the person reads |
| `contents[0].type: "template"`, `templateId`, `fields` | a declared template's text with `{{field}}` filled from `fields`; an undeclared id is sent as `[template <first 8 chars>] key: value, …` unless `strict_templates` |
| `externalId` | kept, echoed in the answer and in every `MESSAGE_STATUS` |
| `traceparent` header | the trace id, kept and shown on the Timeline |
| `X-Client-Name` or `User-Agent` header | names the caller (first word) |

Answer `200` with `id` (a UUID), `from`, `to` (digits), `direction: "OUT"`, `channel: "sms"`, `contents`,
`timestamp`, and `externalId` when sent. Only this endpoint exists: no batch, no `GET` of a message, no
subscription API (callbacks are set in Stuntman), no other channel (`whatsapp`, `rcs`… answer `400`; WhatsApp is its
own service: [WhatsApp](whatsapp.md)).

### Encoding and segments

Each text is counted as an SMS: GSM-7 (160 characters in one segment, 153 per segment after; `^{}\[~]|€` count
double) or, as soon as one character is outside GSM-7, UCS-2 (70, then 67 per segment). The message keeps
`encoding`, `segments`, `units` and `forcedBy` (the first character that forced UCS-2, e.g. `ó`).

### Recipients: the person behind a number

`sms/recipients.yaml` is a list matched in order; the first match wins. `number` is an exact number
(`+14155550142`), a prefix ending in `*` (`+1 702 555 01*`), or `*` (everyone else, always present). Digits are
compared, so spaces do not matter. Each recipient has an optional `carrier` (a label) and `steps`; each step sets one
key:

| Step | Meaning | Default |
|---|---|---|
| `delivered_after: 3s` | `DELIVERED` this long after `SENT` | `3s` |
| `not_delivered: <reason>` | `NOT_DELIVERED` instead, after `delivered_after`. Reasons: `invalid`, `carrier_rejected`, `phone_off`, `blocked`, `not_covered` | — |
| `never_delivered: 48h` | stays `SENT`, then `NOT_DELIVERED` `phone_off` after this long | — |
| `on_question: {reply, after}` | after `DELIVERED`, replies when the text ends with `?` or contains `reply` or `responda` | `after: 5s` |
| `on_nth: {n, reply, after}` | replies when this is the n-th message the app sent to this number in the last 24 h (refused ones count) | `after: 5s` |
| `on_text: {reply, after}` | replies to any delivered text | `after: 5s` |

The default file has one recipient, `*`, with `delivered_after: 3s`. At most one reply per message: the first step
(in list order) that has a matching `on_nth`, then `on_question`, then `on_text`, wins.

```text
set_sms_recipient {"number": "+14155550142", "carrier": "Acme Mobile", "steps": [{"delivered_after": "1s"}, {"on_text": {"reply": "STOP", "after": "1s"}}]}
set_sms_recipient {"number": "+13125550110", "steps": [{"delivered_after": "1s"}, {"not_delivered": "carrier_rejected"}]}
```

A new recipient goes before `*`. `number` must start with `+` and have at least 4 digits, or be `*`.

### Service block

`sms/service.yaml`, changed with `set_sms_service` (each key replaces that block):

| Key | Default |
|---|---|
| `any_token` | `true` |
| `senders` | `[{id: Acme, type: alphanumeric}, {id: "28282", type: short_code}, {id: "+15550104499", type: long}]` |
| `strict_senders` | `false` (any `from`) |
| `callbacks` | `{status: "", inbound: ""}` |
| `opt_out` | `{keywords: [STOP, CANCEL, UNSUBSCRIBE, SAIR], back_in: START}` |
| `limits` | `{throughput: 30, max_segments: 10}` |
| `templates` | `[]`, each `{id, name, text}` with `{{field}}` placeholders |
| `strict_templates` | `false` |

`GET /api/sms/files/service.yaml` and `GET /api/sms/files/recipients.yaml` download the files.

### Webhooks to your app

`MESSAGE_STATUS`:

```json
{"id": "…", "timestamp": "2026-11-21T14:02:12.184Z", "type": "MESSAGE_STATUS", "subscriptionId": "stuntman-status",
 "channel": "sms", "messageId": "6f1c…", "externalId": "order-58213", "contentIndex": 0,
 "messageStatus": {"timestamp": "…", "code": "NOT_DELIVERED", "description": "The message was not delivered",
                   "causes": [{"channelErrorCode": "carrier_rejected", "reason": "Carrier rejected (filtered as spam)", "details": "…"}]}}
```

`code` is `SENT`, `DELIVERED` or `NOT_DELIVERED`; `causes` only on `NOT_DELIVERED`.

`MESSAGE` (the person texts back):

```json
{"id": "…", "timestamp": "…", "type": "MESSAGE", "subscriptionId": "stuntman-inbound", "channel": "sms", "direction": "IN",
 "message": {"id": "…", "from": "14155550142", "to": "Acme", "direction": "IN", "channel": "sms",
             "contents": [{"type": "text", "text": "STOP"}], "timestamp": "…"}}
```

Headers: `Content-Type: application/json`, `User-Agent: Zenvia-Webhook`. Webhooks are not signed.

## How it behaves

### Checks, in order

| Check | Answer (and the message is kept as `refused`) |
|---|---|
| no `X-API-TOKEN`, or the wrong one with `any_token: false` | `401 {"code":"UNAUTHORIZED","message":"Unauthorized: missing or invalid X-API-TOKEN"}` (not kept) |
| body is not JSON | `400 VALIDATION_ERROR Malformed JSON` (not kept) |
| a forced failure is waiting | the forced answer (see [Provoke failures](#provoke-failures)) |
| channel is not `sms` | `400 VALIDATION_ERROR`, `details[0].path: "channel"` (not kept) |
| no `contents` | `400`, `details: [{"code":"REQUIRED","path":"contents"}]` |
| `contents[0].type` is neither `text` nor `template` | `400`, `path: "contents[0]"`, `INVALID_VALUE` |
| undeclared `templateId` with `strict_templates` | `400 Template <id> not found`, `path: "contents[0]"` |
| bad `to` | `400 invalid "to": <to>`, `{"code":"INVALID_FORMAT","path":"to"}` |
| no `from` | `400 "from" is required`, `{"code":"REQUIRED","path":"from"}` |
| unknown `from` with `strict_senders` | `400 unknown sender "<from>"`, `INVALID_VALUE` |
| number opted out | `400 +<digits> opted out (texted STOP)`, `{"code":"OPTED_OUT","path":"to"}` |
| more than `throughput` sends in the last second | `429 {"code":"TOO_MANY_REQUESTS","message":"Over 30 messages/s"}` |
| more than `max_segments` segments | `400 message too long: <n> segments (max 10)`, `{"code":"MAX_LENGTH","path":"contents[0].text"}` |

Error bodies are `{"code", "message", "details": [{"code", "path", "message"}]}`. Refused sends count toward the
throughput window.

### Lifecycle

1. `200`; status `accepted`.
2. 0.4 s later `SENT` (callback).
3. Per the recipient: `DELIVERED` after `delivered_after` (default 3 s), or `NOT_DELIVERED` with the reason after
   the same wait, or `NOT_DELIVERED` `phone_off` after `never_delivered`.
4. After `DELIVERED` only: the reply, if a step matches, after its `after` (default 5 s), as a `MESSAGE` webhook.

A status set by hand (`set_sms_status`) stops the automatic steps that would follow from another status.

### Opt-out

A reply whose whole text equals one of `opt_out.keywords` (case-insensitive) adds the number to the opt-out list;
`opt_out.back_in` (`START`) removes it. New sends to an opted-out number are refused with `OPTED_OUT`.
`sms_opt_out` / `sms_opt_in` change the list without a reply.

### Callbacks

Each callback is tried up to 4 times, 20 s apart, until the app answers 2xx (10 s timeout per attempt). Every
attempt is recorded (`list_sms_callbacks`) and its answer is noted on the matching status in the message's
`history`. `retry_sms_callbacks` sends again the ones not taken in the last hour.

### Persistence and time

- Messages, opt-outs and callbacks are in the environment's database; the YAML files are the configuration. Both
  survive restarts.
- After a restart, outgoing messages of the last hour still `accepted` or `SENT` start their lifecycle again; a
  message that was `SENT` is set back to `accepted`, so the app receives `SENT` a second time.
- SMS keeps real time; the environment clock ([Clock](../clock.md)) does not move it. "Today" for `on_nth` is the
  last 24 hours.

### Differences from Zenvia

- Message ids are UUIDs; Zenvia's subscription ids are fixed (`stuntman-status`, `stuntman-inbound`).
- Templates are Stuntman's own list in `service.yaml`, not Zenvia's template registry.
- `service.yaml` has a `flavour` field (`zenvia-v2`); no other API shape is served.

## Provoke failures

| Failure | How | What the app sees | Turn it off |
|---|---|---|---|
| Zenvia 500 | `force_sms_failure {"answer": "500", "count": 2}` | `500 {"code":"INTERNAL_ERROR","message":"Internal Server Error"}` for the next 2 sends | `force_sms_failure {"answer": "normal"}`, or the count runs out |
| Zenvia 400 | `force_sms_failure {"answer": "400"}` | `400 {"code":"VALIDATION_ERROR","message":"Validation error","details":[{"code":"INVALID_VALUE","path":"to",…}]}` | same |
| Connection reset | `force_sms_failure {"answer": "reset"}` | TCP RST, no answer | same |
| Empty reply | `force_sms_failure {"answer": "empty"}` | connection closed without a byte | same |
| Timeout | `force_sms_failure {"answer": "timeout"}` | no answer for 10 minutes | same |
| Not delivered | `set_sms_recipient {"number", "steps": [{"not_delivered": "phone_off"}]}`, or `set_sms_status {"id", "status": "NOT_DELIVERED", "reason": "blocked"}` | `MESSAGE_STATUS` `NOT_DELIVERED` with `causes` | `remove_sms_recipient {"number"}` |
| Never confirmed | `steps: [{"never_delivered": "2m"}]` | `SENT`, then `NOT_DELIVERED` `phone_off` after 2 m | remove the recipient |
| The person replies / texts STOP | `reply_sms_as_user {"number": "+14155550142", "text": "STOP"}`, or an `on_text` step | `MESSAGE` webhook; the number is opted out; the next send gets `400 OPTED_OUT` | `reply_sms_as_user … "text": "START"` or `sms_opt_in` |
| Duplicate callback | `resend_sms_status {"id"}` (optional `status`) | the same `MESSAGE_STATUS` again | — |
| App's webhook down | point `callbacks.status` at a URL that fails | 4 attempts, 20 s apart | `retry_sms_callbacks` after the app is back |
| Wrong token | `set_sms_service {"any_token": false}` | `401` for any other token | `{"any_token": true}` |
| Unknown sender | `set_sms_service {"strict_senders": true}` | `400` for a `from` not in `senders` | `false` |
| Too fast | `set_sms_service {"limits": {"throughput": 1, "max_segments": 10}}` | `429` | set it back |
| Too long | `set_sms_service {"limits": {"throughput": 30, "max_segments": 1}}` | `400 MAX_LENGTH` | set it back |

Each forced failure is kept as a `refused` message whose `reason` says `… (forced by <who>)`; `get_sms_service`
shows the waiting one in `forced` (`answer`, `left`, `by`, `at`).

## See what the app did

| Need | Tool | HTTP |
|---|---|---|
| Wait for the app's SMS and its code | `wait_for_sms {"number": "+14155550142", "text_contains": "code", "timeout": "10s"}` | `GET /api/sms/wait?number=%2B14155550142&timeout=5s` |
| List | `list_sms` (`number`, `status`, `dir`, `q`, `since` default `1d`, `limit` default 200) | `GET /api/sms/messages?number=%2B14155550142&since=5m` |
| One message | `get_sms {"id"}` | `GET /api/sms/messages/{id}` |
| Callbacks and the app's answers | `list_sms_callbacks {"limit": 100}` | `GET /api/sms/callbacks` |
| Opt-outs | `list_sms_opt_outs` | `GET /api/sms/opt-outs` |

`wait_for_sms` looks back 60 s by default (`since`), ignores refused sends, returns the newest match as
`{message, id, extracted}`, and answers `408 No SMS to <number> in 30s` on timeout. `extracted.otp` is the value of a
template field named `token`, `code`, `otp` or `codigo`, else the first 4–8 digit group of the text outside links;
`extracted.from` says which.

Fields that matter: `id`, `dir` (`out`, `in`), `number`, `sender`, `text`, `status` (`accepted`, `SENT`,
`DELIVERED`, `NOT_DELIVERED`, `refused`, `received`), `reason`, `history[]` (`status`, `at`, `reason`, `by`,
`webhook` with the app's `status`, `ms`, `attempt`, `error`, `url`), `body` (the app's request, with `template` when
it used one), `encoding`, `segments`, `externalId`, `traceId`, `caller`, `extracted`.

The [Timeline](../timeline.md) shows each send, refusal, status change and reply (`SMS to +14155550142: …`, `host
api.zenvia.com`); forced and hand-made ones are marked provoked. `timeline {"service": "sms"}` filters it.

## Recipes

1. **Read the OTP the app texted.**
   ```text
   wait_for_sms {"number": "+14155550142", "timeout": "10s"}   → extracted.otp = "482913"
   ```
2. **Render the app's Zenvia template.**
   ```text
   set_sms_service {"templates": [{"id": "11111111-2222-4333-8444-555555555555", "name": "signing code", "text": "{{company}}: your signing code is {{token}}."}]}
   ```
   The app sends `{"type": "template", "templateId": "1111…", "fields": {"company": "Acme", "token": "482913"}}`;
   `list_sms` shows `Acme: your signing code is 482913.`
3. **Check the app handles a failed delivery.**
   ```text
   set_sms_recipient {"number": "+13125550110", "steps": [{"delivered_after": "1s"}, {"not_delivered": "invalid"}]}
   ```
   The app sends to that number and gets `NOT_DELIVERED` with `channelErrorCode: "invalid"`.
4. **The person replies STOP; the app must stop texting.**
   ```text
   reply_sms_as_user {"number": "+14155550142", "text": "STOP"}
   list_sms_opt_outs {}                                              → 14155550142
   ```
   The app's next send to it gets `400 OPTED_OUT`.
5. **Zenvia is down for two sends, then back.**
   ```text
   force_sms_failure {"answer": "500", "count": 2}
   ```
   Check the app retries and the third send goes.
6. **The app must ignore duplicate status callbacks.** `resend_sms_status {"id": "<id>"}`, then check the app did
   not act twice.
7. **The app's webhook was down; deliver what it missed.** `list_sms_callbacks {}` shows the failed attempts;
   `retry_sms_callbacks {}` → `{"retried": n}`.

## Errors and troubleshooting

| Symptom or error text | Cause | Fix |
|---|---|---|
| `401 Unauthorized: missing or invalid X-API-TOKEN` | no token, or `any_token: false` and another token | send `X-API-TOKEN`; use `zenvia-token` from `secrets.yaml`, or `any_token: true` |
| `400 Channel <x> is not simulated here` | the app's URL has another channel than `sms`, or `{0}` was not replaced | use `/v2/channels/sms/messages` |
| `400 invalid "to"` | fewer than 10 or more than 15 digits, or letters | send the full number with country code |
| `400 … opted out (texted STOP)` | the number is on the opt-out list | `sms_opt_in {"number"}` or reply `START` |
| `429 Over 30 messages/s` | a burst beyond `throughput` (refused sends count) | raise `limits.throughput` |
| No callback at the app | `callbacks` and the environment's default are empty | `set_sms_service {"callbacks": {…}}` |
| History shows `connection refused` for each callback | the URL is not reachable from Stuntman (e.g. `localhost` from inside Docker) | use `host.docker.internal` or the app's service name |
| `wait_for_sms` 408 | the SMS was sent more than 60 s before, or to another number, or was refused | `since: "5m"`; `list_sms {"status": "refused"}` |
| Text shows `[template d5d33983] …` | the `templateId` is not declared | add it to `templates` |
| `Use +<digits> (with country code)…` from `set_sms_recipient` | the number has no `+` | write `+14155550142`, a prefix with `*`, or `*` |
| `"Everyone else" stays; change its steps instead` | `remove_sms_recipient` on `*` | `set_sms_recipient {"number": "*", "steps": […]}` |

## Not done

- Only Zenvia API v2 and only the SMS channel. No Twilio, Vonage, AWS SNS or other SMS APIs; the `flavour` field is
  not used to switch API shape.
- No batch sends, no message lookup API, no subscription management API, no delivery reports by polling.
- Only `contents[0]` is sent; multi-content requests lose the rest.
- Webhooks are not signed.
- The environment clock does not apply.

Missing something? Tell the developer what the fix would be and offer a pull request — see [Contributing](../../CONTRIBUTING.md).

## Tools and routes
<!-- tools: sms -->
_Generated by `tools/gen_tool_reference.py` from the engine source; each tool is an MCP tool and the HTTP route shown (under `/api`)._

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_sms` | `GET /api/sms/messages` | — | number, status, dir, q, since, limit | SMS both ways (the app's sends, refusals and the person's replies), newest first: text, encoding, segments, status history, the code found. |
| `get_sms` | `GET /api/sms/messages/{id}` | id | — | One SMS with its status history (and what the app answered to each callback). |
| `wait_for_sms` | `GET /api/sms/wait` | number | text_contains, timeout, since | Waits for the app to send an SMS to a number (optionally with text containing…); returns it with the code found (extracted.otp). |
| `reply_sms_as_user` | `POST /api/sms/numbers/{number}/reply` | number, text | to, by | The person texts back (STOP, HELP or anything): Zenvia's MESSAGE webhook goes to the app. STOP-like keywords opt the number out; START opts it back in. |
| `set_sms_status` | `POST /api/sms/messages/{id}/status` | id, status | reason, by | Moves an SMS to SENT, DELIVERED or NOT_DELIVERED (reason: invalid, carrier_rejected, phone_off, blocked, not_covered); the MESSAGE_STATUS callback goes to the app. |
| `resend_sms_status` | `POST /api/sms/messages/{id}/resend` | id | status | Sends the same MESSAGE_STATUS callback again (a duplicate), to test that the app ignores it. |
| `list_sms_callbacks` | `GET /api/sms/callbacks` | — | limit | The callbacks Stuntman sent to the app (MESSAGE_STATUS, MESSAGE) with the app's answers. |
| `retry_sms_callbacks` | `POST /api/sms/callbacks/retry` | — | — | Sends again the callbacks the app did not take (non-2xx or no answer) in the last hour. |
| `list_sms_opt_outs` | `GET /api/sms/opt-outs` | — | — | Numbers that texted STOP (or were opted out): new sends to them are refused until START. |
| `sms_opt_out` | `POST /api/sms/opt-outs` | number | — | Adds a number to the opt-out list without texting STOP. |
| `sms_opt_in` | `DELETE /api/sms/opt-outs/{number}` | number | — | Removes a number from the opt-out list. |
| `force_sms_failure` | `POST /api/sms/force` | answer | count, by | The next sends (count, default 1) fail as Zenvia does: 500 (INTERNAL_ERROR), 400 (VALIDATION_ERROR body), or the connection: reset, empty, timeout. normal stops it. Each one is listed as refused. |
| `list_sms_recipients` | `GET /api/sms/recipients` | — | — | The simulated people behind numbers, in match order (first match wins; * is everyone else), with their steps. |
| `set_sms_recipient` | `PUT /api/sms/recipients` | number | carrier, steps | Creates or replaces a recipient (sms/recipients.yaml): carrier and steps (delivered_after, not_delivered: reason, never_delivered: 48h, on_question / on_nth / on_text: {reply, after}). |
| `remove_sms_recipient` | `DELETE /api/sms/recipients` | number | — | Removes a recipient; the number falls back to the next match or “Everyone else”. |
| `get_sms_service` | `GET /api/sms/service` | — | — | The service block (sms/service.yaml): API shape, access, senders, callbacks, opt-out keywords, limits, declared templates; plus the base URL for the app. |
| `set_sms_service` | `PUT /api/sms/service` | — | any_token, strict_senders, senders, callbacks, opt_out, limits, templates, strict_templates | Changes the service block: any_token, strict_senders, senders, callbacks {status, inbound}, opt_out {keywords, back_in}, limits {throughput, max_segments}, templates [{id, name, text}], strict_templates. |
<!-- /tools -->
