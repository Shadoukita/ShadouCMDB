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
| `backend/` | Node + Fastify + Drizzle (TypeScript). Owns the schema, migrations and API. |
| `backend/src/db/schema/` | Drizzle table definitions (source of truth for generated migrations). |
| `sql/` | Database artifacts: versioned migrations, bootstrap scripts, ER diagram. See [`sql/README.md`](sql/README.md). |
| `frontend/` | React + Vite + TanStack Query web UI. See [`frontend/README.md`](frontend/README.md). |
| `docs/data-model.md` | Data model, integrity rules and soft-delete decisions. |
| `.github/` | CI workflow and pull request template. |
| `docs/api.md` | API conventions, error envelope, endpoint overview, extension seams. |
| `backend/openapi.json` | Generated OpenAPI contract (`npm run openapi -w backend`). |

## Requirements

- Node.js 22.9+ (or Docker)
- A reachable PostgreSQL **14 or newer**, anywhere: a managed service (RDS,
  Cloud SQL, Azure), another host on your network, or a local install.

## Pointing the app at an external PostgreSQL

1. **Create a database and a role** on your PostgreSQL server (run as an admin there):

   ```sql
   CREATE ROLE shadoucmdb_app LOGIN PASSWORD '<a strong password>';
   CREATE DATABASE shadoucmdb OWNER shadoucmdb_app;
   ```

   The role does not need superuser. Migrations run `CREATE EXTENSION IF NOT EXISTS pg_trgm`;
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

   **or** the discrete `PGHOST`, `PGPORT`, `PGDATABASE`, `PGUSER`, `PGPASSWORD` variables.
   Then choose TLS with `DATABASE_SSL`:

   | `DATABASE_SSL` | Use when |
   | --- | --- |
   | `verify-full` | Production and managed databases. Add `DATABASE_SSL_CA_FILE` if the CA is not publicly trusted (e.g. the RDS CA bundle). |
   | `require` (default) | Encrypted, but the server certificate is not verified. |
   | `disable` | Only for a database on a trusted private network without TLS. |

   There is no default host. If nothing is configured, the backend exits with an error naming
   the missing variable. Every variable is documented in [`.env.example`](.env.example).

3. **Run the migrations.**

   ```sh
   npm install
   npm run db:migrate
   ```

   Expected output on an empty database:

   ```
   Connected to database "shadoucmdb" (PostgreSQL 18.1), ssl=verify-full
   Migrations: 3 in repo, 0 applied, 3 pending
     applied 0000_extensions
     applied 0001_core_schema
     applied 0002_integrity_triggers
   Database is at migration 3/3
   ```

   Re-running is safe; it reports `nothing to do`. Applied migrations are tracked in
   `drizzle.__drizzle_migrations`, and all pending migrations run in one transaction.

4. **Load reference data** (CI classes, attributes, statuses, environments, locations,
   relationship types). This step is idempotent and never overwrites rows you have edited:

   ```sh
   npm run db:seed                     # reference data only
   npm run db:seed -w backend -- --demo  # plus a small sample inventory (only into an empty CI table)
   ```

5. **Check the schema** (optional). This runs the acceptance checks inside a transaction that
   is rolled back, so it writes nothing:

   ```sh
   npm run db:verify -w backend
   ```

6. **Start the backend:** `npm run dev:backend`, or `npm run build -w backend && npm start -w backend`.
   `GET /healthz` reports liveness. `GET /readyz` returns 200 only when the database is reachable and
   all migrations are applied, and 503 otherwise.

7. **Use the API.** It lives under `/api/v1`. The OpenAPI 3.1 contract is served at `/openapi.json`, and
   there is a browsable UI at `/docs`. The same contract is committed as
   [`backend/openapi.json`](backend/openapi.json). See [docs/api.md](docs/api.md) for conventions
   (pagination, errors, attributes) and extension points. To check a deployment end to end:

   ```sh
   API_URL=http://localhost:3000 npm run smoke -w backend
   ```

### With Docker Compose

`docker-compose.yml` builds the backend and reads `.env`. It contains **no** database service.

```sh
cp .env.example .env                     # point it at your PostgreSQL
docker compose run --rm migrate          # apply migrations
docker compose run --rm seed             # reference data
docker compose up api                    # http://localhost:3000/readyz
```

For a PostgreSQL running on the Docker host itself, set `PGHOST=host.docker.internal`, not
`localhost`: inside a container, localhost is the container.

## Changing the schema

Every schema change is a migration. No hand-applied DDL.

1. Edit the Drizzle tables in `backend/src/db/schema/`.
2. `npm run db:generate -w backend -- --name=<what_changed>` writes the next SQL file into
   `sql/migrations/`. Review it.
3. For triggers, functions or data fixes that Drizzle cannot express, create an empty migration
   with `npx drizzle-kit generate --custom --name=<what>` (from `backend/`) and write the SQL.
4. `npm run db:migrate`, then commit the schema change and the migration together.

Adding a CI class, attribute or relationship type is **data**, not a schema change. See
[docs/data-model.md](docs/data-model.md#extending-the-model-without-migrations).

## Contributing

Branching, pull request and schema-change rules are in [CONTRIBUTING.md](CONTRIBUTING.md).
