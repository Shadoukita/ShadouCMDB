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
| any of the above from Linux | [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild): `cargo zigbuild --release --target <triple>`; use `x86_64-pc-windows-gnu` for Windows |

CI (`.github/workflows/rust.yml`) builds Linux x64 natively, cross-builds Linux
ARM64 and runs it under QEMU, and builds Windows x64 with MSVC.

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
docker run -d --name shadoucmdb --env-file .env -p 3000:3000 shadoucmdb    # CMD is `serve`
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
