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
--      shadoucmdb_app), so migrations 0007 to 0065 are applied. The script
--      takes the API role's rights from migration 0065 on (GH#713); use the
--      script of the release you migrated with.
--   2. Stop the server. Run this script as an administrator, connected to the
--      ShadouCMDB database:
--        psql "postgres://admin@db.example.internal:5432/shadoucmdb" \
--             -f sql/bootstrap/10_split_roles.sql
--      The role names are the defaults. If your API role is named differently
--      (the DATABASE_URL user), add -v app_role=<that name>; name the new
--      roles with -v owner_role=... and -v maintenance_role=... if you like.
--   3. Set the passwords of the roles the script created (it lists them at
--      the end); they cannot log in before. \password prompts for each and
--      sends the server only a SCRAM-SHA-256 verifier, never the password:
--        psql "postgres://admin@db.example.internal:5432/shadoucmdb" \
--             -c '\password shadoucmdb_owner' -c '\password shadoucmdb_maintenance'
--      Passwords are not psql variables (-v), which would show up in the
--      process list and in the server log. A re-run leaves existing roles and
--      their passwords alone. Without a terminal, \password reads the password
--      and its confirmation from standard input; see 00_create_role_and_database.sql.
--   4. Set MIGRATION_DATABASE_URL (owner) and MAINTENANCE_DATABASE_URL
--      (maintenance); DATABASE_URL stays on the API role. Start the server.

\set ON_ERROR_STOP on
-- Older versions took the passwords as psql variables: stop rather than
-- silently create the roles without the password passed in.
\if :{?owner_password}
  \set password_var 1
\elif :{?maintenance_password}
  \set password_var 1
\endif
\if :{?password_var}
DO $$ BEGIN RAISE EXCEPTION 'the owner_password and maintenance_password variables are no longer read'
  USING HINT = 'Run the script without them, then set each new role''s password with \password <role>, as described in its header.'; END $$;
\endif
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
     OR to_regprocedure('cmdb.apply_api_role_grants(name)') IS NULL THEN
    RAISE EXCEPTION 'migrations 0007 to 0065 are not applied: run `shadoucmdb migrate` of this release first';
  END IF;
END;
$$;

SELECT NOT EXISTS (SELECT FROM pg_roles WHERE rolname = :'owner_role') AS create_owner,
       NOT EXISTS (SELECT FROM pg_roles WHERE rolname = :'maintenance_role') AS create_maintenance
\gset
-- No PASSWORD here: set it with \password afterwards (step 3 above).
\if :create_owner
CREATE ROLE :"owner_role" LOGIN;
\endif
\if :create_maintenance
CREATE ROLE :"maintenance_role" LOGIN;
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

-- The maintenance role prunes the audit log (migrations 0007 and 0008).
GRANT USAGE ON SCHEMA cmdb TO :"maintenance_role";
GRANT EXECUTE ON FUNCTION cmdb.prune_audit_log(interval, text, boolean, text) TO :"maintenance_role";
-- The API role gets the same rights on the system tables and routines as on a
-- fresh three-role install, from the list the migrations keep
-- (cmdb.api_role_privileges, migration 0065, GH#713).
SELECT cmdb.apply_api_role_grants(:'app_role') \gset ignored_
ALTER DEFAULT PRIVILEGES FOR ROLE :"owner_role" IN SCHEMA cmdb
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO :"app_role";
ALTER DEFAULT PRIVILEGES FOR ROLE :"owner_role" IN SCHEMA cmdb
  GRANT USAGE, SELECT ON SEQUENCES TO :"app_role";

COMMIT;

\if :create_owner
\echo 'Created role' :"owner_role" '- set its password before it can log in:  \\password' :"owner_role"
\endif
\if :create_maintenance
\echo 'Created role' :"maintenance_role" '- set its password before it can log in:  \\password' :"maintenance_role"
\endif
