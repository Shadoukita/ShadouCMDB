-- ShadouCMDB: one-time setup on the external PostgreSQL server (14+).
-- Run as an administrator, e.g.:
--   psql "postgres://admin@db.example.internal:5432/postgres" \
--        -v owner_password='<strong password 1>' \
--        -v app_password='<strong password 2>' \
--        -v maintenance_password='<strong password 3>' \
--        -f sql/bootstrap/00_create_role_and_database.sql
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

CREATE ROLE shadoucmdb_owner LOGIN PASSWORD :'owner_password';
CREATE ROLE shadoucmdb_app LOGIN PASSWORD :'app_password';
CREATE ROLE shadoucmdb_maintenance LOGIN PASSWORD :'maintenance_password';
GRANT shadoucmdb_app TO shadoucmdb_owner;
-- Pin the search_path: the default "$user", public would look first in a
-- schema named after the role, and schemas are what the API role creates.
ALTER ROLE shadoucmdb_owner SET search_path = cmdb, public;
ALTER ROLE shadoucmdb_maintenance SET search_path = cmdb, public;
CREATE DATABASE shadoucmdb OWNER shadoucmdb_owner ENCODING 'UTF8';
REVOKE ALL ON DATABASE shadoucmdb FROM PUBLIC;
GRANT CONNECT ON DATABASE shadoucmdb TO shadoucmdb_app, shadoucmdb_maintenance;
GRANT CREATE ON DATABASE shadoucmdb TO shadoucmdb_app;

\connect shadoucmdb
-- Only shadoucmdb_owner creates objects in public and cmdb. PostgreSQL 14 lets
-- every role create in schema public, which would let the API role plant a
-- function that the owner-privileged prune_audit_log() or a migration then
-- runs (15+ already withholds it; the REVOKE is then a no-op).
GRANT CREATE ON SCHEMA public TO shadoucmdb_owner;
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
--   CREATE ROLE report_reader LOGIN PASSWORD '<another strong password>' IN ROLE cmdb_reporting;
