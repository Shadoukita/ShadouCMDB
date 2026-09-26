# sql/

Everything that defines or sets up the ShadouCMDB database lives here. The backend
(`backend/`) is the only application that connects to PostgreSQL. The `shadoucmdb`
binary embeds the migrations from this folder at build time.

| Path | What |
| --- | --- |
| [`migrations/`](migrations/) | Versioned schema migrations `<NNNN>_<name>.sql`, applied in order by `shadoucmdb migrate` and tracked in `_sqlx_migrations`. `meta/_journal.json` only exists for the Node API's migration count and is removed with it (SHAA-9). |
| [`bootstrap/`](bootstrap/) | One-time admin scripts run **before** the first migration (role and database creation). Not tracked by the migration runner. |
| [`diagrams/`](diagrams/) | Schema diagrams. Start with [`diagrams/erd.md`](diagrams/erd.md). |

The table-by-table reference, integrity rules and soft-delete decisions are in
[`docs/data-model.md`](../docs/data-model.md).

## Setting up a new database

```sh
# 1. As a PostgreSQL admin, create the app role and database
psql "<admin connection string>" -v app_password='<strong password>' \
     -f sql/bootstrap/00_create_role_and_database.sql

# 2. Point the backend at it (DATABASE_URL or PG* variables)
cp .env.example .env

# 3. Apply migrations and load reference data
shadoucmdb migrate
shadoucmdb seed
```

Credentials never go into this folder or anywhere else in git; they belong in `.env`
(ignored) or your deployment's secret store.

## Rules for changing the schema

- Every change is a new migration. Never edit a migration that has been merged to `main`:
  `shadoucmdb migrate` compares checksums and refuses to run against an edited, already-applied file.
- Write migrations by hand as `migrations/<NNNN>_<snake_case_name>.sql`, where `NNNN` is the next number.
  The file name is the version (`0003_…` is version 3). Each file runs in its own transaction; start
  the file with `-- no-transaction` only for statements such as `CREATE INDEX CONCURRENTLY`.
  (The Drizzle snapshots were removed, so `drizzle-kit generate` no longer applies.)
- Until the Node API is retired (SHAA-9), also mirror the change in `backend/src/db/schema/` and append
  an entry to `migrations/meta/_journal.json` (`idx`, `tag` = file name without `.sql`, `when`).
- Commit the migration, the schema mirror and any ERD update in the same pull request.

## Databases migrated by the old Node runner

Before `shadoucmdb`, migrations were tracked in `drizzle.__drizzle_migrations`. Run
`shadoucmdb migrate --adopt-drizzle` once on such a database: it checks the recorded SHA-256
hashes against the files here and records them in `_sqlx_migrations` without re-running
anything. Resetting the database is the alternative. See
[`docs/deployment.md`](../docs/deployment.md#moving-a-dev-database-off-the-nodedrizzle-migration-runner).
- Adding CI classes, attributes or relationship types is data, not schema: do it through the API
  or the seed, not a migration.
