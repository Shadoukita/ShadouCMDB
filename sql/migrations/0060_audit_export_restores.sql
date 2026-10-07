-- backup.restore entries the audit export has sent (SHAA-2179, GH#677).
--
-- The export does not persist its position: after a restart it resumes at the
-- newest audit row. For the backup.restore entry (GH#513) it started before
-- any such entries at the end of the chain, so an entry followed by another
-- row before the server started (`mfa reset-undecryptable`, which restore
-- itself recommends, or `create-admin`) was never sent, and the SIEM copy did
-- not learn of the rollback. The export now starts before the oldest
-- backup.restore entry not listed here, and lists each one once it is sent.
--
-- Existing entries count as sent when a user or API client wrote a row after
-- them: a server served requests since, and the export (if any) was past
-- them. The others are sent, with every row after them, at the next start.
--
-- The table is in backups, so a restored database keeps what was sent from
-- it. The API role may read and add rows, never change or remove them. The
-- partial index finds the entries at start-up without reading the whole log.
CREATE INDEX audit_log_backup_restore_idx ON cmdb.audit_log (chain_seq) WHERE action = 'backup.restore';
--> statement-breakpoint
CREATE TABLE cmdb.audit_export_restores (
  chain_seq bigint PRIMARY KEY,
  sent_at timestamptz NOT NULL DEFAULT now()
);
--> statement-breakpoint
INSERT INTO cmdb.audit_export_restores (chain_seq)
SELECT r.chain_seq FROM cmdb.audit_log r
WHERE r.action = 'backup.restore'
  AND EXISTS (
    SELECT 1 FROM cmdb.audit_log a WHERE a.chain_seq > r.chain_seq AND a.actor_type IN ('user', 'api_client')
  );
--> statement-breakpoint
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = app_role) AND current_user <> app_role THEN
    EXECUTE format('REVOKE ALL ON cmdb.audit_export_restores FROM %I', app_role);
    EXECUTE format('GRANT SELECT, INSERT ON cmdb.audit_export_restores TO %I', app_role);
  END IF;
END;
$$;
