# AI models

Stuntman answers the model APIs your app calls (OpenAI, Azure OpenAI, Anthropic, Gemini, OpenRouter and
DeepSeek) without running a model. The app keeps its official SDK and only changes the base URL; any key works and
is never sent anywhere. Rules decide the answer (a text, a tool call, a JSON object, a length), failures can be
forced for the next calls, and every call is kept with its prompt, answer, tokens and cost.

## At a glance

| | |
|---|---|
| **Stands in for** | OpenAI (Chat Completions, Responses, Embeddings, Models), Azure OpenAI (deployments and `/openai/v1`), Anthropic Messages (`/v1/messages`, `count_tokens`, Models), Gemini (`generateContent`, `streamGenerateContent`, `embedContent`, `batchEmbedContents`, `countTokens`, Models), OpenRouter and DeepSeek (OpenAI-compatible chat). Streamed or not. Tested with the `openai` and `anthropic` Python SDKs and raw HTTP |
| **Ports** | `8025` (`STUNTMAN_PORT`); `8445` is the same over TLS with the Test CA (`STUNTMAN_HTTPS_PORT`) |
| **Point your app at** | OpenAI `http://localhost:8025/openai/v1`, Anthropic `http://localhost:8025/anthropic`, Azure OpenAI `http://localhost:8025/azure-openai`, Gemini `http://localhost:8025/gemini`, OpenRouter `http://localhost:8025/openrouter/api/v1`, DeepSeek `http://localhost:8025/deepseek` |
| **It keeps** | Every call: provider, API, model, status, why it got that answer, tokens in, out and cached, cost, streamed or not, duration, caller, prompt summary, answer text, the request and the raw response (256 KB each). Rules, default answer and prices in `ai/config.yaml` |
| **You can provoke** | For the next N calls (`force_ai_failure`): `429` with `Retry-After`, `500`, `529` overloaded, `filter` (content filtered), `length` (cut by max tokens), `bad_json` (broken tool arguments), `slow` (40 ms per stream part), `cut` (the stream breaks halfway). A missing key gives `401` |
| **Screen** | `/ai` (calls), `/ai/rules`, `/ai/spend` |
| **Tools** | `list_ai_calls`, `get_ai_call`, `wait_for_ai_call`, `list_ai_rules`, `set_ai_rule`, `delete_ai_rule`, `force_ai_failure`, `ai_spend`, `set_ai_price` |

## Connect your app

Each provider has a prefix on port 8025: `/openai`, `/azure-openai`, `/anthropic`, `/gemini`, `/openrouter`,
`/deepseek`. Stuntman reads the rest of the path the way the provider does (see What it supports).

```python
# OpenAI, as the tests run it
from openai import OpenAI
oc = OpenAI(base_url="http://localhost:8025/openai/v1", api_key="sk-test", max_retries=0)
oc.chat.completions.create(model="gpt-6.1", messages=[{"role": "user", "content": "hi"}])

# Anthropic, as the tests run it
import anthropic
ac = anthropic.Anthropic(base_url="http://localhost:8025/anthropic", api_key="sk-ant-test", max_retries=0)
ac.messages.create(model="claude-sonnet-5-5", max_tokens=256, messages=[{"role": "user", "content": "hi"}])
```

```text
# Azure OpenAI deployment, raw HTTP as the tests send it
POST http://localhost:8025/azure-openai/openai/deployments/gpt-6.1/chat/completions?api-version=2025-04-01-preview
api-key: k

# Gemini
POST http://localhost:8025/gemini/v1beta/models/gemini-3-flash:generateContent
x-goog-api-key: k
```

Environment variables most SDKs read: `OPENAI_BASE_URL=http://localhost:8025/openai/v1`,
`ANTHROPIC_BASE_URL=http://localhost:8025/anthropic`.

| App runs | Base host |
|---|---|
| On the host | `http://localhost:8025` |
| In Docker, Stuntman on the host | `http://host.docker.internal:8025` |
| In the same compose file | `http://stuntman:8025` |
| Another environment | `http://localhost:8025/e/<env>/openai/v1` (and so on), or a host name of that environment ([Environments](../environments.md)) |
| HTTPS only | `https://localhost:8445/openai/v1` (trust the Test CA) |

### Keys

Any non-empty key is accepted and never forwarded. Where the key is read:

| Provider | Headers read |
|---|---|
| OpenAI, OpenRouter, DeepSeek | `Authorization` |
| Azure OpenAI | `api-key` or `Authorization` (Entra ID bearer tokens are accepted unchecked) |
| Anthropic | `x-api-key` or `Authorization` |
| Gemini | `x-goog-api-key`, `Authorization`, or `?key=` in the query |

No key at all answers `401` in the provider's error shape (`invalid_api_key` "You didn't provide an API key.",
Anthropic `authentication_error` "x-api-key header is required", Gemini `UNAUTHENTICATED`).

## What it supports

### Which API a path reaches

| Provider | Path (after the prefix) | API (`api` in rules and calls) |
|---|---|---|
| any but Gemini | ends in `chat/completions` | `chat` |
| any but Gemini | ends in `responses` | `responses` |
| any but Gemini | ends in `embeddings` | `embeddings` |
| any but Gemini | ends in `messages/count_tokens` | `count` |
| any but Gemini | ends in `messages` | `messages` |
| any but Gemini | ends in `models`, or `GET .../models/...` | model list |
| Gemini | `.../models/<model>:generateContent` or `:streamGenerateContent` | `generate` |
| Gemini | `.../models/<model>:embedContent` or `:batchEmbedContents` | `embeddings` |
| Gemini | `.../models/<model>:countTokens` | `count` |
| Gemini | any other `GET` | model list |

The API is chosen by the path, not by the provider: `/anthropic/v1/messages` and `/openai/v1/chat/completions`
both work, and so would `/deepseek/messages`. Any other path answers `404` `Stuntman: <provider> has no API at
/<path>`.

The model is the body's `model`. When it is missing, it is the Azure deployment name
(`/openai/deployments/<name>/...`) or the Gemini model in the path.

### Requests read

| API | Read from the request |
|---|---|
| `chat` | `messages` (roles `system`, `developer`, `user`, `assistant`, `tool`, `function`; string or parts content), `tools[].function.name`, `max_completion_tokens` or `max_tokens`, `stream`, `stream_options.include_usage` |
| `responses` | `instructions`, `input` (a string, or items: messages and `function_call_output`), `tools[].name`, `max_output_tokens`, `stream` |
| `messages`, `count` | `system` (string or blocks), `messages` (with `tool_result` blocks), `tools[].name`, `max_tokens`, `stream` |
| `generate` | `systemInstruction.parts`, `contents[].parts` (with `functionResponse`), `tools[].functionDeclarations[].name`, `generationConfig.maxOutputTokens`; streamed for `:streamGenerateContent` or `?alt=sse` |
| `embeddings` | `input` (a string or a list), Gemini `content.parts` or `requests[].content.parts`, `dimensions` or `outputDimensionality`, `encoding_format: base64` |

Everything else (`temperature`, `top_p`, `n`, `seed`, `response_format`, `tool_choice`, `logprobs`, `stop`,
`previous_response_id`, `store`, images, files, audio…) is accepted and ignored.

### Answers

| API | Non-streamed answer | Streamed answer |
|---|---|---|
| `chat` | `chat.completion` with `choices[0].message` (`content`, `refusal: null`, `tool_calls`), `finish_reason` (`stop`, `tool_calls`, `length`, `content_filter`), `usage` (`prompt_tokens`, `completion_tokens`, `total_tokens`, `prompt_tokens_details.cached_tokens`), `system_fingerprint: fp_stuntman`, header `x-request-id`. Azure adds `prompt_filter_results`; DeepSeek adds `prompt_cache_hit_tokens` and `prompt_cache_miss_tokens` | SSE `chat.completion.chunk`s: a role chunk, content in parts of three words, tool call name then arguments in 12-byte pieces, a final chunk with `finish_reason`, a usage chunk when `stream_options.include_usage` is true, then `data: [DONE]` |
| `responses` | `response` with `status` (`completed` or `incomplete` with `incomplete_details.reason: max_output_tokens`), `output` (`function_call` and/or `message` with `output_text`), `output_text`, `usage` (with `input_tokens_details.cached_tokens`, `output_tokens_details.reasoning_tokens: 0`) | SSE events with `sequence_number`: `response.created`, `response.output_item.added`, `response.function_call_arguments.delta`/`.done`, `response.output_text.delta`/`.done`, `response.output_item.done`, then `response.completed` or `response.incomplete` |
| `messages` | `message` with `content` (`text` and/or `tool_use` blocks), `stop_reason` (`end_turn`, `tool_use`, `max_tokens`, `refusal`), `usage` (`input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens: 0`, `output_tokens`), header `request-id` | SSE: `message_start`, `content_block_start`, `content_block_delta` (`text_delta` or `input_json_delta`), `content_block_stop`, `message_delta`, `message_stop` |
| `generate` | `candidates[0].content.parts` (`text` and/or `functionCall`), `finishReason` (`STOP`, `MAX_TOKENS`, `SAFETY`), `usageMetadata` (with `cachedContentTokenCount`), `modelVersion`, `responseId` | SSE `data:` lines with `candidates`, the last one with `finishReason` and `usageMetadata` |
| `embeddings` | OpenAI shape: `data[].embedding` (floats, or base64 little-endian float32 with `encoding_format: base64`), `usage`. Gemini: `embedding.values` for one `content`, else `embeddings[].values` | not streamed |
| `count` | `{"input_tokens": N}`, Gemini `{"totalTokens": N}` | |
| model list | OpenAI shape `gpt-6.1`, `gpt-6.1-mini`, `text-embedding-3-small`, `text-embedding-3-large`; Anthropic `claude-opus-5-5`, `claude-sonnet-5-5`, `claude-haiku-5-5`; Gemini `gemini-3-pro`, `gemini-3-flash`, `text-embedding-005` | |

Any model name is accepted; the list is only what `GET models` shows. `GET .../models/<id>` returns the whole
list, not one model.

### Rules

Rules are kept in `ai/config.yaml` and edited with `set_ai_rule`. The first rule that matches wins. With none, the
answer is the default text, "This is a simulated answer from Stuntman." (change it with `set_ai_price
{"default_text": "..."}`).

| `when` key | Matches when |
|---|---|
| `provider` | `openai`, `azure-openai`, `anthropic`, `gemini`, `openrouter` or `deepseek` |
| `api` | `chat`, `responses`, `messages` or `generate` |
| `model` | The model exactly, or a pattern with `*` (`gpt-6.1*`, `claude-*haiku*`) |
| `contains` | The last user message contains this text (any case) |
| `system_contains` | The system prompt (or instructions) contains this text (any case) |
| `tool` | The app offered a tool of this name |

| `answer` key | Effect |
|---|---|
| `text` | The answer. `{{ last_user }}` (or `{{last_user}}`) is replaced by the last user message |
| `tool_call` | `{name, arguments}`: the model calls that tool first. When the request already carries a tool result (last `tool` message, a `tool_result` block, a `functionResponse` part, a `function_call_output` item), the rule answers `text` instead (`Done.` when `text` is empty) |
| `json` | Answer this JSON, serialised as the text |
| `tokens` | Pad the text with filler words to about N tokens |
| `stop` | `length`: cut as if by max tokens (see `length` below) |

```yaml
rules:
  - name: refund
    when: { contains: refund }
    answer:
      text: The refund takes 5 days.
      tool_call: { name: lookup_order, arguments: { order_id: A-1042 } }
default_text: ""
prices:
  gpt-*: { in: 2.0, out: 8.0, cached: 0.5 }
```

Rules do not apply to embeddings; embeddings are always deterministic.

## How it behaves

- **Tokens** are counted as about 4 characters per token. Input tokens are the system prompt plus every message
  or input, plus 8. Output tokens are the answer text plus the tool arguments (plus 8 for a tool call).
- **Cached tokens**: a prompt whose first 4096 characters (at least 256 tokens) were already sent since Stuntman
  started gets `min(4/5 of the input tokens, the tokens of that head)` as cached. The memory is per process: a
  restart forgets it, and it is shared by every environment.
- **Embeddings** are unit vectors derived from SHA-256 of the text: the same text always gives the same vector.
  Default dimensions: 3072 when the model name contains `large`, 768 for Gemini, else 1536.
- **Streaming** sends one SSE part every 8 ms (40 ms with `slow`). Gemini streams SSE even without `?alt=sse`.
- **Cost** = ((input − cached) × in + cached × cached price + output × out) ÷ 1 000 000, with the price of the
  longest pattern that matches the model. Built-in prices (USD per million tokens, in / out / cached):

  | Pattern | In | Out | Cached |
  |---|---:|---:|---:|
  | `gpt-*` | 2.00 | 8.00 | 0.50 |
  | `gpt-*-mini` | 0.40 | 1.60 | 0.10 |
  | `o*` | 2.00 | 8.00 | 0.50 |
  | `text-embedding-*` | 0.02 | 0 | 0 |
  | `claude-*` | 3.00 | 15.00 | 0.30 |
  | `claude-*haiku*` | 1.00 | 5.00 | 0.10 |
  | `gemini-*` | 1.25 | 10.00 | 0.31 |
  | `deepseek-*` | 0.27 | 1.10 | 0.07 |

  A model no pattern matches costs 0. Embedding calls record 0 output tokens and 0 cost for output.
- **IDs**: `chatcmpl-…`, `resp_…`, `msg_…` (Anthropic), `call_…`, `toolu_…`, `fc_…`.
- **why** on each call: `rule: <name>`, `rule: <name> · tool call`, `no rule: default answer`, `deterministic`
  (embeddings) or `forced by <who> · <kind>`.
- **caller** is the first word of the `User-Agent` (`OpenAI`, `Anthropic`, `acme-agent`), or `app`.
- Calls stay until the environment is deleted. Rules and prices are YAML and survive restarts.
- Each call is a row on the [Timeline](../timeline.md): `<provider> <api> · <model> · <in> → <out> tokens`
  (or the status), marked provoked or failed.
- Pub/Sub's AI transform calls these same models from inside Stuntman, with the same rules, forced failures and
  call log ([Pub/Sub](pubsub.md)).

## Provoke failures

`force_ai_failure` applies to the next `count` calls (default 1) to one `provider`, or to any provider when
`provider` is empty. Forces queue: the oldest one with calls left is used first. `models` and `count_tokens`
calls never take one. A forced failure is checked after the key: a call with no key gets `401` and keeps the
force for the next call.

| `kind` | What the app sees |
|---|---|
| `429` | OpenAI-compatible and Azure: `429` `rate_limit_exceeded` "Rate limit reached for requests. Please try again in 2s.", `retry-after: 2`, `x-ratelimit-remaining-requests: 0`. Anthropic: `429` `rate_limit_error`, `retry-after: 2`, `anthropic-ratelimit-requests-remaining: 0`. Gemini: `429` `RESOURCE_EXHAUSTED`, no `Retry-After` |
| `500` | `500` `server_error` (Anthropic `api_error`, Gemini `INTERNAL`) |
| `529` | Anthropic: `529` `overloaded_error` "Overloaded". Gemini: `503` `UNAVAILABLE`. Others: `503` `server_overloaded` |
| `filter` | Azure OpenAI: `400` `content_filter` with `innererror.code: ResponsibleAIPolicyViolation`. Others: an empty answer with `finish_reason: content_filter` (Anthropic `stop_reason: refusal`, Gemini `finishReason: SAFETY`) |
| `length` | The text is padded to `max_tokens` (16 when not set, at most 4096) and cut to about three quarters of it: `finish_reason: length`, Anthropic `max_tokens`, Gemini `MAX_TOKENS`, Responses `status: incomplete` |
| `bad_json` | A tool call to the first offered tool (else `lookup`) with arguments `{"order_id": "A-10` (broken JSON) |
| `slow` | Streams send a part every 40 ms instead of 8 ms. A non-streamed call is not slowed |
| `cut` | Streams send half their parts, then the connection breaks. A non-streamed call answers normally |
| `none` | Clears the forces of that `provider` (with no `provider`, the forces set for any provider) |

Error bodies follow each provider: OpenAI-compatible `{"error": {"message", "type", "param", "code"}}`, Anthropic
`{"type": "error", "error": {"type", "message"}, "request_id"}`, Gemini `{"error": {"code", "message",
"status"}}`.

Forced calls are recorded with `why: forced by <who> · <kind>`, counted under `forced` in `ai_spend`, and marked
provoked on the Timeline. `list_ai_rules` shows the forces still waiting in `forced`.

## See what the app did

| Need | Tool | HTTP |
|---|---|---|
| Calls, newest first | `list_ai_calls {"provider": "anthropic", "since": <ms>, "limit": 100}` | `GET /api/ai/calls` |
| One call with the whole request and response | `get_ai_call {"id": "chatcmpl-…"}` | `GET /api/ai/calls/{id}` |
| Wait for a call | `wait_for_ai_call {"provider": "openai", "contains": "refund", "timeout": "30s", "within": "10s"}` | `GET /api/ai/calls/wait` |
| Rules, default text, prices, failure kinds, waiting forces | `list_ai_rules` | `GET /api/ai/rules` |
| Spend per provider | `ai_spend {"since": <ms>}` (default: since 00:00 UTC today) | `GET /api/ai/spend` |

Call fields: `id`, `at`, `provider`, `api`, `model`, `status`, `why`, `tokensIn`, `tokensOut`, `cached`, `cost`,
`stream`, `ms`, `caller`, `prompt` (`{system, lastUser, tools, toolResult, maxTokens, inputs}`), `answer` (the
text, `tool <name> <arguments>`, or `N vectors × D`); `get_ai_call` adds `request` and `response` (the raw body or
the SSE text).

`wait_for_ai_call` returns the newest call that matches; `contains` searches the prompt summary (system prompt,
last user message, tool names), any case. Without `within` it waits only for calls made after it starts. It polls
every 200 ms and answers `408 No such model call yet` after `timeout`.

`ai_spend` returns `{since, cost, providers: [{provider, calls, tokensIn, tokensOut, cached, cost, failures,
forced}]}`; `failures` counts status ≥ 400.

## Recipes

1. **Test the app's tool loop.**

   ```text
   set_ai_rule {"name": "refund", "contains": "refund", "text": "The refund takes 5 days.",
                "tool_call": {"name": "lookup_order", "arguments": {"order_id": "A-1042"}}}
   ```

   The first call returns a call to `lookup_order`. When the app sends the tool result back, the next call answers
   "The refund takes 5 days.".

2. **Prove the app backs off on rate limits.**

   ```text
   force_ai_failure {"provider": "openai", "kind": "429", "count": 2}
   # the app makes one request; its SDK retries
   list_ai_calls {"provider": "openai", "limit": 3}   → two 429 calls, then a 200
   ```

3. **Handle Anthropic overload.** `force_ai_failure {"provider": "anthropic", "kind": "529"}`. The SDK raises
   `APIStatusError` with status 529 (with `max_retries=0`).

4. **Make the app parse structured output.**

   ```text
   set_ai_rule {"name": "classify", "system_contains": "classify", "json": {"category": "billing", "confidence": 0.92}}
   ```

5. **Handle a stream that breaks.** `force_ai_failure {"provider": "anthropic", "kind": "cut"}`, then a streamed
   call: half the events arrive, then the connection drops without `message_stop`.

6. **Assert the app's token budget.** Run the flow, then `ai_spend {}` and assert `cost` or `tokensIn`. Set your
   own prices first: `set_ai_price {"model": "gpt-6.1", "in": 1.25, "out": 10, "cached": 0.125}`.

7. **Assert what the app sent.** `wait_for_ai_call {"provider": "openai", "contains": "A-1042", "within": "30s"}`,
   then `get_ai_call {"id": "<id>"}` and read `request.messages`.

## Errors and troubleshooting

| Symptom or error text | Cause | Fix |
|---|---|---|
| `401` `You didn't provide an API key.` | No key header | Send any key |
| `404` `Stuntman: openai has no API at /v1/completions` | A path Stuntman does not answer (legacy completions, assistants, images, audio, files…) | Use chat, responses, embeddings or messages |
| Always "This is a simulated answer from Stuntman." | No rule matches (rules match the last user message, not the whole prompt) | `list_ai_rules`; use `system_contains` or `tool` |
| A tool-call rule loops | The app does not send the tool result back as a `tool` message / `tool_result` block / `functionResponse` / `function_call_output` | Send the result in the provider's format |
| `set_ai_rule` → `400 provider: openai \| azure-openai \| ...` | Unknown provider name | Use one of the six |
| `force_ai_failure` → `400 kind: 429 \| 500 \| ...` | Unknown kind | Use a kind from the table |
| `delete_ai_rule` → `404 No rule “x”` | Unknown rule name | `list_ai_rules` |
| `wait_for_ai_call` → `408` | No matching call within `timeout`, or the call came before the wait and `within` was not set | Add `within`; raise `timeout` |
| Gemini SDK fails to parse a stream | Stuntman always streams SSE, also without `alt=sse` | Use the SDK's SSE mode (`alt=sse`) |

## Not done

- No model runs: answers come only from rules or the default text. Tokens and costs are estimates (4 characters
  per token).
- No legacy Completions, Assistants, Threads, Batch, Files, Images, Audio, Realtime, moderation or fine-tuning
  APIs. No Vertex AI or Amazon Bedrock paths.
- Responses API state is not kept: `previous_response_id`, `store` and `GET /responses/{id}` are not served.
- `response_format` / JSON schema is not enforced; a `json` rule answers what it says.
- One tool call per answer; parallel tool calls are not produced.
- `GET models/{id}` returns the list, not one model.
- Gemini `streamGenerateContent` always streams SSE; the JSON-array stream form is not served.
- The cached-token estimate lives in memory and is shared across environments.

Missing something? Tell the developer what the fix would be and offer a pull request — see [Contributing](../../CONTRIBUTING.md).

## Tools and routes

<!-- tools: ai -->
_Generated by `tools/gen_tool_reference.py` from the engine source; each tool is an MCP tool and the HTTP route shown (under `/api`)._

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
<!-- /tools -->
