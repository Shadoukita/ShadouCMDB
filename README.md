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
| `docs/data-model.md` | Data model, integrity rules and soft-delete decisions. |
| `.github/` | CI and release workflows, pull request template. Releases: see [CONTRIBUTING.md](CONTRIBUTING.md#cutting-a-release). |
| `SECURITY.md`, `docs/security/` | Vulnerability reporting and disclosure policy, hardening guide, support period, telemetry statement, secure development lifecycle, CRA incident process, risk assessment. See [docs/security](docs/security/README.md). |
| `docs/api.md` | API conventions, error envelope, endpoint overview, extension seams. |
| `backend/openapi.json` | OpenAPI contract generated from the code (`shadoucmdb openapi --out backend/openapi.json`; CI fails if it is stale). |
| `tools/` | `smoke/smoke.ts`: end-to-end check of every API operation against any API URL. `ldap-it/`: LDAPS sign-in against a real OpenLDAP directory. `openapi-diff.mjs`: semantic diff of two specs. |
| `Dockerfile`, `deploy/` | Multi-arch container image of `shadoucmdb`; systemd unit; release Dockerfile and the READMEs shipped in the release archives. See [docs/deployment.md](docs/deployment.md). |

## Requirements

- The `shadoucmdb` binary (Linux x64/ARM64, Windows x64), built with stable Rust 1.94+ or taken from a
  release, or Docker. Node.js 22.18+ is only needed for the frontend tooling and the smoke test.
- A reachable PostgreSQL **14 or newer**, anywhere: a managed service (RDS,
  Cloud SQL, Azure), another host on your network, or a local install.

## Pointing the app at an external PostgreSQL

Installing from a release? Follow the step-by-step [operator setup guide](docs/operator-setup.md),
from an empty directory to a verified install. The steps below are the same flow for a source checkout.

1. **Create the database and its roles** on your PostgreSQL server (run as an admin there):

   ```sh
   psql "<admin connection string>" -v owner_password='<pw 1>' -v app_password='<pw 2>' \
        -v maintenance_password='<pw 3>' -f sql/bootstrap/00_create_role_and_database.sql
   ```

   This creates `shadoucmdb_owner` (owns the schema, runs migrations), `shadoucmdb_app` (the API:
   reads and writes data, cannot delete audit history) and `shadoucmdb_maintenance` (may only prune
   the audit log). None needs superuser. See
   [docs/deployment.md](docs/deployment.md#database-roles); an install made with the older single-role
   setup is upgraded with `sql/bootstrap/10_split_roles.sql`. Migrations run `CREATE EXTENSION IF NOT EXISTS pg_trgm`;
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

   There is no default host. If nothing is configured, the backend exits with an error naming
   the missing variable. Every variable is documented in [`.env.example`](.env.example).

3. **Run the migrations** with the `shadoucmdb` binary (build it with `cargo build --release` in
   `backend/`; see [docs/deployment.md](docs/deployment.md)). The migrations are embedded in the binary:

   ```sh
   shadoucmdb migrate
   ```

   Expected output on an empty database:

   ```
   Connected to database "shadoucmdb" (PostgreSQL 18.1), ssl=verify-full
   Migrations: 10 in binary, 0 applied, 10 pending
     applied 0000_extensions
     applied 0001_core_schema
     applied 0002_integrity_triggers
     applied 0003_users_and_permission_profiles
     applied 0004_data_model_admin
     applied 0005_ui_settings
     applied 0006_auth_audit
     applied 0007_audit_retention
     applied 0008_cmdb_schema_and_areas
     applied 0009_type_tables
   Database is at migration 10/10
   ```

   Re-running is safe; it reports `nothing to do`. Applied migrations are tracked in `_sqlx_migrations`.
   A dev database that was migrated by the retired Node/Drizzle runner needs a one-time
   `shadoucmdb migrate --adopt-drizzle` (or a reset); see
   [docs/deployment.md](docs/deployment.md#moving-a-dev-database-off-the-nodedrizzle-migration-runner).

4. **Seed.** A fresh install starts bare: no CI classes, attributes, relationship types or
   lookups. You build the data model under *Administration*, or install the **IT infrastructure**
   starter template (servers, VMs, network devices, applications, databases, services and
   locations, with their attributes, relationship rules, statuses, environments and a sample
   location tree). Seeding is idempotent and never overwrites rows you have edited:

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

   Keep a copy apart from the database backups; see
   [docs/security/hardening.md](docs/security/hardening.md#encryption-key).

7. **Start the backend.** `shadoucmdb serve` serves the API, `/healthz`, `/readyz` and the embedded web UI on
   `API_HOST:API_PORT` (default `0.0.0.0:3000`). `GET /healthz` reports liveness. `GET /readyz` returns 200 only
   when the database is reachable and all migrations are applied, and 503 otherwise. If migrations are pending,
   `serve` logs a warning at startup and API calls answer 503 `SCHEMA_NOT_MIGRATED` until you run
   `shadoucmdb migrate`. During development,
   `cargo run -- serve` in `backend/` does the same. To run as a systemd service, a Windows Service or a
   container, see [docs/deployment.md](docs/deployment.md).

8. **Create the first administrator.** Every page and API call except the health probes needs a signed-in
   user. Open the web UI, which offers first-run setup while no user exists (it asks for the one-time setup
   token the server writes to its log; see [the setup token](docs/deployment.md#the-setup-token)), or run:

   ```sh
   shadoucmdb create-admin --username admin --display-name "Jane Admin"   # prompts for the password
   ```

   Further users and their permission profiles are managed under Administration. Behind a TLS proxy, let it
   send `X-Forwarded-Proto` so session cookies are marked `Secure`
   (see [docs/deployment.md](docs/deployment.md#https-and-session-cookies)). For single sign-on through an
   OIDC provider or LDAP/AD, set `PUBLIC_URL` and see [Enterprise sign-in](docs/api.md#enterprise-sign-in).

9. **Use the API.** It lives under `/api/v1`. With `API_DOCS=authenticated` or `public` (off by default) the
   OpenAPI 3.1 contract is served at `/openapi.json`, and there is a browsable UI at `/docs`. The same contract is committed as
   [`backend/openapi.json`](backend/openapi.json). See [docs/api.md](docs/api.md) for conventions
   (pagination, errors, attributes) and extension points. To check a deployment end to end:

   ```sh
   API_URL=http://localhost:3000 SMOKE_USERNAME=admin SMOKE_PASSWORD=... node tools/smoke/smoke.ts   # needs `seed --demo` data
   ```

### With Docker

The root `Dockerfile` builds a small multi-arch (amd64/arm64), non-root image of `shadoucmdb`:
`docker run --rm --env-file .env shadoucmdb migrate`, then `docker run -d --env-file .env -p 3000:3000 shadoucmdb`.
See [docs/deployment.md](docs/deployment.md#docker).

`docker-compose.yml` builds the same image and reads `.env`. It contains **no** database service.

```sh
cp .env.example .env                     # point it at your PostgreSQL
openssl rand -base64 32 > encryption.key && chmod 600 encryption.key && sudo chown 65532:65532 encryption.key
docker compose run --rm migrate          # apply migrations
docker compose run --rm seed             # system rows (`seed seed --demo` adds the IT template and sample CIs)
docker compose up api                    # http://localhost:3000/readyz
```

For a PostgreSQL running on the Docker host itself, set `PGHOST=host.docker.internal`, not
`localhost`: inside a container, localhost is the container.

## Backup, restore and reset

`shadoucmdb backup` writes a consistent, checksummed backup of every table without pg_dump.
`shadoucmdb restore FILE` checks it and restores it in one transaction. `factory-reset` returns
an installation to first-run setup, and `decommission` removes all ShadouCMDB data and settings from
the database before you retire it. See [docs/backup-and-reset.md](docs/backup-and-reset.md).

## Changing the schema

Every schema change is a migration. No hand-applied DDL.

1. Write the next migration by hand as `sql/migrations/<NNNN>_<what_changed>.sql` (next number, four digits).
2. Rebuild and run `shadoucmdb migrate`, then `shadoucmdb verify`. If a compile-time checked query
   (`sqlx::query!`) is affected, refresh `backend/.sqlx` (see [docs/api.md](docs/api.md#layers-and-extension-seams)).
   Commit the migration, the code change and any ERD update together. See [`sql/README.md`](sql/README.md).

Adding an area, CI type, field or relationship type is **data**, not a migration: the application's
DDL engine creates the matching PostgreSQL schema, table or column (area "Bestand" + type "Netzwerk"
→ table `bestand.netzwerk`), so the API role needs the `CREATE` privilege on its database and owns the area schemas (see
[`sql/README.md`](sql/README.md) and [Database roles](docs/deployment.md#database-roles)). See
[docs/data-model.md](docs/data-model.md#areas-type-tables-and-the-ddl-engine).

## Contributing

Branching, pull request and schema-change rules are in [CONTRIBUTING.md](CONTRIBUTING.md).

## Security

Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md), never in a public issue.
For production deployments, follow the [hardening guide](docs/security/hardening.md). ShadouCMDB sends
no telemetry ([details](docs/security/telemetry.md)).
