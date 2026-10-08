-- backup.restore entries that `shadoucmdb restore` has not sent itself
-- (SHAA-2309, GH#716).
--
-- Since 0064 `restore` sends its backup.restore entry to AUDIT_EXPORT, so that
-- whoever holds the API role's credentials cannot hide it by marking it as
-- sent (cmdb.audit_export_mark_restore_sent()) before the server starts. It
-- does not send to stdout or to a file that does not exist yet, and a send
-- can fail; in those cases the entry still depended on audit_export_restores,
-- which the API role writes.
--
-- `restore` now lists its entry here in the restore's own transaction and
-- removes it once its own send succeeded. The server's export sends every
-- entry listed here each time it starts, whatever audit_export_restores
-- says, until a later restore replaces the list. The API role may only read
-- the table: the database cannot tell the export from someone else with its
-- credentials, so it must not be able to remove an entry before it is sent.
--
-- The table is in backups like any other; `restore` empties it before it
-- lists its own entry, as rows loaded from the backup belong to the
-- database the backup was taken from.
CREATE TABLE cmdb.audit_export_restore_pending (
  chain_seq bigint PRIMARY KEY,
  recorded_at timestamptz NOT NULL DEFAULT now()
);
--> statement-breakpoint
COMMENT ON TABLE cmdb.audit_export_restore_pending IS
  'backup.restore entries that shadoucmdb restore did not send itself; the audit export sends them at every start. Written by restore only.';
--> statement-breakpoint
INSERT INTO cmdb.api_role_privileges (object, object_type, privileges) VALUES
  ('cmdb.audit_export_restore_pending', 'table', '{SELECT}');
--> statement-breakpoint
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
BEGIN
  -- Not on a single-role install, where the API role owns cmdb.
  IF to_regrole(app_role) IS NOT NULL AND current_user <> app_role
     AND (SELECT nspowner FROM pg_namespace WHERE nspname = 'cmdb') <> to_regrole(app_role) THEN
    PERFORM cmdb.apply_api_role_grants(app_role);
  END IF;
END $$;
