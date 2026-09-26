-- ShadouCMDB: one-time upgrade of a single-role install to three roles (SHAA-45).
--
-- Before this, shadoucmdb_app owned the database and every table, so the API
-- could disable the audit_log trigger or run the purge itself. Afterwards it
-- owns nothing: shadoucmdb_owner owns the schema, shadoucmdb_maintenance may
-- only prune. Safe to re-run.
--
-- Order:
--   1. Install the new binary and run `shadoucmdb migrate` as before (as
--      shadoucmdb_app), so migration 0005 is applied.
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
  IF to_regprocedure('prune_audit_log(interval, text, boolean, text)') IS NULL THEN
    RAISE EXCEPTION 'migration 0005 is not applied: run `shadoucmdb migrate` first';
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

-- Hand over the database and everything shadoucmdb_app created in it: schemas,
-- tables (their identity sequences follow), other sequences and functions,
-- including _sqlx_migrations. Not REASSIGN OWNED, which would also take other
-- databases shadoucmdb_app might own on the same server.
SELECT format('ALTER DATABASE %I OWNER TO shadoucmdb_owner', current_database()) \gexec
DO $$
DECLARE
  obj record;
BEGIN
  FOR obj IN
    SELECT n.nspname FROM pg_namespace n WHERE n.nspowner = 'shadoucmdb_app'::regrole
  LOOP
    EXECUTE format('ALTER SCHEMA %I OWNER TO shadoucmdb_owner', obj.nspname);
  END LOOP;
  FOR obj IN
    SELECT c.oid::regclass AS name FROM pg_class c
    WHERE c.relowner = 'shadoucmdb_app'::regrole AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
  LOOP
    EXECUTE format('ALTER TABLE %s OWNER TO shadoucmdb_owner', obj.name);
  END LOOP;
  FOR obj IN
    SELECT c.oid::regclass AS name FROM pg_class c
    WHERE c.relowner = 'shadoucmdb_app'::regrole AND c.relkind = 'S'
  LOOP
    EXECUTE format('ALTER SEQUENCE %s OWNER TO shadoucmdb_owner', obj.name);
  END LOOP;
  FOR obj IN
    SELECT p.oid::regprocedure AS name FROM pg_proc p
    WHERE p.proowner = 'shadoucmdb_app'::regrole
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

-- Same grants as migration 0005 makes on a fresh three-role install.
GRANT EXECUTE ON FUNCTION prune_audit_log(interval, text, boolean, text) TO shadoucmdb_maintenance;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO shadoucmdb_app;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO shadoucmdb_app;
REVOKE UPDATE, DELETE, TRUNCATE ON audit_log FROM shadoucmdb_app;
REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON _sqlx_migrations FROM shadoucmdb_app;
ALTER DEFAULT PRIVILEGES FOR ROLE shadoucmdb_owner IN SCHEMA public
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO shadoucmdb_app;
ALTER DEFAULT PRIVILEGES FOR ROLE shadoucmdb_owner IN SCHEMA public
  GRANT USAGE, SELECT ON SEQUENCES TO shadoucmdb_app;

COMMIT;
