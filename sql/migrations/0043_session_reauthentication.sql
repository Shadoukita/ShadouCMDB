-- Recent re-authentication for the user-management writes (GH#498, SHAA-1433).
--
-- GH#413 refused the administration password and MFA resets on the caller's
-- own account, but a session holding users.manage could still create a second
-- user manager, sign in as it and reset the first: a stolen session became a
-- permanent takeover without anyone typing a password. The writes that hand
-- out or take over an account's rights (users, profiles, API tokens, identity
-- providers, the configuration import) now need a session whose owner
-- confirmed their credentials in the last 10 minutes: at sign-in, or again
-- through POST /api/v1/auth/reauthenticate.
--
-- sessions.credentials_confirmed_at is that moment. Sessions open at the
-- upgrade take their sign-in time (created_at), so none of them counts as
-- freshly confirmed because of the upgrade.
--
-- Two new audit events, both in the `auth` retention scope:
--   session.reauthenticate               the session's owner confirmed their credentials
--   session.reauthentication_required    a write was refused because they had not recently

ALTER TABLE cmdb.sessions ADD COLUMN credentials_confirmed_at timestamptz;
--> statement-breakpoint
UPDATE cmdb.sessions SET credentials_confirmed_at = created_at;
--> statement-breakpoint
ALTER TABLE cmdb.sessions ALTER COLUMN credentials_confirmed_at SET DEFAULT now();
--> statement-breakpoint
ALTER TABLE cmdb.sessions ALTER COLUMN credentials_confirmed_at SET NOT NULL;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- audit_log: the two new events (keeps every action up to 0032)
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
  'session.reauthenticate', 'session.reauthentication_required'
));
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
                 'session.reauthenticate', 'session.reauthentication_required')
      AND old_value IS NULL AND new_value IS NOT NULL)
);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- prune_audit_log(): both events join the `auth` scope. Same body as 0032
-- otherwise; CREATE OR REPLACE keeps the owner and the EXECUTE grants.
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
    WHEN 'changes' THEN ARRAY['create', 'update', 'delete', 'restore', 'schema_change.refused', 'import.commit', 'export']
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
