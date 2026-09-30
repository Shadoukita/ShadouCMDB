-- Bulk import from CSV and Excel files (SHAA-714 spec, SHAA-799).
--
-- Adds the global right `cis.import` (granted to no profile), the instance
-- switch (off), import jobs with their uploaded file (1 MiB chunks), their
-- per-row issues and idempotency keys, and saved column mappings. Two audit
-- events join the log: `import.commit` (one per commit, `changes` retention
-- scope) and `import.report_read` (an administrator read another user's error
-- report, `auth` scope). The file is additive and rewrites no data.
--
-- Classes, attributes and relationship types are referenced by key (in
-- `class_key` and inside `mapping` / `definition`), never by id, so saved
-- mappings survive renames and travel with the config export. A future
-- migration that renames field references must rewrite
-- `import_mappings.definition` too (see sql/README.md).

-- ---------------------------------------------------------------------------
-- Global right (D3): the full list of 0003 plus cis.import.
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.permission_profile_global_permissions
  DROP CONSTRAINT permission_profile_global_permissions_valid;
--> statement-breakpoint
ALTER TABLE cmdb.permission_profile_global_permissions
  ADD CONSTRAINT permission_profile_global_permissions_valid CHECK (permission IN (
    'users.manage', 'profiles.manage', 'datamodel.manage', 'customization.manage',
    'config.export_import', 'audit.view',
    'cis.import'));
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Instance switch (D4): exactly one row, off.
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.import_settings (
  id              boolean PRIMARY KEY DEFAULT true CHECK (id),
  enabled         boolean NOT NULL DEFAULT false,
  updated_at      timestamptz NOT NULL DEFAULT now(),
  updated_by_id   uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  updated_by_name text
);
--> statement-breakpoint
INSERT INTO cmdb.import_settings DEFAULT VALUES;
--> statement-breakpoint
CREATE TRIGGER import_settings_set_updated_at BEFORE UPDATE ON cmdb.import_settings
  FOR EACH ROW EXECUTE FUNCTION cmdb.set_updated_at();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Jobs. A job holds metadata and counts only; the file, its issues and the
-- idempotency keys are separate tables that backups leave out.
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.import_jobs (
  id                    uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  created_by_id         uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  created_by_name       text NOT NULL,
  status                text NOT NULL CHECK (status IN (
                          'uploading', 'queued', 'analysing', 'ready', 'validating', 'validated',
                          'committing', 'completed', 'completed_with_errors', 'failed', 'cancelled', 'expired')),
  -- What `queued` and the progress refer to.
  phase                 text CHECK (phase IN ('analyse', 'validate', 'commit')),
  file_name             text NOT NULL CHECK (length(file_name) BETWEEN 1 AND 255),
  file_format           text NOT NULL CHECK (file_format IN ('csv', 'xlsx')),
  -- 0 only while the upload streams in (status 'uploading').
  file_size             bigint NOT NULL CHECK (file_size > 0 OR status = 'uploading'),
  file_sha256           text CHECK (file_sha256 ~ '^[0-9a-f]{64}$'),
  file_options          jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(file_options) = 'object'),
  file_info             jsonb,
  class_key             text,
  mapping               jsonb,
  -- Informational; no FK, the saved mapping may be deleted.
  mapping_id            uuid,
  summary               jsonb,
  preview               jsonb,
  -- SHA-256 hex over the data model tables at the dry run (T14).
  model_fingerprint     text CHECK (model_fingerprint ~ '^[0-9a-f]{64}$'),
  dry_run_finished_at   timestamptz,
  committed_through_row integer NOT NULL DEFAULT 0 CHECK (committed_through_row >= 0),
  -- Claims of the current phase (CR1).
  attempts              integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  progress_done         integer NOT NULL DEFAULT 0 CHECK (progress_done >= 0),
  progress_total        integer NOT NULL DEFAULT 0 CHECK (progress_total >= 0),
  error                 jsonb,
  queued_at             timestamptz,
  lease_owner           text,
  lease_until           timestamptz,
  -- Fencing token (T13).
  lease_epoch           integer NOT NULL DEFAULT 0,
  created_at            timestamptz NOT NULL DEFAULT now(),
  updated_at            timestamptz NOT NULL DEFAULT now(),
  finished_at           timestamptz,
  -- When the file (and the issues) are removed; the record stays for 90 days after the job ends.
  expires_at            timestamptz NOT NULL,
  CONSTRAINT import_jobs_file_hashed CHECK (file_sha256 IS NOT NULL OR status IN ('uploading', 'expired')),
  CONSTRAINT import_jobs_progress CHECK (progress_done <= progress_total)
);
--> statement-breakpoint
CREATE INDEX import_jobs_owner_idx ON cmdb.import_jobs (created_by_id, created_at DESC);
--> statement-breakpoint
-- Per-user limits (T23).
CREATE INDEX import_jobs_active_idx ON cmdb.import_jobs (created_by_id)
  WHERE status NOT IN ('completed', 'completed_with_errors', 'failed', 'cancelled', 'expired');
--> statement-breakpoint
-- Claiming and takeover (T23): queued jobs by queued_at, running ones by lease_until.
CREATE INDEX import_jobs_work_idx ON cmdb.import_jobs (status, coalesce(lease_until, queued_at))
  WHERE status IN ('queued', 'analysing', 'validating', 'committing');
--> statement-breakpoint
CREATE INDEX import_jobs_expiry_idx ON cmdb.import_jobs (expires_at);
--> statement-breakpoint
-- Stale-upload cleanup (CR7).
CREATE INDEX import_jobs_uploading_idx ON cmdb.import_jobs (updated_at) WHERE status = 'uploading';
--> statement-breakpoint
CREATE TRIGGER import_jobs_set_updated_at BEFORE UPDATE ON cmdb.import_jobs
  FOR EACH ROW EXECUTE FUNCTION cmdb.set_updated_at();
--> statement-breakpoint

-- The uploaded file in chunks of at most 1 MiB (D6). XLSX is already deflated,
-- so TOAST compression would only cost CPU (T6).
CREATE TABLE cmdb.import_job_files (
  job_id uuid NOT NULL REFERENCES cmdb.import_jobs (id) ON DELETE CASCADE,
  seq    integer NOT NULL CHECK (seq >= 0),
  data   bytea NOT NULL CHECK (length(data) BETWEEN 1 AND 1048576),
  PRIMARY KEY (job_id, seq)
);
--> statement-breakpoint
ALTER TABLE cmdb.import_job_files ALTER COLUMN data SET STORAGE EXTERNAL;
--> statement-breakpoint

-- Row problems found by the dry run or at commit. `value` is the shortened
-- cell (T17); the rows are deleted together with the file.
CREATE TABLE cmdb.import_job_issues (
  job_id    uuid NOT NULL REFERENCES cmdb.import_jobs (id) ON DELETE CASCADE,
  seq       integer NOT NULL CHECK (seq >= 0),
  row_no    integer NOT NULL CHECK (row_no >= 1),
  col_index integer CHECK (col_index >= 0),
  field     text,
  value     text CHECK (length(value) <= 200),
  severity  text NOT NULL CHECK (severity IN ('error', 'warning')),
  code      text NOT NULL,
  message   text NOT NULL,
  phase     text NOT NULL CHECK (phase IN ('validate', 'commit')),
  PRIMARY KEY (job_id, seq)
);
--> statement-breakpoint
CREATE INDEX import_job_issues_row_idx ON cmdb.import_job_issues (job_id, row_no);
--> statement-breakpoint

-- Idempotency-Key of createImport and commitImport, kept 24 h (T15).
CREATE TABLE cmdb.import_idempotency_keys (
  user_id    uuid NOT NULL REFERENCES cmdb.users (id) ON DELETE CASCADE,
  key        text NOT NULL CHECK (key ~ '^[\x21-\x7e]{1,128}$'),
  operation  text NOT NULL CHECK (operation IN ('create', 'commit')),
  job_id     uuid NOT NULL REFERENCES cmdb.import_jobs (id) ON DELETE CASCADE,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, operation, key)
);
--> statement-breakpoint
CREATE INDEX import_idempotency_keys_created_idx ON cmdb.import_idempotency_keys (created_at);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Saved mappings (D9): shared with everyone who has cis.import and may view
-- the class. Names are unique per class, so a clash says nothing about a
-- class the caller cannot view (W2, T18).
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.import_mappings (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  name            text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
  description     text CHECK (description IS NULL OR length(description) <= 500),
  class_key       text NOT NULL,
  definition      jsonb NOT NULL CHECK (jsonb_typeof(definition) = 'object' AND pg_column_size(definition) <= 65536),
  version         integer NOT NULL DEFAULT 1 CHECK (version >= 1),
  created_at      timestamptz NOT NULL DEFAULT now(),
  created_by_id   uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  created_by_name text NOT NULL,
  updated_at      timestamptz NOT NULL DEFAULT now(),
  updated_by_id   uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  updated_by_name text NOT NULL
);
--> statement-breakpoint
CREATE UNIQUE INDEX import_mappings_name_uq ON cmdb.import_mappings (class_key, lower(name));
--> statement-breakpoint
CREATE TRIGGER import_mappings_set_updated_at BEFORE UPDATE ON cmdb.import_mappings
  FOR EACH ROW EXECUTE FUNCTION cmdb.set_updated_at();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- audit_log: the two new events (keeps every action of 0027). The constraints
-- are dropped and added back in this transaction, as in 0027 (T7); the new
-- lists are supersets, so the re-check of the existing rows cannot fail.
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.audit_log DROP CONSTRAINT audit_log_action_valid;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log ADD CONSTRAINT audit_log_action_valid CHECK (action IN (
  'create', 'update', 'delete', 'restore',
  'login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke',
  'audit.purge', 'token.use',
  'mfa.enrol', 'mfa.disable', 'mfa.failure', 'mfa.recovery_code_used', 'mfa.recovery_codes',
  'schema_change.refused',
  'import.commit', 'import.report_read'
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
                 'schema_change.refused', 'import.commit', 'import.report_read')
      AND old_value IS NULL AND new_value IS NOT NULL)
);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- prune_audit_log(): import.commit joins the `changes` scope (it summarises
-- changes), import.report_read the `auth` scope (an access event, like
-- token.use; T20). Same body as 0027 otherwise; CREATE OR REPLACE keeps the
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
                           'import.report_read']
    WHEN 'changes' THEN ARRAY['create', 'update', 'delete', 'restore', 'schema_change.refused', 'import.commit']
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
