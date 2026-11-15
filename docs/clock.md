# The environment's clock

Some flows wait days: a subscription renews in 30 days, a trial ends in 14, a shipment's tracking moves every 30
minutes. Each environment has a clock you can move forward, so you test them now.

The clock is real time plus an offset. It starts at real time and only moves forward. It is kept in
`<env>/clock.json`, so each environment has its own.

## Move it

```text
clock()                          → now, the offset, and the webhooks waiting for their time
advance_clock(by="29d")          → nothing due yet
advance_clock(by="1d")           → the renewal webhook goes out
advance_clock(reset=true)        → back to real time
```

Over HTTP: `GET /api/clock` and `POST /api/clock/advance` with `{"by": "1d"}` or `{"reset": true}`. `by` takes
`30m`, `1h`, `1d`, `30d` and the like.

## What follows it

| | |
|---|---|
| **Scheduled webhooks** | a plugin's `send` with a `delay` is kept until the clock reaches its time, then sent. Moving the clock past it sends it at once; `clock` lists the ones waiting |
| **Templates** | `now()` and `now_ms()` in plugin expressions, and `now` in [WireMock templates](mocks.md#response-templates), read the clock |
| **Webhook signatures** | the timestamps in `hex_ts`, `stripe`, `svix` and `cloudevents` signatures |

```yaml
routes:
  - route: POST /v1/subscriptions
    decide:
      - answer: created
    send:
      - url: https://app.example.com/hooks/billing
        delay: 30d
        event: invoice.upcoming
        body: '{"type": "invoice.upcoming", "at": "{{ now() }}"}'
```

A test creates the subscription, calls `advance_clock(by="30d")` and waits for the webhook on its own endpoint.

## Limits

- The clock belongs to the HTTP API plugins. Tokens, SAS links, certificates, message locks and the other built-in
  lines keep real time.
- Webhooks that are waiting are kept with the environment; a send with no `delay` goes out at once, whatever the
  clock says.
- The clock never goes back past real time: `reset` is the only way back.
