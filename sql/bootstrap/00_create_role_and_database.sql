-- ShadouCMDB: one-time setup on the external PostgreSQL server (14+).
-- Run as an administrator, e.g.:
--   psql "postgres://admin@db.example.internal:5432/postgres" \
--        -v app_password='<a strong password>' -f sql/bootstrap/00_create_role_and_database.sql
--
-- The application role needs no superuser rights. It owns the database, so it
-- can apply migrations (including CREATE EXTENSION pg_trgm, a trusted extension).

CREATE ROLE shadoucmdb_app LOGIN PASSWORD :'app_password';
CREATE DATABASE shadoucmdb OWNER shadoucmdb_app ENCODING 'UTF8';

\connect shadoucmdb
-- Only needed if your provider does not let the database owner create trusted
-- extensions; harmless otherwise.
CREATE EXTENSION IF NOT EXISTS pg_trgm;
