# Security

Stuntman is a test tool. It holds synthetic data, fake keys and a test certificate authority, and it is designed to
run next to your app in development, CI and staging.

## Sign-in

Off by default: anyone who reaches port `8025` can read every message, change every rule, provoke failures and run
every tool, over the web UI, the HTTP API or MCP. Turn it on with `STUNTMAN_AUTH_ISSUER` (any OIDC provider) and the
list of people (`STUNTMAN_AUTH_ALLOWED_DOMAINS`, `STUNTMAN_AUTH_ALLOWED_EMAILS`): see
[Environments › Sign-in](environments.md#sign-in-the-central-mcp-and-tokens). Then the screens, `/api` and `/mcp` need
a person's session, the admin key (`STUNTMAN_ADMIN_KEY`), an environment's token or an agent's OAuth token, and
the app's provider routes and the simulated identity stay open. There are no roles: everyone on the list sees and
uses every environment.

## What is not protected

- **Provider credentials are accepted loosely by design.** Any SendGrid key, any SMTP login, any Service Bus key is
  accepted unless you tighten it per service.

## How to run it safely

1. **Keep the default bind.** Docker publishes every port on `127.0.0.1`.
2. **On a shared machine,** bind to a private network address only, or put Stuntman behind something that
   authenticates: a VPN, Cloudflare Access, `oauth2-proxy`, or a reverse proxy with sign-in.
3. **Never point production at it,** and never trust the Stuntman Test CA on a machine that talks to real providers.
4. **Use synthetic data.** Seeds and plugins use `example.com` addresses and made-up people; keep it that way.
5. **A shared Stuntman with one public address** (`STUNTMAN_PUBLIC_URL`) exposes every environment's screens, API and
   MCP to whoever reaches it unless sign-in is on. Turn it on, or put sign-in in front of `/`, `/e/*/api` and `/e/*/mcp`
   yourself, and leave open only what the app and the browser of the app under test need: `/e/*/login/` and the
   provider routes. Keep `STUNTMAN_AUTH_CLIENT_SECRET` and `STUNTMAN_ADMIN_KEY` in a secret, never in a repository.
6. **Real e-mail domains** (`allowed_email_domains`) put real people's addresses in an environment. Use them only
   where that data may already be, delete the environment when the branch is done, and keep its data out of reports.

## The Test CA

Stuntman's TLS endpoints use certificates from the Stuntman Test CA. Its private key stays in `data/keys`; the
downloads (`/api/test-ca.pem`, `.crt`, `.p12`) hold only the certificate. Anyone who has the key can make certificates
your machine accepts for **any** host name once the CA is trusted, so:

- Trust it per process (`NODE_EXTRA_CA_CERTS`, `REQUESTS_CA_BUNDLE`, a Java truststore, a .NET validation callback in
  test config): see [Configuration › The Stuntman Test CA](configuration.md#the-stuntman-test-ca).
- Add it to an OS trust store (macOS Keychain, Windows store, `update-ca-certificates`) only when the developer chooses
  to, and remove it when done. Coding agents must ask before doing it, never do it silently.
- Never trust it on a machine or image that also talks to real providers, and never copy `data/keys` anywhere shared.

## What it does to keep you safe

- **Plugins are sandboxed by design.** A plugin cannot reach the network or the disk unless it declares it, and
  expressions cannot call out.
- **It shows leaks.** `real_provider_calls` and the unmatched list reveal calls that still go to real providers.
- **Signed callbacks are real.** Webhooks are signed the way each provider signs them, so your app's verification is
  tested, not skipped.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private vulnerability reporting
(**Security › Report a vulnerability**) on this repository.
