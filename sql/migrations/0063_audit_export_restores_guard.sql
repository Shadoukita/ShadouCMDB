-- audit_export_restores lists only backup.restore entries that exist
-- (SHAA-2241, GH#696).
--
-- 0061 let the API role insert any chain_seq. With the role's credentials
-- (SQL injection, stolen app credentials: the GH#684 threat model) one could
-- list chain_seq values past the head; the backup.restore entry a later
-- restore wrote at one of them then counted as sent, so the audit export
-- started past it and the SIEM copy never learned of the rollback.
--
-- Now:
--   - A row is accepted only for an existing audit_log row whose action is
--     backup.restore; anything else is refused (foreign_key_violation). This
--     holds for every role, the owner included; restore loads the table with
--     triggers off and then removes what does not match (see below).
--   - sent_at is the time of the insert, whatever the inserter gave, so it
--     shows when the export marked the entry.
--   - Rows listed already that match no backup.restore entry are removed
--     here. That covers rows planted before the upgrade, and those in a
--     backup taken before it, which restore upgrades through this migration.
--     `shadoucmdb restore` also removes such rows itself before it writes its
--     own entry.
--
-- An entry purged by prune-audit loses its row here too; the export never
-- goes back to an entry that is no longer in audit_log.
DELETE FROM cmdb.audit_export_restores s
WHERE NOT EXISTS (
  SELECT FROM cmdb.audit_log a WHERE a.chain_seq = s.chain_seq AND a.action = 'backup.restore'
);
--> statement-breakpoint
CREATE FUNCTION cmdb.audit_export_restores_guard() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
  IF NOT EXISTS (
    SELECT FROM cmdb.audit_log a WHERE a.chain_seq = NEW.chain_seq AND a.action = 'backup.restore'
  ) THEN
    RAISE EXCEPTION 'audit_export_restores: chainSeq % is not a backup.restore entry in audit_log', NEW.chain_seq
      USING ERRCODE = 'foreign_key_violation';
  END IF;
  NEW.sent_at := now();
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER audit_export_restores_guard
  BEFORE INSERT ON cmdb.audit_export_restores
  FOR EACH ROW EXECUTE FUNCTION cmdb.audit_export_restores_guard();
