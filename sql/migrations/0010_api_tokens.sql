-- API tokens for scripts and services (SHAA-81).
--
-- A token belongs to a user (its owner, the actor in the audit log) and is
-- scoped to one permission profile. Its effective permissions are the
-- intersection of the owner's current permissions and the profile's, so a
-- token never grants more than its owner holds, and taking a right away from
-- the owner takes it away from their tokens too. It stops working when it
-- expires, is revoked, its owner is disabled, or its profile is deleted.
--
-- Only the SHA-256 of the secret is stored (the secret carries 256 random
-- bits, so a fast hash is enough, as for sessions); the secret is shown once,
-- in the response that creates the token. token_prefix is the first
-- characters of the secret, kept so an operator can match a token found in a
-- script or a log against its row.
--
-- Soft delete: revocation sets revoked_at and keeps the row, so the token's
-- history stays readable next to its audit rows. Deleting the owner deletes
-- their tokens (the API audits each one first).
--
-- Audit: create and revoke are 'create' and 'update' rows with entity_type
-- 'api_tokens'; every request made with a token, accepted or not, is a
-- 'token.use' row (details in new_value; never the secret or its hash).

CREATE TABLE cmdb.api_tokens (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  name text NOT NULL,
  user_id uuid NOT NULL REFERENCES cmdb.users (id) ON DELETE CASCADE,
  -- NULL once the profile is deleted: the token then has no scope and is refused.
  profile_id uuid REFERENCES cmdb.permission_profiles (id) ON DELETE SET NULL,
  token_hash bytea NOT NULL,
  token_prefix text NOT NULL,
  expires_at timestamp with time zone NOT NULL,
  revoked_at timestamp with time zone,
  -- Who revoked it (audit actor name), for the list view.
  revoked_by text,
  last_used_at timestamp with time zone,
  -- Evidence only, like sessions.ip_address: never an input to an access decision.
  last_used_ip inet,
  created_by text,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT api_tokens_name_not_blank CHECK (length(btrim(name)) > 0 AND length(name) <= 200),
  CONSTRAINT api_tokens_token_hash_uq UNIQUE (token_hash),
  CONSTRAINT api_tokens_token_hash_length CHECK (octet_length(token_hash) = 32),
  CONSTRAINT api_tokens_expiry_after_creation CHECK (expires_at > created_at),
  CONSTRAINT api_tokens_revoked_consistent CHECK ((revoked_at IS NULL) = (revoked_by IS NULL))
);
--> statement-breakpoint
CREATE INDEX api_tokens_user_idx ON cmdb.api_tokens (user_id);
--> statement-breakpoint
CREATE INDEX api_tokens_profile_idx ON cmdb.api_tokens (profile_id) WHERE profile_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX api_tokens_created_idx ON cmdb.api_tokens (created_at DESC, id);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- audit_log: the token.use action (keeps 0007's audit.purge)
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.audit_log DROP CONSTRAINT audit_log_action_valid;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log ADD CONSTRAINT audit_log_action_valid CHECK (action IN (
  'create', 'update', 'delete', 'restore',
  'login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke',
  'audit.purge', 'token.use'
));
--> statement-breakpoint
ALTER TABLE cmdb.audit_log DROP CONSTRAINT audit_log_values_present;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log ADD CONSTRAINT audit_log_values_present CHECK (
  (action = 'create' AND old_value IS NULL AND new_value IS NOT NULL)
  OR (action = 'update' AND old_value IS NOT NULL AND new_value IS NOT NULL)
  OR (action IN ('delete', 'restore') AND old_value IS NOT NULL)
  -- Events, not changes: the details are in new_value.
  OR (action IN ('login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke', 'audit.purge', 'token.use')
      AND old_value IS NULL AND new_value IS NOT NULL)
);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- prune_audit_log(): token.use rows are access events (they carry the
-- caller's IP address and user agent), so they follow the 180-day policy of
-- the auth scope. Same body as 0008 otherwise; CREATE OR REPLACE keeps the
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
    WHEN 'auth' THEN ARRAY['login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke', 'token.use']
    WHEN 'changes' THEN ARRAY['create', 'update', 'delete', 'restore']
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
