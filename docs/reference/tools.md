# Tool reference

Every action in Stuntman is one tool. Each tool is an **MCP tool** (same name) and an **HTTP route** under `/api`.
`GET /api/tools` returns this catalog from a running server, with full parameter schemas.

_Generated from the engine source by `tools/gen_tool_reference.py`. Do not edit by hand._

**293 tools** in 11 areas.

- [Environments and timeline](#environments-and-timeline) · 12 tools
- [E-mail](#e-mail) · 35 tools
- [SMS](#sms) · 17 tools
- [WhatsApp](#whatsapp) · 28 tools
- [HTTP APIs (plugins)](#http-apis-plugins) · 46 tools
- [Storage (Azure Blob, S3)](#storage-azure-blob-s3) · 45 tools
- [Messaging (Service Bus, RabbitMQ)](#messaging-service-bus-rabbitmq) · 19 tools
- [Identity (OIDC / OAuth2)](#identity-oidc-oauth2) · 26 tools
- [Secrets (Key Vault)](#secrets-key-vault) · 31 tools
- [Webhook inboxes](#webhook-inboxes) · 9 tools
- [Telemetry (OpenTelemetry)](#telemetry-opentelemetry) · 25 tools

## Environments and timeline

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_environments` | `GET /api/environments` | — | — | Environments on this Stuntman: name, state, lifetime, services, addresses. |
| `get_environment` | `GET /api/environments/{name}` | name | — | One environment: addresses, callbacks, real hosts, lifetime, paused, environment.yaml. |
| `create_environment` | `POST /api/environments` | name | from, copy, expires, callbacks | Make an environment: from another one (a copy), "empty", or a folder; returns its addresses. copy picks what a copy takes: services, state, scenarios, inboxes, secrets (default: all but state). |
| `set_environment` | `PUT /api/environments/{name}` | name | callbacks, lifetime, paused, base_domain, real_hosts, version | Change callbacks, lifetime, paused, base_domain or real_hosts (same keys as environment.yaml). |
| `pause_environment` | `POST /api/environments/{name}/pause` | name | paused | Pause (every service answers 503, nothing is stored, state kept) or resume. |
| `extend_environment` | `POST /api/environments/{name}/extend` | name | by | Push an expiring environment's expiry (default 24 h from its current expiry or now). |
| `reset_environment` | `POST /api/environments/{name}/reset` | name | — | Every service back to its seeds: traffic, deliveries, telemetry and temporary inboxes go; files and kept inboxes stay. |
| `delete_environment` | `DELETE /api/environments/{name}` | name | — | Delete an environment: services, state and history. Irreversible. |
| `get_environment_hosts` | `GET /api/environments/{name}/hosts` | name | — | Lines for /etc/hosts (or a DNS zone) so the environment's host names reach this Stuntman. |
| `timeline` | `GET /api/timeline` | — | range, to, from, sel_from, sel_to, service, who, show, q, limit, buckets | Everything that reached this environment's services in a window: rows (newest first), counts per service and per actor, activity lanes, and offline gaps. |
| `get_timeline_row` | `GET /api/timeline/{id}` | id | — | One Timeline row and the rest of its journey (rows with the same trace id). |
| `wait_for_timeline` | `POST /api/timeline/wait` | — | service, who, show, q, since, timeout | Waits until a row that matches appears (after `since`, default now) and returns it; 408 after the timeout. |

## E-mail

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_messages` | `GET /api/messages` | — | channel, to, subject, kind, inbox, status, lang, has, since, before, q, limit, group | The list and filters of the Notifications screen. group="kind" also returns the kinds with counts. |
| `get_message` | `GET /api/messages/{id}` | id | — | The message pane: headers, text, HTML, extracted values, links, attachments, events. |
| `wait_for_message` | `GET /api/messages/wait` | to | subject, channel, extract, timeout, since | Blocks until a matching message arrives; with extract, returns the value. Fails on timeout with what did arrive. Looks back 15 s by default (since). For a queue or topic, pass namespace (and entity). |
| `expect_no_message` | `GET /api/messages/expect-none` | to | within, subject | Passes if nothing arrives in time; fails with the message that did. For “blocked address gets no e-mail”. |
| `simulate_recipient` | `POST /api/messages/{id}/events` | id, action | link | action: open \| click \| spamreport \| unsubscribe. Sends the provider's event webhook to your app; returns the event and your app's reply. |
| `get_attachment` | `GET /api/messages/{id}/attachments/{index}/info` | id | name, index | The file, as base64 and a download URL. |
| `delete_messages` | `DELETE /api/messages` | — | to, before, since, status, inbox | Clears messages of this environment, e.g. before a test. Same filters as list_messages. |
| `delete_message` | `DELETE /api/messages/{id}` | id | — | Deletes one message. |
| `decide_held` | `POST /api/messages/{id}/decide` | message_id, action | kind | For a held message: deliver \| defer \| bounce \| drop. kind: hard\|soft for bounce, 1\|3\|until for defer. Returns the event your app received and its reply. |
| `create_inbox` | `POST /api/inboxes` | — | ttl, keep, local_part, prefix, address, on_arrival, defer_times, hold_for, bounce_kind, steps, apply_to, label | A new inbox. Temporary by default (expires by itself); keep=true keeps it with the environment. |
| `list_inboxes` | `GET /api/inboxes` | — | kind, q | Inboxes with rule, steps and message count. |
| `get_inbox` | `GET /api/inboxes/{name}` | address | — | The inbox, plus its last 20 messages. |
| `update_inbox` | `PATCH /api/inboxes/{name}` | address | on_arrival, defer_times, hold_for, bounce_kind, steps, apply_to, label | Change what happens on arrival, the steps or Apply to. |
| `extend_inbox` | `POST /api/inboxes/{name}/extend` | address | by | Adds time to a temporary inbox (also revives an expired one). |
| `keep_inbox` | `POST /api/inboxes/{name}/keep` | address | — | Temporary becomes kept: never expires, survives clones. |
| `delete_inbox` | `DELETE /api/inboxes/{name}` | address | with_messages | Deletes the inbox (and its messages unless with_messages=false). Mail sent there afterwards lands in the catch-all. |
| `list_services` | `GET /api/services` | — | channel | E-mail services with intake, access and event target. |
| `get_service` | `GET /api/services/{name}` | name | — | The YAML, parsed, plus addresses to copy and the events public key. |
| `update_service` | `PATCH /api/services/{name}` | name | patch, new_name | Any field of the YAML (camelCase or snake_case); returns the diff applied. name renames, default=true makes it the default. |
| `put_service_yaml` | `PUT /api/services/{name}` | name, yaml | — | Replaces the whole service file (Edit as YAML). |
| `set_access` | `PUT /api/services/{name}/intake/{intake}/access` | name, intake | mode, how, on, starttls, refuse_recipients | How a test makes the provider unreachable or strict. intake: sendgridApi \| smtp · mode: any \| validate \| refuse · how: timeout \| refused \| tls \| 503 \| 429. For smtp also starttls=false and refuse_recipients=550\|451\|none. |
| `create_key` | `POST /api/services/{name}/intake/{intake}/keys` | name, intake | label | A key the app must use when the intake validates keys. The secret is shown once. |
| `revoke_key` | `DELETE /api/services/{name}/intake/{intake}/keys/{key_id}` | name, intake, key_id | — | Revokes a key; sends with it get 401 (or 535 on SMTP) while keys are validated. |
| `send_test_event` | `POST /api/services/{name}/events/test` | name | type | Sends one event to the app now; returns the body sent and your app's status, time and body. |
| `create_service` | `POST /api/services` | name | channel, copy_from | A new e-mail service, blank or copied from another. |
| `remove_service` | `DELETE /api/services/{name}` | name | — | Removes the service; its messages stay until their retention ends. |
| `list_extractors` | `GET /api/channels/{channel}/extractors` | — | channel | Built-ins and yours, with matches today and the last match. |
| `get_extractor` | `GET /api/channels/{channel}/extractors/{name}` | channel, name | — | The rule, where it is used, and its health. |
| `test_extractor` | `POST /api/channels/{channel}/extractors/test` | from, rule | channel, only_on, follow_redirects, all_matches, strip, message_id, last | Runs a rule on one message (message_id) or the last N (last=50). Per message: value or why not. Nothing is saved. |
| `create_extractor` | `POST /api/channels/{channel}/extractors` | name, from, rule | channel, only_on, follow_redirects, all_matches, strip | Saves a new extractor in email/extractors.yaml. |
| `update_extractor` | `PATCH /api/channels/{channel}/extractors/{name}` | name | channel, new_name, from, rule, only_on, follow_redirects, all_matches, strip | Changes an extractor; returns it. |
| `copy_extractor` | `POST /api/channels/{channel}/extractors/{name}/copy` | name, to_channel | channel | Copies to another channel; sources that do not exist there (HTML, header) are refused. |
| `delete_extractor` | `DELETE /api/channels/{channel}/extractors/{name}` | name | channel | Deletes it; tells you who used it. |
| `tune_builtin` | `PATCH /api/channels/{channel}/extractors/builtin/code` | — | channel, on, min_digits, max_digits, words, prefer_prominent | Settings of the built-in “code” extractor. |
| `put_extractors_yaml` | `PUT /api/channels/{channel}/extractors.yaml` | yaml | channel | Replaces email/extractors.yaml. |

## SMS

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

## WhatsApp

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_whatsapp_messages` | `GET /api/whatsapp/messages` | — | number, status, template, dir, q, since, limit | WhatsApp messages both ways (the app's sends and the person's replies), newest first, with status history and the window. |
| `list_whatsapp_conversations` | `GET /api/whatsapp/conversations` | — | q | One row per number: the last message, the 24 h window and the person's name. |
| `get_whatsapp_message` | `GET /api/whatsapp/messages/{wamid}` | wamid | — | One message with its status history (and what the app's webhook answered to each). |
| `wait_for_whatsapp` | `GET /api/whatsapp/wait` | number | template, text, timeout, since | Waits for the app to send a WhatsApp message to a number (optionally a template or a text containing…); returns it. |
| `reply_as_user` | `POST /api/whatsapp/numbers/{number}/reply` | number | text, button, image, location, by | The person writes back: a text, a tap on a template button, an image or a location. Stuntman sends Meta's messages webhook to the app; a reply opens the 24 h window. |
| `set_message_status` | `POST /api/whatsapp/messages/{wamid}/status` | wamid, status | error_code, by | Moves a message to sent, delivered, read or failed (with a Meta error code); the status webhook goes to the app. |
| `resend_status` | `POST /api/whatsapp/messages/{wamid}/resend` | wamid | status | Sends the same status webhook again, to test that the app ignores duplicates. |
| `get_window` | `GET /api/whatsapp/numbers/{number}/window` | number | — | The 24 h customer service window of a number: open or closed and until when (outside it only templates are delivered; texts fail with 131047). |
| `list_whatsapp_webhooks` | `GET /api/whatsapp/webhooks` | — | limit | The webhooks Stuntman sent to the app (statuses, messages, template and quality updates) with the app's answers. |
| `retry_whatsapp_webhooks` | `POST /api/whatsapp/webhooks/retry` | — | — | Sends again the webhooks the app did not take (non-2xx or no answer) in the last hour, the newest per message. |
| `list_templates` | `GET /api/whatsapp/templates` | — | status, category | WhatsApp templates (whatsapp/templates.yaml): languages, status, quality, category and today's use. |
| `set_template_status` | `PUT /api/whatsapp/templates/{name}/status` | name, status | reason | Approved, pending, rejected (with a reason), paused or disabled; Stuntman sends message_template_status_update to the app and sends follow the new status. |
| `update_template` | `PATCH /api/whatsapp/templates/{name}` | name | category, quality, body, header, footer, buttons, languages, variables | Changes a template's category (sends template_category_update), quality (low pauses it, as Meta does) or text. |
| `add_template` | `POST /api/whatsapp/templates` | name, body | language, category, status, header, footer, buttons | Declares a template (writes whatsapp/templates.yaml). |
| `remove_template` | `DELETE /api/whatsapp/templates/{name}` | name | — | Deletes a template from whatsapp/templates.yaml (sends with it then fail 132001). |
| `template_usage` | `GET /api/whatsapp/templates/{name}/usage` | name | since | Sends, failures by error code, reads and buttons tapped for a template, plus the values the app sent last. |
| `undeclared_templates` | `GET /api/whatsapp/templates/undeclared` | — | — | Templates the app asked for that are not declared; Stuntman answered them 132001. |
| `list_recipients` | `GET /api/whatsapp/recipients` | — | — | The simulated people behind numbers, in match order (the first match wins; * is everyone else), with their steps. |
| `set_recipient` | `PUT /api/whatsapp/recipients/{number}` | number | whatsapp, profile_name, steps, position | Creates or replaces a recipient (whatsapp/recipients.yaml): whether the number has WhatsApp, the profile name and the steps each message goes through. |
| `remove_recipient` | `DELETE /api/whatsapp/recipients/{number}` | number | — | Removes a recipient; the number falls back to the next match or “Everyone else”. |
| `explain_number` | `GET /api/whatsapp/recipients/explain` | number | — | Which recipient matches a number, which others also match, and what will happen to the next message. |
| `get_whatsapp_number` | `GET /api/whatsapp/number` | — | — | The number block (whatsapp/number.yaml): ids, access mode, webhook (URL, last verification), tier, throughput, quality. |
| `set_whatsapp_number` | `PUT /api/whatsapp/number` | — | any_token, tier, throughput, quality, webhook_url, sign, display_name, display_number, approve_after, api_versions, phone_number_id, waba_id | Changes the number block: any_token, tier, throughput, quality (sends its webhook), webhook_url, sign, display_name, approve_after, api_versions. |
| `force_whatsapp_failure` | `POST /api/whatsapp/force` | answer | count, by | The next sends (count, default 1) fail as Meta's Graph does: 500 (#131000), 400 (#100 Invalid parameter), or the connection: reset, empty, timeout. normal stops it. Each one is kept as a refused message. |
| `expire_whatsapp_tokens` | `POST /api/whatsapp/number/tokens/expire` | — | — | Every Graph call gets 190 “Session has expired” until restored. |
| `restore_whatsapp_tokens` | `POST /api/whatsapp/number/tokens/restore` | — | — | Tokens work again. |
| `verify_whatsapp_webhook` | `POST /api/whatsapp/number/webhook/verify` | — | — | Sends Meta's verification GET (hub.mode, hub.verify_token, hub.challenge) to the app's callback and checks it echoes the challenge. |
| `send_bad_webhook` | `POST /api/whatsapp/number/webhook/bad` | kind | — | Sends a status webhook the app must reject: wrong_signature, missing_signature, old_secret or replay (the last webhook again). Returns the app's status code. |

## HTTP APIs (plugins)

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_api_services` | `GET /api/apis/services` | — | — | HTTP API services (plugins) in this environment: origin, region, calls and billable calls today, forced answer, URLs; broken files with their error. |
| `list_api_calls` | `GET /api/apis/calls` | — | service, answer, since, q, limit | Calls any HTTP plugin answered, newest first: sentence, answer name, status, billable, trace id. |
| `get_api_call` | `GET /api/apis/calls/{id}` | id | raw | What the app sent and what Stuntman answered, the steps and webhooks; raw=true adds headers and bodies. |
| `wait_for_api_call` | `GET /api/apis/calls/wait` | service | route, answer, timeout, since | Waits for the next call to a service (and route, and answer): what a test waits for after acting in the app. |
| `force_answer` | `POST /api/apis/{service}/force` | service, answer | count, for, route | The next calls get this named answer (or reset \| empty \| timeout \| 500 \| 503 \| html): count=N calls or for=10m; route= limits it to one route (the token route is forced only this way); answer=default stops. |
| `billing` | `GET /api/apis/billing` | — | service, since | Billable calls and the counter value per service (price per call is only a counter); repeated inputs point at a retry loop. |
| `list_unmatched` | `GET /api/apis/unmatched` | — | group, since, ignored | Requests that reached Stuntman but no route took (answered 404 “no route”), grouped by path \| service \| caller \| host, with the closest route. |
| `route_from_request` | `POST /api/apis/unmatched/{id}/route` | id | — | A draft route prefilled from an unmatched request (method, path, body fields; the closest route's steps copied). Returns its YAML; set_route saves it. |
| `ignore_unmatched` | `POST /api/apis/unmatched/{id}/ignore` | id | — | Hides an unmatched request (and the same method and path); it is still answered 404. |
| `real_provider_calls` | `GET /api/apis/real-provider-calls` | — | since | Hosts the app called directly, from telemetry: calls that never came through Stuntman. |
| `list_plugins` | `GET /api/plugins` | — | query, region, category, collection | The catalog: plugins you can install (origin, region, badges, installed here, update available) and its collections. Filters: query, region (Global \| Brazil), category (Browser pages), collection. |
| `install_plugin` | `POST /api/plugins` | plugin | service, base_url | Installs a plugin from the catalog (or an empty one that records: plugin=empty, service=<name>) into this environment. |
| `remove_api_service` | `DELETE /api/plugins/{service}` | service | — | Removes a plugin from this environment (calls stay in the history). |
| `get_plugin` | `GET /api/plugins/{service}` | service | — | The plugin YAML: service block, routes, answers, data; and its version (send it back to save over it). |
| `put_plugin` | `PUT /api/plugins/{service}` | service, yaml | version | Replaces the whole plugin YAML; validated before saving (409 if it changed since the version you read). |
| `set_route` | `PUT /api/plugins/{service}/routes` | service, method, path | steps, old_route, version | Creates or replaces a route (method, path, steps: receive, let, keep, wait, decide, respond, send, fail, billable); validated before saving. |
| `delete_route` | `DELETE /api/plugins/{service}/routes` | service, method, path | — | Deletes a route. |
| `try_route` | `POST /api/plugins/{service}/try` | service, method, path | request, steps | Runs a route's steps on a sample request; nothing is kept, counted, billed or sent. steps= tries an unsaved version of the route. |
| `validate_plugin` | `POST /api/plugins/{service}/validate` | service | yaml | Schema and expression errors with line numbers; answers checked against the routes' schema. yaml= checks text before saving. |
| `list_answers` | `GET /api/plugins/{service}/answers` | service | — | Named answers: status, body, and which route step uses each (an answer used by none exists only to be forced). |
| `set_answer` | `PUT /api/plugins/{service}/answers/{name}` | service, name | status, body, headers, raw, sentence, note, old_name | Creates or replaces a named answer (status, body template, headers, sentence); checked against the schema unless raw=true (malformed on purpose). |
| `delete_answer` | `DELETE /api/plugins/{service}/answers/{name}` | service, name | — | Deletes a named answer that no route uses. |
| `get_data` | `GET /api/plugins/{service}/data` | service | table | A data table the answers read (irregular_cpfs, people, selfies…), or every table. |
| `set_data` | `PUT /api/plugins/{service}/data/{table}` | service, table, rows | add | Replaces (or with add=true appends to) a data table as the plugin declares it. |
| `synthetic_cpf` | `GET /api/apis/synthetic-cpf` | — | — | A CPF that passes the check digits and is not a real person's (for data tables and tests). |
| `get_api_service` | `GET /api/plugins/{service}/service` | service | — | A service block: base URL and real host, auth (OAuth2 client credentials, Basic), billing, limits; plus Stuntman's URLs. |
| `set_service` | `PUT /api/plugins/{service}/service` | service | any_client, token_ttl, client_certificate, session_ttl, billing, per_call, warn_above, latency, rate, fail_share, base_url, real_host, title, version | Changes the service block: any_client, token_ttl, billing (enabled, per_call, warn_above), latency, rate, fail_share, base_url, real_host, title. |
| `get_test_ca` | `GET /api/apis/test-ca` | — | — | The Stuntman Test CA (PEM): trust it in the app's machine or container and the providers' real host names work over HTTPS against Stuntman. |
| `issue_client_certificate` | `POST /api/apis/client-certificates` | name | — | A client certificate signed by the Stuntman Test CA, for an app calling a provider that requires mTLS. Returns certificate and key PEM. |
| `issue_person_certificate` | `POST /api/apis/people/{cpf}/certificate` | cpf | name, not_before, not_after | (Re)issues a person's cloud certificate (BirdID, ViDaaS people) by CPF with a validity window, signed by the Stuntman Test CA with the person's own key: expired yesterday by default, or not yet valid. Every plugin answers it until clear_person_certificate. Returns the chain PEM; keys stay in Stuntman. |
| `clear_person_certificate` | `DELETE /api/apis/people/{cpf}/certificate` | cpf | — | The person's certificate is again the plugin's (valid or expired as its people table says). |
| `list_forced` | `GET /api/apis/forced` | — | service, last | Who forced what: every forced answer and its stop, newest first. |
| `expire_tokens` | `POST /api/plugins/{service}/tokens/expire` | service | — | Every token and session cookie issued by this service becomes invalid: the app's next call gets 401 and must ask (or log in) again. |
| `get_catalog_plugin` | `GET /api/catalog/{plugin}` | plugin | — | One catalog plugin before installing it: routes, the pages it draws, its synthetic people, situations you can provoke, permissions and files. |
| `install_collection` | `POST /api/catalog/collections/install` | name | — | Installs the plugins of a collection that this environment does not have yet. |
| `import_plugin` | `POST /api/catalog/imports` | source | path, ref | Reads your own plugin from a folder, a .zip or a git URL (path= inside it, ref= branch or tag) and checks it. Installs nothing: install_import does. |
| `install_import` | `POST /api/catalog/imports/{id}/install` | id | service | Installs a plugin import_plugin read (only if it had no errors); service= installs it under another name. |
| `list_pages` | `GET /api/plugins/{service}/pages` | service | — | The pages a service draws (authorize, push approval): fields, buttons, their YAML and a preview URL. |
| `set_page` | `PUT /api/plugins/{service}/pages/{index}` | service, yaml | index | Replaces one page of a service (its YAML: route, title, ask, fields, buttons, checks, push); validated before saving. index= the page (0 first). |
| `pending_browser_steps` | `GET /api/apis/steps` | — | service, all, limit | Pages and push approvals waiting for a person (all=true: the recent ones too), with the URL to open them. |
| `authorize_as` | `POST /api/apis/steps/authorize` | — | person, otp, deny, step, service | Completes the waiting page as this person (name or CPF), as clicking Authorize; deny=true clicks Deny. Returns where the browser goes (the app's redirect_uri with code and state). |
| `approve_push` | `POST /api/apis/steps/push` | — | answer, step, service | The phone side of a push approval: answer=approve (default) \| reject \| timeout. The app's next poll sees it. |
| `list_restpki_sessions` | `GET /api/apis/{service}/sessions` | — | service | REST PKI signatures, newest first: token, status (started \| prepared \| signed \| finished), page, position (cm), stamp image, signer and thumbprint. |
| `get_restpki_session` | `GET /api/apis/{service}/sessions/{token}` | token | service | One REST PKI signature by token, with the certificate and its chain (base64 DER) once the browser started it. |
| `fail_restpki` | `POST /api/apis/{service}/fail` | step | status, count, service | The next call of a step fails: step=start (the app's POST PadesSignatures) \| webpki_start \| webpki_signature \| finish (SignedBytes); status=422 (ValidationError, default) or 500; count=N calls (default 1). status=default stops. |
| `reset_restpki` | `POST /api/apis/{service}/reset` | — | service | Forgets every REST PKI signature of the service and stops a forced failure (calls stay in the history). |

## Storage (Azure Blob, S3)

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_storage_accounts` | `GET /api/storage/accounts` | — | — | Storage accounts (Azure Blob and S3) with containers, counts, endpoint, connection string, reachability and seed. |
| `list_objects` | `GET /api/storage/{account}/{container}/objects` | account, container | prefix, by | Objects and folders under a prefix (one level), newest first; by = app, you, seed, agent or a caller name. |
| `get_object` | `GET /api/storage/{account}/{container}/objects/{*path}` | account, container, path | — | An object: properties, metadata, versions, the next read armed on it and its history (requests, links, events). |
| `wait_for_object` | `GET /api/storage/{account}/{container}/wait` | account, container | prefix, timeout, since | Waits for the first new object under a prefix (what an agent waits for after the app uploads). |
| `put_object` | `PUT /api/storage/{account}/{container}/objects/{*path}` | account, container, path | content, content_base64, file, content_type, truncated | Adds or replaces an object, recorded as added by the caller (never as the app). content (text), content_base64 or file (a local path). |
| `delete_object` | `DELETE /api/storage/{account}/{container}/objects/{*path}` | account, container, path | — | Deletes an object; the delete stays in the history and sends BlobDeleted. |
| `set_next_read` | `PUT /api/storage/{account}/{container}/next-read/{*path}` | account, container, path, result | times | Arms how the next read(s) of an object answer: 403 \| 404 \| 503 \| slow:5s \| truncate \| bad_md5 \| normal. Once by default, then back to normal. |
| `set_account_reachability` | `PUT /api/storage/{account}/reachability` | account, mode | — | Makes a whole account unreachable: timeout (no answer), 503 or 401; ok brings it back. |
| `reset_to_seed` | `POST /api/storage/reset` | — | accounts, to | Every storage account (or the ones named) back to its seed folder; says what changed since the seed was loaded. |
| `list_links` | `GET /api/storage/links` | — | state, issued_by, object, kind | Links (Azure SAS, S3 presigned) with object, what they allow, validity, uses and refusals. state = valid \| expired \| revoked. |
| `get_link` | `GET /api/storage/links/{id}` | id | — | What a link allows, its full URL and every use with the answer given. |
| `wait_for_link` | `GET /api/storage/{account}/{container}/links/wait` | account, container | prefix, timeout, since | Waits for the first link to an object under a prefix (the app's own SAS shows up at its first use); returns the URL. |
| `create_link` | `POST /api/storage/{account}/{container}/links/{*path}` | account, container, path | allows, valid_for, ips | A URL signed by Stuntman (Azure SAS or S3 presigned, by the account's kind), recorded as created by the caller. allows: read, write, delete, list. |
| `set_next_use` | `PUT /api/storage/links/{id}/next-use` | id, result | times | How a link's next use answers: expired \| bad_signature \| ip_denied \| permission \| not_yet_valid \| gone \| normal. Once, then normal. |
| `revoke_link` | `POST /api/storage/links/{id}/revoke` | id | — | Its next use answers 403 AuthenticationFailed, like Azure after the signing key rotates. The app is not told. |
| `extend_link` | `POST /api/storage/links/{id}/extend` | id | by | Pushes a link's expiry (Stuntman then accepts it until the new time). |
| `remove_link` | `DELETE /api/storage/links/{id}` | id | — | Removes a link from the list (its URL keeps working as it would). |
| `list_subscriptions` | `GET /api/storage/subscriptions` | — | — | Where storage events go: webhook or queue topic, filters, format and delivery health. |
| `create_subscription` | `POST /api/storage/subscriptions` | — | name, to, url, entity, events, under, format, account | A subscription: to = webhook (url) or queue (entity), events created/deleted, under (glob over container/path), format event_grid \| s3. Event Grid's validation handshake runs first. |
| `validate_subscription` | `POST /api/storage/subscriptions/{id}/validate` | id | — | Runs the validation handshake again. |
| `delete_subscription` | `DELETE /api/storage/subscriptions/{id}` | id | — | Removes a subscription; its events stay listed. |
| `list_events` | `GET /api/storage/events` | — | subscription, result, last, q | Storage events with where they went and the answer. result = delivered \| retrying \| given_up \| pending \| dropped \| held. |
| `get_event` | `GET /api/storage/events/{id}` | id | — | An event: payload in the provider's schema, delivery attempts, and the request that caused it. |
| `send_event` | `POST /api/storage/events` | subscription, path | type | Sends an event by hand to one subscription: type created \| deleted, for a path (container/path); recorded as sent by you, the object is not changed. |
| `set_next_delivery` | `PUT /api/storage/subscriptions/{id}/next-delivery` | id, behavior | — | How the next event to a subscription is delivered: hold:30s \| duplicate \| reorder \| drop \| missing_field \| normal. Once. For a queue or topic, pass namespace (and entity). |
| `resend_event` | `POST /api/storage/events/{id}/resend` | id | to_all | Delivers an event again (to_all: to every subscription, as new events). |
| `wait_for_storage_delivery` | `GET /api/storage/events/wait` | subscription | type, path, timeout, since | Waits for a delivery to a subscription (optionally of a type and path) and returns it with the app's answer. |
| `list_rule_options` | `GET /api/services/{name}/rules/options` | service | — | The operations, “only when” fields and responses this service offers (the rule editor's lists). Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `list_rules` | `GET /api/services/{name}/rules` | service | — | Rules in order (the first that matches answers): enabled or not, hits today, last hit, warnings. Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `test_rule` | `POST /api/services/{name}/rules/test` | service | rule, when, then, how_often, last | Try it: what the rule would match among the last requests and what the app would see per attempt. Nothing is saved. Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `create_rule` | `POST /api/services/{name}/rules` | service, name, then | when, how_often, enabled | A rule: when (op, account, container, path glob, only_when) → then (respond, seconds) → how_often (first N, random %, every time). Warns if a rule above answers the same requests. Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `update_rule` | `PATCH /api/services/{name}/rules/{rule}` | service, name | when, then, how_often, enabled, new_name | Applies a patch to a rule (when, then, how_often, enabled, new_name). The “first N” count starts again. Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `set_rule_enabled` | `PUT /api/services/{name}/rules/{rule}/enabled` | service, name, enabled | — | Switches a rule on or off; the “how often” count starts again. Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `move_rule` | `PUT /api/services/{name}/rules/{rule}/position` | service, name, position | — | Moves a rule to a position (1 is first); order matters. Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `delete_rule` | `DELETE /api/services/{name}/rules/{rule}` | service, name | — | Deletes a rule. Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `put_rules_yaml` | `PUT /api/services/{name}/rules.yaml` | service, yaml | — | Replaces the whole rules file. Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `get_service_schema` | `GET /api/services/{name}/schema` | service | — | The fields of a storage service's account form: types, limits, help (the form is drawn from it). Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `list_service_accounts` | `GET /api/services/{name}/accounts` | service | — | Accounts of one storage service with endpoint, containers and counts. Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `get_account` | `GET /api/services/{name}/accounts/{account}` | service, account | — | The settings as YAML and JSON, the connection string, keys (masked) and the file's etag for If-Match. Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `create_account` | `POST /api/services/{name}/accounts` | service, account | containers, settings | A new account with its keys (Azure key1/key2, S3 one access key). settings: containers {name: {access}}, behaviour, access. Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `update_account` | `PATCH /api/services/{name}/accounts/{account}` | service, account, patch | if_match | Applies a patch (same keys as the YAML); refused with 412 if the file changed since the etag you read (if_match). Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `rotate_key` | `POST /api/services/{name}/accounts/{account}/keys/{key}/rotate` | service, account, key | — | Makes a new value for a key: apps signing with the old one now get 403. The new value is returned once. Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |
| `add_container` | `POST /api/services/{name}/accounts/{account}/containers` | service, account, container | access | Adds a container (bucket) to an account. |
| `remove_container` | `DELETE /api/services/{name}/accounts/{account}/containers/{container}` | service, account, container | — | Removes a container and its objects; says how many went with it. The seed is not changed, so a reset brings them back. |
| `delete_account` | `DELETE /api/services/{name}/accounts/{account}` | service, account | — | Deletes an account, its objects and links (people confirm on the screen; agents do not). Also for messaging: service = azure-service-bus \| rabbitmq (accounts are namespaces). |

## Messaging (Service Bus, RabbitMQ)

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_namespaces` | `GET /api/messaging/namespaces` | — | — | Namespaces with kind (azure-service-bus, rabbitmq), entities (queues, topics and their subscriptions or bindings), counters per state and the connection string. |
| `list_queue_messages` | `GET /api/messaging/{namespace}/{entity}/messages` | namespace, entity | state, q, last | Messages of a queue, topic or subscription, newest first; state = active \| scheduled \| locked \| dead_letter \| completed. |
| `get_queue_message` | `GET /api/messaging/{namespace}/{entity}/messages/{seq}` | namespace, seq | entity | One message: properties, body and the delivery history (published, delivered, abandoned, completed, dead-lettered…). |
| `publish` | `POST /api/messaging/{namespace}/{entity}/messages` | namespace, entity | body, subject, properties, schedule_at, correlation_id, session_id, routing_key, message_id | Publishes a message as a test (recorded as sent by you or the agent, never as the app). Topics fan out to their subscriptions. |
| `resubmit` | `POST /api/messaging/{namespace}/{entity}/resubmit` | namespace | entity, seq, all_dead_letters | Puts dead-lettered messages back on their queue, with a note in their history: one seq, or all_dead_letters. |
| `dead_letter_message` | `POST /api/messaging/{namespace}/{entity}/messages/{seq}/dead-letter` | namespace, seq | entity | Moves a waiting message to dead-letter by hand (“dead-lettered by you”). |
| `delete_queue_message` | `DELETE /api/messaging/{namespace}/{entity}/messages/{seq}` | namespace, seq | entity | Deletes one message and its history. |
| `purge` | `DELETE /api/messaging/{namespace}/{entity}/messages` | namespace, entity | state | Removes messages of an entity (a state, or all). People confirm on the screen; agents do not. |
| `list_consumers` | `GET /api/messaging/{namespace}/{entity}/consumers` | namespace | entity | Connections receiving from this entity: client id, link, receive mode, prefetch, locks held, completed today; disconnected ones of the last day. |
| `get_consumer` | `GET /api/messaging/{namespace}/consumers/{id}` | namespace, id | — | One consumer: connection, the locks it holds now and its last messages. |
| `disconnect_consumer` | `POST /api/messaging/{namespace}/consumers/{id}/disconnect` | namespace, id | — | Forces a consumer off (RabbitMQ: CONNECTION_FORCED; Service Bus: amqp:link:detach-forced). Its locks are released: tests the app's reconnect logic. |
| `expire_consumer_locks` | `POST /api/messaging/{namespace}/consumers/{id}/expire-locks` | namespace, id | — | Expires the locks a consumer holds now: its next complete fails with MessageLockLost and the messages are delivered again. |
| `set_namespace_reachability` | `PUT /api/messaging/{namespace}/reachability` | namespace, mode | — | The whole namespace: ok \| refuse (connection refused) \| timeout (no answer) \| 401 (key rejected) \| busy (ServerBusy / connection.blocked). |
| `add_entity` | `POST /api/services/{name}/accounts/{account}/entities` | namespace, entity | kind, lock, max_delivery, sessions, exchange_type | Adds a queue or a topic to a namespace (or marks one the app created as declared: “Keep it in the YAML”). |
| `remove_entity` | `DELETE /api/services/{name}/accounts/{account}/entities/{entity}` | namespace, name | — | Removes a queue or a topic and its messages. |
| `add_subscription` | `POST /api/services/{name}/accounts/{account}/topics/{topic}/subscriptions` | namespace, topic, subscription | filter, lock, max_delivery | Adds a subscription to a topic (RabbitMQ: binds a queue to the exchange; filter is the binding key). |
| `rotate_policy_key` | `POST /api/services/{name}/accounts/{account}/policies/{policy}/rotate` | namespace, policy | — | Makes a new key for a shared access policy (RabbitMQ: a user's password); senders still using the old one get 401. |
| `set_throughput` | `PUT /api/messaging/{namespace}/throughput` | namespace, per_second | — | Messages a second the namespace takes before ServerBusy; none, a number, or throttle_now (every send ServerBusy). |
| `set_auto_create` | `PUT /api/messaging/{namespace}/auto-create` | namespace, on | — | On: queues and topics the app declares or sends to are created. Off: they answer 404 (test a missing entity). |

## Identity (OIDC / OAuth2)

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_providers` | `GET /api/identity/providers` | — | — | Identity providers in this environment: flavour, tenant, Authority, users, apps and sign-ins in the last hour. |
| `set_provider` | `PUT /api/identity/providers/{name}` | name | tenant, flavour, other_tenants | Creates an identity provider (or changes its flavour/tenant): entra-v2, google, okta or oidc. Users and apps are added with set_user and set_app. |
| `remove_provider` | `DELETE /api/identity/providers/{name}` | name | — | Removes an identity provider: its file (users, apps, tenant) and its sign-ins, keys and provocations. Apps pointed at its Authority get 404. |
| `list_sign_ins` | `GET /api/identity/sign-ins` | — | provider, user, result, since, limit | Sign-ins and tokens issued: the sentence, the result or error code (AADSTS…), newest first. |
| `get_sign_in` | `GET /api/identity/sign-ins/{id}` | id | — | One sign-in: steps, tokens issued (decoded) and the claims. |
| `sign_in_as` | `POST /api/identity/{provider}/sign-in-as` | user | app, mfa_code, consent, claims, provider | Completes the login page waiting for this app (or starts one) as a user, through MFA and consent; returns the app's callback to follow. |
| `provoke_sign_in` | `POST /api/identity/{provider}/provoke` | answer | count, for, provider | The next sign-ins: mfa_required, mfa_fails, consent_required, user_cancels, wrong_tenant, expired (tokens already expired), bad_signature, idp_down; normal stops it. |
| `pending_logins` | `GET /api/identity/pending` | — | provider | Login pages waiting for a person, with the app and the scopes. |
| `cancel_login` | `POST /api/identity/pending/{id}/cancel` | id | — | The person cancels a waiting login page: the app gets error=access_denied. |
| `list_users` | `GET /api/identity/{provider}/users` | — | provider | Synthetic people of a provider: tenant, groups, roles, MFA, state, consents. |
| `set_user` | `PUT /api/identity/{provider}/users/{user}` | user | name, email, tenant, guest, groups, roles, mfa, phone_lost, enabled, extra, provider | Creates or changes a user; the next tokens carry the claims. groups may be a number (210 makes synthetic groups: the overage path). |
| `remove_user` | `DELETE /api/identity/{provider}/users/{user}` | user | provider | Removes a user from the provider (their refresh tokens stop). |
| `revoke_consent` | `DELETE /api/identity/{provider}/users/{user}/consents/{app}` | user, app | provider | The user's consent to an app is revoked: the next sign-in asks again; refresh tokens stop. |
| `set_user_state` | `PUT /api/identity/{provider}/users/{user}/state` | user, enabled | provider | Disables or enables a user: sign-ins stop at the login page (AADSTS50057); refresh tokens are refused. |
| `list_apps` | `GET /api/identity/{provider}/apps` | — | provider | App registrations: client id, type, redirect URIs, credentials, permissions and the tokens each gets. |
| `set_app` | `PUT /api/identity/{provider}/apps/{app}` | app | name, type, redirect_uris, secret, any_secret, permissions, optional_claims, exposes, app_id_uri, multi_tenant, provider | Registers or changes an app: type (web, spa, daemon, api), redirect_uris (exact match), secret (secret://name), any_secret, permissions, optional_claims, multi_tenant. |
| `remove_app` | `DELETE /api/identity/{provider}/apps/{app}` | app | provider | Removes an app registration: its sign-ins and token requests are refused. |
| `grant_admin_consent` | `POST /api/identity/{provider}/apps/{app}/admin-consent` | app | scopes, provider | Grants scopes for everyone, as an administrator would (the login page stops asking). |
| `get_tenant` | `GET /api/identity/{provider}/tenant` | — | provider | Issuer, discovery, flavour, signing keys (current and next), lifetimes, clock skew and the provider's provocations. |
| `set_tenant` | `PUT /api/identity/{provider}/tenant` | — | flavour, id_lifetime, access_lifetime, refresh_lifetime, clock_skew, other_tenants, provider | Changes flavour (entra-v2, google, okta, oidc), lifetimes (id, access, refresh), clock_skew and other tenants. |
| `rotate_signing_key` | `POST /api/identity/{provider}/keys/rotate` | — | provider | The next key becomes current and a new next one is published: apps that cached the JWKS at start-up reject new tokens. |
| `provoke_idp` | `POST /api/identity/{provider}/provoke-idp` | part, answer | for, provider | The provider itself fails: discovery (500, slow, wrong_issuer), jwks (unavailable, kid_missing, wrong_key) or token (invalid_grant, 500, slow); normal stops it. |
| `list_oauth_clients` | `GET /api/oauth-clients` | — | — | Simulated clients (Stuntman as the client of the app's OAuth server) and their last run. |
| `run_client_checks` | `POST /api/oauth-clients/{client}/runs` | client | resource_url | Runs the ten MCP authorization checks against an MCP server; pauses at the app's own sign-in page (open authorizeUrl, or sign in through the app's IdP) and continues when the code comes back. |
| `get_run` | `GET /api/oauth-clients/runs/{run_id}` | run_id | — | One run: each check with what Stuntman sent and got; authorizeUrl while it waits for the sign-in. |
| `serve_client_metadata` | `PUT /api/oauth-clients/{client}` | client | name, kind, resource_url, client_name, client_id, client_secret | Creates or changes a simulated client: kind (dcr, metadata_url, preregistered), the MCP server it checks, the name in its metadata document; client_id/secret when pre-registered. |

## Secrets (Key Vault)

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_vaults` | `GET /api/keyvault/vaults` | — | — | Vaults in this environment: name, URL, counts of secrets, keys and certificates. |
| `list_secrets` | `GET /api/keyvault/secrets` | — | vault | Secrets of a vault: names, current version, state, expiry and reads in the last hour. |
| `secret_detail` | `GET /api/keyvault/secrets/{name}/detail` | name | vault | One secret without its value: versions with reads, who reads it, provocation, events, and the files that reference secret://<name>. |
| `get_secret` | `GET /api/keyvault/secrets/{name}` | name | version, vault, by | The value (synthetic) and attributes of a secret version; recorded in the access log. |
| `set_secret` | `PUT /api/keyvault/secrets/{name}` | name, value | expires, content_type, tags, vault, by | A new version of a secret (creates it); SecretNewVersionCreated goes to subscribers. Values are synthetic: never paste a real one. |
| `set_secret_state` | `PATCH /api/keyvault/secrets/{name}` | name | enabled, expires, version, vault | Disables, enables or expires a secret version (default the current one). |
| `provoke_secret` | `POST /api/keyvault/secrets/{name}/provoke` | name, answer | for, vault, by | Makes the next reads of a secret fail: 403, 404, disabled, 429 or slow (3 s), for a while or until changed; normal stops it. |
| `secret_reads` | `GET /api/keyvault/secrets/{name}/reads` | name | since, vault | Who read a secret, how often, and whether it looks cached (per process). |
| `secret_used_by` | `GET /api/keyvault/used-by/{name}` | name | — | Files in this environment that point at secret://<name> (also before the secret exists). |
| `delete_secret` | `DELETE /api/keyvault/secrets/{name}` | name | vault | Soft-deletes a secret (recreating the name gets 409 until recovered or purged). |
| `recover_item` | `POST /api/keyvault/recover/{name}` | name | kind, vault | Recovers a soft-deleted secret, key or certificate. |
| `list_keys` | `GET /api/keyvault/keys` | — | vault | Keys of a vault: type, allowed operations, versions, operations in the last hour. |
| `key_detail` | `GET /api/keyvault/keys/{name}/detail` | name | vault | One key: operations in the last hour (with p95), versions, who uses it, provocation, public key. |
| `create_vault_key` | `POST /api/keyvault/keys/{name}` | name | type, size, ops, expires, tags, vault | Creates a key, or a new version of it (rotation; KeyNewVersionCreated to subscribers). RSA 2048/3072/4096. |
| `set_key_state` | `PATCH /api/keyvault/keys/{name}` | name | enabled, expires, vault | Disables or enables a key's current version. |
| `provoke_key` | `POST /api/keyvault/keys/{name}/provoke` | name, answer | for, vault, by | Makes the next operations on a key fail: disabled, 403, 429, slow (2 s) or wrong_signature (sign returns one that fails verify). |
| `key_public` | `GET /api/keyvault/keys/{name}/public` | name | format, vault | The public part of a key, as JWK or PEM, to verify outside. |
| `list_certificates` | `GET /api/keyvault/certificates` | — | vault | Certificates: subject, issuer, validity, days left, readers. |
| `certificate_detail` | `GET /api/keyvault/certificates/{name}/detail` | name | vault | One certificate: subject, issuer, thumbprint, validity, how to get it, readers, provocation. |
| `issue_certificate` | `POST /api/keyvault/certificates/{name}` | name | subject, days, san, vault | Issues a certificate from the Stuntman Test CA (RSA 2048 key, exportable). |
| `import_certificate` | `POST /api/keyvault/certificates/{name}/import` | name, pfx_base64 | password, vault | Imports your own test certificate (.pfx in base64 with its password, or a PEM with key and certificate). |
| `renew_certificate` | `POST /api/keyvault/certificates/{name}/renew` | name | vault | A new version signed by the Test CA (same subject and validity); CertificateNewVersionCreated. |
| `provoke_certificate` | `POST /api/keyvault/certificates/{name}/provoke` | name, answer | for, vault, by | Next reads of a certificate: missing (404), expired, not_yet_valid, wrong_password (.pfx) or untrusted (issuer); normal stops it. |
| `get_vault` | `GET /api/keyvault/vaults/{name}` | — | name | The vault block: identities with roles and scopes, managed identity endpoint, throttling, latency, soft delete, subscribers. |
| `set_vault` | `PUT /api/keyvault/vaults/{name}` | name | any_token, throttle, latency, soft_delete_days, subscribers, identities, seed_on_reset | Creates or changes a vault: any_token, throttle (2000/10s), latency, soft_delete_days, subscribers, seed_on_reset. A new vault imports this environment's secret:// values. |
| `set_vault_identity` | `PUT /api/keyvault/identities/{id}` | id | roles, scope, vault | Adds or changes an identity: roles (administrator, secrets-user, secrets-officer, crypto-user, crypto-officer, certificate-user, certificates-officer) and name scopes. No roles removes it. |
| `provoke_imds` | `POST /api/keyvault/imds/provoke` | answer | for, by | The managed identity endpoint answers 500, slow (5 s) or no_identity; normal stops it. |
| `would_fail` | `GET /api/keyvault/would-fail` | — | since, vault | Operations allowed only because any token is on: what a real vault would have refused. |
| `vault_log` | `GET /api/keyvault/log` | — | identity, operation, item, result, since, limit, vault | Every operation on vault items, newest first: identity, operation, item, answer (kept 24 h). |
| `vault_log_entry` | `GET /api/keyvault/log/{id}` | id | — | One operation: caller claims, request, answer, trace id. |
| `vault_patterns` | `GET /api/keyvault/patterns` | — | since, vault | Findings in the access log: values read on every request, refusals, unknown callers, operations allowed only by any token. |

## Webhook inboxes

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_webhook_inboxes` | `GET /api/webhooks/inboxes` | — | — | Kept and ephemeral webhook inboxes, with how each answers and its URL. |
| `get_webhook_inbox` | `GET /api/webhooks/inboxes/{name}` | name | — | Behaviour, signature settings, lifetime, URL, and the attempts of its last event. |
| `create_webhook_inbox` | `POST /api/webhooks/inboxes` | — | name, expires, keep, mode, status, fail, then, traps, secret | An inbox the app can send webhooks to; ephemeral by default (expires="1h"), keep=true keeps it with the environment. Behaviour keys as in set_inbox; fail=3, then=200 is a shortcut. |
| `set_inbox` | `PUT /api/webhooks/inboxes/{name}` | name | mode, status, times, per, share, then, delay, hang, traps, redirect_to, secret, previous_secret, scheme, header, tolerance, reject_bad, expires, new_name, inbox | How the inbox answers: mode, status, times, per, share, then, delay, hang, traps, secret, previous_secret, scheme, header, tolerance, reject_bad, expires; or a whole inbox object. |
| `remove_inbox` | `DELETE /api/webhooks/inboxes/{name}` | name | — | Removes the inbox; later deliveries get 410 Gone. Its history is kept for 24 h. |
| `list_deliveries` | `GET /api/webhooks/deliveries` | — | inbox, event, since, limit | Event, attempt, signature result, answer and trap results, newest first. |
| `get_delivery` | `GET /api/webhooks/deliveries/{id}` | id | — | Headers, body, checks, and all attempts of the same event. |
| `wait_for_delivery` | `GET /api/webhooks/deliveries/wait` | inbox | event, timeout, since | Blocks until the inbox has answered a delivery (of this event type), then returns it with all its attempts. Looks back 15 s by default. |
| `put_webhooks_yaml` | `PUT /api/webhooks/inboxes.yaml` | yaml | — | Replaces webhooks/inboxes.yaml (the kept inboxes). |

## Telemetry (OpenTelemetry)

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `telemetry_overview` | `GET /api/telemetry/overview` | — | since | What the receiver got: services, counts, last data, dropped and refused batches, the provoked answer, endpoints. |
| `list_traces` | `GET /api/telemetry/traces` | — | service, status, slower_than, touching_provider, since, route, limit | Traces with duration, span count, status and how many spans reached Stuntman, newest first. |
| `get_trace` | `GET /api/telemetry/traces/{trace_id}` | trace_id | — | The span tree; provider spans carry the trip, what answered it, and a link. |
| `wait_for_trace` | `GET /api/telemetry/traces/wait` | — | route, span_name, status, timeout, since | The first matching trace: what an agent waits for after acting in the app. Looks back 15 s by default. |
| `trace_of` | `GET /api/telemetry/trips/{trip_id}/trace` | — | trip_id, message_id | From a trip, message or delivery back to the app trace that caused it. |
| `find_slow_spans` | `GET /api/telemetry/traces/{trace_id}/slow` | trace_id | over_ms | Where the time went: spans over the threshold, provider spans first. |
| `list_logs` | `GET /api/telemetry/logs` | — | service, level, text, trace_id, since, limit, before_seq | Newest first, with level, service, message, trace id and attributes; facets give counts per level and service. |
| `get_log` | `GET /api/telemetry/logs/{log_id}` | log_id | — | One log line. |
| `wait_for_log` | `GET /api/telemetry/logs/wait` | — | text, level, service, timeout, since | The first matching line: what an agent waits for after acting in the app. Looks back 15 s by default. |
| `logs_of_trace` | `GET /api/telemetry/traces/{trace_id}/logs` | trace_id | — | Every line the app wrote while serving one trace, in order. |
| `lines_around` | `GET /api/telemetry/logs/{log_id}/around` | log_id | before, after | The lines just before and after one line, across services. |
| `count_logs` | `GET /api/telemetry/logs/count` | — | level, service, text, since | Quick check that an error stopped happening after a fix. |
| `get_telemetry_settings` | `GET /api/telemetry/settings` | — | — | Endpoints, signals with keep, collector answer, forwarding, storage used. |
| `put_telemetry_settings` | `PUT /api/telemetry/settings` | — | settings, yaml | Replaces the settings (Save on T3): a settings object, or yaml with the whole file. |
| `set_signal` | `PUT /api/telemetry/signals/{signal}` | signal | on, keep | Turn traces, logs or metrics on or off; off means answered 200 and dropped. |
| `provoke_collector` | `POST /api/telemetry/collector/provoke` | answer | only_for, for | 403, 401, 429, 503, refused, slow or drop_half; ends by itself. |
| `stop_provoking` | `DELETE /api/telemetry/collector/provoke` | — | — | The collector answers normally again; the strip goes away. |
| `set_forwarding` | `PUT /api/telemetry/forwarding` | — | on, endpoint, header_secret, header | Copy everything to a real APM; the key is a Secrets reference, never a value. |
| `clear_telemetry` | `DELETE /api/telemetry/data` | confirm | — | Deletes traces, logs and metrics of the environment; settings stay. |
| `list_metrics` | `GET /api/telemetry/metrics` | — | service, text | Name, type, unit, category, services and number of series. |
| `query_metric` | `GET /api/telemetry/metrics/{name}` | name | stat, by, since, step | One value per step, per breakdown value. stat: count, rate, sum, avg, p50, p95, p99, last, errors. |
| `compare_windows` | `GET /api/telemetry/metrics/{name}/compare` | name, before, after | stat, by | The two values and the change, to say whether a fix or a rule made a difference. Windows: 30m-15m (ago) or epoch-ms..epoch-ms. |
| `metric_exemplars` | `GET /api/telemetry/metrics/{name}/exemplars` | name | at, by, by_value, step | The trace ids the app attached to points near a time; open them with get_trace. |
| `top_series` | `GET /api/telemetry/metrics/{name}/series` | name, attribute | since | How many values an attribute has: finds the attribute that will cost money in a real APM. |
| `why_it_jumped` | `GET /api/telemetry/metrics/{name}/why` | name | stat, by, since, step | Finds steps where a metric jumped (over twice its median) and says which breakdown value drove it, its errors, and what Stuntman answered then. |
