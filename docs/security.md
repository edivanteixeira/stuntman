# Security

Stuntman is a test tool. It holds synthetic data, fake keys and a test certificate authority, and it is designed to
run next to your app in development, CI and staging.

## What is not protected today

- **There is no sign-in yet.** Anyone who reaches port `8025` can read every message, change every rule, provoke
  failures and run every tool, over the web UI, the HTTP API or MCP.
- **There are no API keys yet.** Keys per agent, with roles and environments, are designed and will come.
- **Provider credentials are accepted loosely by design.** Any SendGrid key, any SMTP login, any Service Bus key is
  accepted unless you tighten it per service.

## How to run it safely

1. **Keep the default bind.** Docker publishes every port on `127.0.0.1`.
2. **On a shared machine,** bind to a private network address only, or put Stuntman behind something that
   authenticates: a VPN, Cloudflare Access, `oauth2-proxy`, or a reverse proxy with sign-in.
3. **Never point production at it,** and never trust the Stuntman Test CA on a machine that talks to real providers.
4. **Use synthetic data.** Seeds and plugins use `example.com` addresses and made-up people; keep it that way.

## What it does to keep you safe

- **Plugins are sandboxed by design.** A plugin cannot reach the network or the disk unless it declares it, and
  expressions cannot call out.
- **It shows leaks.** `real_provider_calls` and the unmatched list reveal calls that still go to real providers.
- **Signed callbacks are real.** Webhooks are signed the way each provider signs them, so your app's verification is
  tested, not skipped.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private vulnerability reporting
(**Security › Report a vulnerability**) on this repository.
