-- Two-factor sign-in with TOTP authenticator apps and one-time recovery codes (SHAA-83).
--
-- user_totp holds one authenticator per user. A row with confirmed_at NULL is
-- an enrolment in progress (the secret was shown, no code has proven the app
-- has it yet); it does not count as MFA. The secret must be readable to check
-- codes (RFC 6238), so it is stored as is, like the backup file stores it:
-- whoever can read this table can compute codes, but still needs the password.
-- last_used_step is the 30-second time step of the last accepted code, so a
-- code cannot be used twice.
--
-- user_recovery_codes: ten per confirmed authenticator, shown once. Only the
-- SHA-256 is stored (each code carries 80 random bits, so a fast hash is
-- enough, as for sessions and API tokens). A used code keeps its row with
-- used_at set, so "codes left" is countable and a used code stays refused.
--
-- mfa_challenges: a sign-in whose password was right and whose second factor
-- is still due. The browser holds the random token (a short-lived cookie),
-- the table its SHA-256. Ephemeral like sessions: never backed up.
--
-- permission_profiles.require_mfa: holders of such a profile must set up TOTP;
-- until they do, their session can only reach the MFA set-up endpoints.
--
-- Audit: mfa.enrol, mfa.disable, mfa.failure, mfa.recovery_code_used and
-- mfa.recovery_codes (regenerated) are events with entity_type 'users' and the
-- user's id; details in new_value, never a secret, code or hash. They are
-- authentication events and follow the auth retention scope.

CREATE TABLE cmdb.user_totp (
  user_id uuid PRIMARY KEY REFERENCES cmdb.users (id) ON DELETE CASCADE,
  secret bytea NOT NULL,
  confirmed_at timestamp with time zone,
  last_used_step bigint,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT user_totp_secret_length CHECK (octet_length(secret) = 20),
  CONSTRAINT user_totp_step_after_confirmation CHECK (last_used_step IS NULL OR confirmed_at IS NOT NULL)
);
--> statement-breakpoint

CREATE TABLE cmdb.user_recovery_codes (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  user_id uuid NOT NULL REFERENCES cmdb.users (id) ON DELETE CASCADE,
  code_hash bytea NOT NULL,
  used_at timestamp with time zone,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT user_recovery_codes_hash_length CHECK (octet_length(code_hash) = 32),
  CONSTRAINT user_recovery_codes_user_hash_uq UNIQUE (user_id, code_hash)
);
--> statement-breakpoint

CREATE TABLE cmdb.mfa_challenges (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  token_hash bytea NOT NULL,
  user_id uuid NOT NULL REFERENCES cmdb.users (id) ON DELETE CASCADE,
  expires_at timestamp with time zone NOT NULL,
  failed_attempts integer DEFAULT 0 NOT NULL,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT mfa_challenges_token_hash_uq UNIQUE (token_hash),
  CONSTRAINT mfa_challenges_token_hash_length CHECK (octet_length(token_hash) = 32),
  CONSTRAINT mfa_challenges_failed_attempts_valid CHECK (failed_attempts >= 0)
);
--> statement-breakpoint
CREATE INDEX mfa_challenges_user_idx ON cmdb.mfa_challenges (user_id);
--> statement-breakpoint
CREATE INDEX mfa_challenges_expires_idx ON cmdb.mfa_challenges (expires_at);
--> statement-breakpoint

ALTER TABLE cmdb.permission_profiles ADD COLUMN require_mfa boolean DEFAULT false NOT NULL;
--> statement-breakpoint

-- The built-in Administrator profile stays read-only except for require_mfa
-- (and the updated_at that goes with it): the profile that most needs MFA
-- must be able to require it. Same body as 0003 otherwise; the search_path
-- is the one 0008 gave the function.
CREATE OR REPLACE FUNCTION cmdb.permission_profiles_protect_builtin() RETURNS trigger
LANGUAGE plpgsql
SET search_path = cmdb, public
AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    IF OLD.is_builtin THEN
      RAISE EXCEPTION 'permission_profiles: the built-in Administrator profile cannot be deleted'
        USING ERRCODE = 'check_violation', CONSTRAINT = 'permission_profiles_builtin_protected';
    END IF;
    RETURN OLD;
  END IF;
  IF (OLD.is_builtin OR NEW.is_builtin)
     AND to_jsonb(NEW) - 'require_mfa' - 'updated_at' IS DISTINCT FROM to_jsonb(OLD) - 'require_mfa' - 'updated_at' THEN
    RAISE EXCEPTION 'permission_profiles: the built-in Administrator profile cannot be changed'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'permission_profiles_builtin_protected';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- audit_log: the mfa.* events (keeps 0010's token.use)
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.audit_log DROP CONSTRAINT audit_log_action_valid;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log ADD CONSTRAINT audit_log_action_valid CHECK (action IN (
  'create', 'update', 'delete', 'restore',
  'login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke',
  'audit.purge', 'token.use',
  'mfa.enrol', 'mfa.disable', 'mfa.failure', 'mfa.recovery_code_used', 'mfa.recovery_codes'
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
                 'mfa.enrol', 'mfa.disable', 'mfa.failure', 'mfa.recovery_code_used', 'mfa.recovery_codes')
      AND old_value IS NULL AND new_value IS NOT NULL)
);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- prune_audit_log(): the mfa.* events carry the client's IP address and user
-- agent like sign-ins, so they follow the 180-day policy of the auth scope;
-- the auth scope also clears expired MFA challenges. Same body as 0010
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
                           'mfa.enrol', 'mfa.disable', 'mfa.failure', 'mfa.recovery_code_used', 'mfa.recovery_codes']
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
