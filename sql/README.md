# sql/

Everything that defines or sets up the ShadouCMDB database lives here. The backend
(`backend/`) is the only application that connects to PostgreSQL. The `shadoucmdb`
binary embeds the migrations from this folder at build time.

| Path | What |
| --- | --- |
| [`migrations/`](migrations/) | Versioned schema migrations `<NNNN>_<name>.sql`, applied in order by `shadoucmdb migrate` and tracked in `_sqlx_migrations`. |
| [`bootstrap/`](bootstrap/) | One-time admin scripts: `00_…` creates the three roles and the database before the first migration; `10_split_roles.sql` upgrades an install made with the older single-role script. Not tracked by the migration runner. |
| [`diagrams/`](diagrams/) | Schema diagrams. Start with [`diagrams/erd.md`](diagrams/erd.md). |
| [`checks/`](checks/) | Optional operator scripts, e.g. the before/after comparison for the upgrade to per-type tables (migration 0009). |

The table-by-table reference, integrity rules and soft-delete decisions are in
[`docs/data-model.md`](../docs/data-model.md).

## Setting up a new database

```sh
# 1. As a PostgreSQL admin, create the roles (owner, app, maintenance) and the database
psql "<admin connection string>" -v owner_password='<pw 1>' -v app_password='<pw 2>' \
     -v maintenance_password='<pw 3>' -f sql/bootstrap/00_create_role_and_database.sql

# 2. Point the backend at it: DATABASE_URL (or PG*) as shadoucmdb_app,
#    MIGRATION_DATABASE_URL as shadoucmdb_owner, MAINTENANCE_DATABASE_URL as shadoucmdb_maintenance
cp .env.example .env

# 3. Apply migrations and check the system rows (add --template it_infrastructure for a starter data model)
shadoucmdb migrate
shadoucmdb seed
```

The application role must be able to **create schemas** in its database (the `CREATE` privilege
on the database): every area an administrator adds becomes a PostgreSQL schema, and its types
become tables in it. The bootstrap script grants it, and on a three-role install migration 0008
does too; the API role then owns the area schemas, and `shadoucmdb_owner` must be a member of it
(`GRANT shadoucmdb_app TO shadoucmdb_owner`, also in the bootstrap script). On a single-role
install owned by another role, run `GRANT CONNECT, CREATE ON DATABASE <db> TO <app role>`. See
[`docs/deployment.md`](../docs/deployment.md#database-roles).

System tables live in the `cmdb` schema; `public` keeps only extensions and `_sqlx_migrations`.
Area schemas (`bestand`, `infrastruktur`, …) and their tables are created by the application at run
time (the DDL engine, `backend/src/schema/`) and recorded in `cmdb.schema_changes`, not by migrations.

Credentials never go into this folder or anywhere else in git; they belong in `.env`
(ignored) or your deployment's secret store.

## Rules for changing the schema

- Every change is a new migration. Never edit a migration that has been merged to `main`:
  `shadoucmdb migrate` compares checksums and refuses to run against an edited, already-applied file.
- Write migrations by hand as `migrations/<NNNN>_<snake_case_name>.sql`, where `NNNN` is the next number.
  The file name is the version (`0003_…` is version 3). Each file runs in its own transaction; start
  the file with `-- no-transaction` only for statements such as `CREATE INDEX CONCURRENTLY`.
- If a compile-time checked query in `backend/src/data/` is affected, refresh `backend/.sqlx`
  (see [`docs/api.md`](../docs/api.md#layers-and-extension-seams)).
- Commit the migration, the code change and any ERD update in the same pull request.
- Migrations run as `shadoucmdb_owner`. Tables they create get `SELECT, INSERT, UPDATE, DELETE` for
  `shadoucmdb_app` automatically (default privileges set by `0007` for `public` and `0008` for
  `cmdb`). A table the API must not change, like `audit_log`, revokes those rights explicitly in
  its migration. After 0009, migrations do not write to area schemas: those belong to
  `shadoucmdb_app` (see [`docs/deployment.md`](../docs/deployment.md#database-roles)). The one
  exception is 0016, which moves the fixed CI columns into type tables; it runs as the schema owner,
  a member of `shadoucmdb_app`, so the tables keep their owner.
- Adding areas, CI types, fields or relationship types is data, not a migration: do it through the
  API or a starter template (`backend/src/modules/templates/`). The DDL engine creates the matching
  schema, table or column; never create or alter type tables by hand.

## Databases migrated by the retired Node runner

Before `shadoucmdb`, migrations were tracked in `drizzle.__drizzle_migrations`. Run
`shadoucmdb migrate --adopt-drizzle` once on such a database: it checks the recorded SHA-256
hashes against the files here and records them in `_sqlx_migrations` without re-running
anything. Resetting the database is the alternative. See
[`docs/deployment.md`](../docs/deployment.md#moving-a-dev-database-off-the-nodedrizzle-migration-runner).

## Upgrading to per-type tables (migration 0009)

Migration 0009 copies every value from `ci_attribute_values` into the new type tables, checks the
counts per attribute, and drops the EAV table; if a count differs, it fails and nothing changes.
For an independent check, run [`checks/eav_upgrade_1_before.sql`](checks/eav_upgrade_1_before.sql)
before the upgrade and [`checks/eav_upgrade_2_after.sql`](checks/eav_upgrade_2_after.sql) after it;
expect `missing = 0` and `unexpected = 0`. Take a backup first, as for any upgrade.

## Upgrading to the barebone CI core (migration 0016)

Migration 0016 gives every CI an `ident`, a validity period and a derived `label`, and moves the fixed
columns (`name`, `status_id`, `environment_id`, `owner_id`, `location_id`, `hostname`, `ip_address`,
`serial_number`, `notes`) into class fields in the type tables. It checks the counts per field and
drops the columns only if every value arrived; otherwise it fails and nothing changes. A view of your
own that reads those columns of `cmdb.configuration_items` blocks the migration: drop or rewrite it
first. For an independent check, run [`checks/core_ci_upgrade_1_before.sql`](checks/core_ci_upgrade_1_before.sql)
before the upgrade and [`checks/core_ci_upgrade_2_after.sql`](checks/core_ci_upgrade_2_after.sql) after
it; expect `missing = 0` and `wrong_labels = 0`. See
[`docs/data-model.md`](../docs/data-model.md#the-ci-core-ident-validity-and-label).
