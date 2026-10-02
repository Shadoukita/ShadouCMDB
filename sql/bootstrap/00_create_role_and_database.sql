-- ShadouCMDB: one-time setup on the external PostgreSQL server (14+).
-- Run as an administrator, e.g.:
--   psql "postgres://admin@db.example.internal:5432/postgres" \
--        -f sql/bootstrap/00_create_role_and_database.sql
-- then set the three passwords with psql's \password, which prompts for them
-- and sends the server only a SCRAM-SHA-256 verifier computed by psql:
--   psql "postgres://admin@db.example.internal:5432/postgres" \
--        -c '\password shadoucmdb_owner' -c '\password shadoucmdb_app' \
--        -c '\password shadoucmdb_maintenance'
-- The roles can log in only once their password is set. Passwords are not
-- psql variables (-v): those show up in the process list, and the statement
-- would carry them in plain text into the server log (log_statement = ddl).
-- Without a terminal (automation), \password reads the password and its
-- confirmation as two lines from standard input. Guard the variable and check
-- the result, e.g. from bash:
--   : "${PW:?PW is unset or empty}"
--   printf '%s\n%s\n' "$PW" "$PW" | setsid -w psql "<admin URL>" -X -v ON_ERROR_STOP=1 \
--        -c '\password shadoucmdb_owner'
--   test "$(psql "<admin URL>" -XAtc "SELECT rolpassword LIKE 'SCRAM-SHA-256\$%' \
--        FROM pg_authid WHERE rolname = 'shadoucmdb_owner'")" = t
-- The guard and the check are needed because empty input (an unset or empty
-- variable, or fewer lines than \password commands) makes the server answer
-- "empty string is not a valid password, clearing password" while psql exits 0:
-- the role ends up without a password and the step still reports success.
-- With several roles, supply one password and one confirmation line per
-- \password command, in order, and check every role. Without a superuser
-- administrator, pg_authid is unreadable; test by logging in as the role instead
-- (PGPASSWORD="$PW" psql "postgres://<role>@<db-host>:5432/<db>" -X -c 'SELECT 1').
--
-- The role and database names below are the defaults. To use your own naming
-- scheme, add any of -v owner_role=... -v app_role=... -v maintenance_role=...
-- -v db_name=...; the connection strings then name those roles and that
-- database, and `shadoucmdb migrate` grants to whatever roles they use.
--
-- Three roles, none of them superuser:
--   shadoucmdb_owner        owns the database and the cmdb system schema;
--                           `shadoucmdb migrate` connects as it
--                           (MIGRATION_DATABASE_URL). Keep its password away
--                           from the running server.
--   shadoucmdb_app          the API (DATABASE_URL): reads and writes data, may
--                           only INSERT into audit_log, cannot prune it. It
--                           owns the data side of the model: at run time it
--                           creates a PostgreSQL schema for every area an
--                           administrator adds ("Bestand" -> schema bestand)
--                           and the tables, columns and reporting views of its
--                           types, so it holds CREATE on the database.
--   shadoucmdb_maintenance  `shadoucmdb prune-audit` (MAINTENANCE_DATABASE_URL):
--                           may only execute cmdb.prune_audit_log().
-- shadoucmdb_owner is a member of shadoucmdb_app, so the migrations can build
-- and hand over area schemas and type tables. Never the other way round.
-- The table grants themselves are made by the migrations, which run as the owner.
-- Installs created with the older single-role version of this script are split
-- with 10_split_roles.sql.

\set ON_ERROR_STOP on
-- Older versions of this script took the passwords as psql variables. Stop
-- instead of silently creating the roles without the password passed in.
\if :{?owner_password}
  \set password_var 1
\elif :{?app_password}
  \set password_var 1
\elif :{?maintenance_password}
  \set password_var 1
\endif
\if :{?password_var}
DO $$ BEGIN RAISE EXCEPTION 'the owner_password, app_password and maintenance_password variables are no longer read'
  USING HINT = 'Run the script without them, then set each password with \password <role>, as described in its header.'; END $$;
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
\if :{?db_name}
\else
  \set db_name shadoucmdb
\endif

-- No PASSWORD here: see the header. Set them with \password afterwards.
CREATE ROLE :"owner_role" LOGIN;
CREATE ROLE :"app_role" LOGIN;
CREATE ROLE :"maintenance_role" LOGIN;
GRANT :"app_role" TO :"owner_role";
-- Pin the search_path: the default "$user", public would look first in a
-- schema named after the role, and schemas are what the API role creates.
ALTER ROLE :"owner_role" SET search_path = cmdb, public;
ALTER ROLE :"maintenance_role" SET search_path = cmdb, public;
CREATE DATABASE :"db_name" OWNER :"owner_role" ENCODING 'UTF8';
REVOKE ALL ON DATABASE :"db_name" FROM PUBLIC;
GRANT CONNECT ON DATABASE :"db_name" TO :"app_role", :"maintenance_role";
GRANT CREATE ON DATABASE :"db_name" TO :"app_role";

\connect :"db_name"
-- Only the owner role creates objects in public and cmdb. PostgreSQL 14 lets
-- every role create in schema public, which would let the API role plant a
-- function that the owner-privileged prune_audit_log() or a migration then
-- runs (15+ already withholds it; the REVOKE is then a no-op).
GRANT CREATE ON SCHEMA public TO :"owner_role";
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
-- Only needed if your provider does not let the database owner create trusted
-- extensions; harmless otherwise.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Optional: read-only reporting. The application grants the role named
-- cmdb_reporting USAGE on every area schema and SELECT on every reporting view
-- (<area>.v_<type>), and nothing else: no access to the cmdb system tables or
-- the raw type tables. Create it, then give it to the accounts of your
-- reporting tool. It is picked up by the next `shadoucmdb migrate` or
-- POST /api/v1/schema-changes/reconcile, and kept up to date afterwards.
--   CREATE ROLE cmdb_reporting NOLOGIN;
--   CREATE ROLE report_reader LOGIN IN ROLE cmdb_reporting;
--   \password report_reader

\echo
\echo 'Created the roles and the database. Next, set the passwords of the three roles; they cannot log in before:'
\echo '  \\password' :"owner_role"
\echo '  \\password' :"app_role"
\echo '  \\password' :"maintenance_role"
