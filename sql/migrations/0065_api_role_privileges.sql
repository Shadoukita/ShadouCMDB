-- One statement of what the API role may do with the system tables (GH#713).
--
-- Until now each migration that narrowed the API role (0007, 0018, 0021,
-- 0038, 0046, 0050, 0051, 0061, 0064) revoked the rights itself, and
-- sql/bootstrap/10_split_roles.sql, which grants a split single-role install
-- everything again, had to repeat each narrowing. It missed 0046, 0050 and
-- 0051: on a split install the API role could change and delete workflow
-- history and approval decisions and write the workflow archive.
--
--   - cmdb.api_role_privileges lists every system table and routine the API
--     role holds other than the default rights: exactly the rights listed.
--     The default is SELECT, INSERT, UPDATE, DELETE on every table and view
--     in cmdb (as 0008's default privileges give new ones) and USAGE, SELECT
--     on every sequence there; routines keep their own grants.
--   - cmdb.apply_api_role_grants(role) gives the role those rights. This
--     migration and 10_split_roles.sql call it; a migration that narrows the
--     API role adds a row and calls it (see sql/README.md).
--   - On a three-role install this migration calls it, which repairs an
--     install split before this release. No data changes.
--   - 0007's default privileges in schema public go: public holds nothing
--     of the API role's but _sqlx_migrations, which it may only read.
CREATE TABLE cmdb.api_role_privileges (
  object      text PRIMARY KEY,
  object_type text NOT NULL CHECK (object_type IN ('table', 'routine')),
  privileges  text[] NOT NULL CHECK (
    (object_type = 'table' AND privileges <@ ARRAY['SELECT', 'INSERT', 'UPDATE', 'DELETE', 'REFERENCES'])
    OR (object_type = 'routine' AND privileges <@ ARRAY['EXECUTE']))
);
--> statement-breakpoint
COMMENT ON TABLE cmdb.api_role_privileges IS
  'Exact rights of the API role on these objects; see cmdb.apply_api_role_grants(). Written by migrations only.';
--> statement-breakpoint
INSERT INTO cmdb.api_role_privileges (object, object_type, privileges) VALUES
  -- This table: the API role has no business here.
  ('cmdb.api_role_privileges', 'table', '{}'),
  -- Appended to only; the trigger keeps it a hash chain (0007, 0018).
  ('cmdb.audit_log', 'table', '{SELECT,INSERT}'),
  -- Moved by the audit_log trigger only; read for `shadoucmdb backup` (0018, 0038).
  ('cmdb.audit_log_chain_head', 'table', '{SELECT}'),
  -- Listed through audit_export_mark_restore_sent() only (0061, 0064).
  ('cmdb.audit_export_restores', 'table', '{SELECT}'),
  ('cmdb.audit_export_mark_restore_sent(bigint)', 'routine', '{EXECUTE}'),
  -- The record of the DDL engine's changes (0008).
  ('cmdb.schema_changes', 'table', '{SELECT,INSERT}'),
  -- Read and added, never changed (0021).
  ('cmdb.server_keys', 'table', '{SELECT,INSERT}'),
  -- Type tables reference these (0008).
  ('cmdb.configuration_items', 'table', '{SELECT,INSERT,UPDATE,DELETE,REFERENCES}'),
  ('cmdb.lookup_list_values', 'table', '{SELECT,INSERT,UPDATE,DELETE,REFERENCES}'),
  -- Workflow history and approval decisions are appended to only (0046, 0051);
  -- the archive is written by its trigger (0050).
  ('cmdb.workflow_instance_events', 'table', '{SELECT,INSERT}'),
  ('cmdb.workflow_approval_decisions', 'table', '{SELECT,INSERT}'),
  ('cmdb.workflow_instance_archive', 'table', '{SELECT}'),
  -- Read by /readyz; written by `shadoucmdb migrate` as the owner (0007).
  ('public._sqlx_migrations', 'table', '{SELECT}');
--> statement-breakpoint
-- Not SECURITY DEFINER: the caller must own the objects (or be a superuser),
-- as the schema owner running a migration and the administrator running
-- 10_split_roles.sql do. Idempotent.
--
-- 10_split_roles.sql runs as an administrator on a database whose system
-- objects the API role owned until then, so it must not run anything the API
-- role could have written (GH#725). It replaces this function with its own
-- copy before calling it, and the function reads the list only if it is
-- still the plain table created above, with row security off. Keep the copy
-- in the script identical; upgrade_0065 compares them.
CREATE OR REPLACE FUNCTION cmdb.apply_api_role_grants(app_role name)
RETURNS void
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
SET row_security = off
AS $$
DECLARE
  p record;
BEGIN
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = app_role) THEN
    RAISE EXCEPTION 'role % does not exist', app_role USING ERRCODE = 'undefined_object';
  END IF;
  -- Reading a view, a child table or a column of another type would run its code.
  IF NOT EXISTS (SELECT FROM pg_class c
                 WHERE c.oid = to_regclass('cmdb.api_role_privileges') AND c.relkind = 'r' AND NOT c.relhassubclass)
     OR ARRAY(SELECT a.atttypid FROM pg_attribute a
              WHERE a.attrelid = to_regclass('cmdb.api_role_privileges') AND a.attnum > 0 AND NOT a.attisdropped
              ORDER BY a.attnum) IS DISTINCT FROM ARRAY['text'::regtype, 'text'::regtype, 'text[]'::regtype]::oid[] THEN
    RAISE EXCEPTION 'cmdb.api_role_privileges is not the table migration 0065 created'
      USING ERRCODE = 'object_not_in_prerequisite_state';
  END IF;
  EXECUTE format('GRANT USAGE ON SCHEMA cmdb TO %I', app_role);
  EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA cmdb TO %I', app_role);
  EXECUTE format('GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA cmdb TO %I', app_role);
  -- An object a later migration dropped is skipped.
  FOR p IN
    SELECT to_regclass(l.object)::text AS relation, NULL::text AS routine, l.privileges
      FROM ONLY cmdb.api_role_privileges l WHERE l.object_type = 'table' AND to_regclass(l.object) IS NOT NULL
    UNION ALL
    SELECT NULL, to_regprocedure(l.object)::text, l.privileges
      FROM ONLY cmdb.api_role_privileges l WHERE l.object_type = 'routine' AND to_regprocedure(l.object) IS NOT NULL
  LOOP
    IF p.relation IS NOT NULL THEN
      EXECUTE format('REVOKE ALL ON %s FROM %I', p.relation, app_role);
      IF cardinality(p.privileges) > 0 THEN
        EXECUTE format('GRANT %s ON %s TO %I', array_to_string(p.privileges, ', '), p.relation, app_role);
      END IF;
    ELSE
      EXECUTE format('REVOKE ALL ON FUNCTION %s FROM %I', p.routine, app_role);
      IF cardinality(p.privileges) > 0 THEN
        EXECUTE format('GRANT EXECUTE ON FUNCTION %s TO %I', p.routine, app_role);
      END IF;
    END IF;
  END LOOP;
END;
$$;
--> statement-breakpoint
REVOKE ALL ON FUNCTION cmdb.apply_api_role_grants(name) FROM PUBLIC;
--> statement-breakpoint
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
BEGIN
  -- Not on a single-role install, where the API role owns cmdb and its tables
  -- (also when another role runs `migrate` there): the split grants the list.
  IF to_regrole(app_role) IS NOT NULL AND current_user <> app_role
     AND (SELECT nspowner FROM pg_namespace WHERE nspname = 'cmdb') <> to_regrole(app_role) THEN
    PERFORM cmdb.apply_api_role_grants(app_role);
    EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE SELECT, INSERT, UPDATE, DELETE ON TABLES FROM %I', app_role);
    EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA public REVOKE USAGE, SELECT ON SEQUENCES FROM %I', app_role);
  END IF;
END;
$$;
