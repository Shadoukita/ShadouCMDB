# Building and running `shadoucmdb`

`shadoucmdb` is the ShadouCMDB server as one self-contained binary: HTTP API,
health checks, embedded web UI and the admin commands (migrations, seed data,
schema checks). The binary ships for **Linux x64**, **Linux ARM64** and
**Windows Server x64**, plus a multi-arch **Docker image**.

PostgreSQL is always **external**. Configure it through `DATABASE_URL` or the
`PG*` variables. Every variable is documented in [`.env.example`](../.env.example).

## Commands

```
shadoucmdb [--env-file PATH] [--log-file PATH] <COMMAND>

  serve                     Run the HTTP server (/api/v1, /openapi.json, /docs, /healthz, /readyz, web UI)
  migrate [--adopt-drizzle] Apply pending migrations; re-running is a no-op
  seed [--demo]             Load reference data (idempotent); --demo adds a sample inventory
  verify                    Schema acceptance checks in a rolled-back transaction
  create-admin --username U [--display-name N] [--email E] [--password-stdin]
                            Create a user holding the built-in Administrator profile
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
- `serve` does **not** migrate on start. Run `migrate` as an explicit step when
  you install or upgrade.
- `SIGTERM` or Ctrl+C (Linux), or a service Stop (Windows), triggers a graceful
  shutdown: the listener closes, in-flight requests finish, then the pool closes.

Typical first run against a new database:

```sh
shadoucmdb migrate
shadoucmdb seed            # add --demo for sample CIs
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

The server sets these security headers itself, on every response it sends (the web UI, the
API and Swagger UI at `/docs`). Do not add them again at the proxy: two
`Content-Security-Policy` headers are both enforced, and duplicate values of the others are
confusing at best. Headers the server does not set (for example `Permissions-Policy`) are
the proxy's to add.

| Header | Value | Sent on |
| --- | --- | --- |
| `X-Content-Type-Options` | `nosniff` | every response |
| `Referrer-Policy` | `no-referrer` | every response |
| `Strict-Transport-Security` | `max-age=31536000; includeSubDomains` | only requests that arrived over HTTPS (`X-Forwarded-Proto: https` or `Forwarded: proto=https`); never over plain HTTP, so a lab or LAN install is not locked onto a scheme it cannot serve |
| `Content-Security-Policy` | `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; object-src 'none'; form-action 'self'` | HTML documents only (the web UI and `/docs`), not JSON |

`includeSubDomains` tells browsers to use HTTPS for every subdomain of the host name the
server is reached by, for a year. If that name has subdomains that cannot serve HTTPS, have the
proxy strip or override the header. Because of `frame-ancestors 'none'` the UI cannot be
embedded in a frame on any site, including your own.
- `SESSION_IDLE_TIMEOUT_MINUTES` (default 12 h) and `SESSION_MAX_AGE_HOURS` (default 7 days)
  bound how long a session lives. Sessions are stored in PostgreSQL, so they survive restarts
  and work across several instances. The login backoff counters are per process.

## Release downloads

Each [GitHub Release](https://github.com/Shadoukita/ShadouCMDB/releases) has:

| Asset | Contents |
| --- | --- |
| `shadoucmdb-<version>-linux-x64.tar.gz` | `x86_64-unknown-linux-musl`: statically linked, runs on any x64 distribution (glibc or musl, old or new). Plus the systemd unit, `shadoucmdb.env.example`, `README.txt`. |
| `shadoucmdb-<version>-linux-arm64.tar.gz` | The same for `aarch64-unknown-linux-musl` (Graviton, Ampere, Raspberry Pi 4/5 with a 64-bit OS). |
| `shadoucmdb-<version>-windows-x64.zip` | `shadoucmdb.exe` (`x86_64-pc-windows-msvc`, static C runtime, so no Visual C++ Redistributable), `shadoucmdb.env.example`, and a `README.txt` with the Windows Service install steps. |
| `SHA256SUMS` | `sha256sum --ignore-missing -c SHA256SUMS` |
| image `ghcr.io/shadoukita/shadoucmdb:<version>` | `linux/amd64` + `linux/arm64`, made from the two Linux binaries above. |

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
sudo install -m 0640 -o root -g shadoucmdb .env /etc/shadoucmdb/shadoucmdb.env   # your settings
sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env migrate
sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env create-admin --username admin   # or use first-run setup in the UI
sudo cp deploy/systemd/shadoucmdb.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now shadoucmdb
curl -s http://127.0.0.1:3000/readyz
journalctl -u shadoucmdb -f
```

The unit runs as an unprivileged user and has no capabilities or writable
paths. To upgrade:
1. replace the binary;
2. run `migrate`;
3. `systemctl restart shadoucmdb`.

## Windows Server (Windows Service)

Run the following in an **elevated** PowerShell:

```powershell
$bin  = 'C:\Program Files\ShadouCMDB'
$data = 'C:\ProgramData\ShadouCMDB'
New-Item -ItemType Directory -Force $bin, $data | Out-Null
Copy-Item .\shadoucmdb.exe $bin
Copy-Item .\.env "$data\shadoucmdb.env"          # your settings (DATABASE_URL or PG*)

# The service runs as the low-privilege LocalService account: let it read the
# settings and write its log. Keep the env file away from other users.
icacls $data /inheritance:r /grant:r 'Administrators:(OI)(CI)F' 'SYSTEM:(OI)(CI)F' 'NT AUTHORITY\LocalService:(OI)(CI)M'

& "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" migrate
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
