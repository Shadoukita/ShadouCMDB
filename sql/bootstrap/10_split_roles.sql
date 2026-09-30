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
--      shadoucmdb_app), so migrations 0007 to 0021 are applied.
--   2. Stop the server. Run this script as an administrator, connected to the
--      ShadouCMDB database:
--        psql "postgres://admin@db.example.internal:5432/shadoucmdb" \
--             -v owner_password='<strong password>' \
--             -v maintenance_password='<strong password>' \
--             -f sql/bootstrap/10_split_roles.sql
--      The role names are the defaults. If your API role is named differently
--      (the DATABASE_URL user), add -v app_role=<that name>; name the new
--      roles with -v owner_role=... and -v maintenance_role=... if you like.
--   3. Set MIGRATION_DATABASE_URL (owner) and MAINTENANCE_DATABASE_URL
--      (maintenance); DATABASE_URL stays on the API role. Start the server.

\set ON_ERROR_STOP on
\if :{?owner_role}
\else
  \set owner_role shadoucmdb_owner
\endif
\if :{?app_role}
\else
  \set app_role shadoucmdb_app
\endif
\if :{?maintenance_role}
\else
  \set maintenance_role shadoucmdb_maintenance
\endif
BEGIN;
-- For the DO block below, which cannot see psql variables.
SELECT set_config('shadoucmdb.owner_role', :'owner_role', true),
       set_config('shadoucmdb.app_role', :'app_role', true) \gset ignored_

DO $$
BEGIN
  IF to_regprocedure('cmdb.prune_audit_log(interval, text, boolean, text)') IS NULL
     OR to_regclass('cmdb.areas') IS NULL
     OR to_regclass('cmdb.audit_log_chain_head') IS NULL
     OR to_regclass('cmdb.server_keys') IS NULL THEN
    RAISE EXCEPTION 'migrations 0007 to 0021 are not applied: run `shadoucmdb migrate` first';
  END IF;
END;
$$;

SELECT NOT EXISTS (SELECT FROM pg_roles WHERE rolname = :'owner_role') AS create_owner,
       NOT EXISTS (SELECT FROM pg_roles WHERE rolname = :'maintenance_role') AS create_maintenance
\gset
\if :create_owner
CREATE ROLE :"owner_role" LOGIN PASSWORD :'owner_password';
\endif
\if :create_maintenance
CREATE ROLE :"maintenance_role" LOGIN PASSWORD :'maintenance_password';
\endif
GRANT :"app_role" TO :"owner_role";
-- See 00_create_role_and_database.sql: no "$user" schema lookup for these roles.
ALTER ROLE :"owner_role" SET search_path = cmdb, public;
ALTER ROLE :"maintenance_role" SET search_path = cmdb, public;

-- Hand over the database and everything the API role created in it outside
-- the area schemas: schemas, tables (their identity sequences follow), other
-- sequences and functions, including _sqlx_migrations. Not REASSIGN OWNED,
-- which would also take the area schemas and other databases the API role
-- might own on the same server.
SELECT format('ALTER DATABASE %I OWNER TO %I', current_database(), :'owner_role') \gexec
DO $$
DECLARE
  obj record;
  owner_role name := current_setting('shadoucmdb.owner_role');
  app_role regrole := current_setting('shadoucmdb.app_role')::regrole;
  -- Read up front: the loops below change the owner of cmdb.areas itself.
  area_keys text[] := ARRAY(SELECT key FROM cmdb.areas);
BEGIN
  FOR obj IN
    SELECT n.nspname FROM pg_namespace n WHERE n.nspowner = app_role
      AND n.nspname <> ALL (area_keys)
  LOOP
    EXECUTE format('ALTER SCHEMA %I OWNER TO %I', obj.nspname, owner_role);
  END LOOP;
  FOR obj IN
    SELECT c.oid::regclass AS name FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relowner = app_role AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
      AND n.nspname <> ALL (area_keys)
  LOOP
    EXECUTE format('ALTER TABLE %s OWNER TO %I', obj.name, owner_role);
  END LOOP;
  FOR obj IN
    SELECT c.oid::regclass AS name FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relowner = app_role AND c.relkind = 'S'
      AND n.nspname <> ALL (area_keys)
  LOOP
    EXECUTE format('ALTER SEQUENCE %s OWNER TO %I', obj.name, owner_role);
  END LOOP;
  FOR obj IN
    SELECT p.oid::regprocedure AS name FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE p.proowner = app_role
      AND n.nspname <> ALL (area_keys)
      AND NOT EXISTS (SELECT FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e')
  LOOP
    EXECUTE format('ALTER ROUTINE %s OWNER TO %I', obj.name, owner_role);
  END LOOP;
END;
$$;

SELECT format('REVOKE ALL ON DATABASE %I FROM PUBLIC', current_database()) \gexec
-- Only the owner role creates objects: PostgreSQL 14 grants CREATE on public
-- to every role, the API role included (see 00_create_role_and_database.sql).
GRANT CREATE ON SCHEMA public TO :"owner_role";
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
SELECT format('GRANT CONNECT ON DATABASE %I TO %I, %I', current_database(), :'app_role', :'maintenance_role') \gexec
-- New areas are new schemas, created by the API.
SELECT format('GRANT CREATE ON DATABASE %I TO %I', current_database(), :'app_role') \gexec

-- Same grants as migrations 0007, 0008, 0018 and 0021 make on a fresh three-role install.
GRANT USAGE ON SCHEMA cmdb TO :"app_role", :"maintenance_role";
GRANT EXECUTE ON FUNCTION cmdb.prune_audit_log(interval, text, boolean, text) TO :"maintenance_role";
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA cmdb TO :"app_role";
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA cmdb TO :"app_role";
REVOKE UPDATE, DELETE, TRUNCATE ON cmdb.audit_log, cmdb.schema_changes FROM :"app_role";
GRANT REFERENCES ON cmdb.configuration_items, cmdb.lookup_list_values TO :"app_role";
GRANT SELECT ON public._sqlx_migrations TO :"app_role";
REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON public._sqlx_migrations FROM :"app_role";
-- Only the audit_log trigger moves the hash-chain head (migration 0018).
REVOKE ALL ON cmdb.audit_log_chain_head FROM :"app_role";
-- Server keys are read and added, never changed (migration 0021).
REVOKE UPDATE, DELETE, TRUNCATE ON cmdb.server_keys FROM :"app_role";
ALTER DEFAULT PRIVILEGES FOR ROLE :"owner_role" IN SCHEMA cmdb
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO :"app_role";
ALTER DEFAULT PRIVILEGES FOR ROLE :"owner_role" IN SCHEMA cmdb
  GRANT USAGE, SELECT ON SEQUENCES TO :"app_role";

COMMIT;
