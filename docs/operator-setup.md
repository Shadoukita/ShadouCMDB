# Operator setup guide

This guide takes you from an empty directory and an empty PostgreSQL server to a working
ShadouCMDB: API, web UI, a first administrator and a first configuration item. Follow the
steps in order. Every command below was run as written (with the placeholders filled in);
see [How this guide was verified](#how-this-guide-was-verified).

For reference material behind each step see [deployment.md](deployment.md) (commands,
services, Docker, Windows), [`.env.example`](../.env.example) (every variable) and
[security/hardening.md](security/hardening.md).

```
External PostgreSQL  ->  shadoucmdb (API + embedded web UI)  ->  browser
```

PostgreSQL is always external: ShadouCMDB never installs, bundles or assumes a database, and
there is no default host. Only the `shadoucmdb` server talks to the database; the web UI talks
only to the API and never needs database credentials.

## 1. Prerequisites

| You need | Notes |
| --- | --- |
| A PostgreSQL **14 or newer** server reachable over the network | Any host and port: a VM, a container, a managed instance (RDS, Cloud SQL, Azure Flexible Server). TLS strongly recommended. |
| An administrator login on that server | Only for the one-time bootstrap in step 3: it creates roles and a database. The application itself never runs as an administrator. |
| `psql` (PostgreSQL client) on the machine you bootstrap from | Any version that can reach the server. |
| The ShadouCMDB release archive for your platform | `linux-x64`, `linux-arm64` (`.tar.gz`) or `windows-x64` (`.zip`) from the [releases page](https://github.com/Shadoukita/ShadouCMDB/releases), or the image `ghcr.io/shadoukita/shadoucmdb:<version>`. |
| `curl` and `sha256sum` | Download, checksum and health checks. |
| A modern browser | For the web UI. |

The Linux binary is statically linked: no glibc, OpenSSL or other runtime dependency. The
server needs no Node.js; the web UI is embedded in the binary.

In the commands below, replace:

| Placeholder | Example |
| --- | --- |
| `<version>` | `0.1.0` (without the leading `v`) |
| `<db-host>`, `<db-port>` | `db.example.internal`, `5432` |
| `<pg-admin>` | your PostgreSQL administrator login |

## 2. Download and verify

```sh
mkdir shadoucmdb-install && cd shadoucmdb-install
VER=<version>
curl -fsSLO https://github.com/Shadoukita/ShadouCMDB/releases/download/v$VER/shadoucmdb-$VER-linux-x64.tar.gz
curl -fsSLO https://github.com/Shadoukita/ShadouCMDB/releases/download/v$VER/SHA256SUMS
sha256sum --ignore-missing -c SHA256SUMS          # must print: shadoucmdb-<version>-linux-x64.tar.gz: OK
tar xzf shadoucmdb-$VER-linux-x64.tar.gz
./shadoucmdb-$VER-linux-x64/shadoucmdb --version
```

The archive contains `shadoucmdb`, `shadoucmdb.service`, `shadoucmdb.env.example` and
`README.txt`. It does **not** contain the database bootstrap script
([#47](https://github.com/Shadoukita/ShadouCMDB/issues/47)); fetch it from the same tag:

```sh
curl -fsSLO https://raw.githubusercontent.com/Shadoukita/ShadouCMDB/v$VER/sql/bootstrap/00_create_role_and_database.sql
```

## 3. Create the database and its roles (once, as a PostgreSQL administrator)

The bootstrap script creates one database, `shadoucmdb`, and three login roles, none of them
superuser:

| Role | Used by | Configured as |
| --- | --- | --- |
| `shadoucmdb_owner` | `shadoucmdb migrate` only | `MIGRATION_DATABASE_URL` (passed on the command line, never stored in the server's env file) |
| `shadoucmdb_app` | the running server, `seed`, `verify`, `create-admin` | `DATABASE_URL` or `PG*` |
| `shadoucmdb_maintenance` | `shadoucmdb prune-audit` only | `MAINTENANCE_DATABASE_URL` |

> **Keep these role names.** The current migrations only work with exactly these three role
> names ([#42](https://github.com/Shadoukita/ShadouCMDB/issues/42)). The database *host,
> port and TLS settings* are free; only the role names are fixed for now.

Generate three passwords and keep them in files readable only by you:

```sh
umask 077
for r in owner app maintenance; do openssl rand -base64 24 | tr -d '/+=' > .pw-$r; done
```

Run the script as the administrator (connect to the `postgres` maintenance database):

```sh
psql "host=<db-host> port=<db-port> dbname=postgres user=<pg-admin> sslmode=verify-full sslrootcert=db-ca.pem" \
     -v ON_ERROR_STOP=1 \
     -v owner_password="$(cat .pw-owner)" \
     -v app_password="$(cat .pw-app)" \
     -v maintenance_password="$(cat .pw-maintenance)" \
     -f 00_create_role_and_database.sql
```

Use `sslmode=require` instead of `verify-full sslrootcert=…` if you do not have the server's
CA certificate at hand. Expected output ends with:

```
You are now connected to database "shadoucmdb" as user "<pg-admin>".
GRANT
REVOKE
CREATE EXTENSION
```

If your provider does not let you create the `pg_trgm` extension this way, have its
administrator run `CREATE EXTENSION pg_trgm;` in the `shadoucmdb` database once.

## 4. Write the configuration file

```sh
cp shadoucmdb-$VER-linux-x64/shadoucmdb.env.example shadoucmdb.env
chmod 600 shadoucmdb.env
```

Edit `shadoucmdb.env`. For a standard install you set these; everything else can keep its
default:

| Variable | Set to | Why |
| --- | --- | --- |
| `DATABASE_URL` | `postgres://shadoucmdb_app:<app password>@<db-host>:<db-port>/shadoucmdb` | The server's connection. Takes precedence over `PG*`. |
| `PGHOST`, `PGPASSWORD` | *(empty)* when you use `DATABASE_URL` | Alternatively leave `DATABASE_URL` empty and set `PGHOST`, `PGPORT`, `PGDATABASE`, `PGUSER`, `PGPASSWORD`. |
| `MAINTENANCE_DATABASE_URL` | `postgres://shadoucmdb_maintenance:<maintenance password>@<db-host>:<db-port>/shadoucmdb` | Only `prune-audit` uses it. |
| `MIGRATION_DATABASE_URL` | *(leave empty)* | Pass it on the command line in step 5, so the owner's password is not in the server's environment. |
| `DATABASE_SSL` | `verify-full` (recommended), `require` or `disable` | `verify-full` checks the server certificate and host name. Any `sslmode` inside `DATABASE_URL` is ignored. |
| `DATABASE_SSL_CA_FILE` | absolute path to the CA bundle (PEM) | Needed with `verify-full` when the server certificate is from a private or managed CA. |
| `API_HOST`, `API_PORT` | e.g. `0.0.0.0` and `3000` (defaults) | Where the API and UI listen. |
| `CORS_ORIGINS` | *(empty)* | Only needed when the UI is served from another origin (section 9). |
| `COOKIE_SECURE` | `auto` (default); `always` behind a TLS proxy that does not send `X-Forwarded-Proto` | Session cookie `Secure` flag. |

The remaining variables (`DATABASE_POOL_MAX`, `DATABASE_STATEMENT_TIMEOUT_MS`,
`DATABASE_CONNECT_TIMEOUT_MS`, `CSP_REPORT_URI`, `LOG_LEVEL`,
`SESSION_IDLE_TIMEOUT_MINUTES`, `SESSION_MAX_AGE_HOURS`) are optional and documented line by
line in the example file. Variables already set in the process environment win over the file.

Put the CA bundle next to it if you use `verify-full`, e.g. `db-ca.pem`, and reference it by
absolute path.

A missing database setting stops every command with a clear list, for example:

```
Invalid configuration:
  - PGHOST: PGHOST is required when DATABASE_URL is not set
```

There is no fallback to `localhost`.

## 5. Apply the migrations (as the owner)

```sh
B=./shadoucmdb-$VER-linux-x64/shadoucmdb
MIGRATION_DATABASE_URL="postgres://shadoucmdb_owner:$(cat .pw-owner)@<db-host>:<db-port>/shadoucmdb" \
  $B --env-file shadoucmdb.env migrate
```

Expected on an empty database (the count grows with new releases):

```
Connected to database "shadoucmdb" (PostgreSQL 18.4), ssl=verify-full
Migrations: 10 in binary, 0 applied, 10 pending
  applied 0000_extensions
  …
  applied 0009_type_tables
Database is at migration 10/10
```

Re-running reports `nothing to do`. If you forget `MIGRATION_DATABASE_URL`, `migrate` refuses
with `this database user may not change the schema; set MIGRATION_DATABASE_URL …`. That is
expected: the server's own role cannot change the schema.

`serve` never migrates on its own. Run this step again after every upgrade.

## 6. Seed, check and create the first administrator

```sh
$B --env-file shadoucmdb.env seed --template it_infrastructure   # or plain `seed` for an empty data model
$B --env-file shadoucmdb.env verify                              # optional; writes nothing
$B --env-file shadoucmdb.env create-admin --username admin --display-name "Ops Admin"   # prompts for the password twice
```

- `seed` alone installs only system rows; the data model (CI classes, attributes, relationship
  types, statuses) stays empty and you build it under *Administration*. `--template
  it_infrastructure` installs a starter model: Server, Virtual machine, Network device,
  Application, Database, Service and Location, with their relationship rules. You can also
  install it later from *Administration → Templates*. `--demo` adds a small sample inventory
  (only into an empty CI table).
- `verify` ends with `23/23 checks passed (transaction rolled back, no data written)`.
- `create-admin` needs a password of at least 12 characters. For scripts:
  `$B --env-file shadoucmdb.env create-admin --username admin --password-stdin < .pw-admin`.
  You can skip `create-admin` entirely: while no user exists, the web UI opens a
  first-run setup page instead of the sign-in page.

## 7. Start the server

### Try it in the foreground

```sh
$B --env-file shadoucmdb.env serve
```

It logs one JSON line per event, starting with
`"server listening","address":"0.0.0.0:3000",…,"ui":true,…,"ssl":"verify-full"`. Stop it
with Ctrl+C: it drains in-flight requests, then closes the pool.

### As a systemd service (Linux)

```sh
sudo install -m 0755 shadoucmdb-$VER-linux-x64/shadoucmdb /usr/local/bin/shadoucmdb
sudo useradd --system --no-create-home --shell /usr/sbin/nologin shadoucmdb
sudo install -d -m 0750 -o root -g shadoucmdb /etc/shadoucmdb
sudo install -m 0640 -o root -g shadoucmdb shadoucmdb.env /etc/shadoucmdb/shadoucmdb.env
sudo install -m 0640 -o root -g shadoucmdb db-ca.pem /etc/shadoucmdb/db-ca.pem     # if you use verify-full
sudoedit /etc/shadoucmdb/shadoucmdb.env        # DATABASE_SSL_CA_FILE=/etc/shadoucmdb/db-ca.pem
sudo install -m 0644 shadoucmdb-$VER-linux-x64/shadoucmdb.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now shadoucmdb
journalctl -u shadoucmdb -f
```

The unit starts `shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env serve` as the
unprivileged `shadoucmdb` user with a hardened sandbox, restarts it on failure, and stops it
with `SIGTERM` (graceful). It does not migrate; after upgrades run step 5, then
`sudo systemctl restart shadoucmdb`.

### Windows Server and Docker

Use [deployment.md](deployment.md): `shadoucmdb service install` for a Windows Service,
`docker run --env-file shadoucmdb.env ghcr.io/shadoukita/shadoucmdb:<version> migrate|serve`
for the multi-arch image (amd64 and arm64). The configuration, migrations and checks in this
guide are the same. Inside a container `localhost` is the container: point `DATABASE_URL` at
the database's real host name.

## 8. Verify the installation

Run these from the server (replace host and port if you changed `API_HOST`/`API_PORT`):

```sh
curl -s http://127.0.0.1:3000/healthz     # {"status":"ok"}  (process is alive)
curl -s http://127.0.0.1:3000/readyz      # {"status":"ready","database":"ok","migrations":{"applied":10,"expected":10,"upToDate":true}}
```

`/readyz` answers `503` with `"status":"not_ready"` when the database is unreachable
(`"database":"unreachable"`, after `DATABASE_CONNECT_TIMEOUT_MS`, 5 s by default) or when
migrations are pending (`"upToDate":false`). It turns green again by itself once the cause is
fixed; no restart needed. Use it as the load balancer / orchestrator readiness probe, with a
probe timeout longer than `DATABASE_CONNECT_TIMEOUT_MS`.

Then, in a browser, open `http://<server>:3000/`:

1. Sign in as the administrator from step 6 (or complete first-run setup).
2. Click **+ New CI**, choose class **Server**, enter a name, pick a status, and click
   **Create Server**. The detail page opens with "Created …".
3. Read the same CI back through the API to prove it was stored:

   ```sh
   curl -s -c jar -H 'Content-Type: application/json' \
        -d '{"username":"admin","password":"<admin password>"}' http://127.0.0.1:3000/api/v1/auth/login > /dev/null
   curl -s -b jar 'http://127.0.0.1:3000/api/v1/configuration-items?q=<the name you entered>'
   ```

   The response lists the CI with the class, status and version `1` you saw in the UI.

The API contract is served at `/openapi.json`, with a browsable version at `/docs`.

Optional deep check: the repository's smoke suite exercises every API operation and checks
every response against the OpenAPI document. It needs Node.js 22.18+ and a checkout of the
same tag, and expects `seed --demo` data, so run it against a **fresh test database**, not
production:

```sh
API_URL=http://127.0.0.1:3000 SMOKE_USERNAME=admin SMOKE_PASSWORD='<admin password>' node tools/smoke/smoke.ts
# … ALL CHECKS PASSED
```

## 9. Serving the web UI separately (optional)

The binary serves the UI on the same origin as the API, which is the simplest setup. To host
the UI elsewhere (a CDN, a separate web server), build or copy `frontend/dist` and set the API
location at run time in `dist/config.js`:

```js
window.__SHADOUCMDB_CONFIG__ = { apiBaseUrl: "https://cmdb-api.example.com" };
```

Then add the UI's origin to `CORS_ORIGINS` on the server (exactly `scheme://host[:port]`,
no path) and restart it. Alternatively, put a reverse proxy in front of both that routes
`/api` to the server; then no CORS setting is needed. Either way the UI host needs no
database settings. See [frontend/README.md](../frontend/README.md).

## 10. Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `Invalid configuration: - PGHOST: PGHOST is required …` | No database configured: set `DATABASE_URL` or the `PG*` variables (step 4). |
| `invalid peer certificate: UnknownIssuer` | `DATABASE_SSL=verify-full` without the right CA: set `DATABASE_SSL_CA_FILE` to the CA that signed the server certificate. |
| `invalid peer certificate: … CaUsedAsEndEntity` | The server uses a self-signed certificate that is its own CA; `verify-full` rejects that. Issue the server certificate from a CA, or use `DATABASE_SSL=require`. |
| `no pg_hba.conf entry for host …, no encryption` | The server requires TLS but `DATABASE_SSL=disable`. |
| `password authentication failed for user …` | Wrong password in `DATABASE_URL` / `PGPASSWORD`. Special characters in a URL password must be percent-encoded. |
| `migrate failed: this database user may not change the schema; set MIGRATION_DATABASE_URL …` | Run `migrate` with `MIGRATION_DATABASE_URL` as `shadoucmdb_owner` (step 5). |
| `role … must be a member of shadoucmdb_app …` during `migrate` | The roles were created with other names; see the note in step 3 ([#42](https://github.com/Shadoukita/ShadouCMDB/issues/42)). |
| UI shows "Request failed (500) An unexpected error occurred" right after install, and `/readyz` says `"upToDate":false` | Migrations were not applied: run step 5 ([#43](https://github.com/Shadoukita/ShadouCMDB/issues/43) tracks a clearer message). |
| `/readyz` returns `503 "database":"unreachable"` | The server cannot reach or log in to PostgreSQL: check host, port, firewall, `pg_hba.conf` and credentials; the server log has the underlying error. |
| The UI on another origin cannot sign in | Add its origin to `CORS_ORIGINS` and restart; `*` is not allowed. |

## 11. Upgrading

```sh
sudo install -m 0755 shadoucmdb /usr/local/bin/shadoucmdb
sudo -u shadoucmdb env MIGRATION_DATABASE_URL='postgres://shadoucmdb_owner:…@<db-host>:<db-port>/shadoucmdb' \
  shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env migrate
sudo systemctl restart shadoucmdb
curl -s http://127.0.0.1:3000/readyz
```

A failed migration rolls back and leaves the database at the previous version; the old binary
keeps working. Installs created with the older single-role bootstrap are split into the three
roles with `sql/bootstrap/10_split_roles.sql` (see
[deployment.md](deployment.md#upgrading-a-single-role-install)). For backups before an upgrade
see [backup-and-reset.md](backup-and-reset.md).

### Upgrade paths tested in CI

Every pull request and every push to `main` and `release/*` upgrades a real database in place
(`.github/workflows/upgrade.yml`):

| From | To | How |
| --- | --- | --- |
| `v0.1.0-rc.1` (single role, migrations 0000–0006) | the commit under test | `migrate` as `shadoucmdb_app`, then `10_split_roles.sql`, then `migrate` as `shadoucmdb_owner` |

For each path the job installs the old release from its GitHub Release archive (checksum
verified), bootstraps PostgreSQL with that release's own `00_create_role_and_database.sql`, loads
its demo inventory and creates more data through its API: a CI class with one attribute of every
data type, CIs, a relationship, a deleted CI, a restricted permission profile and user, and the
audit rows all of that writes. After the upgrade, `tools/upgrade/upgrade-check.ts` reads every
object again and fails if an id, attribute value, relationship, profile grant or audit row is
missing or changed, or if the restricted user sees anything other than before. Then `verify` and
the smoke suite run against the upgraded instance.

One difference is expected when upgrading from a release before migration 0008: every CI class
that existed before gets the time of the upgrade as its `updatedAt`, because 0008 assigns each
class to the area "Infrastruktur".

Paths not in the table are not tested. Upgrade through the newest tested release, or test on a
copy of your database first ([backup-and-reset.md](backup-and-reset.md)).

## How this guide was verified

Followed end to end on 2026-09-27 (SHAA-5), from an empty directory and an empty database:

- **Database:** PostgreSQL 18.4 on its own address and a non-default port (not `localhost:5432`),
  TLS only (`hostssl` in `pg_hba.conf`), server certificate from a private CA,
  `DATABASE_SSL=verify-full` with `DATABASE_SSL_CA_FILE`.
- **Server:** Linux x64. The release archive `v0.1.0-rc.1` was downloaded and checksummed as in
  step 2; because the three-role setup is not in a release yet, its binary was replaced with
  one built from `main` at `d38e160` (10 migrations). Steps 3–8 as written, with
  `shadoucmdb.service` run as a **user-level** systemd unit (no root on the test host), so the
  unit's `User=` and hardening directives were not exercised.
- **Checks:** `/healthz`, `/readyz`, sign-in and CI creation in the UI (headless Chromium), the
  API read-back, `prune-audit` dry run, and the smoke suite on a fresh `--demo` database
  (`120/120 OpenAPI operations exercised … ALL CHECKS PASSED`).
- **Upgrade (section 11):** a failing migration was observed to roll back and leave the old
  binary serving; the `sudo` commands themselves were not run.
- **Not verified by running:** Windows Service, Linux ARM64, and `docker run` of the published
  image (the image's amd64 binary is byte-identical to the archive's, and its manifest lists
  amd64 and arm64).
