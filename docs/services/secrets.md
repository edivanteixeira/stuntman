# Secrets (Azure Key Vault)

Stuntman stands in for Azure Key Vault and for the Azure managed identity token endpoints. The app uses the
Azure SDK as in production. It gets a token from IMDS or the App Service endpoint, then reads secrets, signs and
wraps with keys, and downloads certificates over HTTPS. Stuntman keeps every secret version, holds RSA keys that
really sign, verify, wrap and encrypt, issues certificates from the Stuntman Test CA, and logs every operation with
the identity that made it.

![Key Vault secrets with versions, readers and read counts](../images/screen-secrets.png)

## At a glance

| | |
|---|---|
| **Stands in for** | Azure Key Vault data plane (any `api-version` 7.x; the tests send `7.4`), Azure managed identity (IMDS `api-version=2018-02-01`, App Service `api-version=2019-08-01`). Tested with .NET `Azure.Security.KeyVault.Secrets` / `.Keys` / `.Keys.Cryptography` / `.Certificates` with `ManagedIdentityCredential`, and Python `azure-keyvault-keys` / `azure-keyvault-secrets` |
| **Ports** | `8444` Key Vault, TLS only (`STUNTMAN_KEYVAULT_PORT`). `8025` managed identity endpoints and the control API (`STUNTMAN_PORT`); `8445` is the same over TLS (`STUNTMAN_HTTPS_PORT`) |
| **Point your app at** | Vault URL `https://<vault>.vault.<environment base domain>:8444` (`KEYVAULT_URL` in `get_connections`). Managed identity `IDENTITY_ENDPOINT=http://<stuntman>:8025/msi/token` and `IDENTITY_HEADER=<identity>`. Trust the Test CA |
| **It keeps** | Secrets with all versions, content type, tags, `nbf`/`exp`; RSA keys (2048/3072/4096) with versions and allowed operations; certificates with their private key and `.pfx`; soft-deleted items; the access log (identity, operation, item, version, status, duration, client, trace id, "would fail") |
| **You can provoke** | Per secret: `403`, `404`, `disabled`, `429`, `503`, `slow` (`provoke_secret`). Per key: `disabled`, `403`, `429`, `slow`, `wrong_signature` (`provoke_key`). Per certificate: `missing`, `expired`, `not_yet_valid`, `wrong_password`, `untrusted` (`provoke_certificate`). Whole vault: `503`, `timeout`, `refused` (`set_vault` `reachability`). Throttling and latency per vault (`set_vault`). Managed identity: `500`, `slow`, `no_identity` (`provoke_imds`). Missing RBAC role or scope (`any_token: false` + identities) |
| **Screen** | `/secrets` (secrets), `/secrets/keys`, `/secrets/certificates`, `/secrets/log` (access log), `/secrets/settings` (vault, identities, behaviour) |
| **Tools** | `list_vaults`, `get_vault`, `set_vault`, `set_vault_identity`, `list_secrets`, `secret_detail`, `get_secret`, `set_secret`, `set_secret_state`, `provoke_secret`, `secret_reads`, `secret_used_by`, `delete_secret`, `recover_item`, `purge_item`, `list_keys`, `key_detail`, `create_vault_key`, `set_key_state`, `delete_key`, `provoke_key`, `key_public`, `list_certificates`, `certificate_detail`, `issue_certificate`, `import_certificate`, `renew_certificate`, `provoke_certificate`, `provoke_imds`, `would_fail`, `vault_log`, `vault_log_entry`, `vault_patterns` |

## Connect your app

### 1. Create a vault

A vault exists only after you create it. Its name is 3 to 24 lowercase letters, digits or dashes, and starts with
a letter.

```text
set_vault {"name": "acme-kv"}
list_vaults {}            → vaults[].url = https://acme-kv.vault.<base domain>:8444
```

A new vault imports the environment's `secrets.yaml` values (the `secret://name` references other services use)
and the vault's seed file (`seed/secrets.yaml` by default, a map of name to value) as secrets.

### 2. Resolve the vault host name

The vault is found by host name only: the first label of the `Host` header (or the TLS SNI name) is the vault
name, the rest must belong to an environment (its base domain, see [Environments](../environments.md)). There is
no `/e/<env>/` path prefix on port `8444`. `localhost` or an IP address does not work: the vault name would be
`localhost` (404 `VaultNotFound`).

| Where the app runs | What to do |
|---|---|
| On the machine | Add `127.0.0.1  acme-kv.vault.<base domain>` to `/etc/hosts`. `GET /api/environments/{name}/hosts` does **not** list vault host names: add the line yourself |
| On the machine, no hosts file | `https://acme-kv.vault.<env>.localhost:8444` also works: a `<x>.<env>.localhost` name finds environment `<env>`. Only where the resolver maps `*.localhost` to `127.0.0.1` |
| In Docker, Stuntman on the host | `extra_hosts: ["acme-kv.vault.<base domain>:host-gateway"]` on the app service |
| In the same compose project | A network alias `acme-kv.vault.<base domain>` on the Stuntman service |
| Shared Stuntman | Give the environment a base domain under a wildcard DNS name (`*.stuntman.example.com` → the server) |

The default base domain is `<env>.stuntman.local` (for the default environment `staging`:
`acme-kv.vault.staging.stuntman.local`). Every vault host name gets a server certificate for that name, signed by
the Test CA.

### 3. Trust the Test CA

The Azure SDKs refuse a Bearer token over plain HTTP, so the vault is TLS only. Download the CA from
`http://localhost:8025/api/test-ca.pem` (also `/api/keyvault/test-ca.pem`; `.crt`, `.p12` and
`/api/ca-bundle.pem` are in [Configuration](../configuration.md#the-stuntman-test-ca)).

```sh
curl -s http://localhost:8025/api/test-ca.pem -o stuntman-test-ca.pem
export REQUESTS_CA_BUNDLE=$PWD/stuntman-test-ca.pem     # Python (requests, the Azure SDKs)
export NODE_EXTRA_CA_CERTS=$PWD/stuntman-test-ca.pem    # Node
```

### 4. Get a token: managed identity

**App Service form (recommended).** `ManagedIdentityCredential` and `DefaultAzureCredential` use it when both
variables are set:

```sh
IDENTITY_ENDPOINT=http://localhost:8025/msi/token     # /MSI/token also answers
IDENTITY_HEADER=api-mi                                 # the identity name, see below
KEYVAULT_URL=https://acme-kv.vault.staging.stuntman.local:8444
```

The SDK calls `GET /msi/token?resource=https://vault.azure.net&api-version=2019-08-01` with `X-IDENTITY-HEADER`.

- The identity is the first of the query parameters `client_id`, `mi_res_id`, `object_id`, `principal_id`. Without
  one, it is the `X-IDENTITY-HEADER` value. Without either, it is `app-mi`.
- A call without `X-IDENTITY-HEADER` gets 400.
- With `STUNTMAN_IDENTITY_HEADER` set on Stuntman, the header must equal that value. The header then no longer
  names the identity: pass `client_id` (`ManagedIdentityCredential("api-mi")`), or the identity is `app-mi`.
- The answer is `{access_token, expires_on, resource, token_type: "Bearer", client_id}`.

**IMDS form.** `GET /metadata/identity/oauth2/token?resource=…&client_id=api-mi` with the header `Metadata: true`
(or `X-IDENTITY-HEADER`). It answers `{access_token, client_id, expires_in: "86399", expires_on, ext_expires_in,
not_before, resource, token_type}`. The SDKs call IMDS at `169.254.169.254`; point them here with the
`IDENTITY_ENDPOINT` form instead, or give a custom credential this URL (as the Python test does). The .NET test
sets `IDENTITY_ENDPOINT` to this path and `IDENTITY_HEADER` to the identity, and that also works.

Several environments: both endpoints answer under `/e/<env>/` too
(`http://localhost:8025/e/pr-5462/msi/token`). The token works on any vault; the vault host name picks the
environment.

The token is an unsigned JWT (`alg: none`) valid 24 hours, with `oid`, `name`, `appid` and `sub` set to the
identity and `aud` set to `resource`.

**Any other token.** The vault does not check signatures. It reads the identity from the JWT claims `oid`, `name`,
`appid`, `azp`, `sub` or `upn` (first match against the vault's identities, case-insensitive). So a token from
Stuntman's own identity service, or a test JWT, also works.

### 5. SDK snippets

.NET (from the .NET test):

```csharp
var cred = new ManagedIdentityCredential();     // IDENTITY_ENDPOINT + IDENTITY_HEADER
var vault = new Uri(Environment.GetEnvironmentVariable("KEYVAULT_URL"));
var secrets = new SecretClient(vault, cred);
var keys = new KeyClient(vault, cred);
var certs = new CertificateClient(vault, cred);
var crypto = new CryptographyClient(keyId, cred);
```

Python (from the e2e test). The Python SDK compares the challenge resource with the vault host *and port*; a port
is never in Azure's vault URLs, so turn that check off:

```python
from azure.identity import ManagedIdentityCredential
from azure.keyvault.secrets import SecretClient
from azure.keyvault.keys import KeyClient

url = "https://acme-kv.vault.staging.stuntman.local:8444"
cred = ManagedIdentityCredential()
secrets = SecretClient(url, cred, verify_challenge_resource=False)
keys = KeyClient(url, cred, verify_challenge_resource=False)
```

`connection_verify="/path/stuntman-test-ca.pem"` on the client trusts the CA for that client only.

### 6. Name the app in the log

The access log's `client` is the first word of `X-Stuntman-App`, else `X-Duble-App`, else `User-Agent` (cut at
`/` or a space). Set `X-Stuntman-App: acme-api` when several apps share one identity. A W3C `traceparent` header
puts its trace id on the log entry and the [Timeline](../timeline.md) row.

### Apps of an environment

`apps` in `environment.yaml` can set `KEYVAULT_URL: ${KEYVAULT_URL}` and fill a variable from a secret with
`${secret:name}` or `${secret:vault/name}`. `GET /api/env/<app>` resolves them; each secret read goes to the access
log as `app <app>`. See [Environments](../environments.md#the-apps-of-an-environment).

## What it supports

All data-plane routes are on `https://<vault>.vault.<base domain>:8444`. The `api-version` query parameter is
accepted and ignored. Every list answers in one page (`nextLink: null`); `maxresults` is ignored.

### Authentication

| Request | Answer |
|---|---|
| Any request without `Authorization: Bearer …` | 401 `Unauthorized`, "AKV10000: Request is missing a Bearer or PoP token.", header `WWW-Authenticate: Bearer authorization="https://login.microsoftonline.com/stuntman", resource="https://vault.<base domain>"` (the SDK challenge; no port in `resource`) |

### Secrets

| Operation | Route | Notes |
|---|---|---|
| Set secret | `PUT /secrets/{name}` | Creates a new version. Body `value` (required: 400 "Property value is required."), `contentType`, `tags`, `attributes.nbf`, `attributes.exp`, `attributes.enabled` (default true). Returns the SecretBundle with `value` |
| Get secret | `GET /secrets/{name}[/{version}]` | No version: the newest. Returns `value`, `id`, `contentType`, `attributes`, `tags`. A certificate's name returns its `.pfx` (see Certificates) |
| List secrets | `GET /secrets` | `id`, `attributes`, `tags`, `contentType`. Certificates' secrets are not listed |
| List versions | `GET /secrets/{name}/versions` | Newest first; 404 `SecretNotFound` when none |
| Update properties | `PATCH /secrets/{name}[/{version}]` | No version: the current one. Body `attributes.enabled`, `attributes.nbf`, `attributes.exp`, `contentType`, `tags`. Returns the bundle without `value` |
| Delete | `DELETE /secrets/{name}` | Soft delete. Returns the DeletedSecretBundle: `recoveryId`, `deletedDate`, `scheduledPurgeDate` |
| Deleted | `GET /deletedsecrets`, `GET /deletedsecrets/{name}` | |
| Recover | `POST /deletedsecrets/{name}/recover` | Returns the bundle without `value` |
| Purge | `DELETE /deletedsecrets/{name}` | 204; the name is free again |

### Keys

| Operation | Route | Notes |
|---|---|---|
| Create key | `POST /keys/{name}/create` | `kty` `RSA` or `RSA-HSM` (default `RSA`; anything else is 400 "Key type … is not supported in Stuntman yet (RSA, RSA-HSM)."). `key_size` (default 2048, or the previous version's size). `key_ops` (default all six: `sign`, `verify`, `wrapKey`, `unwrapKey`, `encrypt`, `decrypt`, or the previous version's). `tags`, `attributes.exp`. `attributes.nbf` and `attributes.enabled` are ignored on create |
| Rotate key | `POST /keys/{name}/rotate` | The same as create: a new version with the previous size and operations |
| Get key | `GET /keys/{name}[/{version}]` | `key` as JWK (`kid`, `kty: RSA`, `key_ops`, `n`, `e`), `attributes`, `tags`. A certificate's name returns the certificate's key, with `sign`/`verify` |
| List keys | `GET /keys` | `kid`, `attributes`, `tags` |
| List versions | `GET /keys/{name}/versions` | `kid`, `attributes`, `tags` |
| Update properties | `PATCH /keys/{name}[/{version}]` | As secrets (`enabled`, `nbf`, `exp`, `tags`). Disabling one older version leaves the others working |
| Sign | `POST /keys/{name}[/{version}]/sign` | Body `alg` and `value` (the digest, base64url). `RS256`, `RS384`, `RS512`, `PS256`, `PS384`, `PS512`. Returns `{kid, value}` |
| Verify | `POST /keys/{name}[/{version}]/verify` | Body `alg`, `digest`, `value` (signature). Returns `{value: true\|false}` |
| Wrap / unwrap key | `POST /keys/{name}[/{version}]/wrapkey`, `/unwrapkey` | `RSA-OAEP` (SHA-1), `RSA-OAEP-256`, `RSA1_5`. Returns `{kid, value}` |
| Encrypt / decrypt | `POST /keys/{name}[/{version}]/encrypt`, `/decrypt` | Same algorithms as wrap and unwrap |
| Delete, deleted, recover, purge | `DELETE /keys/{name}`, `GET /deletedkeys[/{name}]`, `POST /deletedkeys/{name}/recover`, `DELETE /deletedkeys/{name}` | As secrets; DeletedKeyBundle carries `key.kid` |

Operation names are case-insensitive (`wrapKey` and `wrapkey`). The cryptography is real: a signature verifies
with the public key outside the vault (the .NET test checks it with `RSA.VerifyHash`), and only the same key
unwraps what it wrapped. Private keys never leave Stuntman (`key_public` gives the public part).

### Certificates

| Operation | Route | Notes |
|---|---|---|
| Create certificate | `POST /certificates/{name}/create` | Reads `policy.x509_props.subject` (default `CN=<name>`), `policy.x509_props.sans.dns_names`, `policy.x509_props.validity_months` (× 30 days; default 365 days). Always an RSA 2048 exportable key, always signed by the Stuntman Test CA. Answers 202 with an operation already `completed` |
| Pending operation | `GET /certificates/{name}/pending` | Always `status: completed` once the certificate exists; else 404 `PendingCertificateNotFound` |
| Import certificate | `POST /certificates/{name}/import` | Body `value`: a base64 `.pfx` with `pwd`, or a PEM with a `-----BEGIN PRIVATE KEY-----` (PKCS#8) block and a `-----BEGIN CERTIFICATE-----` block |
| Get certificate | `GET /certificates/{name}[/{version}]` | `id`, `kid`, `sid`, `x5t` (SHA-1 thumbprint), `cer` (DER, base64), `attributes` (`nbf`/`exp` from the certificate), `tags`, `policy` |
| Get policy | `GET /certificates/{name}/policy` | Built from the certificate: subject, SANs, `validity_months`, `key_props` (`RSA`, size, `exportable: true`, `reuse_key: false`), `secret_props.contentType: application/x-pkcs12`, issuer `Self`, lifetime action `AutoRenew` 30 days before expiry |
| Certificate's secret | `GET /secrets/{name}[/{version}]` | The `.pfx` with the private key, base64, empty password, `contentType: application/x-pkcs12`, `managed: true`, `kid`. This is what `DownloadCertificateAsync` reads |
| Certificate's key | `GET /keys/{name}`, `POST /keys/{name}/sign` … | The certificate's RSA key |
| List, versions, update, delete, deleted, recover, purge | `GET /certificates`, `GET /certificates/{name}/versions`, `PATCH /certificates/{name}[/{version}]`, `DELETE /certificates/{name}`, `/deletedcertificates…` | As secrets; list items carry `x5t` |

A route Stuntman does not know answers 400 `BadParameter`, "`<METHOD> /<path>` is not a Key Vault operation
Stuntman knows".

### Managed identity (port 8025)

| Route | Notes |
|---|---|
| `GET /metadata/identity/oauth2/token` | IMDS. Needs `Metadata` or `X-IDENTITY-HEADER`, else 400 `invalid_request` "Required metadata header not specified" |
| `GET /msi/token`, `GET /MSI/token` | App Service. Needs `X-IDENTITY-HEADER` |

Both read `resource` (default `https://vault.azure.net`) and the identity as described in
[Connect your app](#4-get-a-token-managed-identity). Neither checks `api-version`.

### Control routes (port 8025)

Every tool is also a route under `/api` (and `/e/<env>/api`): for example `PUT /api/keyvault/vaults/{name}`,
`PUT /api/keyvault/secrets/{name}`, `GET /api/keyvault/log`. The full list is in [Tools and routes](#tools-and-routes).
Two routes have no tool:

| Route | Answer |
|---|---|
| `GET /api/keyvault/certificates/{name}/download?vault=…` | The certificate as `.cer` (DER, `application/pkix-cert`); `&as=pfx` gives the `.pfx` with the key, no password. Provocations do not apply |
| `GET /api/keyvault/vaults/{name}/yaml` | The vault's YAML file |

### The vault file

Each vault is `secrets/<vault>.yaml` in the environment folder, the source of truth. `set_vault` and
`set_vault_identity` write it; you can also edit it.

```yaml
kind: azure-key-vault
name: acme-kv
environment: staging
access:
  any_token: true          # default: every token may do everything (refusals are only recorded)
  identities:
    - id: api-mi
      roles: [secrets-user]
      scope: [smtp-*, storage-*]
    - id: worker-mi
      roles: [secrets-user, crypto-user, certificate-user]
      scope: [signing-*]
behaviour:
  throttle: 2000/10s       # or off
  latency: none            # or 300ms
  soft_delete_days: 90
  seed_on_reset: true
subscribers: [http://localhost:8027/in/kv-events]
seed: seed/secrets.yaml
```

Roles, by YAML name (Azure built-in role in brackets):

| Role | Grants |
|---|---|
| `administrator` (Key Vault Administrator) | everything |
| `secrets-officer` (Key Vault Secrets Officer) | read and write secrets |
| `secrets-user` (Key Vault Secrets User) | read secrets |
| `crypto-officer` (Key Vault Crypto Officer) | use and manage keys |
| `crypto-user` (Key Vault Crypto User) | use keys (get, list, sign, verify, wrap, unwrap, encrypt, decrypt) |
| `certificates-officer` (Key Vault Certificates Officer) | read and manage certificates |
| `certificate-user` (Key Vault Certificate User) | read certificates and read secrets (any secret in scope, including the `.pfx`) |

"Write secrets" covers set, update and delete. "Manage keys" covers create, rotate, update and delete. "Manage
certificates" covers create, import, update and delete. `scope` holds item names or prefixes ending in `*`; empty
means the whole vault. Lists check the role with item `*`, so a scoped identity can list only if a pattern is `*`.

## How it behaves

**Order of checks on a data-plane call.** Host → environment (else 404 `no environment for this host`, plain
text) → vault (else 404 `VaultNotFound`) → vault reachability provocation → Bearer token (else the 401 challenge)
→ throttle → latency → the operation (role and scope, then the item provocation, then the item).

**Access.** With `any_token: true` (the default) every call is allowed. What a real vault would have refused is
still recorded on the log entry as `wouldFail` (for example `403 ForbiddenByRbac`), shown by `would_fail` and
`vault_patterns`. With `any_token: false`:

| Caller | Answer |
|---|---|
| Token that is not a JWT and names no known identity | 401 `Unauthorized`, "AKV10032: Invalid issuer or the token has no known object id for this vault.", inner `TokenWithoutKnownOid` |
| JWT whose identity is not in `identities` | 403 `Forbidden`, "Caller is not authorized to perform action on resource. Caller: … · roles: …", inner `ForbiddenByRbac` |
| Identity without a role for the operation | Same 403, listing its roles |
| Identity with the role, item outside `scope` | 403 `Forbidden`, "<id> has no role on <item>: its scope is …", inner `ForbiddenByRbac` |

The deleted-item routes (`/deleted…` list, get, recover, purge), version lists, `pending` and `policy` do not check
roles.

**Disabled versions.** A disabled secret version answers 403 `Forbidden`, "Operation get is not allowed on a
disabled secret.", inner `SecretDisabled`. A disabled key version answers the same for `get` and for each
operation ("Operation sign is not allowed on a disabled key.", inner `KeyDisabled`). A disabled certificate
version refuses its `.pfx` (`GET /secrets/{name}`, `SecretDisabled`); `GET /certificates/{name}` still answers.

**Expiry.** Key operations check `exp` and `nbf`: 403 `Forbidden`, "Operation sign is not allowed on an expired
key." (inner `KeyExpired`) or "… on a not yet valid key." (inner `KeyNotYetValid`). `GET /keys/{name}` still
answers. Secrets are returned after `exp` and before `nbf`: Stuntman does not refuse them.

**Allowed operations.** A key created with `key_ops` refuses the others: 403 `Forbidden`, "Operation encrypt is
not permitted on this key.", inner `KeyOperationForbidden`.

**Crypto errors** answer 400 `BadParameter`: an unknown `alg` ("Algorithm X is not supported for RSA keys (RS256,
RS384, RS512, PS256, PS384, PS512)" or "… (RSA-OAEP, RSA-OAEP-256, RSA1_5)"), a `value` that is not base64url
("value is not base64url"), or a ciphertext from another key ("The ciphertext was wrapped with another key or was
changed"). An import that fails answers 400 with the reason ("The .pfx could not be opened with that password",
"The .pfx has no private key", "The PEM has no PRIVATE KEY (PKCS#8) block").

**Names.** Items: 1 to 127 letters, digits or dashes, else 400 "Names are 1 to 127 letters, digits or dashes".
Vaults: see [Create a vault](#1-create-a-vault).

**Not found.** 404 `SecretNotFound` / `KeyNotFound` / `CertificateNotFound`, "A secret with (name/id) X was not
found in this key vault. If you recently deleted this secret you may be able to recover it using the correct
recovery command." A deleted item answers the same on `GET /secrets/{name}`.

**Soft delete.** Always on (`recoveryLevel: Recoverable+Purgeable`). `soft_delete_days` (default 90) sets
`recoverableDays` and `scheduledPurgeDate`; nothing is purged automatically. Setting a deleted secret's name or
creating a deleted key's name answers 409 `Conflict`, inner `ObjectIsDeletedButRecoverable` ("Secret X is
currently in a deleted but recoverable state, and its name cannot be reused; …"). Creating, importing or renewing
a deleted certificate's name does **not** answer 409: it brings the certificate back with a new version.

**Throttling.** `throttle: N/window` counts the vault's logged operations in the last window (all but `Show`,
`Event`, `SetSecret`, deletes and earlier throttled calls). At N, every call answers 429 `Throttled`, "Request was
not processed because too many requests were received. Reason: VaultRequestTypeLimitReached", `Retry-After: 4`.
`off` turns it off. The real vault's limits are per operation type; Stuntman has one counter per vault.

**Latency.** `latency: 300ms` delays every authenticated data-plane call.

**Versions.** Version ids are 32 hex characters. The current version is the newest, enabled or not: disabling the
current version makes `GET /secrets/{name}` answer 403, it does not fall back to an older one.

**Secret values elsewhere.** Every new secret version (REST or tool) is also written to the environment's
`secrets.yaml`, so `secret://name` references in other services follow the vault's current value.
`secret_used_by` lists the YAML files that reference `secret://name`.

**Events.** On a new secret, key or certificate version, and from a sweep every 60 seconds (`SecretNearExpiry` and
`CertificateNearExpiry` within 30 days of `exp`, `SecretExpired` and `CertificateExpired` after it), Stuntman POSTs
to each URL in `subscribers`, header `aeg-event-type: Notification`, body an Event Grid array of one event:

```json
[{"id": "…", "topic": "/subscriptions/stuntman/resourceGroups/<env>/providers/Microsoft.KeyVault/vaults/acme-kv",
  "subject": "smtp-password", "eventType": "Microsoft.KeyVault.SecretNewVersionCreated", "eventTime": 1767225600000,
  "dataVersion": "1", "metadataVersion": "1",
  "data": {"Id": "https://acme-kv.vault.<base>/secrets/smtp-password/<version>", "VaultName": "acme-kv",
           "ObjectType": "Secret", "ObjectName": "smtp-password", "Version": "<version>", "NBF": null, "EXP": null}}]
```

Each event is sent once per item and version, with a 10 s timeout and no retry. `eventTime` is Unix milliseconds
(Event Grid sends an ISO 8601 string). There is no subscription validation handshake. Point `subscribers` at a
[webhook inbox](webhooks.md) to read them.

**Clock.** Key Vault uses the real time, not the environment clock ([Clock](../clock.md)): `advance_clock` does
not expire keys or send near-expiry events. Use `set_secret_state` / `set_key_state` with `expires: now`, or
`provoke_certificate` `expired`.

**Persistence.** Items, versions, deletions, provocations, events sent and the access log are in the environment's
database; vault settings in `secrets/<vault>.yaml`; the Test CA in `data/keys/`. All survive a restart. On an
environment reset, a vault with `seed_on_reset: true` loads its seed file again.

**Timeline.** Successful reads and crypto operations fold into one row per identity per minute ("api-mi read 4
item(s) in acme-kv"). Changes, refusals, provoked answers and "would fail" calls get a row each. Rows link to
`/secrets/log?vault=<vault>`.

## Provoke failures

Item provocations last until `normal`, or for `for` (`"10m"`). They apply to every gated operation on that item
(get, set, update, delete, create, sign…), not to lists.

| Failure | How | What the app sees | Turn it off |
|---|---|---|---|
| Secret forbidden | `provoke_secret {"name": "smtp-password", "answer": "403"}` | 403 `Forbidden`, inner `ForbiddenByRbac` | `"answer": "normal"` |
| Secret missing | `provoke_secret {…, "answer": "404"}` | 404 `SecretNotFound` | `normal` |
| Secret disabled | `provoke_secret {…, "answer": "disabled"}` | 403, "Operation get is not allowed on a disabled secret.", inner `SecretDisabled` | `normal` |
| Secret throttled | `provoke_secret {…, "answer": "429"}` | 429 `Throttled`, `Retry-After: 4` | `normal` |
| Secret unavailable | `provoke_secret {…, "answer": "503"}` | 503 `ServiceUnavailable` (no `Retry-After`) | `normal` |
| Slow secret | `provoke_secret {…, "answer": "slow"}` | The answer after 3 s | `normal` |
| Key disabled, forbidden, throttled, slow | `provoke_key {"name": "signing", "answer": "disabled"}` (or `403`, `429`, `slow` = 2 s) | As above, inner `KeyDisabled` for disabled | `normal` |
| Wrong signature | `provoke_key {"name": "signing", "answer": "wrong_signature"}` | `sign` returns 200 with a signature that fails `verify` | `normal` |
| Certificate missing | `provoke_certificate {"name": "acme-signing", "answer": "missing"}` | 404 on `GET /certificates/{name}` (`CertificateNotFound`) and on its secret (`SecretNotFound`) | `normal` |
| Certificate expired | `provoke_certificate {…, "answer": "expired"}` | Certificate and `.pfx` re-issued valid from 400 days ago to 35 days ago | `normal` |
| Certificate not yet valid | `provoke_certificate {…, "answer": "not_yet_valid"}` | Valid from 30 days from now | `normal` |
| Untrusted issuer | `provoke_certificate {…, "answer": "untrusted"}` | Signed by a throwaway "Unknown Test CA" | `normal` |
| Wrong `.pfx` password | `provoke_certificate {…, "answer": "wrong_password"}` | `GET /secrets/{name}` returns a `.pfx` protected by a random password (the app's empty password fails) | `normal` |
| Vault down | `set_vault {"name": "acme-kv", "reachability": "503"}` | Every call, the challenge too: 503 `ServiceUnavailable`, `Retry-After: 10` | `"reachability": "normal"` |
| Vault hangs | `set_vault {…, "reachability": "timeout", "for": "30s"}` | The request is held while it lasts (at most 10 minutes), then answered normally | `normal`, or the end of `for` |
| Vault refuses | `set_vault {…, "reachability": "refused"}` | The connection closes after the TLS handshake, no HTTP answer | `normal` |
| Throttling | `set_vault {…, "throttle": "3/10s"}` | 429 after 3 calls in 10 s | `"throttle": "2000/10s"` or `off` |
| Slow vault | `set_vault {…, "latency": "800ms"}` | Every call 800 ms slower | `"latency": "none"` |
| Missing role or scope | `set_vault {…, "any_token": false}` + `set_vault_identity {"id": "api-mi", "roles": ["secrets-user"], "scope": ["smtp-*"]}` | 403 `ForbiddenByRbac` outside the role or scope | `"any_token": true` |
| Disabled version | `set_secret_state {"name": "smtp-password", "enabled": false}` / `set_key_state {"name": "signing", "version": "…", "enabled": false}` | 403 `SecretDisabled` / `KeyDisabled` | `"enabled": true` |
| Expired key | `set_key_state {"name": "signing", "expires": "now"}` | 403 `KeyExpired` on operations | `"expires": "never"` |
| Managed identity fails | `provoke_imds {"answer": "500"}` | 500 `{"error": "internal_error", …}` on both endpoints | `"answer": "normal"` |
| Managed identity slow | `provoke_imds {"answer": "slow"}` | Token after 5 s | `normal` |
| No identity | `provoke_imds {"answer": "no_identity"}` | 400 `{"error": "invalid_request", "error_description": "Identity not found"}` | `normal` |

`provoke_imds` applies to the whole environment, not to one vault. See also [Failures](../failures.md).

## See what the app did

| Question | Tool | Fields that matter |
|---|---|---|
| Every operation | `vault_log {"vault": "acme-kv", "identity": "api-mi", "operation": "GetSecret", "item": "…", "result": "ok \| refused \| would_fail \| 403", "since": "1h", "limit": 500}` | `op` (`GetSecret`, `SetSecret`, `UpdateSecret`, `DeleteSecret`, `ListSecrets`, `GetKey`, `CreateKey`, `Sign`, `Verify`, `WrapKey`, `UnwrapKey`, `Encrypt`, `Decrypt`, `GetCertificate`, `CreateCertificate`, `ImportCertificate`, `Recover`, `Purge`, `Throttled`, `Unavailable`, `Event`, `Show`), `identity`, `client`, `status`, `version`, `ms`, `why`, `wouldFail`, `traceId` |
| One operation in full | `vault_log_entry {"id": 42}` | `request` (method, path, token start, oid, traceparent), the identity's `roles` and `scope` |
| Is the secret cached? | `secret_reads {"name": "smtp-password", "since": "1h"}` | per identity and client: `reads`, `gapMs`, `pattern` ("at start-up only", "about 1 every 6 min per process: cached", "every 40 ms: not cached"), `cached` |
| Problems found | `vault_patterns {"vault": "acme-kv"}` | `kind`: `not_cached` (20+ reads under 60 s apart), `refused`, `unknown_caller`, `would_fail` |
| What a real vault would refuse | `would_fail {"vault": "acme-kv"}` | `operations[]` with `wouldFail` |
| Versions and their reads | `secret_detail`, `key_detail`, `certificate_detail` | `versions[]` (`state`: current, enabled older, disabled, expired; `reads` in 24 h), `readers`, `provoked`, `events`, `refusals`; keys add `operations[]` (count and p95 per Sign, Verify…) and `jwk`; certificates add `subject`, `issuer`, `thumbprint`, `notBefore`, `notAfter`, `daysLeft` |
| Names the app asked for that do not exist | `list_secrets`, `list_keys`, `list_certificates` | `askedFor[]` (404s in 24 h: `name`, `count`, `identity`) |
| A value | `get_secret {"name": "smtp-password", "by": "agent"}` | `value`; logged as `Show` |

Screens: `/secrets` (read counts in the last hour), `/secrets/log` (the access log),
[Timeline](../timeline.md) (service `keyvault`).

## Recipes

1. **Give the app a vault with a secret.**
   ```text
   set_vault {"name": "acme-kv"}
   set_secret {"vault": "acme-kv", "name": "smtp-password", "value": "synthetic-pass-1", "content_type": "text/plain", "tags": {"owner": "messaging"}}
   list_vaults {}     → url for KEYVAULT_URL
   ```
   Add the vault host name to `/etc/hosts`, trust the Test CA, set `IDENTITY_ENDPOINT` and `IDENTITY_HEADER`.

2. **Prove the app caches a secret.** Run the app's traffic, then:
   ```text
   secret_reads {"vault": "acme-kv", "name": "smtp-password", "since": "15m"}
   vault_patterns {"vault": "acme-kv", "since": "15m"}
   ```
   `cached: false` or a `not_cached` finding means it reads the secret on every request.

3. **Make the app survive a secret rotation.**
   ```text
   set_secret {"vault": "acme-kv", "name": "smtp-password", "value": "synthetic-pass-2"}
   set_secret_state {"vault": "acme-kv", "name": "smtp-password", "version": "<old version>", "enabled": false}
   ```
   With `subscribers` set, the app gets `SecretNewVersionCreated`. Check in `vault_log` that it reads the new version
   and no 403 on the old one follows.

4. **Check one identity per app (least privilege).**
   ```text
   set_vault_identity {"vault": "acme-kv", "id": "api-mi", "roles": ["secrets-user"], "scope": ["smtp-*"]}
   set_vault_identity {"vault": "acme-kv", "id": "worker-mi", "roles": ["crypto-user", "certificate-user"], "scope": ["signing-*"]}
   would_fail {"vault": "acme-kv"}                  → what production would refuse, with any_token still on
   set_vault {"name": "acme-kv", "any_token": false}  → now it fails here too
   ```
   Give each app its own `IDENTITY_HEADER` (`api-mi`, `worker-mi`).

5. **Sign with a key and catch a bad signature.**
   ```text
   create_vault_key {"vault": "acme-kv", "name": "signing", "size": 2048, "ops": ["sign", "verify"], "expires": "365d"}
   provoke_key {"vault": "acme-kv", "name": "signing", "answer": "wrong_signature", "for": "10m"}
   ```
   The app's `sign` succeeds; its own `verify` (or a verifier with `key_public {"name": "signing", "format": "pem"}`)
   must reject the result. `provoke_key … "normal"` ends it.

6. **Test certificate expiry handling.**
   ```text
   issue_certificate {"vault": "acme-kv", "name": "acme-signing", "subject": "CN=Acme Signing STG, O=Acme Corp", "days": 365, "san": ["signing.acme.example.com"]}
   provoke_certificate {"vault": "acme-kv", "name": "acme-signing", "answer": "expired"}
   ```
   The app downloads the `.pfx` (`GET /secrets/acme-signing`) and must reject the expired certificate. Then
   `renew_certificate {"vault": "acme-kv", "name": "acme-signing"}` and `"answer": "normal"`.

7. **Make the vault unreachable for a while.**
   ```text
   set_vault {"name": "acme-kv", "reachability": "timeout", "for": "2m"}
   ```
   Calls hang for 2 minutes, then succeed. Check that the app times out, retries and recovers. Use `503` for an
   Azure outage answer and `refused` for a dropped connection.

8. **Test soft delete handling.**
   ```text
   delete_secret {"vault": "acme-kv", "name": "old-key"}
   ```
   The app's `PUT /secrets/old-key` now answers 409 `ObjectIsDeletedButRecoverable`.
   `recover_item {"vault": "acme-kv", "name": "old-key", "kind": "secret"}` or `purge_item {…}` clears it.

## Errors and troubleshooting

| Symptom or error text | Cause | Fix |
|---|---|---|
| Name resolution fails for `<vault>.vault.…` | The host name is not in DNS or `/etc/hosts` (the hosts route does not list vaults) | Add the line, use `extra_hosts`, or a `*.localhost` name |
| TLS error: unknown issuer / certificate verify failed | The app does not trust the Test CA | `REQUESTS_CA_BUNDLE`, `NODE_EXTRA_CA_CERTS`, the system store, or a custom validation callback |
| `404 no environment for this host` (plain text) | The host is under `*.stuntman.local` but no environment has that base domain | Use the base domain from `list_vaults` `url` |
| 404 `VaultNotFound`, "No vault X in staging" | No vault of that name in that environment, or you called by `localhost` / an IP | `set_vault {"name": "X"}`; call by the vault host name |
| Python: "challenge resource … does not match the requested domain" | The SDK compares the challenge with host:port | `verify_challenge_resource=False` |
| 401 "AKV10000: Request is missing a Bearer or PoP token." | No token (normal on the first call: the SDK then fetches one) | If it repeats, the credential fails: check `IDENTITY_ENDPOINT` |
| 401 "AKV10032: …", inner `TokenWithoutKnownOid` | `any_token: false` and the token is not a JWT | Use a managed identity token from Stuntman |
| 403 `ForbiddenByRbac`, "Caller: …" | `any_token: false` and the identity lacks the role, or is not listed | `set_vault_identity` with the role |
| 403 "… has no role on X: its scope is …" | The item is outside the identity's `scope` | Add a pattern to `scope` |
| 403 `SecretDisabled` / `KeyDisabled` | The current version is disabled, or a `disabled` provocation | `set_secret_state` / `set_key_state` `enabled: true`, or `provoke_* normal` |
| 403 `KeyExpired` / `KeyNotYetValid` | The key version's `exp` / `nbf` | `set_key_state {"expires": "never"}` |
| 403 `KeyOperationForbidden` | The key's `key_ops` do not include the operation | Create a new version with the operation in `ops` |
| 409 `ObjectIsDeletedButRecoverable` | The name is soft-deleted | `recover_item` or `purge_item` |
| 429 `Throttled` you did not provoke | Over the vault's `throttle`, or a `429` item provocation | `get_vault` (behaviour, provocations); raise `throttle` |
| Managed identity 400 "The X-IDENTITY-HEADER header is missing or does not match IDENTITY_HEADER." | No header, or `STUNTMAN_IDENTITY_HEADER` is set to another value | Set `IDENTITY_HEADER` in the app to the expected value |
| Managed identity 400 "Required metadata header not specified" | IMDS call without `Metadata: true` | Add the header |
| Every app shows as `app-mi` in the log | No `client_id` and no usable header (or `STUNTMAN_IDENTITY_HEADER` is set) | Set `IDENTITY_HEADER` per app, or pass `client_id` |
| `bindError` in `list_vaults` | Port 8444 is taken | Free it or set `STUNTMAN_KEYVAULT_PORT` |
| `set_secret` tool: "That looks like a real token" | The value starts with `sk_live` or `AKIA`, or is a JWT over 200 characters | Use a synthetic value (the REST API does not check) |
| 400 "… is not a Key Vault operation Stuntman knows" | The operation is not implemented | See Not done |

## Not done

- Only RSA keys. EC keys (`EC`, `EC-HSM`), `oct` keys and their algorithms (`ES256`…, `A256KW`…) answer 400.
- No key import (`PUT /keys/{name}`), backup and restore, `release`, `getrandombytes`, or rotation policy
  (`/keys/{name}/rotationpolicy`). `rotate` is a plain new version.
- No secret or certificate backup and restore; no certificate merge, contacts, issuers or certificate operation
  cancel/delete. Certificate creation ignores the policy's key type, size, issuer and `reuse_key`: always RSA 2048,
  always the Test CA, completed at once.
- `GET /certificates/{name}` ignores a disabled version. Secrets are not refused after `exp` or before `nbf`.
- Lists have no paging (`maxresults` is ignored, `nextLink` is always `null`) and no `/secrets` listing of
  certificate-backed secrets.
- Soft delete cannot be turned off, and soft-deleted items are never purged automatically.
- Bearer token signatures, issuer and audience are not checked; PoP tokens are not supported.
- One throttle counter per vault, not per operation type.
- The Key Vault port takes no `/e/<env>/` path prefix: one environment per host name.
- Management plane (ARM: creating vaults, access policies, network rules) is not served; use `set_vault`.
- Events have no retry and no Event Grid subscription validation; `eventTime` is a number.
- The environment clock does not apply.

Missing something? Tell the developer what the fix would be and offer a pull request — see [Contributing](../../CONTRIBUTING.md).

## Tools and routes

<!-- tools: keyvault -->
_Generated by `tools/gen_tool_reference.py` from the engine source; each tool is an MCP tool and the HTTP route shown (under `/api`)._

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_vaults` | `GET /api/keyvault/vaults` | — | — | Vaults in this environment: name, URL, counts of secrets, keys and certificates. |
| `list_secrets` | `GET /api/keyvault/secrets` | — | vault | Secrets of a vault: names, current version, state, expiry and reads in the last hour. |
| `secret_detail` | `GET /api/keyvault/secrets/{name}/detail` | name | vault | One secret without its value: versions with reads, who reads it, provocation, events, and the files that reference secret://<name>. |
| `get_secret` | `GET /api/keyvault/secrets/{name}` | name | version, vault, by | The value (synthetic) and attributes of a secret version; recorded in the access log. |
| `set_secret` | `PUT /api/keyvault/secrets/{name}` | name, value | expires, content_type, tags, vault, by | A new version of a secret (creates it); SecretNewVersionCreated goes to subscribers. Values are synthetic: never paste a real one. |
| `set_secret_state` | `PATCH /api/keyvault/secrets/{name}` | name | enabled, expires, version, vault | Disables, enables or expires a secret version (default the current one). |
| `provoke_secret` | `POST /api/keyvault/secrets/{name}/provoke` | name, answer | for, vault, by | Makes the next reads of a secret fail: 403, 404, disabled, 429, 503 or slow (3 s), for a while or until changed; normal stops it. |
| `secret_reads` | `GET /api/keyvault/secrets/{name}/reads` | name | since, vault | Who read a secret, how often, and whether it looks cached (per process). |
| `secret_used_by` | `GET /api/keyvault/used-by/{name}` | name | — | Files in this environment that point at secret://<name> (also before the secret exists). |
| `delete_secret` | `DELETE /api/keyvault/secrets/{name}` | name | vault | Soft-deletes a secret (recreating the name gets 409 until recovered or purged). |
| `recover_item` | `POST /api/keyvault/recover/{name}` | name | kind, vault | Recovers a soft-deleted secret, key or certificate. |
| `purge_item` | `DELETE /api/keyvault/deleted/{name}` | name | kind, vault | Purges a soft-deleted secret, key or certificate for good: the name is free again. |
| `list_keys` | `GET /api/keyvault/keys` | — | vault | Keys of a vault: type, allowed operations, versions, operations in the last hour. |
| `key_detail` | `GET /api/keyvault/keys/{name}/detail` | name | vault | One key: operations in the last hour (with p95), versions, who uses it, provocation, public key. |
| `create_vault_key` | `POST /api/keyvault/keys/{name}` | name | type, size, ops, expires, tags, vault | Creates a key, or a new version of it (rotation; KeyNewVersionCreated to subscribers). RSA 2048/3072/4096. |
| `set_key_state` | `PATCH /api/keyvault/keys/{name}` | name | version, enabled, expires, vault | Disables or enables a key version (default the current one). |
| `delete_key` | `DELETE /api/keyvault/keys/{name}` | name | vault | Soft-deletes a key: GET gets 404 KeyNotFound, it is listed under deleted keys until recovered (recover_item) or purged (purge_item). |
| `provoke_key` | `POST /api/keyvault/keys/{name}/provoke` | name, answer | for, vault, by | Makes the next operations on a key fail: disabled, 403, 429, slow (2 s) or wrong_signature (sign returns one that fails verify). |
| `key_public` | `GET /api/keyvault/keys/{name}/public` | name | format, vault | The public part of a key, as JWK or PEM, to verify outside. |
| `list_certificates` | `GET /api/keyvault/certificates` | — | vault | Certificates: subject, issuer, validity, days left, readers. |
| `certificate_detail` | `GET /api/keyvault/certificates/{name}/detail` | name | vault | One certificate: subject, issuer, thumbprint, validity, how to get it, readers, provocation. |
| `issue_certificate` | `POST /api/keyvault/certificates/{name}` | name | subject, days, san, vault | Issues a certificate from the Stuntman Test CA (RSA 2048 key, exportable). |
| `import_certificate` | `POST /api/keyvault/certificates/{name}/import` | name, pfx_base64 | password, vault | Imports your own test certificate (.pfx in base64 with its password, or a PEM with key and certificate). |
| `renew_certificate` | `POST /api/keyvault/certificates/{name}/renew` | name | vault | A new version signed by the Test CA (same subject and validity); CertificateNewVersionCreated. |
| `provoke_certificate` | `POST /api/keyvault/certificates/{name}/provoke` | name, answer | for, vault, by | Next reads of a certificate: missing (404), expired, not_yet_valid, wrong_password (.pfx) or untrusted (issuer); normal stops it. |
| `get_vault` | `GET /api/keyvault/vaults/{name}` | — | name | The vault block: identities with roles and scopes, managed identity endpoint, throttling, latency, soft delete, subscribers. |
| `set_vault` | `PUT /api/keyvault/vaults/{name}` | name | any_token, throttle, latency, soft_delete_days, subscribers, identities, seed_on_reset, reachability, for, by | Creates or changes a vault: any_token, throttle (2000/10s), latency, soft_delete_days, subscribers, seed_on_reset; reachability (503, timeout, refused, normal; with for) takes the whole vault down. A new vault imports this environment's secret:// values. |
| `set_vault_identity` | `PUT /api/keyvault/identities/{id}` | id | roles, scope, vault | Adds or changes an identity: roles (administrator, secrets-user, secrets-officer, crypto-user, crypto-officer, certificate-user, certificates-officer) and name scopes. No roles removes it. |
| `provoke_imds` | `POST /api/keyvault/imds/provoke` | answer | for, by | The managed identity endpoint answers 500, slow (5 s) or no_identity; normal stops it. |
| `would_fail` | `GET /api/keyvault/would-fail` | — | since, vault | Operations allowed only because any token is on: what a real vault would have refused. |
| `vault_log` | `GET /api/keyvault/log` | — | identity, operation, item, result, since, limit, vault | Every operation on vault items, newest first: identity, operation, item, answer (kept 24 h). |
| `vault_log_entry` | `GET /api/keyvault/log/{id}` | id | — | One operation: caller claims, request, answer, trace id. |
| `vault_patterns` | `GET /api/keyvault/patterns` | — | since, vault | Findings in the access log: values read on every request, refusals, unknown callers, operations allowed only by any token. |
<!-- /tools -->
