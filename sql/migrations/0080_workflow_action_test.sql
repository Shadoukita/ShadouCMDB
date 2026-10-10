-- The designer's test send of a workflow action (SHAA-3042, design SHAA-2725
-- §11.1): `POST /admin/workflow-definitions/{id}/actions/{key}/test`.
--
--   * audit_log: the action `workflow.action_test` (an event: details in
--     new_value), in the `changes` scope of prune_audit_log(). The checks are
--     re-added NOT VALID; 0081 validates them in its own transaction (see
--     sql/README.md).
--   * notifications: an inbox test is about the workflow, not an instance:
--     entity type `workflow_definitions`. Small table: re-added and validated
--     in place, as in 0073.

-- ---------------------------------------------------------------------------
-- audit_log: keeps every action up to 0073.
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
  'backup.restore', 'workflow.approval_refresh',
  'webhook_endpoint.rotate_secret', 'webhook_endpoint.suspend', 'webhook_endpoint.resume',
  'workflow.action_dead', 'workflow.action_retry', 'workflow.action_discard', 'workflow.action_suppressed',
  'mail.test', 'workflow.action_test'
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
                 'workflow.approval_overdue', 'backup.restore',
                 'webhook_endpoint.rotate_secret', 'webhook_endpoint.suspend', 'webhook_endpoint.resume',
                 'workflow.action_dead', 'workflow.action_retry', 'workflow.action_discard',
                 'workflow.action_suppressed', 'mail.test', 'workflow.action_test')
      AND old_value IS NULL AND new_value IS NOT NULL)
  -- Workflow steps that change a CI's state, and an approval request's
  -- approvers re-resolved: before and after.
  OR (action IN ('workflow.transition', 'workflow.migrate', 'workflow.force', 'workflow.approval_refresh')
      AND old_value IS NOT NULL AND new_value IS NOT NULL)
) NOT VALID;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- prune_audit_log(): the new action joins the `changes` scope. Same body as
-- 0073 otherwise; CREATE OR REPLACE keeps the owner and the EXECUTE grants.
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
  ranges jsonb := '[]';
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
                              'workflow.approval_overdue', 'workflow.approval_refresh',
                              'webhook_endpoint.rotate_secret', 'webhook_endpoint.suspend', 'webhook_endpoint.resume',
                              'workflow.action_dead', 'workflow.action_retry', 'workflow.action_discard',
                              'workflow.action_suppressed', 'mail.test', 'workflow.action_test']
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
    -- Consecutive deleted chainSeq values become one [first, last] range.
    WITH gone AS (
      DELETE FROM cmdb.audit_log l WHERE l.action = ANY (actions) AND l.occurred_at < cutoff
      RETURNING l.action, l.chain_seq
    ),
    island AS (
      SELECT min(i.chain_seq) AS first_seq, max(i.chain_seq) AS last_seq
      FROM (SELECT gone.chain_seq, gone.chain_seq - row_number() OVER (ORDER BY gone.chain_seq) AS grp FROM gone) i
      GROUP BY i.grp
    )
    SELECT (SELECT coalesce(jsonb_object_agg(g.action, g.n), '{}')
            FROM (SELECT gone.action, count(*) AS n FROM gone GROUP BY gone.action) g),
           (SELECT coalesce(jsonb_agg(jsonb_build_array(island.first_seq, island.last_seq) ORDER BY island.first_seq), '[]')
            FROM island)
    INTO counts, ranges;
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
      'deletedRanges', ranges,
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
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- notifications
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.notifications DROP CONSTRAINT notifications_entity_type_check;
--> statement-breakpoint
ALTER TABLE cmdb.notifications ADD CONSTRAINT notifications_entity_type_check CHECK (entity_type IN (
  'workflow_approval_requests', 'workflow_instances', 'import_jobs', 'webhook_endpoints', 'workflow_definitions'));
