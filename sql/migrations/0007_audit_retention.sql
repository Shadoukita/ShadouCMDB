-- Retention for audit_log and sessions, and the database role split (SHAA-45).
--
-- audit_log stays append-only. The one supported way to delete from it is
-- prune_audit_log(), a SECURITY DEFINER function owned by the schema owner.
-- It deletes by age only, refuses a window under 30 days, never deletes its own
-- audit.purge records, and writes one audit.purge row in the same transaction.
-- EXECUTE is revoked from PUBLIC and granted to shadoucmdb_maintenance only.
--
-- Roles (sql/bootstrap/): shadoucmdb_owner owns the schema and runs migrations,
-- shadoucmdb_app is the API (DML only, no UPDATE/DELETE on audit_log),
-- shadoucmdb_maintenance may only EXECUTE prune_audit_log(). The grants below
-- apply when those roles exist and the migration does not run as the API role;
-- a single-role install keeps working and is split with
-- sql/bootstrap/10_split_roles.sql.
--
-- Not reversible once an audit.purge row exists (audit_log is append-only).

-- ---------------------------------------------------------------------------
-- audit_log: the audit.purge action
-- ---------------------------------------------------------------------------
ALTER TABLE audit_log DROP CONSTRAINT audit_log_action_valid;
--> statement-breakpoint
ALTER TABLE audit_log ADD CONSTRAINT audit_log_action_valid CHECK (action IN (
  'create', 'update', 'delete', 'restore',
  'login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke',
  'audit.purge'
));
--> statement-breakpoint
ALTER TABLE audit_log DROP CONSTRAINT audit_log_values_present;
--> statement-breakpoint
ALTER TABLE audit_log ADD CONSTRAINT audit_log_values_present CHECK (
  (action = 'create' AND old_value IS NULL AND new_value IS NOT NULL)
  OR (action = 'update' AND old_value IS NOT NULL AND new_value IS NOT NULL)
  OR (action IN ('delete', 'restore') AND old_value IS NOT NULL)
  -- Events, not changes: the details are in new_value.
  OR (action IN ('login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke', 'audit.purge')
      AND old_value IS NULL AND new_value IS NOT NULL)
);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Append-only trigger: one exemption, and TRUNCATE is rejected too
-- ---------------------------------------------------------------------------
-- A DELETE passes only while prune_audit_log() runs: it sets
-- shadoucmdb.audit_purge for its own transaction and runs as the table owner.
-- Any other role that sets the variable is still rejected, and audit.purge
-- rows are never deleted. UPDATE is always rejected. The fixed search_path keeps
-- the lookups below on pg_catalog whoever fires the trigger.
CREATE OR REPLACE FUNCTION audit_log_append_only() RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
  IF TG_OP = 'DELETE' AND current_setting('shadoucmdb.audit_purge', true) = 'on' THEN
    IF OLD.action <> 'audit.purge' AND current_user = (
      SELECT r.rolname FROM pg_class c JOIN pg_roles r ON r.oid = c.relowner WHERE c.oid = TG_RELID
    ) THEN
      RETURN OLD;
    END IF;
  END IF;
  RAISE EXCEPTION 'audit_log is append-only (% rejected)', TG_OP
    USING ERRCODE = 'insufficient_privilege';
END;
$$;
--> statement-breakpoint
-- TRUNCATE skips row triggers, so it needs its own.
CREATE TRIGGER audit_log_no_truncate
  BEFORE TRUNCATE ON audit_log
  FOR EACH STATEMENT EXECUTE FUNCTION audit_log_append_only();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- prune_audit_log(p_older_than, p_scope, p_dry_run, p_operator)
-- ---------------------------------------------------------------------------
-- scope 'auth':    login.*, logout and session.revoke rows older than the
--                  window (personal data: IP address, user agent), plus
--                  sessions whose absolute expiry is more than 30 days past.
-- scope 'changes': CI and configuration change history (create, update,
--                  delete, restore). Kept indefinitely unless an operator
--                  asks for this scope explicitly.
-- Returns one row per deleted (or, on a dry run, deletable) category.
-- p_operator is who the caller says they are; the audit.purge row also keeps
-- the database login (session_user) and client address, which the caller
-- cannot choose.
--
-- It runs as the schema owner, so nothing it calls may resolve in a schema
-- another role can write to: the search_path is pg_catalog then pg_temp (never
-- public, where a planted function or aggregate with a closer type match than
-- the built-in would run with the owner's rights), and tables are qualified.
-- The 30-day floor is checked on the resulting cutoff, so '1 month' or
-- '0 years 30 days' cannot shorten it.
CREATE FUNCTION prune_audit_log(p_older_than interval, p_scope text, p_dry_run boolean, p_operator text DEFAULT NULL)
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
    WHEN 'auth' THEN ARRAY['login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke']
    WHEN 'changes' THEN ARRAY['create', 'update', 'delete', 'restore']
  END;
  IF actions IS NULL OR p_dry_run IS NULL THEN
    RAISE EXCEPTION 'prune_audit_log: scope must be auth or changes, and dry_run true or false'
      USING ERRCODE = 'invalid_parameter_value';
  END IF;

  IF p_dry_run THEN
    SELECT coalesce(jsonb_object_agg(a.action, a.n), '{}') INTO counts
    FROM (SELECT l.action, count(*) AS n FROM public.audit_log l
          WHERE l.action = ANY (actions) AND l.occurred_at < cutoff GROUP BY l.action) a;
    IF p_scope = 'auth' THEN
      SELECT count(*) INTO sessions_count FROM public.sessions s WHERE s.expires_at < session_cutoff;
    END IF;
  ELSE
    PERFORM set_config('shadoucmdb.audit_purge', 'on', true);
    WITH gone AS (
      DELETE FROM public.audit_log l WHERE l.action = ANY (actions) AND l.occurred_at < cutoff RETURNING l.action
    )
    SELECT coalesce(jsonb_object_agg(g.action, g.n), '{}') INTO counts
    FROM (SELECT gone.action, count(*) AS n FROM gone GROUP BY gone.action) g;
    PERFORM set_config('shadoucmdb.audit_purge', '', true);
    IF p_scope = 'auth' THEN
      DELETE FROM public.sessions s WHERE s.expires_at < session_cutoff;
      GET DIAGNOSTICS sessions_count = ROW_COUNT;
    END IF;

    INSERT INTO public.audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
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
--> statement-breakpoint
REVOKE ALL ON FUNCTION prune_audit_log(interval, text, boolean, text) FROM PUBLIC;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- No role but the owner creates objects in public
-- ---------------------------------------------------------------------------
-- PostgreSQL 14 and older grant CREATE on public to every role. The bootstrap
-- scripts revoke it; this covers installs made before them. Only the schema
-- owner can revoke, so a migration role that is not the owner leaves a warning.
DO $$
BEGIN
  IF pg_has_role(current_user, (SELECT nspowner FROM pg_namespace WHERE nspname = 'public'), 'USAGE') THEN
    REVOKE CREATE ON SCHEMA public FROM PUBLIC;
  ELSIF has_schema_privilege('public', 'public', 'CREATE') THEN
    RAISE WARNING 'every role may create objects in schema public; as its owner run: REVOKE CREATE ON SCHEMA public FROM PUBLIC';
  END IF;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Grants for the three-role setup
-- ---------------------------------------------------------------------------
-- The role names are whatever the operator chose: `shadoucmdb migrate` passes
-- the users of DATABASE_URL and MAINTENANCE_DATABASE_URL as the session
-- settings shadoucmdb.app_role and shadoucmdb.maintenance_role. Without them
-- (psql) the names from sql/bootstrap/ apply.
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
  maintenance_role name :=
    COALESCE(NULLIF(current_setting('shadoucmdb.maintenance_role', true), ''), 'shadoucmdb_maintenance');
BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = maintenance_role) AND current_user <> maintenance_role THEN
    EXECUTE format('GRANT EXECUTE ON FUNCTION prune_audit_log(interval, text, boolean, text) TO %I', maintenance_role);
  END IF;
  -- On a single-role install the API role owns everything; the split script handles it.
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = app_role) AND current_user <> app_role THEN
    EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO %I', app_role);
    EXECUTE format('GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO %I', app_role);
    EXECUTE format('REVOKE UPDATE, DELETE, TRUNCATE ON audit_log FROM %I', app_role);
    EXECUTE format('REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON _sqlx_migrations FROM %I', app_role);
    -- Tables created by later migrations (run as this role) get the same DML grants.
    EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO %I', app_role);
    EXECUTE format('ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO %I', app_role);
  END IF;
END;
$$;
