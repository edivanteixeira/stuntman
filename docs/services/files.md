# Files (FTP, FTPS and SFTP)

Stuntman stands in for the FTP, FTPS and SFTP servers that partners and your app exchange files on: EDI drops,
bank remittance files, nightly exports. Your app connects with its normal FTP or SFTP client and lists, downloads,
uploads, renames and deletes files. Each server is a folder on disk. FTP, FTPS and SFTP all see the same files.
Stuntman keeps the servers, their users, the files, and a log of every login and transfer.

## At a glance

| | |
|---|---|
| **Stands in for** | Any FTP server (replies modelled on vsftpd), explicit FTPS (`AUTH TLS`), and an OpenSSH-style SFTP server (SFTP subsystem, protocol version 3). Tested with Python `ftplib` (`FTP`, `FTP_TLS`) and `paramiko` |
| **Ports** | `2121` FTP and explicit FTPS on the same port (`STUNTMAN_FTP_PORT`); passive data ports: any free port, or the range in `STUNTMAN_FTP_PASSIVE_PORTS` (`30000-30009` in Docker); `2222` SFTP (`STUNTMAN_SFTP_PORT`). No implicit FTPS port |
| **Point your app at** | host `localhost`, port `2121` (FTP/FTPS) or `2222` (SFTP); login `user` or `user@server`; FTPS trusts the [Test CA](../configuration.md#the-stuntman-test-ca) |
| **It keeps** | servers and users in `files/servers.yaml`; files under `files/<server>/`; the SFTP host key in `files/ssh_host_ed25519`; every login and transfer (`list_transfers`) |
| **You can provoke** | `login_refused` (530 / auth rejected) · `disk_full` (552 / `SSH_FX_FAILURE` on uploads) · `permission_denied` (550 / `SSH_FX_PERMISSION_DENIED`) · `drop_mid` (transfer breaks after 4 KB) · `slow` (about 20 KB/s) · `host_key_changed` (SFTP shows another host key) · read-only users |
| **Screen** | `/ftp` (**Files** in the menu) |
| **Tools** | `list_file_servers`, `set_file_server`, `list_files`, `get_file`, `put_file`, `delete_file`, `list_transfers`, `wait_for_file` |

## Connect your app

### Make a server first

A login works only for a user that a server declares. Create the server with `set_file_server`:

```text
set_file_server {"name": "edi-acme", "title": "edi.acme.example",
                 "users": [{"user": "edi", "password": "s3cret"}, {"user": "auditor", "read_only": true}],
                 "folders": ["/in", "/out"]}
```

- `name`: lowercase letters, digits, dots and dashes. It is also the folder `files/<name>/`.
- A user with no `password` accepts any non-empty password except `wrong`.
- `password` can be a literal, `secret://<name>` (read from the environment's `secrets.yaml`) or `env:<VAR>` (read
  from Stuntman's own environment variables).
- A server with no users gets one user `edi` with any password.

### Login names

| Login | Goes to |
|---|---|
| `edi` | the first server (in `servers.yaml` order) that has a user `edi` |
| `edi@edi-acme` | the user `edi` of the server `edi-acme` |

Use `user@server` when two servers have the same user name.

### Hosts

| App runs | FTP / FTPS | SFTP |
|---|---|---|
| On the host, Stuntman on the host or in Docker with the ports published | `localhost:2121` | `localhost:2222` |
| In a container of the same compose project | `stuntman:2121` (the service name), see passive mode below | `stuntman:2222` |
| In a container, Stuntman on the host | `host.docker.internal:2121` | `host.docker.internal:2222` |

Passive mode and Docker:

- `EPSV` answers only a port (`229 Entering Extended Passive Mode (|||30001|)`). The client connects to the same host
  as the control connection. This works from every place above.
- `PASV` answers an IPv4 address. The address is `STUNTMAN_FTP_PASSIVE_HOST` if set (an IP; a host name is ignored),
  else the local address of the control connection. The Docker image sets `STUNTMAN_FTP_PASSIVE_HOST=127.0.0.1`, so
  a client on the host works, and a client in another container that trusts the PASV address does not. Use EPSV, a
  client that ignores the PASV address (Python `ftplib` does by default), or set `STUNTMAN_FTP_PASSIVE_HOST` to the
  IP the app reaches Stuntman on.
- In Docker only `30000-30009` are published. Each `PASV`/`EPSV` holds one port until its transfer starts, so at most
  ten passive transfers wait at once. When no port is free the reply is `425 Cannot open a data connection`.

Active mode (`PORT`, `EPRT`): Stuntman connects back only to the IP the control connection came from. Through NAT
(for example the Docker bridge), the address the client announces is usually not the address Stuntman sees, and the
reply is `500 Illegal PORT command.`. Use passive mode across NAT.

### Environments

The FTP and SFTP ports serve the **default** environment only: every login goes to its servers. The tools and HTTP
routes work in every environment (`/e/<env>/api/files/...`, `/e/<env>/mcp`), but servers made in another
environment cannot be reached over FTP or SFTP. See [Environments](../environments.md).

### FTPS

Explicit FTPS only: connect in plain text to `2121`, send `AUTH TLS` (or `AUTH SSL`), then `PBSZ 0` and `PROT P` to
encrypt the data connections too. The server certificate is issued by the Stuntman Test CA for the name
`localhost` only (no IP, no other names). A client that checks the host name must connect to `localhost`; from a
container (`stuntman`, `host.docker.internal`) turn the host-name check off in test configuration, or the TLS
handshake fails. Download the CA from `http://localhost:8025/api/test-ca.pem`.

### Snippets

Python, FTP and FTPS (as in `engine/tests/e2e_files.py`):

```python
import ftplib, io, ssl

f = ftplib.FTP(timeout=15)
f.connect("localhost", 2121)
f.login("edi", "s3cret")
f.cwd("/in")
print(f.nlst())
f.storbinary("STOR /out/ORDERS_1042.txt", io.BytesIO(b"000GLOBEX ORDERS 1042\r\n"))
f.quit()

ctx = ssl.create_default_context(cafile="stuntman-test-ca.pem")
t = ftplib.FTP_TLS(context=ctx, timeout=15)
t.connect("localhost", 2121)
t.login("edi", "s3cret")   # sends AUTH TLS first
t.prot_p()                 # PBSZ 0 + PROT P
```

Python, SFTP with paramiko:

```python
import paramiko

t = paramiko.Transport(("localhost", 2222))
t.connect(username="edi", password="s3cret")
sf = paramiko.SFTPClient.from_transport(t)
print(sf.listdir("/in"))
with sf.open("/out/ORDERS_1042.txt", "wb") as fh:
    fh.write(b"000GLOBEX ORDERS 1042\r\n")
t.close()
```

Command line:

```sh
curl -s ftp://edi:s3cret@localhost:2121/in/                                   # list
curl -s -T ORDERS_1042.txt ftp://edi:s3cret@localhost:2121/out/               # upload
curl -s --ssl-reqd --cacert stuntman-test-ca.pem ftp://edi:s3cret@localhost:2121/in/   # FTPS
sftp -P 2222 -o User=edi@edi-acme localhost
ssh-keyscan -p 2222 localhost                                                 # the host key to pin
```

`list_file_servers` returns a `connect` block per server with the FTP, FTPS and SFTP strings for the configured
ports.

## What it supports

### FTP commands

Every command except `USER`, `PASS`, `AUTH`, `PBSZ`, `PROT`, `FEAT`, `SYST`, `NOOP`, `QUIT` and `OPTS` needs a login
first; before it the reply is `530 Please log in with USER and PASS`.

| Command | Reply and notes |
|---|---|
| `USER` | `331 Password required`. Never refused at this step |
| `PASS` | `230 Logged in`, or `530 Login incorrect` (unknown user, wrong password, `login_refused`) |
| `AUTH TLS` / `AUTH SSL` | `234 Proceed with negotiation`, then the TLS handshake. `431 TLS is not available` if the Test CA cannot make a certificate. Other `AUTH` arguments: `502` |
| `PBSZ` | `200 PBSZ=0` |
| `PROT P` / `PROT C` | `200 Protection level set to Private` / `Clear`. `P` puts the data connections in TLS |
| `SYST` | `215 UNIX Type: L8` |
| `FEAT` | `UTF8 PASV EPSV SIZE MDTM REST STREAM MLSD AUTH TLS PBSZ PROT` |
| `OPTS`, `TYPE`, `MODE`, `STRU`, `NOOP`, `ALLO` | `200 <CMD> ok`. Accepted and ignored: `TYPE A` does no line-ending conversion; every transfer is binary |
| `PWD` / `XPWD` | `257 "/in" is the current directory`. The root `/` is the server folder |
| `CWD` / `XCWD` / `CDUP` | `250 Directory changed`, or `550 No such directory` (also for a file) |
| `PASV` | `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)`; see [Connect your app](#hosts) for the address. The listener waits 15 s for the client |
| `EPSV` | `229 Entering Extended Passive Mode (\|\|\|port\|)` |
| `PORT` / `EPRT` | `200 PORT command successful. Consider using PASV.` (`EPRT` with `\|1\|` IPv4 or `\|2\|` IPv6). Address not the client's own IP: `500 Illegal PORT command.`; unreadable: `501 Syntax error in PORT`. Stuntman connects to the address on the next transfer, with a 10 s timeout |
| `LIST` | `ls -l` lines: `-rw-r--r-- 1 ftp ftp         1200 Nov 15 01:42 OCOREN_0142.txt` (folders `drwxr-xr-x`), sorted by name, time in UTC, never a year. Flags such as `-la` are ignored; one path argument is honoured |
| `NLST` | names only, one per line |
| `MLSD` | `type=file;size=1200;modify=20261115014200; OCOREN_0142.txt` (`type=dir` for folders). No `.` or `..` entries |
| `RETR` | downloads; honours a preceding `REST` offset |
| `STOR` | uploads; creates or truncates the file. After `REST n` (any `n > 0`) it appends instead |
| `APPE` | appends; creates the file if missing |
| `DELE` | `250 Deleted`, or `550 No such file` |
| `MKD` / `XMKD` | `257 "/out/new" created`. Creates missing parent folders too |
| `RMD` / `XRMD` | `250 Removed`, or `550 Cannot remove (not empty?)` |
| `RNFR` + `RNTO` | `350 Ready for RNTO`, then `250 Renamed`. `RNTO` without `RNFR`: `503 RNFR first`; target folder missing: `550 Rename failed` |
| `SIZE` | `213 <bytes>`, or `550 No such file` (also for a folder) |
| `MDTM` | `213 YYYYMMDDHHMMSS` (UTC), or `550 No such file` |
| `REST` | `350 Restarting at <n>`; used once by the next `RETR`/`STOR` |
| `QUIT` | `221 Goodbye` |
| anything else | `502 <CMD> not implemented` (for example `STAT`, `SITE`, `HELP`, `ABOR`, `MLST`, `STOU`, `CCC`, `REIN`) |

Transfer replies: `150` on start, then `226 Transfer complete` (`226 Listing sent` for listings),
`426 Connection closed; transfer aborted` when `drop_mid` cuts it, `425 Can't open data connection.` when the data
connection does not open.

### SFTP operations

SSH with password authentication, then the `sftp` subsystem. The host key is Ed25519.

| Operation | Notes |
|---|---|
| password auth | accepted for a declared user; rejected otherwise (200 ms delay after a rejection) |
| `realpath` | normalizes the path (`.` and empty mean `/`); does not check that it exists |
| `stat`, `lstat`, `fstat` | real size, times and mode from disk; `SSH_FX_NO_SUCH_FILE` if missing |
| `opendir`, `readdir` | all entries in one batch, then EOF; no `.` or `..` |
| `open` | read, write, create, truncate, append flags honoured. Folder missing: `SSH_FX_NO_SUCH_FILE` |
| `read`, `write` | at the offset the client gives |
| `close` | logs the transfer (bytes read or written) |
| `remove`, `mkdir` (creates parents), `rmdir`, `rename` | `SSH_FX_FAILURE` when the file system call fails |
| `setstat`, `fsetstat` | answer OK and change nothing (mode and times are not kept) |
| anything else (`symlink`, `readlink`, extensions…) | `SSH_FX_OP_UNSUPPORTED` |

Only the `sftp` subsystem is served: no shell, no `exec`, so `scp` (which runs a command) does not work.

### Tools

| Tool | What it does |
|---|---|
| `set_file_server` | creates or changes a server: `name` (required), `title`, `users` (`[{user, password, read_only}]`, replaces the whole list), `folders` (made on disk, replaces the list), `provoke` (a kind, or `none` to clear). Fields you leave out stay as they are |
| `list_file_servers` | every server: users (password shown as `set` or `any (not “wrong”)`), `provoke`, file and folder counts, `connect` strings, and the list of provocations |
| `list_files` | every file and folder under `path` (default `/`), recursively, sorted by path: `path`, `dir`, `size`, `modified` (ms) |
| `get_file` | one file: `text` if it is valid UTF-8, else `base64`; plus `path`, `size` |
| `put_file` | writes a file "as the partner": `text` or `base64`. Creates missing parent folders. Overwrites |
| `delete_file` | deletes one file (not a folder) |
| `list_transfers` | the log, newest first; `server` filters, `limit` default 200 |
| `wait_for_file` | waits until a file matches `path`; `timeout` default `30s` |

There is no tool to delete a server or a folder; remove the entry from `files/servers.yaml` or delete the folder
over FTP/SFTP.

## How it behaves

- **Folders.** A folder exists only when `set_file_server` declares it (`folders`), `put_file` writes under it, or a
  client makes it (`MKD`, `mkdir`). An upload into a folder that does not exist fails and makes no file: FTP
  `553 Could not create file.` (vsftpd's answer), SFTP `SSH_FX_NO_SUCH_FILE` (OpenSSH's). `CWD` into it answers `550`.
  A download from a missing folder: FTP `550 No such file`, SFTP `SSH_FX_NO_SUCH_FILE`.
- **Listing a missing folder** over FTP answers an empty listing with `226`, not `550`.
- **Paths** are clamped to the server folder: `..` above `/` stays at `/`. Relative FTP paths resolve against the
  current directory; SFTP paths resolve against `/`.
- **Read-only users** (`read_only: true`). FTP: `STOR`, `APPE`, `DELE`, `MKD`, `RMD`, `RNFR`, `RNTO` answer
  `550 Permission denied`. SFTP: opening for write, `remove`, `mkdir`, `rmdir`, `rename` answer
  `SSH_FX_PERMISSION_DENIED`. Reads work.
- **Provocations are read at each command.** A `provoke` set while a client is connected applies to its next
  transfer. `login_refused` applies at the next login. `host_key_changed` applies at the next SSH connection.
- **Overwrite and partial files.** `STOR` replaces a file. A transfer cut by `drop_mid` leaves the partial upload on
  disk (the first 4096 bytes).
- **Resume.** `REST n` + `RETR` starts reading at byte `n`. `REST n` + `STOR` appends to the existing file, whatever
  `n` is, so resume only after `SIZE` gives the same `n`.
- **Timeouts.** FTP control connection: closed after 600 s with no command. Passive listener: 15 s for the client to
  connect, then `425`. Active connect: 10 s. SSH: closed after 600 s of inactivity.
- **Host key.** Made on first SFTP connection and kept in `files/ssh_host_ed25519`, so a client that pinned it keeps
  trusting it across restarts.
- **Clock.** File times and log times are the real wall clock, not the [environment clock](../clock.md). `LIST`,
  `MLSD` and `MDTM` show UTC.
- **Persistence.** Servers (`files/servers.yaml`), files and the host key are files in the data folder and survive
  restarts. The transfer log is in the SQLite database (`files_log`).
- **No locks.** Two clients writing the same file at once both write; the last one wins.
- **No quotas or size limits**, except the `disk_full` provocation.

## Provoke failures

Set with `set_file_server {"name": "<server>", "provoke": "<kind>"}`. One provocation per server. Clear with
`"provoke": "none"`. The screen has the same buttons.

| Failure | How | What the app sees (FTP / FTPS) | What the app sees (SFTP) | Turn off |
|---|---|---|---|---|
| Login refused | `provoke: login_refused` | `530 Login incorrect` after `PASS` | authentication rejected | `provoke: none` |
| Wrong password | log in with a wrong password, or `wrong` for a user with no password | `530 Login incorrect` | authentication rejected | use the right password |
| Disk full | `provoke: disk_full` | `552 Requested file action aborted: exceeded storage allocation` on `STOR`/`APPE`. Reads, `MKD`, `DELE` still work | `SSH_FX_FAILURE` when opening a file for write | `provoke: none` |
| Permission denied | `provoke: permission_denied` | `550 Permission denied` on `LIST`/`NLST`/`MLSD`, `RETR`, `STOR`/`APPE`, `DELE`, `MKD`, `RMD`, `RNFR`/`RNTO`. `CWD`, `SIZE`, `MDTM` still work | `SSH_FX_PERMISSION_DENIED` on `opendir`, `open` (read or write), `remove`, `mkdir`, `rename`. `rmdir` and `stat` still work | `provoke: none` |
| Read-only account | user with `read_only: true` | `550 Permission denied` on writes | `SSH_FX_PERMISSION_DENIED` on writes | `set_file_server` with `read_only: false` |
| Transfer dropped halfway | `provoke: drop_mid` | exactly 4096 bytes move, the data connection closes, then `426 Connection closed; transfer aborted` (downloads and uploads; listings are not cut) | after 4096 bytes have been read or written, the next `read`/`write` fails with `SSH_FX_CONNECTION_LOST` | `provoke: none` |
| Slow transfer | `provoke: slow` | data in 2 KB chunks with 100 ms between them: about 20 KB/s | 100 ms per 2 KB of each `read`/`write` request (at least 100 ms per request) | `provoke: none` |
| Host key changed | `provoke: host_key_changed` on **any** server | no effect | every SSH connection, to every server, gets a new random host key (a new one each connection). A client with the old key pinned refuses to connect | `provoke: none` (the kept key comes back) |
| No such folder | upload into an undeclared folder | `553 Could not create file.` | `SSH_FX_NO_SUCH_FILE` | declare it in `folders` |
| Active mode refused | `PORT`/`EPRT` to another IP than the client's | `500 Illegal PORT command.` | — | use the client's own IP or passive mode |
| Data connection fails | passive client never connects (15 s), or the active address is closed | `425 Can't open data connection.` | — | — |

## See what the app did

| Need | Use | Fields |
|---|---|---|
| What the app uploaded | `wait_for_file {"server": "edi-acme", "path": "/out/*.txt"}` | the first matching file: `path`, `dir`, `size`, `modified` |
| The content | `get_file {"server": "edi-acme", "path": "/out/OCOREN_0142.txt"}` | `text` or `base64`, `size` |
| Everything on the server | `list_files {"server": "edi-acme", "path": "/out"}` | `files[]` |
| Logins and transfers | `list_transfers {"server": "edi-acme"}` | `at` (ms), `server`, `user`, `proto` (`FTP`, `FTPS`, `SFTP`, or `Stuntman` for your own tool calls), `op` (`login`, `get`, `put`, `delete`, `rename`), `path` (`from → to` for renames), `size`, `ok`, `detail` |

`wait_for_file` matching:

- With `*`: a glob over the whole path, which always starts with `/`. `/out/*.txt` matches; `*.txt` matches too;
  `out/*.txt` never matches.
- Without `*`: any file whose path contains the text, e.g. `OCOREN_0142`.
- It matches files that were already there before the call. Delete old files first (`delete_file`) when you wait for
  a new one.
- Timeout: `408 No file like /out/*.txt on edi-acme yet`.

`detail` tells why a row failed: `530 wrong password`, `530 login refused (provoked)`, `550 read-only user`,
`550 permission denied (provoked)`, `552 disk full (provoked)`, `553 no such folder`,
`426 dropped mid-transfer (provoked); the partial file stays`, and on SFTP `wrong password`,
`no space left (provoked)`, `no such folder`, `dropped mid-transfer (provoked)`. A login with an unknown user name is
not logged. `user` is the login name as the client typed it on FTP (for example `edi@edi-acme`).

The [Timeline](../timeline.md) gets a trip for every `login`, `put`, `get` and `delete` (not renames or folder
changes), labelled `<proto> <op> <path> · <size> B`, with outcome `ok`, `failed`, or `provoked` when a provocation
caused it; clicking a row opens `/ftp?s=<server>` (`/files` is the Storage screen). The `/ftp` screen shows the folders, the files (click one for a preview), a box to put a file as the
partner, the transfers, and the provocation buttons; `?s=<server>` opens one server.

## Recipes

1. **The partner drops a file; the app must pick it up.**
   ```text
   set_file_server {"name": "edi-acme", "users": [{"user": "edi", "password": "s3cret"}], "folders": ["/in", "/out"]}
   put_file        {"server": "edi-acme", "path": "/in/ORDERS_1042.txt", "text": "000GLOBEX ORDERS 1042\r\n"}
   ```
   Run the app's poller against `localhost:2121` (user `edi`, password `s3cret`). Check that it downloaded:
   `list_transfers {"server": "edi-acme"}` shows `op: get`, `path: /in/ORDERS_1042.txt`, `ok: true`. If the app
   deletes or moves the file after reading, `list_files {"server": "edi-acme", "path": "/in"}` is empty.

2. **Assert what the app uploads.**
   ```text
   delete_file   {"server": "edi-acme", "path": "/out/OCOREN_0142.txt"}
   (run the app)
   wait_for_file {"server": "edi-acme", "path": "/out/*.txt", "timeout": "60s"}
   get_file      {"server": "edi-acme", "path": "/out/OCOREN_0142.txt"}
   ```
   Compare `text` with the expected content. If the app uploads as `.tmp` and renames, `list_transfers` shows a
   `put` of the `.tmp` path, then a `rename` row `… .tmp → … .txt`.

3. **Make the upload retry survive a transfer cut halfway.**
   ```text
   set_file_server {"name": "edi-acme", "provoke": "drop_mid"}
   ```
   The app's next upload or download over 4 KB gets `426` (FTP) or `SSH_FX_CONNECTION_LOST` (SFTP). Check that it
   reports the failure and retries, then clear the provocation so the retry succeeds:
   ```text
   set_file_server {"name": "edi-acme", "provoke": "none"}
   wait_for_file   {"server": "edi-acme", "path": "/out/*.txt"}
   ```
   The partial upload (4096 bytes) stays on the server until the app overwrites it; `get_file` shows its `size`.

4. **Handle a full disk and a refused login.**
   ```text
   set_file_server {"name": "edi-acme", "provoke": "disk_full"}        → STOR answers 552
   set_file_server {"name": "edi-acme", "provoke": "login_refused"}    → PASS answers 530
   set_file_server {"name": "edi-acme", "provoke": "none"}
   ```
   `list_transfers` shows each attempt with `ok: false` and the `detail`.

5. **Test SFTP host key pinning.**
   ```sh
   ssh-keyscan -p 2222 localhost > known_hosts     # pin the kept key
   ```
   ```text
   set_file_server {"name": "edi-acme", "provoke": "host_key_changed"}
   ```
   The app's next SFTP connection sees another key and must refuse it. `provoke: none` brings the pinned key back.

6. **Two partners with the same user name.**
   ```text
   set_file_server {"name": "edi-acme",   "users": [{"user": "edi", "password": "acme-pass"}],   "folders": ["/in"]}
   set_file_server {"name": "edi-globex", "users": [{"user": "edi", "password": "globex-pass"}], "folders": ["/in"]}
   ```
   Configure the app with `edi@edi-acme` and `edi@edi-globex`. A plain `edi` always goes to `edi-acme` (the first).

7. **A read-only partner account.**
   ```text
   set_file_server {"name": "edi-acme", "users": [{"user": "edi", "password": "s3cret"}, {"user": "auditor", "password": "s3cret", "read_only": true}]}
   ```
   `auditor` can list and download; every write answers `550` / `SSH_FX_PERMISSION_DENIED`. `users` replaces the whole
   list, so repeat every user you want to keep.

## Errors and troubleshooting

| Symptom or error text | Cause | Fix |
|---|---|---|
| `530 Login incorrect` | no server declares the user; wrong password; password `wrong`; or `login_refused` is set | `list_file_servers`; check `provoke`; use `user@server` |
| `530 Please log in with USER and PASS` | a command sent before login | log in first |
| Login works over the API but not with the app in another environment | the FTP and SFTP ports serve only the default environment | make the server in the default environment |
| `553 Could not create file.` / `SSH_FX_NO_SUCH_FILE` on upload | the target folder does not exist | add it to `folders`, or `MKD` it first |
| `550 No such directory` on `CWD` | the folder does not exist | as above |
| Hangs after `PASV`, then `425 Can't open data connection.` | the client connects to the PASV address (`127.0.0.1` in Docker) from another container | use EPSV, a client that ignores the PASV address, or set `STUNTMAN_FTP_PASSIVE_HOST` to the IP the app reaches |
| `425 Cannot open a data connection` right after `PASV` | every port in `STUNTMAN_FTP_PASSIVE_PORTS` is in use | fewer parallel transfers, or a wider range (publish it too) |
| `500 Illegal PORT command.` | active mode through NAT, or a `PORT` address that is not the client's IP | passive mode |
| TLS handshake fails: certificate verify failed | the app does not trust the Test CA | add `stuntman-test-ca.pem` to the client's trust store |
| TLS handshake fails: host name mismatch | the certificate is for `localhost` only | connect to `localhost`, or turn the host-name check off in test configuration |
| FTPS client on port 990 or "implicit" mode gets no answer | only explicit FTPS (`AUTH TLS`) on `2121` | switch the client to explicit FTPS |
| `502 AUTH not implemented` | an `AUTH` argument other than `TLS`/`SSL` (for example `TLS-C`) | use `AUTH TLS` |
| `502 <CMD> not implemented` | the command is not served (`STAT`, `SITE CHMOD`, `MLST`, `ABOR`…) | avoid it, or see Not done |
| SFTP client: "Host key verification failed" | `host_key_changed` is set on some server, or the data folder (and `files/ssh_host_ed25519`) was reset | `provoke: none`; re-pin with `ssh-keyscan -p 2222 localhost` |
| SFTP client fails to negotiate a host key algorithm | the host key is Ed25519 only | use a client that supports `ssh-ed25519` |
| `scp` fails | only the `sftp` subsystem is served | use SFTP |
| Line endings changed in a real server, not here | `TYPE A` is accepted and ignored: bytes are stored as sent | expected; Stuntman always transfers binary |
| `wait_for_file` returns at once with an old file | it matches files already there | `delete_file` the old one first, or match a unique name |
| `408 No file like … yet` | no matching file within `timeout` | check the path pattern starts with `/` or has no `*`; raise `timeout` |
| `404 No file server “x” (set_file_server)` | the server does not exist in this environment | `set_file_server` |
| `400 name: lowercase letters, digits, dots and dashes` | invalid server name | rename |

## Not done

- Implicit FTPS (TLS from the first byte, usually port 990).
- FTP commands `STAT`, `SITE` (no `CHMOD`), `HELP`, `ABOR`, `MLST`, `STOU`, `CCC`, `REIN`, `ACCT`; `AUTH TLS-C`.
- ASCII mode conversion (`TYPE A` is ignored).
- A login over FTP/SFTP to any environment other than the default one.
- An FTPS certificate for names other than `localhost`.
- SSH public-key and keyboard-interactive authentication; only passwords.
- Host key types other than Ed25519.
- SSH `exec` and shell, so no `scp`.
- SFTP `symlink`, `readlink` and extensions; `setstat` is accepted but times and modes are not kept.
- A `MKD`/`mkdir` whose parent is missing succeeds (it creates the parents), where a real server fails.
- `permission_denied` does not block SFTP `rmdir`, nor FTP `CWD`, `SIZE`, `MDTM`.
- Quotas, per-user home folders (every user sees the whole server folder), and a tool to delete a server or a folder.
- The environment clock does not move file or log times.

Missing something? Tell the developer what the fix would be and offer a pull request — see [Contributing](../../CONTRIBUTING.md).

## Tools and routes

<!-- tools: files -->
_Generated by `tools/gen_tool_reference.py` from the engine source; each tool is an MCP tool and the HTTP route shown (under `/api`)._

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
<!-- /tools -->
