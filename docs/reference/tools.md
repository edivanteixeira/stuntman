# Tool reference

Every action in Stuntman is one tool. Each tool is an **MCP tool** (same name) and an **HTTP route** under `/api`.
`GET /api/tools` returns this catalog from a running server, with full parameter schemas.

_Generated from the engine source by `tools/gen_tool_reference.py`. Do not edit by hand._

**389 tools** in 17 areas.

- [Environments and timeline](#environments-and-timeline) · 13 tools
- [Email and mailboxes](#email-and-mailboxes) · 39 tools
- [SMS](#sms) · 17 tools
- [WhatsApp](#whatsapp) · 28 tools
- [HTTP APIs (plugins)](#http-apis-plugins) · 68 tools
- [AI models](#ai-models) · 9 tools
- [Product events](#product-events) · 8 tools
- [Storage (Azure Blob, S3)](#storage-azure-blob-s3) · 45 tools
- [Messaging (Service Bus, RabbitMQ, ActiveMQ, Event Hubs)](#messaging-service-bus-rabbitmq-activemq-event-hubs) · 31 tools
- [Identity (OIDC, OAuth2, SAML, LDAP, Bot Framework)](#identity-oidc-oauth2-saml-ldap-bot-framework) · 28 tools
- [Files (FTP, SFTP)](#files-ftp-sftp) · 8 tools
- [Streams (Kafka, Schema Registry)](#streams-kafka-schema-registry) · 10 tools
- [Pub/Sub (Google Cloud)](#pub-sub-google-cloud) · 14 tools
- [Kinesis (Amazon Kinesis Data Streams)](#kinesis-amazon-kinesis-data-streams) · 6 tools
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
| `usage` | `GET /api/usage` | — | — | Which services this environment's apps use: per service kind (email, whatsapp, sms, storage, queues, streams, pubsub, kinesis, files, keyvault, identity, apis, ai, events, webhooks, telemetry) the calls today and in the last 7 days, the last call, and live (a call in the last 5 minutes). Your own tool calls do not count. |

## Email and mailboxes

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
| `list_mailboxes` | `GET /api/mailboxes` | — | — | The mailboxes the app reads over IMAP or POP3, with their messages and unread counts, and how to connect. |
| `set_mailbox` | `PUT /api/mailboxes/{address}` | address | password, provoke | Creates or changes a mailbox the app reads: its address (the login), a password (empty: any but "wrong") and a provocation: login_refused, busy, drop_fetch, tls_required, token_expired (none clears). |
| `put_mail` | `POST /api/mailboxes/{address}/messages` | address | from, subject, text, html, attachments, raw, folder | Puts a message in a mailbox as if someone sent it: from, subject, text (and html), attachments [{filename, content_type, base64 or text}] — or raw (a whole RFC 822 message). The app finds it on its next poll. |
| `list_mailbox_messages` | `GET /api/mailboxes/{address}/messages` | address | folder | The messages in a mailbox (a folder): sender, subject, flags (read, deleted), size; and what the app did on it (IMAP/POP3 commands, without passwords). |

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
| `set_answer` | `PUT /api/plugins/{service}/answers/{name}` | service, name | status, body, headers, raw, stream, stream_format, template, latency, rpc_error, sentence, note, old_name, fault | Creates or replaces a named answer (status, body template, headers, sentence); checked against the schema unless raw=true (malformed on purpose); fault={code, reason} makes it a SOAP fault; stream=[{after, event, data}] sends it in parts (stream_format sse \| ndjson \| chunks); template=handlebars renders WireMock templates; latency waits before answering. |
| `delete_answer` | `DELETE /api/plugins/{service}/answers/{name}` | service, name | — | Deletes a named answer that no route uses. |
| `get_data` | `GET /api/plugins/{service}/data` | service | table | A data table the answers read (irregular_cpfs, people, selfies…), or every table. |
| `set_data` | `PUT /api/plugins/{service}/data/{table}` | service, table, rows | add | Replaces (or with add=true appends to) a data table as the plugin declares it. |
| `synthetic_cpf` | `GET /api/apis/synthetic-cpf` | — | — | A CPF that passes the check digits and is not a real person's (for data tables and tests). |
| `get_api_service` | `GET /api/plugins/{service}/service` | service | — | A service block: base URL and real host, auth (OAuth2 client credentials, Basic), billing, limits; plus Stuntman's URLs. |
| `set_service` | `PUT /api/plugins/{service}/service` | service | any_client, token_ttl, client_certificate, client_certificate_icp, session_ttl, billing, per_call, warn_above, latency, rate, fail_share, base_url, real_host, title, version, webhook_signing, webhook_secret, webhook_header, webhook_tolerance, webhook_provoke, idempotency, xml_signature_check, xml_signature_sign, xml_signature_invalid, xml_signature_sha256 | Changes the service block: any_client, token_ttl, billing (enabled, per_call, warn_above), latency, rate, fail_share, base_url, real_host, title; webhook signing (webhook_signing, webhook_header, webhook_secret, webhook_tolerance, webhook_provoke), idempotency, xml_signature_* and client_certificate_icp. |
| `clock` | `GET /api/clock` | — | — | The environment's clock (real time plus what advance_clock moved) and the webhooks waiting for their time. |
| `advance_clock` | `POST /api/clock/advance` | — | by, reset | Moves the environment's clock forward (1h, 1d, 30d): sends that wait (delay) and now() in templates follow it. reset=true goes back to real time. |
| `set_mcp_tool` | `PUT /api/plugins/{service}/mcp-tools` | service, name | description, input_schema, decide | Adds or changes a tool of a service's MCP server (the service gets an mcp: block on /mcp if it has none). decide: [{when, answer}] like a route; answers are the tool results (status >= 400 gives isError). |
| `get_test_ca` | `GET /api/apis/test-ca` | — | — | The Stuntman Test CA: download URLs (PEM, DER .crt, PKCS#12 truststore/PFX without a key), its file on disk, and how to trust it per runtime and OS (Linux, macOS, Windows; .NET, Node, Python, Java, Go, curl). Prefer the per-process options; ask the developer before adding it to an OS trust store. |
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
| `import_wsdl` | `POST /api/plugins/wsdl` | service | source, wsdl, title, real_host | A SOAP service from the provider's WSDL (a URL, a file path, or its text): operations with their SOAPActions, types for checking bodies, a default answer per operation and the faults as named answers. The WSDL is served back on ?wsdl. |
| `reimport_wsdl` | `POST /api/plugins/{service}/wsdl` | service | source, wsdl | Reads the WSDL again (or a new one): new operations start with their default answer; existing ones keep their steps; answers, data, security and service settings stay. Lists what was added and removed. |
| `list_operations` | `GET /api/plugins/{service}/operations` | service | — | A SOAP service's operations: SOAPAction, request and response elements, the answers each can give; plus the version, endpoint and the URL of the served WSDL. |
| `set_operation` | `PUT /api/plugins/{service}/operations` | service, operation | steps | Replaces an operation's steps (receive, let, keep, wait, decide, respond, send, fail, billable), like set_route for SOAP; its SOAPAction and types stay as the WSDL says. Validated before saving. |
| `try_operation` | `POST /api/plugins/{service}/operations/try` | service | envelope, operation, body, action | Runs a SOAP service on an envelope (or an operation and its body XML): the operation it picks, the XSD and WS-Security checks, the steps and the envelope it answers. Nothing is kept, counted, billed or sent. |
| `send_callback` | `POST /api/plugins/{service}/callback` | service, url, operation, body | version | Calls the app's endpoint as the provider would: an envelope with this operation's SOAPAction and body (the payload XML). Returns the app's status and answer. |
| `import_spec` | `POST /api/plugins/spec` | service | source, spec, title, real_host, validate | A service from the provider's spec: OpenAPI 3.x or Swagger 2.0 (JSON or YAML), or a WSDL (then it is a SOAP service, as import_wsdl). Routes with their success answer (the spec's example or one built from the schema), the other responses as named answers, bodies checked against the schemas, OAuth client credentials with a token route. The spec is served back. |
| `reimport_spec` | `POST /api/plugins/{service}/spec` | service | source, spec | Reads the spec again (or a new one): new routes start with their success answer; existing ones keep their steps; answers, data and service settings stay. Lists what was added and removed. Works for WSDLs too. |
| `start_recording` | `POST /api/plugins/{service}/recording` | service, upstream | mode, stop_after, max, mask, title | Records the real provider: each call to the service goes to upstream (its sandbox) and back; the app gets the real answer and Stuntman keeps the pair with secrets, card numbers, CPFs and e-mails masked. mode=unknown on an existing service: its routes answer and only unknown calls go. Creates the service if it does not exist. Use a sandbox key, never a production one. |
| `recording_status` | `GET /api/plugins/{service}/recording` | service | review | A recording: recording \| paused \| time is up \| limit reached \| stopped, calls kept, time left; once it stops (or review=true), the routes and answers it found and the rules it suggests. |
| `pause_recording` | `POST /api/plugins/{service}/recording/pause` | service | resume | Pauses a recording (calls get 404 “no route”, or the routes' answers) or, with resume=true, goes on. |
| `stop_recording` | `DELETE /api/plugins/{service}/recording` | service | — | Stops a recording and returns the review: calls grouped into routes, the answers each gave, ids and dates made templates, and the rules that tell the answers apart. make_plugin writes it. |
| `make_plugin` | `POST /api/plugins/{service}/recording/plugin` | service | routes, rules | Writes the plugin from a recording: the routes it found (routes= only these, “METHOD /path”), their answers (binary ones as files) and the suggested rules (rules=false: each route gives its usual answer). Routes the service has stay as they are. The recording ends; the provider is no longer needed. |
| `import_mocks` | `POST /api/plugins/import` | source | kind, group_by, name, dry, replace | Services from mocks you already have. kind=wiremock: a folder with mappings/*.json (and __files/) becomes plugins: request patterns → routes (regex paths kept), matchers → rules, responses → answers, fixedDelay → latency, faults → reset/empty, scenarios → set_scenario. kind=wsdl: every .wsdl under the folder becomes a SOAP service. dry=true only shows what it would make. |
| `set_scenario` | `PUT /api/plugins/{service}/scenario` | service | scenario | Moves a service to a scenario (imported WireMock scenarios: rules with `scenario == 'x'`). Started is the normal one. |
| `new_test_certificate` | `POST /api/apis/test-certificates` | holder | cnpj, cpf, valid_days, expired, not_yet_valid, untrusted, password | An ICP-Brasil style A1 test certificate: an e-CNPJ (cnpj) or e-CPF (cpf) with an RSA 2048 key, signed by the Stuntman Test CA (or an unknown CA with untrusted=true), valid for valid_days or already expired. Returns the .pfx (base64) and its password, the certificate and key PEM. For mTLS to services with client_certificate_icp and for signing XML. |
| `list_test_certificates` | `GET /api/apis/test-certificates` | — | — | The test certificates made in this environment (holder, CNPJ or CPF, validity, state). Keys are not shown. |
| `sign_xml` | `POST /api/apis/xml/sign` | xml, id, certificate | sha256, exclusive, place, prefix | Signs an XML document's element (by Id) with a test certificate, as an app would sign an NF-e or CT-e: enveloped ds:Signature after the element (or inside it for SAML-like documents), RSA-SHA1 by default. |
| `verify_xml_signature` | `POST /api/apis/xml/verify` | xml | — | Checks every ds:Signature of an XML document: digests and signature value, with the certificate in KeyInfo. Says what was signed and by which CNPJ. |

## AI models

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_ai_calls` | `GET /api/ai/calls` | — | provider, since, limit | The calls the app made to the simulated models, newest first: provider, API, model, tokens in and out, cost, why (the rule, the default, a forced failure). |
| `get_ai_call` | `GET /api/ai/calls/{id}` | id | — | One model call with the whole request and response. |
| `wait_for_ai_call` | `GET /api/ai/calls/wait` | — | provider, contains, timeout, within | Waits until the app calls a model (optionally a provider, and a prompt containing a text). Returns the call. |
| `list_ai_rules` | `GET /api/ai/rules` | — | — | The rules that shape the simulated answers (first match wins), the default answer and the prices per million tokens. |
| `set_ai_rule` | `PUT /api/ai/rules` | name | when, contains, provider, model, answer, text, tool_call, json, tokens, stop, position | Adds or changes a rule (by name). when: provider, api, model (prefix*), contains (the last user message), system_contains, tool (offered). answer: text, tool_call {name, arguments} (the model calls the tool first; text comes after the tool's result), json, tokens, stop=length. position: where in the list (0 = first). |
| `delete_ai_rule` | `DELETE /api/ai/rules/{name}` | name | — | Removes a rule. |
| `force_ai_failure` | `POST /api/ai/force` | kind | provider, count | The next calls (count) to a provider (or any) fail on purpose: 429 (rate limit), 500, 529 (overloaded), filter (content filtered), length (cut by max_tokens), bad_json (broken tool arguments), slow (40 ms per token), cut (the stream stops halfway). kind=none clears. |
| `ai_spend` | `GET /api/ai/spend` | — | since | Calls, tokens in/out, cached tokens, cost and failures per provider since a time (default: today). |
| `set_ai_price` | `PUT /api/ai/prices` | — | model, in, out, cached, default_text | The price of a model (or a pattern with *) per million tokens, for the spend counter. default_text changes the answer when no rule matches. |

## Product events

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

## Messaging (Service Bus, RabbitMQ, ActiveMQ, Event Hubs)

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
| `list_activemq_destinations` | `GET /api/activemq/destinations` | — | — | ActiveMQ (OpenWire, port 61616) queues and topics with their messages per state (scheduled, pending, delivered, acked, dead_letter, expired, rolled_back, discarded), the consumers connected now (with their selector and durable subscription), each topic's durable subscriptions (online or offline, with what they keep), the open and recent transactions (local and XA, prepared ones included), the connections logged in now with their negotiated wire format (loose or tight, with or without the marshal cache), the broker URL and what set_activemq changed. |
| `list_activemq_messages` | `GET /api/activemq/messages` | — | destination, state, last | ActiveMQ messages, newest first, with their state: scheduled (AMQ_SCHEDULED_DELAY not over), pending (waiting), delivered (dispatched, not acked yet), acked, dead_letter (poison ack), expired (JMSExpiration passed), rolled_back (sent in a transaction that rolled back), discarded (topic copy whose subscriber left). A message still inside an open transaction carries its id; a repeating or cron one its schedule; one moved to the dead-letter queue (expired, or poisoned past max_redeliveries) points at its copy there (movedTo), and the copy names its originalDestination. |
| `activemq_send` | `POST /api/activemq/messages` | destination | text, kind, blob_url, mime_type, body_base64, properties, correlation_id, type, reply_to, group_id, persistent, priority, count, delay, ttl, period, repeat, cron | Puts a message on an ActiveMQ queue (or topic) as the partner system would; it is dispatched to the app's consumers. Recorded as sent by you or the agent. |
| `set_activemq` | `PUT /api/activemq/settings` | — | users, refuse_login, drop_after, slow_dispatch, redeliver, fail_commits, max_redeliveries, individual_dlq, advisory_delivered, advisory_consumed, reset | ActiveMQ broker behaviour: users (name → password; empty: anyone logs in), refuse_login (every login fails with a SecurityException), drop_after (close the connection after N dispatched messages), slow_dispatch (wait before each dispatch), redeliver (the next N acks are lost: those messages come again with a higher redelivery count), fail_commits (the next N transaction commits fail with a TransactionRolledBackException, or an XAException for XA, and roll back), max_redeliveries (past it a message goes to the dead-letter queue; default 6), individual_dlq (one DLQ per destination instead of ActiveMQ.DLQ), advisory_delivered / advisory_consumed (the MessageDelivered / MessageConsumed advisory topics, off by default as in ActiveMQ). Only what you pass changes; reset=true goes back to defaults. |
| `cancel_activemq_scheduled` | `POST /api/activemq/scheduled/cancel` | — | seq, destination | Cancels scheduled ActiveMQ messages (AMQ_SCHEDULED_DELAY, _PERIOD/_REPEAT, _CRON) before they go out: one (seq) or all of a destination. They are kept as discarded. |
| `wait_for_activemq_ack` | `GET /api/activemq/wait-ack` | destination | seq, count, timeout | Waits until the app acked: one message (seq), at least `count` messages of the destination, or (neither) everything sent to it: nothing pending or delivered and at least one acked. 408 after the timeout. |
| `list_eventhubs` | `GET /api/eventhubs` | — | — | Azure Event Hubs namespaces (AMQP port, the SDKs in emulator mode): each event hub with its partitions (begin and last sequence number, last offset, last enqueued time, how many events it keeps), its consumer groups with the receivers reading each partition now (owner level/epoch, position) and the checkpoints the app wrote with BlobCheckpointStore, retention, the connection string (EntityPath per hub), the Kafka endpoint and what set_eventhubs provokes. |
| `create_eventhub` | `POST /api/eventhubs/{namespace}/hubs` | namespace, hub | partitions, consumer_groups, retention | Creates an event hub (and its namespace when new) with partitions (default 4, can only grow), consumer groups ($Default is always there) and retention (default 24h, on the environment clock). Again on an existing hub: adds partitions and consumer groups, changes retention. |
| `list_eventhub_events` | `GET /api/eventhubs/{namespace}/{hub}/events` | namespace, hub | partition, last | Events of an event hub, newest first, with partition, sequence number, offset, enqueued time (environment clock), partition key, properties, body and who sent it (the app over AMQP or Kafka, you, an agent). |
| `eventhub_send` | `POST /api/eventhubs/{namespace}/{hub}/events` | namespace, hub | body, properties, partition_key, partition, events | Sends events to an event hub as another producer would; the app's receivers get them. One event (body) or a batch (events: [{body, properties, partition_key}]). partition_key is hashed to a partition as the service does; partition picks one. |
| `set_eventhubs` | `PUT /api/eventhubs/{namespace}/settings` | namespace | server_busy, quota_exceeded, slow_sends, not_found, steal_owner, drop_connections, any_key, reset | Event Hubs provocations for a namespace: server_busy (the next N sends get ServerBusy, which the SDKs retry), quota_exceeded (the next N sends get QuotaExceeded), slow_sends (wait before each send is accepted), not_found (hubs that answer 'entity not found' while listed), steal_owner (the receivers of a hub, or all with true, are disconnected as by a higher epoch: OwnershipLost / link stolen), drop_connections (closes every AMQP connection of the namespace now; the SDKs reconnect), any_key. Only what you pass changes; reset=true goes back to normal. |
| `wait_for_eventhub_event` | `GET /api/eventhubs/{namespace}/{hub}/wait` | namespace, hub | where, text, partition, since, timeout | Waits for the first event the app sent (over AMQP or Kafka, not you or an agent) to an event hub that matches: properties or JSON body fields in `where`, text in the body, a partition. 408 after the timeout. |

## Identity (OIDC, OAuth2, SAML, LDAP, Bot Framework)

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_providers` | `GET /api/identity/providers` | — | — | Identity providers in this environment: flavour, tenant, Authority, users, apps and sign-ins in the last hour. |
| `set_provider` | `PUT /api/identity/providers/{name}` | name | tenant, flavour, other_tenants, saml, saml_sign_response, saml_sign_assertion, saml_name_id, ldap, ldap_base_dn, ldap_provoke, password_grant | Creates an identity provider (or changes its flavour/tenant): entra-v2, google, okta or oidc. Users and apps are added with set_user and set_app. |
| `remove_provider` | `DELETE /api/identity/providers/{name}` | name | — | Removes an identity provider: its file (users, apps, tenant) and its sign-ins, keys and provocations. Apps pointed at its Authority get 404. |
| `list_sign_ins` | `GET /api/identity/sign-ins` | — | provider, user, result, since, limit | Sign-ins and tokens issued: the sentence, the result or error code (AADSTS…), newest first. |
| `get_sign_in` | `GET /api/identity/sign-ins/{id}` | id | — | One sign-in: steps, tokens issued (decoded) and the claims. |
| `sign_in_as` | `POST /api/identity/{provider}/sign-in-as` | user | app, mfa_code, consent, claims, provider | Completes the login page waiting for this app (or starts one) as a user, through MFA and consent; returns the app's callback to follow. |
| `provoke_sign_in` | `POST /api/identity/{provider}/provoke` | answer | count, for, provider | The next sign-ins: mfa_required, mfa_fails, consent_required, user_cancels, wrong_tenant, expired (tokens already expired), bad_signature, idp_down; normal stops it. |
| `pending_logins` | `GET /api/identity/pending` | — | provider | Login pages waiting for a person, with the app and the scopes. |
| `cancel_login` | `POST /api/identity/pending/{id}/cancel` | id | — | The person cancels a waiting login page: the app gets error=access_denied. |
| `list_users` | `GET /api/identity/{provider}/users` | — | provider | Synthetic people of a provider: tenant, groups, roles, MFA, state, consents. |
| `set_user` | `PUT /api/identity/{provider}/users/{user}` | user | name, email, tenant, guest, groups, roles, mfa, phone_lost, enabled, password, locked, extra, provider | Creates or changes a user; the next tokens carry the claims. groups may be a number (210 makes synthetic groups: the overage path). |
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
| `send_bot_activity` | `POST /api/identity/bot/activities` | endpoint, bot_app_id | text, value, activity, conversation, from, from_name, tenant_id, service_url, twist | Sends the app's bot an activity as Microsoft Teams would: a message (text) or a card action (value), signed by the Bot Connector (JWT with aud = the bot's app id and the serviceurl claim). twist: bad_signature \| expired \| wrong_audience \| no_token. Returns the bot's answer; its replies land in list_bot_messages. |
| `list_bot_messages` | `GET /api/identity/bot/messages` | — | conversation | What went between Teams (Stuntman) and the app's bot: activities sent to the bot and the bot's replies and cards to the connector. |

## Files (FTP, SFTP)

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_file_servers` | `GET /api/files/servers` | — | — | The FTP/FTPS/SFTP servers of this environment: users, provocation, how to connect, file counts. |
| `set_file_server` | `PUT /api/files/servers/{name}` | name | title, users, folders, provoke | Creates or changes a file server: users [{user, password, read_only}], folders to make (/in, /out), and a provocation for the next sessions: login_refused, disk_full, permission_denied, drop_mid, slow, host_key_changed (none clears). |
| `list_files` | `GET /api/files/servers/{server}/files` | server | path | The files and folders on a server (or under a path). |
| `get_file` | `GET /api/files/servers/{server}/file` | server, path | — | A file's content (text, or base64 for binary). |
| `put_file` | `POST /api/files/servers/{server}/put` | server, path | text, base64 | Puts a file on a server as if the partner dropped it (text, or base64 for binary). The app finds it on its next poll. |
| `delete_file` | `DELETE /api/files/servers/{server}/file` | server, path | — | Deletes a file from a server. |
| `list_transfers` | `GET /api/files/transfers` | — | server, limit | Logins and transfers on the file servers, newest first. |
| `wait_for_file` | `GET /api/files/servers/{server}/wait` | server, path | timeout | Waits until a file whose path contains a text (or matches a * pattern) is on a server: what the app uploaded. |

## Streams (Kafka, Schema Registry)

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_topics` | `GET /api/streams/topics` | — | — | The Kafka topics of this environment with their partitions, messages and high watermarks; how to connect. |
| `create_topic` | `PUT /api/streams/topics/{name}` | name | partitions | Creates a topic with N partitions. |
| `delete_topic` | `DELETE /api/streams/topics/{name}` | name | — | Deletes a topic and its messages. |
| `list_stream_messages` | `GET /api/streams/topics/{topic}/messages` | topic | partition, limit | The newest messages of a topic: partition, offset, key, headers and the value decoded (Avro with its registry schema, JSON or text). |
| `produce` | `POST /api/streams/topics/{topic}/produce` | topic | key, value, headers, partition, subject, schema_id, bad_schema | Produces a message as the partner would: key, value (JSON or text), headers, partition; with subject (or schema_id) the value is Avro-encoded with that registry schema in Confluent framing. bad_schema=true sends one the schema rejects (an unknown schema id). |
| `list_consumer_groups` | `GET /api/streams/groups` | — | — | Consumer groups: state, generation, members (client, host, assigned partitions) and the lag of each partition (committed offset vs high watermark). |
| `rebalance` | `POST /api/streams/groups/{group}/rebalance` | group | — | Starts a rebalance of a consumer group: members get REBALANCE_IN_PROGRESS on their next heartbeat and must join again. |
| `set_streams` | `PUT /api/streams/config` | — | default_partitions, auto_create, advertised_host, sasl_required, sasl_users, provoke | Broker settings: default_partitions, auto_create, advertised_host, sasl_required, sasl_users [{user, password}], and a provocation: leader_not_available, sasl_refused, slow_fetch, duplicate, registry_down (none clears). |
| `list_schemas` | `GET /api/streams/schemas` | — | — | The Schema Registry's subjects with their versions and ids. |
| `register_schema` | `POST /api/streams/schemas` | subject, schema | schema_type | Registers a schema under a subject (Avro by default; JSON or PROTOBUF as schema_type). The same schema keeps its id. |

## Pub/Sub (Google Cloud)

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_pubsub_topics` | `GET /api/pubsub/topics` | — | — | The Pub/Sub topics of this environment (schema, retention, labels) with their subscriptions (pull, push, BigQuery or Cloud Storage; filter, ack deadline, ordering, exactly-once, dead-letter) and how many messages each has pending, outstanding, acked and dead-lettered; snapshots, schemas and how apps connect. |
| `list_pubsub_messages` | `GET /api/pubsub/messages` | — | topic, subscription, project, limit | The newest messages of a topic with their state in each subscription (pending, outstanding, expired, acked, dead), or of one subscription: data (JSON or text), attributes, ordering key, delivery attempts, who published. |
| `pubsub_publish` | `POST /api/pubsub/topics/{topic}/publish` | topic | data, attributes, ordering_key, project | Publishes a message to a topic as another service would (the app's subscribers receive it): data (JSON or text), attributes, ordering_key. Makes the topic when it does not exist. A topic with a schema validates the data, as Pub/Sub does. |
| `create_pubsub_subscription` | `PUT /api/pubsub/subscriptions/{subscription}` | subscription, topic | filter, ack_deadline_seconds, ordering, exactly_once, retain_acked, dead_letter_topic, max_delivery_attempts, push_endpoint, unwrapped, write_metadata, oidc_service_account, oidc_audience, bigquery_table, use_topic_schema, bucket, filename_prefix, filename_suffix, max_messages, max_duration, avro, bigtable_table, expiration_ttl, min_backoff, max_backoff, udf_code, udf_function, labels, project | Creates a subscription on a topic (the topic too when missing): pull by default; push (push_endpoint, unwrapped, write_metadata, oidc_service_account), BigQuery (bigquery_table, use_topic_schema, write_metadata), Cloud Storage (bucket, filename_prefix, max_messages, max_duration, avro) or Bigtable (bigtable_table); filter, JavaScript UDF, ack deadline, message ordering, exactly-once, expiration ttl, retry backoff, a dead-letter topic with max delivery attempts. |
| `set_pubsub_schema` | `PUT /api/pubsub/schemas/{schema}` | schema, definition | type, topic, encoding, project | Creates a Pub/Sub schema (Avro or Protocol Buffer) or commits a new revision of it; with topic (and encoding JSON or BINARY), the topic validates every publish against it. |
| `list_pubsub_bigquery_rows` | `GET /api/pubsub/bigquery` | — | table, subscription, limit | The rows BigQuery subscriptions wrote (Stuntman keeps the tables): one per message, the topic schema's fields with use_topic_schema, else data; subscription_name, message_id, publish_time and attributes with write_metadata. |
| `list_pubsub_storage_files` | `GET /api/pubsub/storage` | — | bucket, subscription, limit | The files Cloud Storage subscriptions wrote: bucket, object name, messages, format (text or Avro) and content; storedIn names the Stuntman storage account when one has that bucket. |
| `list_pubsub_bigtable_rows` | `GET /api/pubsub/bigtable` | — | table, subscription, limit | The rows Bigtable subscriptions wrote (Stuntman keeps the tables): row key subscription#salt#message_id (or the delimited key), the data in family data (qualifier ""), and with write_metadata the pubsub_metadata family (subscription_name, message_id, publish_time, attributes, ordering_key). |
| `set_pubsub_bigtable_table` | `PUT /api/pubsub/bigtable/tables/{table}` | table, row_key_schema | — | Defines a Bigtable table's row key schema for Bigtable subscriptions with row_key_schema: fields [{name, type STRING \| INT64 \| BYTES}] taken from the message's JSON, encoding singleton, delimited_bytes (delimiter, default #) or ordered_code_bytes. |
| `set_pubsub_bigquery_table` | `PUT /api/pubsub/bigquery/tables/{table}` | table, schema | — | Defines a BigQuery table's schema for BigQuery subscriptions: columns [{name, type, mode}] (STRING, BYTES, INTEGER, FLOAT, NUMERIC, BOOLEAN, TIMESTAMP, DATE, JSON, RECORD; NULLABLE, REQUIRED, REPEATED). With it, use_table_schema maps the message's JSON fields to columns, drop_unknown_fields drops extras, and a row that does not fit is not written (retried, then dead-lettered). |
| `set_pubsub_topic` | `PUT /api/pubsub/topics/{topic}` | topic | ingestion, udf_code, udf_function, project | Sets a topic's ingestion source (kind aws_kinesis, cloud_storage, azure_event_hubs, aws_msk, confluent_cloud with that source's fields) and/or a JavaScript UDF transform (udf_code, udf_function), making the topic when missing. Cloud Storage reads a Stuntman storage container of the bucket's name; MSK and Confluent read the Streams (Kafka) topic; Kinesis reads the Stuntman Kinesis stream its stream_arn names, from TRIM_HORIZON. |
| `pubsub_ingest` | `POST /api/pubsub/topics/{topic}/ingest` | topic, records | project | Feeds records into an import topic as its source would: records [{data, key, attributes}] — Kafka (MSK, Confluent): value → data, key → ordering key, headers → attributes; Kinesis: partition key → ordering key; Event Hubs: partition key → ordering key, properties → attributes; Cloud Storage: each record one text line. They go through the topic's transforms and schema. |
| `wait_for_pubsub_message` | `GET /api/pubsub/topics/{topic}/wait` | topic | contains, attributes, ordering_key, since, timeout, project | Waits for the first message the app published (not you or an agent) on a topic that matches: text in the data, attributes, ordering key. Looks back 15 s by default; 408 after the timeout. |
| `set_pubsub` | `PUT /api/pubsub/config` | — | project, provoke, times, topic, delay_ms | Pub/Sub settings: project (for short names) and a provocation on Publish: unavailable, deadline_exceeded, resource_exhausted, not_found, slow_publish (delay_ms), duplicate (each message delivered again after its first ack), ack_transient (exactly-once acks fail with a transient error the client retries), kms_disabled (publishes to topics with a KMS key fail with FAILED_PRECONDITION); times = for the next N publishes (0 = until cleared), topic = only that topic; none clears. |

## Kinesis (Amazon Kinesis Data Streams)

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_kinesis_streams` | `GET /api/kinesis/streams` | — | — | The Kinesis streams of this environment: status (CREATING, UPDATING, ACTIVE, DELETING), mode, encryption (KMS key), shard-level metrics, retention, shards (hash key range, sequence range, parent shards, open or closed, records), enhanced fan-out consumers, tags; how to connect; whether the throughput limits are enforced; the provocation. |
| `create_kinesis_stream` | `PUT /api/kinesis/streams/{name}` | name | shards, mode | Creates a stream: shards = N for provisioned mode (hash key space split evenly), or mode = on_demand (4 shards). It is CREATING for transition_ms (1 s), then ACTIVE. |
| `list_kinesis_records` | `GET /api/kinesis/streams/{stream}/records` | stream | shard, limit, since | The newest records of a stream (or one shard): shard, sequence number, partition key, data (JSON or text), who put it, when. |
| `kinesis_put` | `POST /api/kinesis/streams/{stream}/records` | stream, data | partition_key, explicit_hash_key | Puts a record as another producer would: data (JSON or text), partition_key (MD5 of it picks the shard), explicit_hash_key to choose the shard. The app's consumers read it. |
| `wait_for_kinesis_record` | `GET /api/kinesis/streams/{stream}/wait` | stream | contains, partition_key, since, timeout | Waits for the first record the app put (not you or an agent) on a stream that matches: text in the data, partition key. Looks back 15 s by default; 408 after the timeout. |
| `set_kinesis` | `PUT /api/kinesis/config` | — | provoke, times, stream, delay_ms, limits, transition_ms | The Kinesis endpoint's settings and provocations. limits = on \| off: the real throughput limits (per provisioned shard 1 MiB/s or 1,000 records/s in, 2 MiB/s and 5 GetRecords/s out; on-demand streams 4 MiB/s or 4,000 records/s in) answer ProvisionedThroughputExceededException; on by default. transition_ms: how long CREATING, UPDATING and DELETING last (1000; 0 = at once). provoke: throughput_puts, throughput_gets (ProvisionedThroughputExceededException), not_found (ResourceNotFoundException), expired_iterator, kms_throttling (KMSThrottlingException), kms_disabled (KMSDisabledException on puts; both KMS ones only on encrypted streams), slow_puts (delay_ms), partial_failures (PutRecords fails every second record); times = for the next N calls (0 = until cleared), stream = only that stream; none clears. reshard splits the stream's widest open shard now. |

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
