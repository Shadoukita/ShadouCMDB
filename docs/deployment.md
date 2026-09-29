# Building and running `shadoucmdb`

`shadoucmdb` is the ShadouCMDB server as one self-contained binary: HTTP API,
health checks, embedded web UI and the admin commands (migrations, seed data,
schema checks). The binary ships for **Linux x64**, **Linux ARM64** and
**Windows Server x64**, plus a multi-arch **Docker image**.

PostgreSQL is always **external**. Configure it through `DATABASE_URL` or the
`PG*` variables. Every variable is documented in [`.env.example`](../.env.example).

The connection to PostgreSQL verifies the server certificate and host name by default
(`DATABASE_SSL=verify-full`). If the certificate comes from a private CA or a managed service
(Amazon RDS, Azure Database for PostgreSQL), set `DATABASE_SSL_CA_FILE` to that CA bundle. The host
in `PGHOST` or `DATABASE_URL` must match a name in the certificate. `DATABASE_SSL=require` skips
the certificate check and logs a warning at startup; use it only while you fix the certificate.

## Commands

```
shadoucmdb [--env-file PATH] [--log-file PATH] <COMMAND>

  serve                     Run the HTTP server (/api/v1, /healthz, /readyz, web UI; /openapi.json and /docs with API_DOCS)
  migrate [--adopt-drizzle] Apply pending migrations; re-running is a no-op
  seed [--template KEY]... [--demo]
                            Check system rows; install a starter template (it_infrastructure);
                            --demo installs it and adds a sample inventory. Idempotent
  verify                    Schema acceptance checks in a rolled-back transaction
  audit-verify [--allow-gaps]
                            Check the audit_log hash chain and print its head; exits 1 if it is broken
  create-admin --username U [--display-name N] [--email E] [--password-stdin]
                            Create a user holding the built-in Administrator profile
  prune-audit --older-than 180d [--scope auth|changes] [--execute]
                            Delete audit_log rows past the retention window; a dry run without --execute
  backup [--out F]          Write a consistent backup of all data and settings, then check it
  restore FILE [--replace] [--dry-run] [--yes]
                            Check a backup and restore it in one transaction
  factory-reset [--yes]     Delete everything and return to first-run setup
  decommission [--yes]      Remove every ShadouCMDB table, row and setting before retiring the database
                            (backup, restore and reset: see docs/backup-and-reset.md)
  openapi [--out F|--check F]  Print the OpenAPI document, write it, or fail if F is stale
  service install|uninstall|run   Windows Service management (Windows only)
```

- `--env-file PATH` loads variables from a file. Variables already set in the
  environment take precedence. Without the flag, `./.env` is loaded if it exists.
- `--log-file PATH` appends the JSON logs to a file instead of stdout. The Windows
  Service needs it, because a service has no console.
- Logs are one JSON object per line. Every request is logged with its
  `request_id`, which comes from `X-Request-Id` or is generated, and is echoed in
  the response.
- `migrate`, `restore`, `factory-reset` and `decommission` connect with `MIGRATION_DATABASE_URL`
  when it is set, `prune-audit` only with
  `MAINTENANCE_DATABASE_URL`; everything else uses `DATABASE_URL` / `PG*`. See
  [Database roles](#database-roles).
- `serve` does **not** migrate on start. Run `migrate` as an explicit step when
  you install or upgrade.
- `SIGTERM` or Ctrl+C (Linux), or a service Stop (Windows), triggers a graceful
  shutdown: the listener closes, in-flight requests finish, then the pool closes.

Typical first run against a new database:

```sh
shadoucmdb migrate
shadoucmdb seed            # bare data model; --template it_infrastructure or --demo for a starter set
shadoucmdb verify          # optional, writes nothing
shadoucmdb serve           # http://<host>:3000/readyz
```

Then open the web UI and create the first administrator, or run `create-admin` (below).

## The first administrator

Every page and API call except the health probes needs a signed-in user. On a new
installation there are no users yet, and there are two ways to create the first one:

- **In the browser:** while no user exists, the UI offers first-run setup
  (`GET /api/v1/setup` reports `setupRequired: true`; `POST /api/v1/setup` creates the
  account and signs it in). It only works while the user table is empty.
- **On the command line**, from any machine that can reach the database:

  ```sh
  shadoucmdb create-admin --username admin --display-name "Jane Admin"          # prompts twice for the password
  printf '%s\n' "$ADMIN_PASSWORD" | shadoucmdb create-admin --username admin --password-stdin   # scripts
  ```

`create-admin` works at any time, not only on an empty database: it is also the way back in
if every administrator is locked out or has forgotten their password (create a second
administrator, sign in, reset the other account). It needs the database fully migrated and
records `actor_type = system` in the audit log. Passwords need at least 12 characters and are
stored as argon2id hashes.

Further users, and the permission profiles that decide what they may do, are managed under
Administration in the UI (`/api/v1/admin/users`, `/api/v1/admin/profiles`). See
[api.md](api.md#authentication-and-permissions).

## HTTPS and session cookies

For TLS settings, network layout, database roles and backups, see the
[hardening guide](security/hardening.md).

`shadoucmdb` serves plain HTTP; put a TLS-terminating reverse proxy (nginx, Caddy, Traefik,
a cloud load balancer) in front of it for anything beyond a lab. Sessions are cookies:

- The proxy must pass `X-Forwarded-Proto: https` (or `Forwarded: proto=https`). The server
  then marks the cookies `Secure`, so a browser never sends them over plain HTTP. If the proxy
  cannot send that header, set `COOKIE_SECURE=always`. With the default `COOKIE_SECURE=auto`,
  the first session cookie issued without `Secure` logs a one-time warning naming this fix;
  `COOKIE_SECURE=never` is taken as deliberate and is not warned about.
- Serve the UI and the API from the same origin (the embedded UI does this). A UI on another
  origin needs that origin in `CORS_ORIGINS`, spelled exactly as the browser sends it
  (`https://cmdb.example.com`: no path, no trailing slash); those origins may send the session
  cookie. `*` and anything that is not an origin stop the server at startup.
- The proxy should set `X-Forwarded-For` (or `Forwarded: for=`) to the client address,
  **replacing** any value the client sent. The API records the first address in it (else the
  TCP peer) with sign-in events in the audit log. Without a proxy that overwrites the header a
  client can claim any address, and so can one behind a proxy that *appends* to it, so treat
  it as evidence, never as an access control. When the recorded address is not the TCP peer,
  the row also has `peerIpAddress`: the proxy's address, or the real client's when there is
  no proxy, which the client cannot forge.
- `SESSION_IDLE_TIMEOUT_MINUTES` (default 12 h) and `SESSION_MAX_AGE_HOURS` (default 7 days)
  bound how long a session lives. Sessions are stored in PostgreSQL, so they survive restarts
  and work across several instances. The login backoff counters are per process.

The server sets these security headers itself, on every response it sends (the web UI, the
API and Swagger UI at `/docs`). Do not add them again at the proxy: two
`Content-Security-Policy` headers are two independently enforced policies, and duplicate values
of the others are confusing at best. In particular, a proxy cannot bolt violation reporting
onto our policy: a second header containing only `report-uri`/`report-to` is a separate policy
that blocks nothing and so reports nothing, while violations of ours still go nowhere. The
proxy's only alternative is to strip our header and serve a complete policy of its own, which
drifts from ours with every release. Use `CSP_REPORT_URI` instead (below). Since this release that includes `Permissions-Policy`,
`X-Frame-Options` and `Cross-Origin-Opener-Policy`: remove any copies a proxy adds.

| Header | Value | Sent on |
| --- | --- | --- |
| `X-Content-Type-Options` | `nosniff` | every response |
| `Referrer-Policy` | `no-referrer` | every response |
| `X-Frame-Options` | `DENY` | every response. CSP `frame-ancestors 'none'` covers HTML documents; this header keeps API responses, assets and uploaded logos out of frames too. ShadouCMDB cannot be embedded in another site's frame, a portal or an intranet dashboard |
| `Permissions-Policy` | `accelerometer=(), bluetooth=(), camera=(), display-capture=(), geolocation=(), gyroscope=(), hid=(), magnetometer=(), microphone=(), midi=(), payment=(), serial=(), usb=()` | every response. Clipboard access is left at the browser default: the UI copies API token secrets and recovery codes |
| `Cross-Origin-Opener-Policy` | `same-origin` | every response. Pages on other origins opened from ShadouCMDB, or that open it, get no `window.opener` handle to it. Browsers apply it on HTTPS only |
| `Strict-Transport-Security` | `max-age=31536000; includeSubDomains` | only requests that arrived over HTTPS (`X-Forwarded-Proto: https` or `Forwarded: proto=https`); never over plain HTTP, so a lab or LAN install is not locked onto a scheme it cannot serve |
| `Content-Security-Policy` | `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'; form-action 'self'` | HTML documents only (the web UI and `/docs`), not JSON |
| `Cache-Control` | `no-store` | every `/api/` response, so no shared cache stores one user's data. The web UI keeps its own caching: `assets/*` `public, max-age=31536000, immutable`, `index.html` `no-cache` |
| `Vary` | `Cookie` (appended to any `Vary` the CORS layer set) | every `/api/` response |

API responses are authenticated by the session cookie and carry per-user data, including the
CSRF token in `/api/v1/auth/me`. A proxy or CDN that rewrites `Cache-Control` on `/api/`
responses, or caches them regardless, can serve one user's data to another; do not put a
caching layer in front of `/api/`. If the deployment ran behind a caching CDN before this
header was sent, purge it: `no-store` does not evict what was already stored.

`includeSubDomains` tells browsers to use HTTPS for every subdomain of the host name the
server is reached by, for a year. If that name has subdomains that cannot serve HTTPS, have the
proxy strip or override the header. Because of `frame-ancestors 'none'` the UI cannot be
embedded in a frame on any site, including your own. `connect-src 'self'` means the embedded
UI can only call the API on its own origin. If you point the UI at another origin
(`VITE_API_BASE_URL` at build time, or `apiBaseUrl` in `config.js`), serve that UI from your
own web server and set its CSP there.

`CSP_REPORT_URI` (default unset: no reporting) makes browsers report CSP violations, so a policy
break in production shows up somewhere other than a browser console nobody watches. Point it at
an absolute `http`/`https` URL or at a path on this server (`/csp-reports`) where your own
collector listens; reports go only there, the ShadouCMDB project never receives them. Setting it
appends `; report-uri <uri>` to the policy. When the request arrived over HTTPS and the URI is
`https:` or a path, it also appends `; report-to csp` and sends `Reporting-Endpoints: csp="<uri>"`.
Chromium and Firefox ignore `report-uri` whenever `report-to` is present, and drop Reporting API
endpoints that are not HTTPS, so on plain HTTP or with an `http:` collector `report-uri` is sent
alone, since it is the one that still delivers. A collector on another origin needs no change to
`connect-src`: reports are not subject to the page's policy. Whitespace, `;`, `,`, `"`, a scheme
other than `http`/`https` and `user:password@` are rejected at startup, because the value becomes
part of the policy.

## Database roles

The bootstrap script [`sql/bootstrap/00_create_role_and_database.sql`](../sql/bootstrap/00_create_role_and_database.sql)
creates three roles. None is a superuser. Generate their passwords with
`openssl rand -hex 24`: they go into connection URLs, where characters such as `@ : / # % ?`
must be percent-encoded (`@` becomes `%40`), and hex needs none.

| Role | Used by | Variable | May |
| --- | --- | --- | --- |
| `shadoucmdb_owner` | `shadoucmdb migrate`, `restore`, `factory-reset`, `decommission` | `MIGRATION_DATABASE_URL` | Own the database and the `cmdb` system schema; run migrations. Member of `shadoucmdb_app`. |
| `shadoucmdb_app` | `serve`, `seed`, `verify`, `create-admin` | `DATABASE_URL` or `PG*` | Read and write data. Only `SELECT` and `INSERT` on `audit_log`, `schema_changes` and `server_keys`; no `EXECUTE` on the purge. Owns the area schemas (`CREATE` on the database). |
| `shadoucmdb_maintenance` | `shadoucmdb prune-audit` | `MAINTENANCE_DATABASE_URL` | Execute `cmdb.prune_audit_log()`, nothing else. |

These are the default names; any others work, as does a database not named `shadoucmdb`.
Pass your names to the bootstrap script with `-v owner_role=… -v app_role=…
-v maintenance_role=… -v db_name=…` and use them in the connection strings. `migrate` takes
the API role from the `DATABASE_URL` (or `PGUSER`) user and the maintenance role from the
`MAINTENANCE_DATABASE_URL` user, prints both, and grants to and hands over to those roles. So
set all three variables when you run `migrate`; it stops if either role does not exist. When
the API role is the migrating user itself (a single-role install), there is nothing to grant
or hand over.

Areas are PostgreSQL schemas whose tables and columns the API changes at run time, when an
administrator adds an area, type or field (see [data-model.md](data-model.md)). So
`shadoucmdb_app` owns the area schemas, their type tables and reporting views, but nothing
in `cmdb` or `public`. `shadoucmdb_owner` is a member of `shadoucmdb_app` so that `migrate`
can build the tables of existing types (migration 0009) and hand them over, and build the
reporting views as that role. The membership only runs that way round; the API role gains
nothing from it. Migration 0008 stops with the `GRANT` to run if the membership is missing,
e.g. on an install split before this version: run `GRANT shadoucmdb_app TO shadoucmdb_owner;`
(with your role names) as an administrator, or re-run `10_split_roles.sql`, then `migrate` again.

Both bootstrap scripts pin `search_path = cmdb, public` for `shadoucmdb_owner` and
`shadoucmdb_maintenance`. PostgreSQL's default `"$user", public` would look first in a schema
named after the role, and creating schemas is what the API role does; the API also refuses area
keys that start with `shadoucmdb_` or match any existing role. On an install bootstrapped before
this, run the two `ALTER ROLE … SET search_path = cmdb, public;` lines from
`00_create_role_and_database.sql` as an administrator.

Only `shadoucmdb_owner` may create objects in schema `public`. PostgreSQL 14 lets every role
do so by default, which would let the API role plant a function that owner-privileged code
then runs; both bootstrap scripts revoke it, and `migrate` warns if it finds it still open
and cannot revoke it itself (run `REVOKE CREATE ON SCHEMA public FROM PUBLIC` as the schema's
owner). PostgreSQL 15 and later withhold it already.

The owner can change anything, including the audit log's trigger, so keep its connection
string out of the running server's environment. For example, prompt for the password and
pass it only to the command that needs it (variables already set win over the env file).
Use the same host, port and database as `DATABASE_URL`; any `sslmode` in the URL is ignored,
`DATABASE_SSL` applies.
Prompting also keeps it out of shell history, the sudo log and `ps`:

```sh
read -rsp 'shadoucmdb_owner password: ' PW; echo
MIGRATION_DATABASE_URL="postgres://shadoucmdb_owner:$PW@db.example.internal:5432/shadoucmdb" \
  shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env migrate
unset PW
```

Running `migrate` as `shadoucmdb_app` on a three-role install fails with a hint to set
`MIGRATION_DATABASE_URL`.

### Upgrading a single-role install

Installs created before SHAA-45 have one role, `shadoucmdb_app`, which owns everything. That
still runs, but the API's database user can then disable the append-only trigger and run the
purge itself. To split it, once:

1. Install the new binary and run `shadoucmdb migrate` as before (as `shadoucmdb_app`).
2. Stop the server. As a PostgreSQL admin, connected to the ShadouCMDB database, run:

   ```sh
   psql "postgres://admin@db.example.internal:5432/shadoucmdb" \
        -v owner_password='<password>' -v maintenance_password='<password>' \
        -f sql/bootstrap/10_split_roles.sql
   ```

   If your API role is not named `shadoucmdb_app`, add `-v app_role=<the DATABASE_URL user>`;
   `-v owner_role=…` and `-v maintenance_role=…` name the two new roles.

   It creates `shadoucmdb_owner` and `shadoucmdb_maintenance`, hands the database and every
   object `shadoucmdb_app` owns in it to `shadoucmdb_owner`, and grants `shadoucmdb_app` the
   API's rights. It is safe to re-run, and does not touch other databases on the server.
3. Set `MIGRATION_DATABASE_URL` and `MAINTENANCE_DATABASE_URL`, keep `DATABASE_URL` on
   `shadoucmdb_app`, start the server and run `shadoucmdb verify`.

CI runs this path from `v0.1.0-rc.1` on every pull request; see
[operator-setup.md](operator-setup.md#upgrade-paths-tested-in-ci).

## Audit log retention

Authentication events in `audit_log` hold client IP addresses and user agents, which are
personal data. The retention policy is **180 days** for them; CI change history is kept
indefinitely; `audit.purge` records are kept forever. The full policy, the list of
personal-data fields and why per-person erasure is not offered are in
[data-model.md](data-model.md#retention-and-personal-data).

Nothing is deleted automatically. The operator runs:

```sh
# Report what would go (the default is a dry run):
shadoucmdb prune-audit --older-than 180d
# Delete it:
shadoucmdb prune-audit --older-than 180d --execute
```

```
Deleted audit_log rows in scope "auth" older than 180 days:
  login.failure    2
Deleted 0 sessions that expired more than 30 days ago
Recorded as an audit.purge entry in audit_log.
```

- `--older-than` takes days (`180d` or `180`); anything under 30 days is refused, by the
  command and by the database function.
- `--scope auth` (default): `login.success`, `login.failure`, `login.locked`, `logout` and
  `session.revoke` rows, API token `token.use` rows, two-factor `mfa.*` rows, plus `sessions`
  rows that expired more than 30 days ago (and expired sign-in challenges in `mfa_challenges`).
  `--scope changes`: `create`, `update`, `delete` and `restore` rows, only if you decide to
  cut change history too.
- The command needs `MAINTENANCE_DATABASE_URL` and refuses to run without it. Each executed
  run adds an `audit.purge` row (visible in the audit log with `action=audit.purge`) naming
  the database user, client address, OS user, window and the number of rows deleted.
- The first run on a large table can take a while; the command does not apply
  `DATABASE_STATEMENT_TIMEOUT_MS`. Under a sustained sign-in attack the table can gain about
  43,000 `login.failure` rows a day, so run it regularly, e.g. a daily systemd timer or cron
  job with `--execute`.

## Hardening settings

All optional; every variable is in [`.env.example`](../.env.example).

- **Database TLS:** keep the default `DATABASE_SSL=verify-full` (with `DATABASE_SSL_CA_FILE` for a private
  CA). `require` encrypts but does not check the server certificate.
- **API documentation:** `/openapi.json` and `/docs` are off by default (`API_DOCS=off`). Use
  `authenticated` to offer them to signed-in users, `public` for development. The contract is
  committed as `backend/openapi.json` either way.
- **Timeouts:** `HTTP_HEADER_READ_TIMEOUT_SECS` (default 10) closes connections that do not finish
  their headers in time; `HTTP_REQUEST_TIMEOUT_SECS` (default 120) answers `408 REQUEST_TIMEOUT` to a
  request that runs longer. A reverse proxy in front should have its own, shorter limits.
- **Request rate limits:** the server does not limit requests per client address, because it cannot
  tell a real client address from a forged `X-Forwarded-For` without a trusted proxy. Limit the
  anonymous routes (`/api/v1/auth/*`) per client address at the reverse proxy, which sees the real
  one. Sign-in attempts are throttled per username by the server either way, and starting an OIDC
  sign-in stores nothing, so it cannot fill up the server.
- **Client details:** `AUDIT_CAPTURE_CLIENT_IP=false` and `AUDIT_CAPTURE_USER_AGENT=false` stop the
  server recording the IP address and User-Agent of sign-ins and sessions (for example where a works
  council agreement rules them out). Changes stay attributed to the signed-in user.
- **Audit export to a SIEM:** `AUDIT_EXPORT` copies each committed `audit_log` row, once, to `stdout`,
  a file (`file:/var/log/shadoucmdb/audit.jsonl`), or syslog over UDP or TCP
  (`udp://siem.example.com:514`). `AUDIT_EXPORT_FORMAT` is `rfc5424` (default for UDP/TCP) or `json`
  (default for stdout/file). RFC 5424 messages use facility 13 (log audit; `AUDIT_SYSLOG_FACILITY`),
  severity warning for `login.failure` / `login.locked` and notice otherwise, `MSGID` = the action,
  structured data `[audit@32473 seq=… id=… actorType=… entityType=… entityId=… hash=… actorId=…]`
  (32473 is the RFC 5612 documentation enterprise number) and the full event as JSON in `MSG`. There is
  no TLS: for an encrypted link to a remote SIEM, point it at a local relay (rsyslog, Vector, Fluent
  Bit). A failed send is retried from the same row; delivery is at least once while the server runs.
  UDP is unacknowledged, so datagrams the network or collector drops are lost without notice (the server
  warns at startup); prefer `tcp://`. A row too large for one UDP datagram (65,507 bytes) is sent as a
  stub with `oldValue` and `newValue` null, `oversize` true and `originalBytes`, keeping `chainSeq` and
  `rowHash`; the full row stays in the database.
  Export starts at the newest row when the server starts, so rows written while it was stopped (for
  example by `create-admin`) are not sent; the `chainSeq` gap shows it. Run export on one instance only.
- **Audit integrity:** run `shadoucmdb audit-verify` on a schedule and compare the printed chain head
  with the `rowHash` of the same `chainSeq` in the SIEM (see
  [data model › Tamper evidence](data-model.md#tamper-evidence)). Rows removed by `prune-audit`
  show up as `gap` findings; use `audit-verify --allow-gaps` once retention runs.

## Release downloads

Each [GitHub Release](https://github.com/Shadoukita/ShadouCMDB/releases) has:

| Asset | Contents |
| --- | --- |
| `shadoucmdb-<version>-linux-x64.tar.gz` | `x86_64-unknown-linux-musl`: statically linked, runs on any x64 distribution (glibc or musl, old or new). Plus the systemd unit, `shadoucmdb.env.example`, `README.txt`. |
| `shadoucmdb-<version>-linux-arm64.tar.gz` | The same for `aarch64-unknown-linux-musl` (Graviton, Ampere, Raspberry Pi 4/5 with a 64-bit OS). |
| `shadoucmdb-<version>-windows-x64.zip` | `shadoucmdb.exe` (`x86_64-pc-windows-msvc`, static C runtime, so no Visual C++ Redistributable), `shadoucmdb.env.example`, and a `README.txt` with the Windows Service install steps. |
| `SHA256SUMS` | `sha256sum --ignore-missing -c SHA256SUMS` |
| `shadoucmdb-<version>.cdx.json` | CycloneDX SBOM: every Rust crate and web UI package in the binaries. |
| `*.sigstore.json`, `shadoucmdb-<version>.provenance.jsonl` | cosign keyless signature per file and SLSA build provenance. How to check them: [supply-chain.md](supply-chain.md#verifying-a-download). |
| image `ghcr.io/shadoukita/shadoucmdb:<version>` | `linux/amd64` + `linux/arm64`, made from the two Linux binaries above. Signed with cosign, with provenance and SBOM attestations ([supply-chain.md](supply-chain.md#verifying-the-image)). |

All three binaries embed the same web UI build. The Linux builds are static musl
executables. musl's own allocator serialises threads on a single lock, so those
builds use mimalloc as the global allocator instead (see `backend/src/main.rs`),
and concurrent requests do not queue on `malloc`.

How releases are cut: [CONTRIBUTING.md](../CONTRIBUTING.md#cutting-a-release).

## Building from source

Requirements:
- stable Rust 1.94 or newer (`rustup`);
- a C compiler for the target, which *ring* (the TLS crypto) compiles with.

There is **no OpenSSL dependency** anywhere: TLS is rustls with the Mozilla root
set, so Windows builds need nothing beyond the normal Rust toolchain.

```sh
cd backend
cargo build --release          # -> backend/target/release/shadoucmdb(.exe)
```

| Target | How |
| --- | --- |
| `x86_64-unknown-linux-gnu` | `cargo build --release` on Linux x64 |
| `aarch64-unknown-linux-gnu` | natively on ARM64, or cross: `apt install gcc-aarch64-linux-gnu`, then `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc cargo build --release --target aarch64-unknown-linux-gnu` |
| `x86_64-pc-windows-msvc` | `cargo build --release` on Windows with the Visual Studio Build Tools ("Desktop development with C++") |
| `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (static, what releases ship) | from any Linux host: `rustup target add <triple>`, install [zig](https://ziglang.org/) 0.14 and [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild), then `cargo zigbuild --release --target <triple>` |
| any of the above from Linux | cargo-zigbuild: `cargo zigbuild --release --target <triple>`; use `x86_64-pc-windows-gnu` for Windows |

`backend/.cargo/config.toml` links the MSVC C runtime statically, so a Windows
build runs without the Visual C++ Redistributable.

CI on every PR (`.github/workflows/rust.yml`) builds Linux x64 natively,
cross-builds Linux ARM64 and runs it under QEMU, and builds Windows x64 with
MSVC. The release workflow (`.github/workflows/release.yml`) builds the static
musl binaries and the Windows exe that are shipped.

### Web UI

The contents of `frontend/dist` are embedded into the binary at build time.
Build the UI first (`npm run build -w frontend`), then the binary. If
`frontend/dist` does not exist, the binary builds without a UI and serves only
the API.

When a UI is embedded:
- unknown browser paths get `index.html`, so client-side routing works;
- `/assets/*` are served with a one-year immutable cache;
- `/api/*`, and paths that look like missing files (`/x.js`), return the JSON
  `NOT_FOUND` envelope.

### Migrations are embedded

`sql/migrations/*.sql` is compiled into the binary, and `build.rs` makes cargo
rebuild when that folder changes. Applied migrations are recorded in the table
`_sqlx_migrations`. Each migration runs in its own transaction together with its
bookkeeping row, under an advisory lock, so two concurrent `migrate` runs cannot
collide. Editing a migration that has already been applied is detected by its
checksum, and `migrate` refuses to continue.

## Linux (systemd)

A hardened sample unit is in [`deploy/systemd/shadoucmdb.service`](../deploy/systemd/shadoucmdb.service):

```sh
sudo install -m 0755 shadoucmdb /usr/local/bin/shadoucmdb
sudo useradd --system --no-create-home --shell /usr/sbin/nologin shadoucmdb
sudo install -d -m 0750 -o root -g shadoucmdb /etc/shadoucmdb
sudo install -m 0640 -o root -g shadoucmdb .env /etc/shadoucmdb/shadoucmdb.env   # your settings, as shadoucmdb_app
# migrate as shadoucmdb_owner, passed to this one command only (see Database roles):
read -rsp 'shadoucmdb_owner password: ' PW; echo
export MIGRATION_DATABASE_URL="postgres://shadoucmdb_owner:$PW@db.example.internal:5432/shadoucmdb"
sudo --preserve-env=MIGRATION_DATABASE_URL -u shadoucmdb \
  shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env migrate
unset PW MIGRATION_DATABASE_URL
sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env create-admin --username admin   # or use first-run setup in the UI
sudo cp deploy/systemd/shadoucmdb.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now shadoucmdb
curl -s http://127.0.0.1:3000/readyz
journalctl -u shadoucmdb -f
```

The unit runs as an unprivileged user and has no capabilities or writable
paths. To upgrade:
1. replace the binary;
2. run `migrate` with `MIGRATION_DATABASE_URL` as above (on a single-role install, split the
   roles first: see [Upgrading a single-role install](#upgrading-a-single-role-install));
3. `systemctl restart shadoucmdb`.

## Windows Server (Windows Service)

Run the following in an **elevated** PowerShell:

```powershell
$bin  = 'C:\Program Files\ShadouCMDB'
$data = 'C:\ProgramData\ShadouCMDB'
New-Item -ItemType Directory -Force $bin, $data | Out-Null
Copy-Item .\shadoucmdb.exe $bin
Copy-Item .\.env "$data\shadoucmdb.env"          # your settings (DATABASE_URL or PG*), as shadoucmdb_app

# The service runs as the low-privilege LocalService account: let it read the
# settings and write its log. Keep the env file away from other users.
icacls $data /inheritance:r /grant:r 'Administrators:(OI)(CI)F' 'SYSTEM:(OI)(CI)F' 'NT AUTHORITY\LocalService:(OI)(CI)M'

# migrate as shadoucmdb_owner, prompted for and set for this session only (see Database roles):
$pw = [uri]::EscapeDataString((Get-Credential shadoucmdb_owner).GetNetworkCredential().Password)
$env:MIGRATION_DATABASE_URL = "postgres://shadoucmdb_owner:$pw@db.example.internal:5432/shadoucmdb"
& "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" migrate
Remove-Item Env:MIGRATION_DATABASE_URL; Remove-Variable pw
& "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" --log-file "$data\logs\shadoucmdb.log" service install
Start-Service ShadouCMDB
Invoke-RestMethod http://127.0.0.1:3000/readyz
```

`service install`:
- registers the executable it was started from (auto start, restart after 10 s
  on failure);
- records the absolute `--env-file` / `--log-file` paths as the service's
  start arguments.

Options:
- `--name` for a second instance;
- `--account` / `--password` to run as another account, e.g. a gMSA or
  `NT AUTHORITY\NetworkService`.

To remove the service: `shadoucmdb.exe service uninstall` (stops it first).
`service run` is what the Service Control Manager starts; it is not meant to be
run by hand.

To upgrade:
1. `Stop-Service ShadouCMDB`;
2. replace the exe;
3. run `migrate`;
4. `Start-Service ShadouCMDB`.

Remember to allow the port in Windows Firewall if clients connect remotely:
`New-NetFirewallRule -DisplayName ShadouCMDB -Direction Inbound -Protocol TCP -LocalPort 3000 -Action Allow`.

## Docker

The root [`Dockerfile`](../Dockerfile) builds the UI and the binary and copies
only the binary into `gcr.io/distroless/cc-debian12:nonroot`. The resulting
image has no shell or package manager and runs as uid 65532. The Rust stage
cross-compiles, so a multi-arch build does not emulate the compiler:

```sh
docker buildx build --platform linux/amd64,linux/arm64 -t shadoucmdb .      # add --push for a registry
docker build -t shadoucmdb .                                                 # current platform only

docker run --rm --env-file .env shadoucmdb migrate
docker run --rm --env-file .env shadoucmdb seed
docker run --rm -it --env-file .env shadoucmdb create-admin --username admin   # or use first-run setup in the UI
docker run -d --name shadoucmdb --env-file .env -p 3000:3000 shadoucmdb    # CMD is `serve`
```

Released images (`ghcr.io/shadoukita/shadoucmdb:<version>`) are built differently:
[`deploy/docker/Dockerfile.release`](../deploy/docker/Dockerfile.release) copies the
already-tested static musl binary from the release onto
`gcr.io/distroless/static-debian12:nonroot`. The container runs the same bytes as
the `linux-x64` / `linux-arm64` downloads. Usage is the same:

```sh
docker run --rm --env-file .env ghcr.io/shadoukita/shadoucmdb:<version> migrate
docker run -d --name shadoucmdb --env-file .env -p 3000:3000 ghcr.io/shadoukita/shadoucmdb:<version>
```

The image has no `HEALTHCHECK` because it has no shell or curl. Probe
`GET /healthz` (liveness) and `GET /readyz` (readiness) over HTTP from your
orchestrator. For a database on the Docker host, use
`PGHOST=host.docker.internal` (add `--add-host=host.docker.internal:host-gateway`
on Linux), not `localhost`.

`docker-compose.yml` builds this image and wraps `migrate`, `seed` and `serve`
for local use; see the [README](../README.md#with-docker).

## Moving a dev database off the Node/Drizzle migration runner

The Node API and its Drizzle migration runner were removed in SHAA-9. Databases
it migrated (with `npm run db:migrate`) record their history in
`drizzle.__drizzle_migrations`, not `_sqlx_migrations`. On such a database,
`shadoucmdb migrate` stops with an explanation. Choose one of these:

- **Adopt it once (keeps the data):**

  ```sh
  shadoucmdb migrate --adopt-drizzle
  ```

  This checks that every Drizzle row is the SHA-256 of the corresponding
  embedded migration file, marks those migrations as applied in
  `_sqlx_migrations` without re-running them, and then applies anything newer.
  The `drizzle` schema is left in place; drop it afterwards with
  `DROP SCHEMA drizzle CASCADE;`.

- **Or reset it.** No production database exists yet, so recreating a dev
  database is fine:

  ```sh
  dropdb …; createdb …
  shadoucmdb migrate && shadoucmdb seed --demo
  ```

## Resource footprint

Measured on Linux x64 with PostgreSQL 18 over TLS (`DATABASE_SSL=require`):

| | |
| --- | --- |
| Release binary (x86_64-linux-gnu, stripped, no UI) | 6.8 MB |
| RSS of `serve` right after start, idle | 6.0 MiB |
| RSS after 2,000 sequential `/readyz` requests, then idle | 8.7 MiB |
