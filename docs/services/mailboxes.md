# Mailboxes (IMAP and POP3)

Mailboxes are the mail your app **reads**: a supplier e-mails an invoice, a customer replies to a ticket. Stuntman
stands in for the IMAP or POP3 server of that mailbox. You put a message in with a tool; the app finds it on its
next poll (or at once, if it waits in IDLE). Stuntman keeps each mailbox with its folders, messages and flags, and a
log of every command the app sent, without passwords. For mail your app *sends*, see [Email](email.md).

## At a glance

| | |
|---|---|
| **Stands in for** | An IMAP4rev1 server (with `STARTTLS`, `AUTH=PLAIN`, `AUTH=XOAUTH2`, `IDLE`, `MOVE`, UID commands) and a POP3 server (with `STLS`, `SASL PLAIN`, `UIDL`, `TOP`). Tested with Python `imaplib` and `poplib` |
| **Ports** | IMAP `1143` (`STUNTMAN_IMAP_PORT`), POP3 `1110` (`STUNTMAN_POP3_PORT`). TLS by `STARTTLS` / `STLS` on the same ports; no implicit-TLS ports (993, 995) |
| **Point your app at** | host `localhost`, port `1143` (IMAP) or `1110` (POP3); user = the mailbox address; password = the one you set (or any but `wrong`) |
| **It keeps** | Mailboxes (address, password, provocation, folders, UIDVALIDITY, next UID); messages (raw RFC 822, folder, UID, arrival time, `\Seen`, `\Deleted`, `\Flagged`); a log of the app's commands per mailbox (passwords masked) |
| **You can provoke** | `set_mailbox` `provoke`: `login_refused`, `busy` (mailbox in use), `drop_fetch` (connection drops mid-`FETCH`/`RETR`), `tls_required` (plain logins refused before STARTTLS), `token_expired` (XOAUTH2 refused) |
| **Screen** | `/mailboxes` |
| **Tools** | `list_mailboxes`, `set_mailbox`, `put_mail`, `list_mailbox_messages`, `clear_mailbox`, `delete_mailbox` |

## Connect your app

1. Make the mailbox (or let `put_mail` make it):
   ```text
   set_mailbox {"address": "invoices@acme.example", "password": "s3cret"}
   ```
2. Point the app's IMAP or POP3 client at `localhost:1143` or `localhost:1110`, user `invoices@acme.example`,
   password `s3cret`. Use STARTTLS / STLS with certificate checks off, or plain text.

```python
import imaplib, poplib, ssl
ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE

m = imaplib.IMAP4("localhost", 1143); m.starttls(ctx); m.login("invoices@acme.example", "s3cret"); m.select("INBOX")

p = poplib.POP3("localhost", 1110); p.stls(ctx); p.user("invoices@acme.example"); p.pass_("s3cret")
```

XOAUTH2 (Gmail and Microsoft 365 style): any non-empty bearer token logs in.

```python
m = imaplib.IMAP4("localhost", 1143)
m.authenticate("XOAUTH2", lambda _: b"user=invoices@acme.example\x01auth=Bearer ya29.any\x01\x01")
```

**Passwords.** A mailbox with no password takes any non-empty password except `wrong`. A password can be a literal,
`secret://<name>` (a value in the environment's `secrets.yaml`) or `env:<VAR>` (a process environment variable).
`list_mailboxes` shows `"password": "set"` or `"any (not "wrong")"`, never the value.

**TLS.** STARTTLS and STLS use the same self-signed certificate as SMTP, made at each start for `localhost` and
`stuntman.local`. Turn certificate validation off in the client.

**Docker.** Publish `1143` and `1110` (`docker-compose.yml` does). From a container use `host.docker.internal` (Stuntman
on the host) or `stuntman` (same compose project).

**Environments.** IMAP and POP3 have no host name or path to tell environments apart: one port serves every
environment, and every login goes to the **default** environment's mailboxes. Put messages in through the default
environment's API (`/api/mailboxes/…`, not `/e/<env>/api/…`) when the app reads them over IMAP or POP3.

## What it supports

### Mailboxes and folders

- A mailbox is an address (`name@domain`; lower-cased). It starts with one folder, `INBOX`.
- More folders come from IMAP `CREATE`. `put_mail` with a `folder` that was never created stores the message, but
  IMAP `SELECT` of that folder answers `NO [NONEXISTENT]` until the app (or you, over IMAP) creates it.
- UIDs come from one counter per mailbox, shared by all folders, and are never reused (also after `EXPUNGE`,
  `clear_mailbox` or POP3 `DELE`). UIDVALIDITY is fixed when the mailbox is made; `delete_mailbox` and a new
  `set_mailbox` give a new one.

### IMAP commands

Greeting: `* OK [CAPABILITY IMAP4rev1 STARTTLS AUTH=PLAIN AUTH=XOAUTH2 IDLE UIDPLUS MOVE] Stuntman IMAP ready`.

| Command | Notes |
|---|---|
| `CAPABILITY` | the same list; adds `LOGINDISABLED` before TLS when any mailbox has `tls_required` |
| `STARTTLS` | `OK Begin TLS negotiation now`, then TLS |
| `LOGIN user pass` | quoted strings and literals (`{n}` and `{n+}`) both work |
| `AUTHENTICATE PLAIN` / `XOAUTH2` | initial response on the line or after `+ ` |
| `NOOP`, `CHECK` | report changes since the last look: `* n EXPUNGE` for each message gone, then `* n EXISTS` when the count changed |
| `LOGOUT` | `* BYE`, then `OK` |
| `LIST`, `LSUB` | reference ignored; pattern `*`, `%`, an exact name, or a `prefix*`. Every folder is `(\HasNoChildren) "/"`. `LSUB` lists every folder |
| `CREATE`, `DELETE` | add or remove a folder (`DELETE INBOX` does nothing). `DELETE` hides the folder; its messages are not removed |
| `SUBSCRIBE`, `UNSUBSCRIBE` | answer `OK`, change nothing |
| `STATUS folder (…)` | always returns `MESSAGES`, `UNSEEN`, `UIDNEXT`, `UIDVALIDITY`, `RECENT 0`, whatever was asked |
| `SELECT`, `EXAMINE` | `FLAGS (\Seen \Deleted \Flagged \Answered \Draft)`, `EXISTS`, `0 RECENT`, `[UNSEEN n]` (first unseen), `[UIDVALIDITY]`, `[UIDNEXT]`, `[PERMANENTFLAGS (\Seen \Deleted \Flagged)]`, then `[READ-WRITE]` or `[READ-ONLY]`. Folder names are case-insensitive |
| `FETCH` | see below |
| `STORE set FLAGS / +FLAGS / -FLAGS [.SILENT] (…)` | only `\Seen`, `\Deleted`, `\Flagged` are kept; untagged `FETCH (FLAGS …)` unless `.SILENT` |
| `SEARCH` | see below |
| `COPY set folder` | a copy with a new UID; target must exist, else `NO [TRYCREATE]` |
| `MOVE set folder` | the message keeps its UID and changes folder; untagged `* n EXPUNGE` per message |
| `EXPUNGE` | removes `\Deleted` messages of the selected folder, `* n EXPUNGE` each (nothing in `EXAMINE`) |
| `CLOSE` | expunges silently (unless read-only) and deselects |
| `IDLE` … `DONE` | `+ idling`; checks every second and sends `EXPUNGE` / `EXISTS` as messages come and go |
| `UID FETCH`, `UID STORE`, `UID SEARCH`, `UID COPY`, `UID MOVE` | sets are UIDs; `UID FETCH` always includes `UID` |
| anything else (`APPEND`, `RENAME`, `NAMESPACE`, `ID`, `ENABLE`, `UNSELECT`, `GETQUOTAROOT`…) | `BAD <cmd> is not supported` |

`FETCH` items:

| Item | Answer |
|---|---|
| `ALL`, `FAST`, `FULL` | the standard macros |
| `UID`, `FLAGS`, `INTERNALDATE` (`"15-Nov-2026 14:02:11 +0000"`), `RFC822.SIZE` | as stored |
| `ENVELOPE` | date, subject, from, sender, reply-to, to, cc, bcc, in-reply-to, message-id |
| `BODY`, `BODYSTRUCTURE` | the MIME tree with type, subtype, parameters, id, description, encoding, size, lines (text); `BODYSTRUCTURE` adds boundary and disposition |
| `BODY[]`, `BODY[HEADER]`, `BODY[TEXT]`, `BODY[HEADER.FIELDS (…)]`, `BODY[HEADER.FIELDS.NOT (…)]` | whole message, header, body, chosen headers |
| `BODY[1]`, `BODY[2.1]`, `BODY[n.MIME]`, `BODY[n.HEADER]`, `BODY[n.TEXT]` | one MIME part (still in its transfer encoding, e.g. base64) |
| `BODY[…]<start.count>` | a partial |
| `BODY.PEEK[…]` | the same, without setting `\Seen` |
| `RFC822`, `RFC822.HEADER`, `RFC822.TEXT` | as `BODY[]`, `BODY.PEEK[HEADER]`, `BODY[TEXT]` |

`BODY[…]`, `RFC822` and `RFC822.TEXT` set `\Seen` (not in `EXAMINE`) and add `FLAGS (\Seen)` to the answer. Unknown
items are skipped. Sequence sets: `n`, `n:m`, `*`, `n:*`, comma lists.

`SEARCH` keys:

| Key | Works |
|---|---|
| `ALL`, `SEEN`, `UNSEEN`, `NEW`, `DELETED`, `UNDELETED`, `FLAGGED`, `UNFLAGGED` | yes |
| `UID <set>`, a sequence set | yes |
| `FROM <s>` | substring of the sender address or name, case-insensitive |
| `SUBJECT <s>` | substring of the subject, case-insensitive |
| `TEXT <s>`, `BODY <s>` | substring of the raw message, case-insensitive |
| `NOT <key>` | yes |
| `SINCE`, `BEFORE`, `ON`, `SENTSINCE`, `SENTBEFORE`, `LARGER`, `SMALLER` | accepted, **ignored** (every message matches) |
| `OR`, `TO`, `CC`, `HEADER`, `ANSWERED`, `KEYWORD`, `CHARSET`… | accepted, match everything (`OR a b` therefore acts as `a AND b`) |

### POP3 commands

Greeting `+OK Stuntman POP3 ready`. POP3 sees `INBOX` only.

| Command | Notes |
|---|---|
| `CAPA` | `USER`, `UIDL`, `TOP`, `STLS`, `SASL PLAIN` |
| `STLS` | `+OK Begin TLS`, then TLS |
| `USER`, `PASS` | the password may contain spaces |
| `AUTH PLAIN <b64>` | the initial response must be on the same line |
| `STAT` | count and total size of the messages not marked for deletion |
| `LIST [n]`, `UIDL [n]` | `UIDL` gives the IMAP UID |
| `RETR n` | the message, dot-stuffed; marks it `\Seen` |
| `TOP n lines` | headers plus `lines` body lines (the `+OK` line reports the full size) |
| `DELE n`, `RSET` | mark / unmark; deletions happen at `QUIT` |
| `NOOP`, `QUIT` | |
| other (`APOP`, `LAST`, `UTF8`…) | `-ERR Unknown command` |

Before login, `STAT`, `LIST`, `RETR`… answer `-ERR Log in first`.

### Putting mail in

`put_mail` builds a MIME message, or takes a whole one:

| Argument | Meaning |
|---|---|
| `address` (required) | the mailbox; made if it does not exist (no password: any but `wrong`) |
| `from` | default `someone@globex.example` |
| `subject`, `text`, `html` | `html` makes a `multipart/alternative` |
| `attachments` | `[{filename, content_type, base64 | text}]`, base64-encoded in a `multipart/mixed` |
| `raw` | a whole RFC 822 message instead of the fields above |
| `folder` | default `INBOX` |

The built message has `Date`, `Message-ID` and `MIME-Version`. It answers `{"address", "folder", "uid"}`.

```text
put_mail {"address": "invoices@acme.example", "from": "Billing <billing@globex.example>", "subject": "Invoice 1234",
          "text": "Attached.", "attachments": [{"filename": "invoice-1234.xml", "content_type": "application/xml", "text": "<invoice/>"}]}
  → {"address": "invoices@acme.example", "folder": "INBOX", "uid": 1}
```

## How it behaves

| Situation | IMAP | POP3 |
|---|---|---|
| Unknown mailbox | `NO [AUTHENTICATIONFAILED] Invalid credentials` | `-ERR [AUTH] Authentication failed` |
| Wrong password | `NO [AUTHENTICATIONFAILED] Invalid credentials` | `-ERR [AUTH] Authentication failed` |
| Command before login | `BAD Not authenticated` | `-ERR Log in first` |
| Command that needs a folder before `SELECT` | `NO Select a mailbox first` | — |
| `SELECT` of a folder that does not exist | `NO [NONEXISTENT] No such mailbox` | — |
| `COPY`/`MOVE` to a folder that does not exist | `NO [TRYCREATE] No such mailbox` | — |
| Idle connection | closed after 30 minutes without a command | closed after 10 minutes |

- **Other clients' changes.** An open IMAP session hears messages removed elsewhere (`clear_mailbox`, another
  session's `EXPUNGE`) as `* n EXPUNGE` and new ones as `* n EXISTS` on its next `NOOP`/`CHECK` or during `IDLE`. A
  POP3 session keeps the list it read at login, as POP3 does: messages added or cleared later do not show until the
  next login.
- **Flags.** `\Answered`, `\Draft` and keywords are not kept. `\Recent` is always 0.
- **UIDNEXT** is the folder's highest UID plus one.
- **Persistence.** Mailboxes, messages, flags and the log are in the environment's database and survive restarts.
  The clock ([Clock](../clock.md)) does not apply; `INTERNALDATE` is the real arrival time.
- **Timeline.** IMAP and POP3 sessions do not make Timeline rows ([Timeline](../timeline.md)); they count in `usage`
  as `email`. Read what the app did from the mailbox log.

## Provoke failures

All with `set_mailbox {"address": "...", "provoke": "<kind>"}`; `"provoke": "none"` turns it off.

| Failure | What the app sees (IMAP) | What the app sees (POP3) |
|---|---|---|
| `login_refused` | `NO [AUTHENTICATIONFAILED] Invalid credentials` for `LOGIN` and `AUTHENTICATE` (XOAUTH2 too), whatever the password | `-ERR [AUTH] Authentication failed` |
| `busy` | login works; every `SELECT` / `EXAMINE` answers `NO [INUSE] Mailbox is locked by another session` | `-ERR [IN-USE] Mailbox locked by another session` at `PASS` / `AUTH` |
| `drop_fetch` | the first message of any `FETCH` is cut in half and the connection closes | `RETR` or `TOP` sends half and closes |
| `tls_required` | before `STARTTLS`: `CAPABILITY` shows `LOGINDISABLED`; `LOGIN` and `AUTHENTICATE PLAIN` answer `NO [PRIVACYREQUIRED] Use STARTTLS first` (XOAUTH2 still works) | before `STLS`: `-ERR [AUTH] Use STLS first` |
| `token_expired` | `AUTHENTICATE XOAUTH2` gets a `+ ` challenge with `{"status":"401","schemes":"Bearer","scope":"https://mail.google.com/"}` (base64), then, after the client's empty line, `NO [AUTHENTICATIONFAILED] Invalid credentials (Failure)`; passwords still work | no effect (POP3 has no XOAUTH2 here) |

A mailbox has one provocation at a time. `login_refused`, `tls_required` and `token_expired` act at login, so an
open session keeps working. `busy` acts at each IMAP `SELECT`/`EXAMINE` (POP3: at login). `drop_fetch` is read at
login and stays with that session: set it before the app connects, and clear it before the app reconnects.

## See what the app did

| Need | Tool | HTTP |
|---|---|---|
| Every mailbox with message and unread counts, connect strings, provocations | `list_mailboxes` | `GET /api/mailboxes` |
| One folder's messages and the command log | `list_mailbox_messages {"address", "folder"}` | `GET /api/mailboxes/{address}/messages?folder=INBOX` |

`list_mailbox_messages` returns `messages[]` with `uid`, `at`, `from`, `subject`, `attachments` (names), `seen`,
`deleted`, `flagged`, `size`, and `log[]` (the last 100 lines, newest first) with `at`, `proto` (`IMAP`, `POP3`,
`Stuntman`), `line` (the command; passwords and tokens shown as `****`, cut at 300 characters) and `result` (`OK`,
`NO wrong password`, `refused (provoked)`, `connection dropped mid-FETCH (provoked)`, `uid 3` for a `put_mail`…).
Commands are logged after the login succeeds. Assert with it: the app logged in, ran `UID SEARCH UNSEEN`, fetched
with `BODY.PEEK[]`, set `\Seen` or moved the message, and expunged.

## Recipes

1. **Deliver a supplier's invoice the app must pick up.**
   ```text
   set_mailbox {"address": "invoices@acme.example", "password": "s3cret"}
   put_mail {"address": "invoices@acme.example", "from": "billing@globex.example", "subject": "Invoice 1234",
             "text": "Attached.", "attachments": [{"filename": "invoice-1234.pdf", "content_type": "application/pdf", "base64": "JVBERi0x…"}]}
   list_mailbox_messages {"address": "invoices@acme.example"}      → check "seen": true after the app's poll
   ```
2. **Check the app moves processed mail.** Have the app `CREATE` (or use an existing) folder, poll, then:
   `list_mailbox_messages {"address": "invoices@acme.example", "folder": "Processed"}`; the `INBOX` list is empty.
3. **Test a reply-to-ticket flow with a real MIME message.**
   `put_mail {"address": "support@acme.example", "raw": "From: Emma Carter <emma.carter@example.com>\r\nTo: support@acme.example\r\nSubject: Re: [#4417] Login issue\r\nIn-Reply-To: <4417@acme.example>\r\n\r\nStill broken.\r\n"}`
4. **Start each test run empty.** `clear_mailbox {"address": "invoices@acme.example"}` → `{"removed": 3}`. The
   password and folders stay; UIDs keep growing. `folder` empties one folder only.
5. **Make the connection drop mid-download and check the app retries.**
   ```text
   set_mailbox {"address": "invoices@acme.example", "provoke": "drop_fetch"}
   # the app's fetch fails; it should not mark the message processed
   set_mailbox {"address": "invoices@acme.example", "provoke": "none"}
   ```
6. **Make an OAuth token expire.** `set_mailbox {"address": "invoices@acme.example", "provoke": "token_expired"}`;
   the app's XOAUTH2 login fails and it should refresh its token.
7. **Require TLS.** `set_mailbox {"address": "invoices@acme.example", "provoke": "tls_required"}`; an app that logs
   in before `STARTTLS` gets `PRIVACYREQUIRED`.

## Errors and troubleshooting

| Symptom or error text | Cause | Fix |
|---|---|---|
| `NO [AUTHENTICATIONFAILED]` with the right password | the mailbox is in another environment than the default, or does not exist, or `login_refused` is set | make it in the default environment; `list_mailboxes`; `provoke: none` |
| Login fails with password `wrong` | `wrong` is the one password refused by a mailbox without a password | use another password |
| `NO [NONEXISTENT] No such mailbox` on `SELECT` | the folder was never created over IMAP (a `put_mail` `folder` alone does not create it) | send `CREATE <folder>` first |
| `BAD APPEND is not supported` | the app saves sent mail or drafts with `APPEND` | not supported; turn that off in the app |
| `SEARCH SINCE …` returns everything | date and size keys are ignored | filter by date in the app, or `clear_mailbox` between runs |
| POP3 login hangs after `AUTH PLAIN` | the client waits for `+ ` before sending credentials | send the credentials on the `AUTH PLAIN` line, or use `USER`/`PASS` |
| TLS handshake fails | the client checks the self-signed certificate | turn certificate validation off for this host |
| `POP3: port 1110 is taken` / `IMAP: port 1143 is taken` in Stuntman's output | another program holds the port | free it or set `STUNTMAN_POP3_PORT` / `STUNTMAN_IMAP_PORT` |
| `address: name@domain …` from `set_mailbox` | the address has no `@` | use a full address |
| `404 No mailbox …` from `clear_mailbox` / `delete_mailbox` | no such mailbox | `list_mailboxes` |

## Not done

- No implicit TLS (IMAPS 993, POP3S 995); TLS only by `STARTTLS` / `STLS`, with a self-signed certificate.
- IMAP: no `APPEND`, `RENAME`, `UNSELECT`, `NAMESPACE`, `ID`, `ENABLE`, `CONDSTORE`/`QRESYNC`, `ESEARCH`, quotas or
  `LITERAL+` advertising (`{n+}` is accepted anyway). `UIDPLUS` is advertised but `APPENDUID`/`COPYUID` are not sent,
  and `UID EXPUNGE` expunges every `\Deleted` message of the folder. No nested folders. Date, size, `OR`, `TO`,
  `HEADER` search keys do not filter. Only `\Seen`, `\Deleted`, `\Flagged` are stored.
- POP3: no `APOP`, no `AUTH PLAIN` with a continuation line, no `AUTH` listing.
- One port for all environments: only the default environment's mailboxes can be read.
- No Timeline rows for mailbox sessions.
- A mailbox has one provocation at a time; there is no "first N" or "for 10m" bound.

Missing something? Tell the developer what the fix would be and offer a pull request — see [Contributing](../../CONTRIBUTING.md).

## Tools and routes
<!-- tools: mailboxes -->
_Generated by `tools/gen_tool_reference.py` from the engine source; each tool is an MCP tool and the HTTP route shown (under `/api`)._

| Tool | HTTP | Required | Optional | What it does |
|---|---|---|---|---|
| `list_mailboxes` | `GET /api/mailboxes` | — | — | The mailboxes the app reads over IMAP or POP3, with their messages and unread counts, and how to connect. |
| `set_mailbox` | `PUT /api/mailboxes/{address}` | address | password, provoke | Creates or changes a mailbox the app reads: its address (the login), a password (empty: any but "wrong") and a provocation: login_refused, busy, drop_fetch, tls_required, token_expired (none clears). |
| `put_mail` | `POST /api/mailboxes/{address}/messages` | address | from, subject, text, html, attachments, raw, folder | Puts a message in a mailbox as if someone sent it: from, subject, text (and html), attachments [{filename, content_type, base64 or text}] — or raw (a whole RFC 822 message). The app finds it on its next poll. |
| `list_mailbox_messages` | `GET /api/mailboxes/{address}/messages` | address | folder | The messages in a mailbox (a folder): sender, subject, flags (read, deleted), size; and what the app did on it (IMAP/POP3 commands, without passwords). |
| `clear_mailbox` | `DELETE /api/mailboxes/{address}/messages` | address | folder | Empties a mailbox (every folder, or one): its messages go, the mailbox, password and folders stay; UIDs are not reused. An open IMAP session hears the EXPUNGEs on its next command; a POP3 session already logged in keeps what it saw. |
| `delete_mailbox` | `DELETE /api/mailboxes/{address}` | address | — | Removes a mailbox with its messages and its log. |
<!-- /tools -->
