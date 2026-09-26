-- ShadouCMDB: one-time setup on the external PostgreSQL server (14+).
-- Run as an administrator, e.g.:
--   psql "postgres://admin@db.example.internal:5432/postgres" \
--        -v owner_password='<strong password 1>' \
--        -v app_password='<strong password 2>' \
--        -v maintenance_password='<strong password 3>' \
--        -f sql/bootstrap/00_create_role_and_database.sql
--
-- Three roles, none of them superuser:
--   shadoucmdb_owner        owns the database and schema; `shadoucmdb migrate`
--                           connects as it (MIGRATION_DATABASE_URL). Keep its
--                           password away from the running server.
--   shadoucmdb_app          the API (DATABASE_URL): reads and writes data, may
--                           only INSERT into audit_log, cannot prune it.
--   shadoucmdb_maintenance  `shadoucmdb prune-audit` (MAINTENANCE_DATABASE_URL):
--                           may only execute prune_audit_log().
-- The grants themselves are made by migration 0005, which runs as the owner.
-- Installs created with the older single-role version of this script are split
-- with 10_split_roles.sql.

CREATE ROLE shadoucmdb_owner LOGIN PASSWORD :'owner_password';
CREATE ROLE shadoucmdb_app LOGIN PASSWORD :'app_password';
CREATE ROLE shadoucmdb_maintenance LOGIN PASSWORD :'maintenance_password';
CREATE DATABASE shadoucmdb OWNER shadoucmdb_owner ENCODING 'UTF8';
REVOKE ALL ON DATABASE shadoucmdb FROM PUBLIC;
GRANT CONNECT ON DATABASE shadoucmdb TO shadoucmdb_app, shadoucmdb_maintenance;

\connect shadoucmdb
-- Only needed if your provider does not let the database owner create trusted
-- extensions; harmless otherwise.
CREATE EXTENSION IF NOT EXISTS pg_trgm;
