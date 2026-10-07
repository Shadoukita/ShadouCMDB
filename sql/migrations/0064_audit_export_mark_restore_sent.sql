-- The API role marks a backup.restore entry as sent only through
-- audit_export_mark_restore_sent() (SHAA-2253, GH#706).
--
-- 0063 refused rows for anything but an existing backup.restore entry, but the
-- API role kept INSERT on audit_export_restores, so with its credentials one
-- could list the real entry between `shadoucmdb restore` and the first start
-- of the server; the audit export then started past it.
--
-- The database cannot tell the export from someone else holding the same
-- credentials, so this narrows what the role can do and `shadoucmdb restore`
-- closes the gap: it sends its backup.restore entry to the configured
-- AUDIT_EXPORT sink itself as soon as it is committed.
--
--   - The API role loses INSERT on audit_export_restores and keeps SELECT.
--   - cmdb.audit_export_mark_restore_sent(chain_seq) lists one entry and
--     returns whether it was not listed yet. It runs as the owner, and the
--     0063 trigger still refuses anything but an existing backup.restore
--     entry. Only the API role may execute it; the maintenance role does not
--     export.
--
-- Rows listed already stay: they were checked by 0063.
CREATE FUNCTION cmdb.audit_export_mark_restore_sent(p_chain_seq bigint)
RETURNS boolean
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
  INSERT INTO cmdb.audit_export_restores (chain_seq) VALUES (p_chain_seq) ON CONFLICT DO NOTHING;
  RETURN FOUND;
END;
$$;
--> statement-breakpoint
REVOKE ALL ON FUNCTION cmdb.audit_export_mark_restore_sent(bigint) FROM PUBLIC;
--> statement-breakpoint
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = app_role) AND current_user <> app_role THEN
    EXECUTE format('REVOKE ALL ON cmdb.audit_export_restores FROM %I', app_role);
    EXECUTE format('GRANT SELECT ON cmdb.audit_export_restores TO %I', app_role);
    EXECUTE format('GRANT EXECUTE ON FUNCTION cmdb.audit_export_mark_restore_sent(bigint) TO %I', app_role);
  END IF;
END;
$$;
