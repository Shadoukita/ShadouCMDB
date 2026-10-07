# ShadouCMDB

A self-hosted Configuration Management Database. Its central object is the
**Configuration Item (CI)**: a server, VM, network device, application,
database, service, location or any other tracked IT asset. CIs belong to a
**CI class** that defines their attributes and are joined by typed,
directional relationships (`Application runs_on Server`, `Device located_in Location`).

```
External PostgreSQL  ->  Backend API (backend/)  ->  Web frontend (frontend/)
```

- **PostgreSQL is external.** Nothing in this repo runs or bundles a database;
  every connection detail comes from environment variables.
- **The backend is the only database client.** The frontend talks to the API only.

| Path | What |
| --- | --- |
| `backend/` | The backend: the Rust server `shadoucmdb` (Axum + Tokio + sqlx). One binary serves `/api/v1`, `/healthz`, `/readyz`, `/openapi.json`, `/docs` and the embedded web UI, and runs `migrate`, `seed`, `verify` and `openapi`. |
| `sql/` | Database artifacts: versioned migrations, bootstrap scripts, ER diagram. See [`sql/README.md`](sql/README.md). |
| `frontend/` | Vue 3 + Vite + TanStack Query web UI. See [`frontend/README.md`](frontend/README.md). |
| `.github/` | CI and release workflows, pull request template. Releases: see [CONTRIBUTING.md](CONTRIBUTING.md#cutting-a-release). |
| `SECURITY.md` | Vulnerability reporting and disclosure policy, supported versions. |
| `backend/openapi.json` | OpenAPI contract generated from the code (`shadoucmdb openapi --out backend/openapi.json`; CI fails if it is stale). |
| `tools/` | `smoke/smoke.ts`: end-to-end check of every API operation against any API URL. `ldap-it/`: LDAPS sign-in against a real OpenLDAP directory. `openapi-diff.mjs`: semantic diff of two specs. |
| `Dockerfile`, `deploy/` | Multi-arch container image of `shadoucmdb`; systemd unit; release Dockerfile and the READMEs shipped in the release archives. |

## Requirements

- The `shadoucmdb` binary (Linux x64/ARM64, Windows x64), built with stable Rust 1.94+ or taken from a
  release, or Docker. Node.js 22.18+ is only needed for the frontend tooling and the smoke test.
- A reachable PostgreSQL **14 or newer**, anywhere: a managed service (RDS,
  Cloud SQL, Azure), another host on your network, or a local install.
- Memory: the compiled attribute validation patterns (`validation.pattern`) use at most about 96 MiB
  in total (64 MiB of compiled patterns, 32 MiB of reusable matching caches), plus about 1 MiB for
  each thread validating a value at that moment. The 96 MiB does not grow with the number of stored
  patterns or CPU cores.

## Pointing the app at an external PostgreSQL

Installing from a release? Each archive has a `README.txt` with the install steps. The steps below are
the same flow for a source checkout.

1. **Create the database and its roles** on your PostgreSQL server (run as an admin there):

   ```sh
   psql "<admin connection string>" -f sql/bootstrap/00_create_role_and_database.sql
   psql "<admin connection string>" -c '\password shadoucmdb_owner' -c '\password shadoucmdb_app' \
        -c '\password shadoucmdb_maintenance'
   ```

   The roles are created without a password and can log in once `\password` has set one. It prompts
   for the password and sends the server only a SCRAM-SHA-256 verifier, so no password reaches the
   process list or the server log; do not pass passwords as psql variables (`-v`).

   This creates `shadoucmdb_owner` (owns the schema, runs migrations), `shadoucmdb_app` (the API:
   reads and writes data, cannot delete audit history) and `shadoucmdb_maintenance` (may only prune
   the audit log). None needs superuser. An install made with the older single-role
   setup is upgraded with `sql/bootstrap/10_split_roles.sql`, from the same release as the binary you
   migrated with: it gives the API role the rights the migrations list in `cmdb.api_role_privileges`,
   so a split install ends up with the same grants as a fresh one. If the API role's credentials may
   have been compromised before the split, check the `cmdb` schema or restore a known-good backup first. Migrations run `CREATE EXTENSION IF NOT EXISTS pg_trgm`;
   `pg_trgm` is a *trusted* extension, so the database owner can create it on PostgreSQL 13+ and
   on RDS / Cloud SQL / Azure Flexible Server. If your provider restricts extensions, have an admin
   run `CREATE EXTENSION pg_trgm;` in the database once beforehand.

2. **Configure the connection.** Copy the template and edit it:

   ```sh
   cp .env.example .env
   ```

   Set **either** `DATABASE_URL`:

   ```sh
   DATABASE_URL=postgres://shadoucmdb_app:<password>@db.example.internal:5432/shadoucmdb
   ```

   **or** the discrete `PGHOST`, `PGPORT`, `PGDATABASE`, `PGUSER`, `PGPASSWORD` variables. Also set
   `MIGRATION_DATABASE_URL` (as `shadoucmdb_owner`) for `migrate`, `restore` and the resets, and `MAINTENANCE_DATABASE_URL`
   (as `shadoucmdb_maintenance`) for `prune-audit`.
   Then choose TLS with `DATABASE_SSL`:

   | `DATABASE_SSL` | Use when |
   | --- | --- |
   | `verify-full` (default) | Production and managed databases. Add `DATABASE_SSL_CA_FILE` if the CA is not publicly trusted (e.g. the RDS CA bundle). |
   | `require` | Encrypted, but the server certificate is not verified. Logs a warning at startup unless the host is loopback. |
   | `disable` | Only for a database on a trusted private network without TLS. |

   `shadoucmdb` reads `.env` from the current directory only, never from a parent directory; run it
   from the directory that holds `.env`, or pass `--env-file /path/to/.env`. The file it loaded is
   logged at startup (`loaded environment from …`).

   There is no default host. If nothing is configured, the backend exits with an error naming
   the missing variable. Every variable is documented in [`.env.example`](.env.example).

3. **Run the migrations** with the `shadoucmdb` binary (build it with `cargo build --release` in
   `backend/`). The migrations are embedded in the binary:

   ```sh
   shadoucmdb migrate
   ```

   Expected output on an empty database (`<N>` is the number of migrations shipped in the binary,
   one per file in [`sql/migrations/`](sql/migrations/); it grows with new releases):

   ```
   Connected to database "shadoucmdb" (PostgreSQL 18.1), ssl=verify-full
   Migrations: <N> in binary, 0 applied, <N> pending
     applied 0000_extensions
     applied 0001_core_schema
     …
     applied <latest migration>
   Database is at migration <N>/<N>
   ```

   `migrate` can also print a `Data model: … statements applied` line when it brings the reporting
   views and grants in line with the data model.

   Re-running is safe; it reports `nothing to do`. Applied migrations are tracked in `_sqlx_migrations`.
   A dev database that was migrated by the retired Node/Drizzle runner needs a one-time
   `shadoucmdb migrate --adopt-drizzle` (or a reset).

4. **Seed.** A fresh install starts bare: no CI classes, attributes, relationship types or
   lookups. You build the data model under *Administration*, or install the **IT infrastructure**
   starter template (servers, VMs, network devices, applications, databases, services and
   locations, with their attributes, relationship rules, and lookup lists for status, environment and
   location). Seeding is idempotent and never overwrites rows you have edited:

   ```sh
   shadoucmdb seed                                # system rows only (the data model stays empty)
   shadoucmdb seed --template it_infrastructure   # install the starter template (or: Administration > Templates)
   shadoucmdb seed --demo                         # the template plus a small sample inventory (only into an empty CI table)
   ```

   Upgrading an existing install keeps its data: the classes and lookups it was seeded with stay,
   and the template reports itself as installed.

5. **Check the schema** (optional). This runs the acceptance checks inside a transaction that
   is rolled back, so it writes nothing:

   ```sh
   shadoucmdb verify
   ```

6. **Create the encryption key.** The server encrypts the users' authenticator (TOTP) secrets with a
   key kept outside the database and does not start without it:

   ```sh
   shadoucmdb generate-encryption-key --out /etc/shadoucmdb/encryption.key   # then, in .env:
   ENCRYPTION_KEY_FILE=/etc/shadoucmdb/encryption.key
   ```

   Keep a copy apart from the database backups: without it, a restored backup has no usable two-factor
   enrolments or identity provider secrets.

7. **Start the backend.** `shadoucmdb serve` serves the API, `/healthz`, `/readyz` and the embedded web UI on
   `API_HOST:API_PORT` (default `0.0.0.0:3000`). `GET /healthz` reports liveness. `GET /readyz` returns 200 only
   when the database is reachable and all migrations are applied, and 503 otherwise. If migrations are pending,
   `serve` logs a warning at startup and API calls answer 503 `SCHEMA_NOT_MIGRATED` until you run
   `shadoucmdb migrate`. During development,
   `cargo run -- serve` in `backend/` does the same. To run as a systemd service, use the unit in
   `deploy/systemd/`; for a Windows Service, follow `deploy/windows/README.txt`; for a container, see below.

8. **Create the first administrator.** Every page and API call except the health probes needs a signed-in
   user. Open the web UI, which offers first-run setup while no user exists (it asks for the one-time setup
   token the server writes to `SETUP_TOKEN_FILE`, by default `setup-token` next to the env file; with no
   such file the token is written to the log instead), or run:

   ```sh
   shadoucmdb create-admin --username admin --display-name "Jane Admin" --email jane.admin@example.com   # prompts for the password
   ```

   Further users and their permission profiles are managed under Administration. The server speaks plain
   HTTP: in production put a TLS reverse proxy in front of it, bind the API to `127.0.0.1` (`API_HOST`, or
   publish the Docker port on `127.0.0.1`) when the proxy runs on the same host, and let the proxy
   send `X-Forwarded-Proto` so session cookies are marked `Secure`. For single sign-on through an
   OIDC provider or LDAP/AD, set `PUBLIC_URL` and add the identity provider under Administration.

9. **Use the API.** It lives under `/api/v1`. With `API_DOCS=authenticated` or `public` (off by default) the
   OpenAPI 3.1 contract is served at `/openapi.json`, and there is a browsable UI at `/docs`. The same contract is committed as
   [`backend/openapi.json`](backend/openapi.json). To check a deployment end to end:

   ```sh
   API_URL=http://localhost:3000 SMOKE_USERNAME=admin SMOKE_PASSWORD=... node tools/smoke/smoke.ts   # needs `seed --demo` data
   ```

### With Docker

The root `Dockerfile` builds a small multi-arch (amd64/arm64), non-root image of `shadoucmdb`:
`docker run --rm --env-file .env shadoucmdb migrate`, then `docker run -d --env-file .env -p 3000:3000 shadoucmdb`.

`docker-compose.yml` builds the same image and reads `.env`. It contains **no** database service.
The schema owner's connection goes into `.env.migrate`, which only the `migrate` service reads; the
running `api` never receives it.

```sh
cp .env.example .env                     # point it at your PostgreSQL (as shadoucmdb_app)
openssl rand -base64 32 > encryption.key && chmod 600 encryption.key && sudo chown 65532:65532 encryption.key
install -m 600 /dev/null .env.migrate    # add MIGRATION_DATABASE_URL and MAINTENANCE_DATABASE_URL
docker compose run --rm migrate          # apply migrations
docker compose run --rm seed             # system rows (`seed seed --demo` adds the IT template and sample CIs)
docker compose up api                    # http://localhost:3000/readyz
```

For a PostgreSQL running on the Docker host itself, set `PGHOST=host.docker.internal`, not
`localhost`: inside a container, localhost is the container.

The container has no env file and no token file, so a generated first-run setup token goes to
`docker compose logs api`, where anyone with access to the container logs (or a log shipper) can read it.
Where that is more people than should own the instance, create the first administrator with
`docker compose run --rm seed create-admin --username admin --email admin@example.com` instead, or set `SETUP_TOKEN` in `.env` and
remove it once setup is done.

## Backup, restore and reset

`shadoucmdb backup` writes a consistent, checksummed backup of every table without pg_dump.
`shadoucmdb restore FILE` checks it and restores it in one transaction. `factory-reset` returns
an installation to first-run setup, and `decommission` removes all ShadouCMDB data and settings from
the database before you retire it.

With separate database roles, `backup` connects as the API role (`DATABASE_URL`) and `restore` as the
schema owner (`MIGRATION_DATABASE_URL`). Keep the owner's credentials out of scheduled backup jobs.

Run `backup` with the server's `ENCRYPTION_KEY_FILE`: the file is then sealed with an HMAC under a key
derived from it, and `restore` refuses a file that was edited after the backup. `restore` checks the
seal with `ENCRYPTION_KEY_FILE` or `ENCRYPTION_KEY_PREVIOUS_FILE`; a backup without a seal (taken
before ShadouCMDB sealed backups, or without the key) or sealed with a key that is not configured is
restored only with `--allow-unsigned`. Each restore records a `backup.restore` audit entry with the
audit chain head it brought back; compare it with the SIEM copy (`AUDIT_EXPORT`). Run `restore` with
the server's `AUDIT_EXPORT` settings: it sends the entry to the collector as soon as the restore is
committed (not to `stdout`, nor to an export file that does not exist yet), and the server sends it
again, with the rows after it, when it starts.

Because `restore`, `factory-reset` and `decommission` each run in one transaction, they hold a lock on
every table, index and constraint of the installation until they commit, and PostgreSQL keeps those in
a lock table sized by `max_locks_per_transaction` × (`max_connections` + `max_prepared_transactions`).
Measured on PostgreSQL 17 with the default 64 locks and 100 connections:

| Data model | Locks held by `restore` | Result at the defaults |
|---|---|---|
| no CI types | about 4,400 | succeeds |
| 200 CI types (8 fields each) | about 9,000 | succeeds |
| 400 CI types | about 13,600 | succeeds |
| 1,000 CI types | about 27,400 | fails: `out of shared memory` (SQLSTATE 53200) |

At the limit, the restore stops with an error such as `could not run: ALTER TABLE … : out of shared
memory`, and PostgreSQL's log adds `HINT: You might need to increase "max_locks_per_transaction".` Nothing
is changed: the transaction is rolled back and the database keeps its previous content. With more than
about 400 CI types, set `max_locks_per_transaction = 256` in `postgresql.conf` (a server restart is
needed), and check it before you need a restore. With that setting, restores of 1,000 and 2,000 CI types
(about 27,400 and 50,400 locks) succeeded in the same measurement. Locks scale with the number of CI
types, at roughly 4,400 plus 23 per type. On a managed PostgreSQL service, the setting is usually
changed in the instance's parameter group.

## Changing the schema

Every schema change is a migration. No hand-applied DDL.

1. Write the next migration by hand as `sql/migrations/<NNNN>_<what_changed>.sql` (next number, four digits).
2. Rebuild and run `shadoucmdb migrate`, then `shadoucmdb verify`. If a compile-time checked query
   (`sqlx::query!`) is affected, refresh `backend/.sqlx`: rebuild once
   against a migrated database with `SQLX_OFFLINE_DIR=$PWD/.sqlx DATABASE_URL=... cargo build` in `backend/`
   and commit the updated files (delete stale ones first).
   Commit the migration, the code change and any ERD update together. See [`sql/README.md`](sql/README.md).

Adding an area, CI type, field or relationship type is **data**, not a migration: the application's
DDL engine creates the matching PostgreSQL schema, table or column (area "Bestand" + type "Netzwerk"
→ table `bestand.netzwerk`), so the API role needs the `CREATE` privilege on its database and owns the area schemas (see
[`sql/README.md`](sql/README.md)).

## Contributing

Branching, pull request and schema-change rules are in [CONTRIBUTING.md](CONTRIBUTING.md).

## Security

Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md), never in a public issue.
ShadouCMDB sends no telemetry.
