-- The API role may read the audit hash-chain head, never move it (SHAA-1056, GH#396).
--
-- `shadoucmdb backup` runs as the API role and copies every system table in
-- one snapshot. Migration 0018 revoked every privilege on
-- cmdb.audit_log_chain_head from that role, so on a three-role installation
-- the backup failed with "permission denied for table audit_log_chain_head".
--
-- SELECT alone discloses nothing new: the head is (last_seq, last_hash), and
-- the API role already reads every row of audit_log, the last row_hash
-- included. It grants no way to change or lock the head either: UPDATE,
-- DELETE, TRUNCATE and SELECT ... FOR UPDATE/SHARE all need privileges that
-- stay revoked, so only the SECURITY DEFINER audit_log_chain() trigger moves
-- it, as before. The backup keeps the head, so a restored database still shows
-- rows missing from the end of the chain ("tail" in audit_log_verify()).
--
-- Single-role installations (the API role owns the table) are unchanged.
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = app_role) AND current_user <> app_role THEN
    EXECUTE format('REVOKE ALL ON cmdb.audit_log_chain_head FROM %I', app_role);
    EXECUTE format('GRANT SELECT ON cmdb.audit_log_chain_head TO %I', app_role);
  END IF;
END;
$$;
