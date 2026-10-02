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

## Setting up a new database

```sh
# 1. As a PostgreSQL admin, create the roles (owner, app, maintenance) and the database
psql "<admin connection string>" -f sql/bootstrap/00_create_role_and_database.sql
#    and set their passwords (prompted; the server only receives a SCRAM-SHA-256 verifier)
psql "<admin connection string>" -c '\password shadoucmdb_owner' -c '\password shadoucmdb_app' \
     -c '\password shadoucmdb_maintenance'

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
install owned by another role, run `GRANT CONNECT, CREATE ON DATABASE <db> TO <app role>`.

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
- If a compile-time checked query in `backend/src/data/` is affected, refresh `backend/.sqlx`: rebuild once
  against a migrated database with `SQLX_OFFLINE_DIR=$PWD/.sqlx DATABASE_URL=... cargo build` in `backend/`
  and commit the updated files (delete stale ones first).
- Commit the migration, the code change and any ERD update in the same pull request.
- Migrations run as `shadoucmdb_owner`. Tables they create get `SELECT, INSERT, UPDATE, DELETE` for
  `shadoucmdb_app` automatically (default privileges set by `0007` for `public` and `0008` for
  `cmdb`). A table the API must not change, like `audit_log`, revokes those rights explicitly in
  its migration. After 0009, migrations do not write to area schemas: those belong to
  `shadoucmdb_app`. The one
  exception is 0016, which moves the fixed CI columns into type tables; it runs as the schema owner,
  a member of `shadoucmdb_app`, so the tables keep their owner.
- On a three-role install `shadoucmdb_app` owns the area schemas, so whatever runs when a row is
  written there (triggers, rules, defaults, checks, domains) or a relation is read there is code the
  API role controls: the API role can replace a type table with a view of the same name (GH#469). A
  migration that reads or writes any relation in an area schema therefore switches to the API role
  first, for the rest of its transaction:

  ```sql
  DO $$
  DECLARE
    app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
  BEGIN
    IF EXISTS (SELECT FROM pg_roles WHERE rolname = app_role) AND current_user <> app_role THEN
      PERFORM set_config('role', app_role, true);  -- SET LOCAL ROLE
    END IF;
  END $$;
  ```

  `RESET ROLE` switches back. `migrate` also refuses to start while the API role's objects carry
  triggers, rules, row-level security, functions, or defaults, checks or domains that call anything
  but the engine's enum checks do, or while a registered type table is no longer a plain table
  (`refuse_planted_code` in `backend/src/db.rs`). That check is the second line, not a reason to
  skip the switch: it does not cover the views the engine itself creates in area schemas. 0036
  predates this rule and reads type tables as the owner; the type-table check covers it on upgrade.
- Bulk import stores classes, attributes and relationship types **by key** in `import_mappings.definition`
  and `import_jobs.mapping`/`class_key` (0029). A migration that renames keys or field references (as
  0020 did for UI settings) must rewrite those too.
- Saved views store classes, attributes and lookup lists and values **by key** in `saved_views.definition`,
  and a class key in `saved_view_defaults.home` (0039). A migration that renames keys or field references
  must rewrite those too.
- A migration that adds a global permission re-creates `permission_profile_global_permissions_valid` with
  the full list from the latest migration and extends `GlobalPermission::ALL`; the upgrade test of that
  migration calls `assert_permissions_match` (see `backend/src/db/upgrade_0039.rs`).
- Adding areas, CI types, fields or relationship types is data, not a migration: do it through the
  API or a starter template (`backend/src/modules/templates/`). The DDL engine creates the matching
  schema, table or column; never create or alter type tables by hand.

## Databases migrated by the retired Node runner

Before `shadoucmdb`, migrations were tracked in `drizzle.__drizzle_migrations`. Run
`shadoucmdb migrate --adopt-drizzle` once on such a database: it checks the recorded SHA-256
hashes against the files here and records them in `_sqlx_migrations` without re-running
anything. Resetting the database is the alternative.

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
it; expect `missing = 0` and `wrong_labels = 0`.

## Upgrading: Application criticality (migration 0036)

Migration 0036 moves the values of the IT infrastructure template's Application field `criticality`
onto the core Criticality of each CI (where that is empty and the value matches the Criticality
list), audits each copy, and archives the field. Nothing is deleted: the column and its values stay.
The field's audit entry (actor `migration 0036`) counts what moved and lists the values that did
not map. See [`changelog.d/GH-354.md`](../changelog.d/GH-354.md).

## Upgrading: layout templates (migration 0042)

Migration 0042 moves each class's detail page and form layout out of `settings.layouts[]` into a named
template in `settings.layoutTemplates[]` ("<class name> layout", keyed by the class key), and sets it as
the class's default (`layouts[].templateKey`); a class layout without tabs or fields uses the built-in
Standard template, which the migration adds. Every tab, section, field, hidden and read-only field
arrives unchanged, so pages look as before. The converted document is saved as a new settings version
and audited as `migration 0042`. It also creates `cmdb.ci_layout_overrides` (a CI's own layout: a
template key or a layout). See [`changelog.d/SHAA-1472.md`](../changelog.d/SHAA-1472.md).
