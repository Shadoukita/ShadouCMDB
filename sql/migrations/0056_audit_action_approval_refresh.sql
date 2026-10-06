-- The audit action `workflow.approval_refresh` (SHAA-1880, GH#663): an
-- administrator re-resolved who may decide the active step of a pending
-- approval request (POST /workflow-approval-requests/{id}/refresh). It is
-- audited on the CI with the approvers before (old_value) and after
-- (new_value), so an auditor sees when the set of approvers changed and who
-- changed it.

-- ---------------------------------------------------------------------------
-- audit_log: the new action (keeps every action up to 0053). Re-added NOT
-- VALID; 0057 validates them in its own transaction (see sql/README.md).
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.audit_log DROP CONSTRAINT audit_log_action_valid;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log ADD CONSTRAINT audit_log_action_valid CHECK (action IN (
  'create', 'update', 'delete', 'restore',
  'login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke',
  'audit.purge', 'token.use',
  'mfa.enrol', 'mfa.disable', 'mfa.failure', 'mfa.recovery_code_used', 'mfa.recovery_codes',
  'schema_change.refused',
  'import.commit', 'import.report_read',
  'export',
  'session.reauthenticate', 'session.reauthentication_required',
  'workflow.publish', 'workflow.start', 'workflow.cancel',
  'workflow.transition', 'workflow.migrate', 'workflow.force',
  'workflow.approval_request', 'workflow.approval_decide', 'workflow.approval_close', 'workflow.approval_overdue',
  'backup.restore', 'workflow.approval_refresh'
)) NOT VALID;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log DROP CONSTRAINT audit_log_values_present;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log ADD CONSTRAINT audit_log_values_present CHECK (
  (action = 'create' AND old_value IS NULL AND new_value IS NOT NULL)
  OR (action = 'update' AND old_value IS NOT NULL AND new_value IS NOT NULL)
  OR (action IN ('delete', 'restore') AND old_value IS NOT NULL)
  -- Events, not changes: the details are in new_value.
  OR (action IN ('login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke', 'audit.purge', 'token.use',
                 'mfa.enrol', 'mfa.disable', 'mfa.failure', 'mfa.recovery_code_used', 'mfa.recovery_codes',
                 'schema_change.refused', 'import.commit', 'import.report_read', 'export',
                 'session.reauthenticate', 'session.reauthentication_required',
                 'workflow.publish', 'workflow.start', 'workflow.cancel',
                 'workflow.approval_request', 'workflow.approval_decide', 'workflow.approval_close',
                 'workflow.approval_overdue', 'backup.restore')
      AND old_value IS NULL AND new_value IS NOT NULL)
  -- Workflow steps that change a CI's state, and an approval request's
  -- approvers re-resolved: before and after.
  OR (action IN ('workflow.transition', 'workflow.migrate', 'workflow.force', 'workflow.approval_refresh')
      AND old_value IS NOT NULL AND new_value IS NOT NULL)
) NOT VALID;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- prune_audit_log(): the new action joins the `changes` scope with the other
-- approval actions. Same body as 0051 otherwise; CREATE OR REPLACE keeps the
-- owner and the EXECUTE grants.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION cmdb.prune_audit_log(p_older_than interval, p_scope text, p_dry_run boolean, p_operator text DEFAULT NULL)
RETURNS TABLE (category text, total bigint)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
  cutoff timestamptz := now() - p_older_than;
  session_cutoff timestamptz := now() - interval '30 days';
  actions text[];
  counts jsonb;
  sessions_count bigint := 0;
BEGIN
  IF p_older_than IS NULL OR cutoff > now() - interval '30 days' THEN
    RAISE EXCEPTION 'prune_audit_log: the window must be at least 30 days (got %)', p_older_than
      USING ERRCODE = 'invalid_parameter_value';
  END IF;
  actions := CASE p_scope
    WHEN 'auth' THEN ARRAY['login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke', 'token.use',
                           'mfa.enrol', 'mfa.disable', 'mfa.failure', 'mfa.recovery_code_used', 'mfa.recovery_codes',
                           'import.report_read', 'session.reauthenticate', 'session.reauthentication_required']
    WHEN 'changes' THEN ARRAY['create', 'update', 'delete', 'restore', 'schema_change.refused', 'import.commit', 'export',
                              'workflow.publish', 'workflow.start', 'workflow.cancel',
                              'workflow.transition', 'workflow.migrate', 'workflow.force',
                              'workflow.approval_request', 'workflow.approval_decide', 'workflow.approval_close',
                              'workflow.approval_overdue', 'workflow.approval_refresh']
  END;
  IF actions IS NULL OR p_dry_run IS NULL THEN
    RAISE EXCEPTION 'prune_audit_log: scope must be auth or changes, and dry_run true or false'
      USING ERRCODE = 'invalid_parameter_value';
  END IF;

  IF p_dry_run THEN
    SELECT coalesce(jsonb_object_agg(a.action, a.n), '{}') INTO counts
    FROM (SELECT l.action, count(*) AS n FROM cmdb.audit_log l
          WHERE l.action = ANY (actions) AND l.occurred_at < cutoff GROUP BY l.action) a;
    IF p_scope = 'auth' THEN
      SELECT count(*) INTO sessions_count FROM cmdb.sessions s WHERE s.expires_at < session_cutoff;
    END IF;
  ELSE
    PERFORM set_config('shadoucmdb.audit_purge', 'on', true);
    WITH gone AS (
      DELETE FROM cmdb.audit_log l WHERE l.action = ANY (actions) AND l.occurred_at < cutoff RETURNING l.action
    )
    SELECT coalesce(jsonb_object_agg(g.action, g.n), '{}') INTO counts
    FROM (SELECT gone.action, count(*) AS n FROM gone GROUP BY gone.action) g;
    PERFORM set_config('shadoucmdb.audit_purge', '', true);
    IF p_scope = 'auth' THEN
      DELETE FROM cmdb.sessions s WHERE s.expires_at < session_cutoff;
      GET DIAGNOSTICS sessions_count = ROW_COUNT;
      DELETE FROM cmdb.mfa_challenges c WHERE c.expires_at < now();
    END IF;

    INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
    VALUES ('system', session_user, 'audit.purge', 'audit_log', gen_random_uuid(), jsonb_build_object(
      'scope', p_scope,
      'olderThan', p_older_than::text,
      'cutoff', cutoff,
      'deleted', counts,
      'sessionsDeleted', sessions_count,
      'sessionsCutoff', CASE WHEN p_scope = 'auth' THEN session_cutoff END,
      'databaseUser', session_user,
      'clientAddress', host(inet_client_addr()),
      'operator', left(p_operator, 128)
    ));
  END IF;

  RETURN QUERY SELECT c.key, c.value::bigint FROM jsonb_each_text(counts) c ORDER BY c.key;
  IF p_scope = 'auth' THEN
    RETURN QUERY SELECT 'sessions'::text, sessions_count;
  END IF;
END;
$$;
