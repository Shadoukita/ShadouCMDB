-- ShadouCMDB: one-time upgrade of a single-role install to three roles (SHAA-45).
--
-- Before this, shadoucmdb_app owned the database and every table, so the API
-- could disable the audit_log trigger or run the purge itself. Afterwards it
-- owns only the data side of the model, the area schemas with their type
-- tables and reporting views, which it alters at run time. shadoucmdb_owner
-- owns the database and the cmdb system schema and is a member of
-- shadoucmdb_app (so migrations can build type tables); shadoucmdb_maintenance
-- may only prune. Safe to re-run.
--
-- Order:
--   1. Install the new binary and run `shadoucmdb migrate` as before (as
--      shadoucmdb_app), so migrations 0007 to 0009 are applied.
--   2. Stop the server. Run this script as an administrator, connected to the
--      ShadouCMDB database:
--        psql "postgres://admin@db.example.internal:5432/shadoucmdb" \
--             -v owner_password='<strong password>' \
--             -v maintenance_password='<strong password>' \
--             -f sql/bootstrap/10_split_roles.sql
--   3. Set MIGRATION_DATABASE_URL (owner) and MAINTENANCE_DATABASE_URL
--      (maintenance); DATABASE_URL stays on shadoucmdb_app. Start the server.
-- See docs/deployment.md, "Database roles".

\set ON_ERROR_STOP on
BEGIN;

DO $$
BEGIN
  IF to_regprocedure('cmdb.prune_audit_log(interval, text, boolean, text)') IS NULL
     OR to_regclass('cmdb.areas') IS NULL THEN
    RAISE EXCEPTION 'migrations 0007 to 0009 are not applied: run `shadoucmdb migrate` first';
  END IF;
END;
$$;

SELECT NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'shadoucmdb_owner') AS create_owner,
       NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'shadoucmdb_maintenance') AS create_maintenance
\gset
\if :create_owner
CREATE ROLE shadoucmdb_owner LOGIN PASSWORD :'owner_password';
\endif
\if :create_maintenance
CREATE ROLE shadoucmdb_maintenance LOGIN PASSWORD :'maintenance_password';
\endif
GRANT shadoucmdb_app TO shadoucmdb_owner;
-- See 00_create_role_and_database.sql: no "$user" schema lookup for these roles.
ALTER ROLE shadoucmdb_owner SET search_path = cmdb, public;
ALTER ROLE shadoucmdb_maintenance SET search_path = cmdb, public;

-- Hand over the database and everything shadoucmdb_app created in it outside
-- the area schemas: schemas, tables (their identity sequences follow), other
-- sequences and functions, including _sqlx_migrations. Not REASSIGN OWNED,
-- which would also take the area schemas and other databases shadoucmdb_app
-- might own on the same server.
SELECT format('ALTER DATABASE %I OWNER TO shadoucmdb_owner', current_database()) \gexec
DO $$
DECLARE
  obj record;
  -- Read up front: the loops below change the owner of cmdb.areas itself.
  area_keys text[] := ARRAY(SELECT key FROM cmdb.areas);
BEGIN
  FOR obj IN
    SELECT n.nspname FROM pg_namespace n WHERE n.nspowner = 'shadoucmdb_app'::regrole
      AND n.nspname <> ALL (area_keys)
  LOOP
    EXECUTE format('ALTER SCHEMA %I OWNER TO shadoucmdb_owner', obj.nspname);
  END LOOP;
  FOR obj IN
    SELECT c.oid::regclass AS name FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relowner = 'shadoucmdb_app'::regrole AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
      AND n.nspname <> ALL (area_keys)
  LOOP
    EXECUTE format('ALTER TABLE %s OWNER TO shadoucmdb_owner', obj.name);
  END LOOP;
  FOR obj IN
    SELECT c.oid::regclass AS name FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relowner = 'shadoucmdb_app'::regrole AND c.relkind = 'S'
      AND n.nspname <> ALL (area_keys)
  LOOP
    EXECUTE format('ALTER SEQUENCE %s OWNER TO shadoucmdb_owner', obj.name);
  END LOOP;
  FOR obj IN
    SELECT p.oid::regprocedure AS name FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE p.proowner = 'shadoucmdb_app'::regrole
      AND n.nspname <> ALL (area_keys)
      AND NOT EXISTS (SELECT FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e')
  LOOP
    EXECUTE format('ALTER ROUTINE %s OWNER TO shadoucmdb_owner', obj.name);
  END LOOP;
END;
$$;

SELECT format('REVOKE ALL ON DATABASE %I FROM PUBLIC', current_database()) \gexec
-- Only shadoucmdb_owner creates objects: PostgreSQL 14 grants CREATE on public
-- to every role, the API role included (see 00_create_role_and_database.sql).
GRANT CREATE ON SCHEMA public TO shadoucmdb_owner;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
SELECT format('GRANT CONNECT ON DATABASE %I TO shadoucmdb_app, shadoucmdb_maintenance', current_database()) \gexec
-- New areas are new schemas, created by the API.
SELECT format('GRANT CREATE ON DATABASE %I TO shadoucmdb_app', current_database()) \gexec

-- Same grants as migrations 0007 and 0008 make on a fresh three-role install.
GRANT USAGE ON SCHEMA cmdb TO shadoucmdb_app, shadoucmdb_maintenance;
GRANT EXECUTE ON FUNCTION cmdb.prune_audit_log(interval, text, boolean, text) TO shadoucmdb_maintenance;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA cmdb TO shadoucmdb_app;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA cmdb TO shadoucmdb_app;
REVOKE UPDATE, DELETE, TRUNCATE ON cmdb.audit_log, cmdb.schema_changes FROM shadoucmdb_app;
GRANT REFERENCES ON cmdb.configuration_items, cmdb.lookup_list_values TO shadoucmdb_app;
GRANT SELECT ON public._sqlx_migrations TO shadoucmdb_app;
REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON public._sqlx_migrations FROM shadoucmdb_app;
ALTER DEFAULT PRIVILEGES FOR ROLE shadoucmdb_owner IN SCHEMA cmdb
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO shadoucmdb_app;
ALTER DEFAULT PRIVILEGES FOR ROLE shadoucmdb_owner IN SCHEMA cmdb
  GRANT USAGE, SELECT ON SEQUENCES TO shadoucmdb_app;

COMMIT;
