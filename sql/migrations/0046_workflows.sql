-- Workflow engine: schema and rights (v0.4.0, SHAA-1422; design on SHAA-1411 §3, §5.1, §8.1).
--
-- Additive only: no existing row is rewritten and no CI data moves. Until an
-- administrator publishes a workflow, nothing changes for an install.
--
--   * ci_classes.kind: 'asset' (every existing class) or 'process'. A process
--     class holds records such as change requests in an ordinary type table;
--     the API keeps its records out of the asset inventory, global search, the
--     relationship graph, impact analysis, business service membership and the
--     dashboard counters (Q1, option A).
--   * The global right workflows.manage, granted to every profile holding
--     datamodel.manage (the views.share pattern of 0039).
--   * Workflow definitions, immutable published versions (states, transitions,
--     transition fields, attribute references), transition grants, instances
--     and their append-only events. The database enforces what the engine
--     relies on: a published or retired version's graph never changes, an
--     instance's state and a transition's endpoints belong to their own
--     version (composite foreign keys), one active instance per definition and
--     CI, and events are never updated or deleted (Q2: kept for the life of
--     the CI; audit retention does not touch them).
--   * The workflow actions of the audit log. The CHECK constraints are re-added
--     NOT VALID here and validated by 0047 in a transaction of its own, so the
--     audit log is never scanned under an ACCESS EXCLUSIVE lock (see
--     sql/README.md).

-- ---------------------------------------------------------------------------
-- Class kind. Adding a column with a constant default rewrites nothing.
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.ci_classes
  ADD COLUMN kind text NOT NULL DEFAULT 'asset',
  ADD CONSTRAINT ci_classes_kind_valid CHECK (kind IN ('asset', 'process')),
  -- The built-in business service type is an asset type: services are members of the inventory.
  ADD CONSTRAINT ci_classes_system_role_asset CHECK (system_role IS NULL OR kind = 'asset');
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Global right: the full list of 0039 plus workflows.manage.
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.permission_profile_global_permissions
  DROP CONSTRAINT permission_profile_global_permissions_valid;
--> statement-breakpoint
ALTER TABLE cmdb.permission_profile_global_permissions
  ADD CONSTRAINT permission_profile_global_permissions_valid CHECK (permission IN (
    'users.manage', 'profiles.manage', 'datamodel.manage', 'customization.manage',
    'config.export_import', 'audit.view',
    'cis.import',
    'views.share',
    'workflows.manage'));
--> statement-breakpoint
INSERT INTO cmdb.permission_profile_global_permissions (profile_id, permission)
  SELECT profile_id, 'workflows.manage' FROM cmdb.permission_profile_global_permissions
  WHERE permission = 'datamodel.manage'
  ON CONFLICT DO NOTHING;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Definitions: the identity of a workflow and its mutable settings.
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.workflow_definitions (
  id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  -- The export identity; never changes (trigger below).
  key                text NOT NULL CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
  name               text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
  description        text CHECK (description IS NULL OR length(description) <= 2000),
  -- The class the workflow runs on; never changes (trigger below).
  class_id           uuid NOT NULL REFERENCES cmdb.ci_classes (id) ON DELETE RESTRICT,
  include_subclasses boolean NOT NULL DEFAULT true,
  -- Optional lookup attribute of the class that the engine keeps in step with the state.
  state_attribute_id uuid REFERENCES cmdb.ci_attribute_definitions (id) ON DELETE RESTRICT,
  -- Start an instance when a CI of the class is created (UI, API and import).
  auto_start         boolean NOT NULL DEFAULT false,
  -- Inactive: no new instances; running ones continue.
  is_active          boolean NOT NULL DEFAULT true,
  -- The newest published version; NULL until the first publish. Foreign key below (cycle).
  current_version_id uuid,
  -- Row version for optimistic concurrency (If-Match / expectedVersion).
  version            integer NOT NULL DEFAULT 1 CHECK (version >= 1),
  created_at         timestamptz NOT NULL DEFAULT now(),
  created_by_id      uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  created_by_name    text NOT NULL,
  updated_at         timestamptz NOT NULL DEFAULT now(),
  updated_by_id      uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  updated_by_name    text NOT NULL
);
--> statement-breakpoint
CREATE UNIQUE INDEX workflow_definitions_key_uq ON cmdb.workflow_definitions (lower(key));
--> statement-breakpoint
-- At most one active definition drives a given state attribute.
CREATE UNIQUE INDEX workflow_definitions_state_attr_uq
  ON cmdb.workflow_definitions (state_attribute_id) WHERE state_attribute_id IS NOT NULL AND is_active;
--> statement-breakpoint
CREATE INDEX workflow_definitions_class_idx ON cmdb.workflow_definitions (class_id);
--> statement-breakpoint
CREATE TRIGGER workflow_definitions_set_updated_at BEFORE UPDATE ON cmdb.workflow_definitions
  FOR EACH ROW EXECUTE FUNCTION cmdb.set_updated_at();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Versions: a definition has at most one draft, any number of published
-- versions (the newest is current) and retired ones.
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.workflow_versions (
  id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  definition_id     uuid NOT NULL REFERENCES cmdb.workflow_definitions (id) ON DELETE CASCADE,
  version_no        integer NOT NULL CHECK (version_no >= 1),
  status            text NOT NULL CHECK (status IN ('draft', 'published', 'retired')),
  -- Composite foreign key below: the initial state is one of this version's states.
  initial_state_id  uuid,
  -- Designer node positions only.
  layout            jsonb CHECK (layout IS NULL OR pg_column_size(layout) <= 65536),
  change_note       text CHECK (change_note IS NULL OR length(change_note) <= 2000),
  -- sha256 of the canonical graph, set at publish.
  checksum          bytea CHECK (checksum IS NULL OR octet_length(checksum) = 32),
  published_at      timestamptz,
  published_by_id   uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  published_by_name text,
  created_at        timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT workflow_versions_no_uq UNIQUE (definition_id, version_no),
  -- Target of the composite foreign keys of definitions and instances.
  CONSTRAINT workflow_versions_definition_uq UNIQUE (id, definition_id),
  CONSTRAINT workflow_versions_published CHECK ((status = 'draft') = (published_at IS NULL)),
  CONSTRAINT workflow_versions_published_complete CHECK (
    status = 'draft' OR (initial_state_id IS NOT NULL AND checksum IS NOT NULL AND published_by_name IS NOT NULL))
);
--> statement-breakpoint
CREATE UNIQUE INDEX workflow_versions_one_draft ON cmdb.workflow_versions (definition_id) WHERE status = 'draft';
--> statement-breakpoint
ALTER TABLE cmdb.workflow_definitions
  ADD CONSTRAINT workflow_definitions_current_version_fk FOREIGN KEY (current_version_id, id)
  REFERENCES cmdb.workflow_versions (id, definition_id);
--> statement-breakpoint

CREATE TABLE cmdb.workflow_states (
  id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  version_id     uuid NOT NULL REFERENCES cmdb.workflow_versions (id) ON DELETE CASCADE,
  key            text NOT NULL CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
  name           text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
  category       text NOT NULL CHECK (category IN ('open', 'active', 'done', 'cancelled')),
  -- Reaching it completes the instance.
  is_terminal    boolean NOT NULL DEFAULT false,
  -- The value of the definition's state attribute while an instance is in this state.
  state_value_id uuid REFERENCES cmdb.lookup_list_values (id) ON DELETE RESTRICT,
  sort_order     integer NOT NULL DEFAULT 0,
  CONSTRAINT workflow_states_key_uq UNIQUE (version_id, key),
  CONSTRAINT workflow_states_version_uq UNIQUE (version_id, id)
);
--> statement-breakpoint
CREATE INDEX workflow_states_value_idx ON cmdb.workflow_states (state_value_id) WHERE state_value_id IS NOT NULL;
--> statement-breakpoint
ALTER TABLE cmdb.workflow_versions
  ADD CONSTRAINT workflow_versions_initial_state_fk FOREIGN KEY (id, initial_state_id)
  REFERENCES cmdb.workflow_states (version_id, id);
--> statement-breakpoint

CREATE TABLE cmdb.workflow_transitions (
  id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  version_id       uuid NOT NULL REFERENCES cmdb.workflow_versions (id) ON DELETE CASCADE,
  key              text NOT NULL CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
  name             text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
  from_state_id    uuid NOT NULL,
  to_state_id      uuid NOT NULL,
  requires_comment boolean NOT NULL DEFAULT false,
  conditions       jsonb CHECK (conditions IS NULL OR pg_column_size(conditions) <= 16384),
  sort_order       integer NOT NULL DEFAULT 0,
  CONSTRAINT workflow_transitions_key_uq UNIQUE (version_id, key),
  -- Both endpoints are states of the transition's own version.
  CONSTRAINT workflow_transitions_from_fk FOREIGN KEY (version_id, from_state_id)
    REFERENCES cmdb.workflow_states (version_id, id) ON DELETE CASCADE,
  CONSTRAINT workflow_transitions_to_fk FOREIGN KEY (version_id, to_state_id)
    REFERENCES cmdb.workflow_states (version_id, id) ON DELETE CASCADE,
  CONSTRAINT workflow_transitions_no_loop CHECK (from_state_id <> to_state_id)
);
--> statement-breakpoint
CREATE INDEX workflow_transitions_from_idx ON cmdb.workflow_transitions (version_id, from_state_id);
--> statement-breakpoint
CREATE INDEX workflow_transitions_to_idx ON cmdb.workflow_transitions (version_id, to_state_id);
--> statement-breakpoint

-- Fields a transition shows and (optionally) requires: real attributes of the class.
CREATE TABLE cmdb.workflow_transition_fields (
  transition_id uuid NOT NULL REFERENCES cmdb.workflow_transitions (id) ON DELETE CASCADE,
  attribute_id  uuid NOT NULL REFERENCES cmdb.ci_attribute_definitions (id) ON DELETE RESTRICT,
  is_required   boolean NOT NULL DEFAULT true,
  sort_order    integer NOT NULL DEFAULT 0,
  PRIMARY KEY (transition_id, attribute_id)
);
--> statement-breakpoint
CREATE INDEX workflow_transition_fields_attr_idx ON cmdb.workflow_transition_fields (attribute_id);
--> statement-breakpoint

-- Every attribute a version depends on (fields, conditions, state attribute),
-- filled at publish, so "this field is used by workflow X v3" is a foreign key.
CREATE TABLE cmdb.workflow_version_attribute_refs (
  version_id   uuid NOT NULL REFERENCES cmdb.workflow_versions (id) ON DELETE CASCADE,
  attribute_id uuid NOT NULL REFERENCES cmdb.ci_attribute_definitions (id) ON DELETE RESTRICT,
  PRIMARY KEY (version_id, attribute_id)
);
--> statement-breakpoint
CREATE INDEX workflow_version_attribute_refs_attr_idx ON cmdb.workflow_version_attribute_refs (attribute_id);
--> statement-breakpoint

-- Who may run which transition: per definition and transition key, so staffing
-- changes need no new version. `_cancel` is the pseudo-key for cancelling.
CREATE TABLE cmdb.workflow_transition_grants (
  definition_id  uuid NOT NULL REFERENCES cmdb.workflow_definitions (id) ON DELETE CASCADE,
  transition_key text NOT NULL CHECK (transition_key ~ '^(_cancel|[a-z][a-z0-9_]{0,62})$'),
  profile_id     uuid NOT NULL REFERENCES cmdb.permission_profiles (id) ON DELETE CASCADE,
  PRIMARY KEY (definition_id, transition_key, profile_id)
);
--> statement-breakpoint
CREATE INDEX workflow_transition_grants_profile_idx ON cmdb.workflow_transition_grants (profile_id);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Instances: one run of one version on one CI, pinned to that version.
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.workflow_instances (
  id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  definition_id      uuid NOT NULL REFERENCES cmdb.workflow_definitions (id) ON DELETE RESTRICT,
  version_id         uuid NOT NULL,
  -- Instances outlive a soft delete of their CI; a purge must archive them first.
  ci_id              uuid NOT NULL REFERENCES cmdb.configuration_items (id) ON DELETE RESTRICT,
  current_state_id   uuid NOT NULL,
  status             text NOT NULL CHECK (status IN ('active', 'completed', 'cancelled')),
  started_at         timestamptz NOT NULL DEFAULT now(),
  started_by_id      uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  started_by_name    text NOT NULL,
  last_transition_at timestamptz NOT NULL DEFAULT now(),
  ended_at           timestamptz,
  -- Optimistic concurrency.
  version            integer NOT NULL DEFAULT 1 CHECK (version >= 1),
  -- The version is one of the definition's, and the state one of the version's.
  CONSTRAINT workflow_instances_version_fk FOREIGN KEY (version_id, definition_id)
    REFERENCES cmdb.workflow_versions (id, definition_id),
  CONSTRAINT workflow_instances_state_fk FOREIGN KEY (version_id, current_state_id)
    REFERENCES cmdb.workflow_states (version_id, id),
  CONSTRAINT workflow_instances_ended CHECK ((status = 'active') = (ended_at IS NULL))
);
--> statement-breakpoint
-- One running instance per workflow per CI.
CREATE UNIQUE INDEX workflow_instances_one_active
  ON cmdb.workflow_instances (definition_id, ci_id) WHERE status = 'active';
--> statement-breakpoint
CREATE INDEX workflow_instances_ci_idx ON cmdb.workflow_instances (ci_id, started_at DESC);
--> statement-breakpoint
CREATE INDEX workflow_instances_state_idx
  ON cmdb.workflow_instances (current_state_id, last_transition_at DESC, id) WHERE status = 'active';
--> statement-breakpoint
CREATE INDEX workflow_instances_def_idx
  ON cmdb.workflow_instances (definition_id, status, last_transition_at DESC, id);
--> statement-breakpoint
CREATE INDEX workflow_instances_version_idx ON cmdb.workflow_instances (version_id);
--> statement-breakpoint

-- Append-only business history of an instance (trigger below). Keys are copied
-- as text, so the history stays readable whatever happens to the version.
CREATE TABLE cmdb.workflow_instance_events (
  id              bigint PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
  instance_id     uuid NOT NULL REFERENCES cmdb.workflow_instances (id) ON DELETE RESTRICT,
  kind            text NOT NULL CHECK (kind IN ('start', 'transition', 'cancel', 'migrate', 'force')),
  transition_key  text,
  from_state_key  text,
  to_state_key    text NOT NULL,
  -- 'migrate' only.
  from_version_no integer,
  to_version_no   integer NOT NULL,
  occurred_at     timestamptz NOT NULL DEFAULT now(),
  -- Same domain as audit_log.actor_type.
  actor_type      text NOT NULL CHECK (actor_type IN ('user', 'system', 'api_client', 'import')),
  actor_id        text,
  actor_name      text,
  comment         text CHECK (comment IS NULL OR length(comment) <= 4000),
  -- {attributeKey: {old, new}} written by this step.
  field_changes   jsonb CHECK (field_changes IS NULL OR jsonb_typeof(field_changes) = 'object'),
  -- Joins to audit_log.request_id.
  request_id      text,
  CONSTRAINT workflow_instance_events_transition CHECK ((kind = 'transition') = (transition_key IS NOT NULL)),
  CONSTRAINT workflow_instance_events_migrate CHECK ((kind = 'migrate') = (from_version_no IS NOT NULL))
);
--> statement-breakpoint
CREATE INDEX workflow_instance_events_instance_idx ON cmdb.workflow_instance_events (instance_id, id);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Immutability. Only a draft version's graph can change. A published version
-- may only be retired; nothing else about it changes. Deleting a non-draft
-- version or its graph rows is refused, except as the cascade of deleting its
-- definition (the API deletes only definitions that never had an instance).
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.workflow_versions_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    IF OLD.status <> 'draft'
       AND EXISTS (SELECT 1 FROM cmdb.workflow_definitions d WHERE d.id = OLD.definition_id) THEN
      RAISE EXCEPTION 'workflow_versions: version % is %, it cannot be deleted', OLD.version_no, OLD.status
        USING ERRCODE = 'object_not_in_prerequisite_state', CONSTRAINT = 'workflow_versions_immutable';
    END IF;
    RETURN OLD;
  END IF;
  IF TG_OP = 'INSERT' THEN
    IF NEW.status <> 'draft' THEN
      RAISE EXCEPTION 'workflow_versions: a version is created as a draft and then published'
        USING ERRCODE = 'object_not_in_prerequisite_state', CONSTRAINT = 'workflow_versions_immutable';
    END IF;
    RETURN NEW;
  END IF;
  IF NEW.id <> OLD.id OR NEW.definition_id <> OLD.definition_id OR NEW.version_no <> OLD.version_no THEN
    RAISE EXCEPTION 'workflow_versions: id, definition and version number never change'
      USING ERRCODE = 'object_not_in_prerequisite_state', CONSTRAINT = 'workflow_versions_immutable';
  END IF;
  -- A draft is edited freely and published once.
  IF OLD.status = 'draft' AND NEW.status IN ('draft', 'published') THEN
    RETURN NEW;
  END IF;
  -- Retiring changes the status and nothing else.
  IF OLD.status = 'published' AND NEW.status = 'retired'
     AND (to_jsonb(NEW) - 'status') = (to_jsonb(OLD) - 'status') THEN
    RETURN NEW;
  END IF;
  IF to_jsonb(NEW) = to_jsonb(OLD) THEN
    RETURN NEW;
  END IF;
  RAISE EXCEPTION 'workflow_versions: version % is %, it cannot be changed (% -> %)',
      OLD.version_no, OLD.status, OLD.status, NEW.status
    USING ERRCODE = 'object_not_in_prerequisite_state', CONSTRAINT = 'workflow_versions_immutable';
END;
$$;
--> statement-breakpoint
CREATE TRIGGER workflow_versions_guard BEFORE INSERT OR UPDATE OR DELETE ON cmdb.workflow_versions
  FOR EACH ROW EXECUTE FUNCTION cmdb.workflow_versions_guard();
--> statement-breakpoint

-- Graph rows: states, transitions and attribute references carry version_id,
-- transition fields reach it through their transition. A row whose version is
-- already gone is the cascade of a definition delete and may go.
CREATE FUNCTION cmdb.workflow_graph_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
DECLARE
  rows_version uuid[];
  v uuid;
  st text;
BEGIN
  IF TG_TABLE_NAME = 'workflow_transition_fields' THEN
    SELECT array_agg(t.version_id) INTO rows_version FROM cmdb.workflow_transitions t
    WHERE t.id = ANY (ARRAY[CASE WHEN TG_OP <> 'INSERT' THEN OLD.transition_id END,
                            CASE WHEN TG_OP <> 'DELETE' THEN NEW.transition_id END]);
  ELSE
    rows_version := ARRAY[CASE WHEN TG_OP <> 'INSERT' THEN OLD.version_id END,
                          CASE WHEN TG_OP <> 'DELETE' THEN NEW.version_id END];
  END IF;
  FOREACH v IN ARRAY coalesce(rows_version, '{}') LOOP
    CONTINUE WHEN v IS NULL;
    SELECT w.status INTO st FROM cmdb.workflow_versions w WHERE w.id = v;
    IF FOUND AND st <> 'draft' THEN
      RAISE EXCEPTION '%: the version is %, only a draft can be changed', TG_TABLE_NAME, st
        USING ERRCODE = 'object_not_in_prerequisite_state', CONSTRAINT = 'workflow_versions_immutable';
    END IF;
  END LOOP;
  RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER workflow_states_guard BEFORE INSERT OR UPDATE OR DELETE ON cmdb.workflow_states
  FOR EACH ROW EXECUTE FUNCTION cmdb.workflow_graph_guard();
--> statement-breakpoint
CREATE TRIGGER workflow_transitions_guard BEFORE INSERT OR UPDATE OR DELETE ON cmdb.workflow_transitions
  FOR EACH ROW EXECUTE FUNCTION cmdb.workflow_graph_guard();
--> statement-breakpoint
CREATE TRIGGER workflow_transition_fields_guard BEFORE INSERT OR UPDATE OR DELETE ON cmdb.workflow_transition_fields
  FOR EACH ROW EXECUTE FUNCTION cmdb.workflow_graph_guard();
--> statement-breakpoint
CREATE TRIGGER workflow_version_attribute_refs_guard
  BEFORE INSERT OR UPDATE OR DELETE ON cmdb.workflow_version_attribute_refs
  FOR EACH ROW EXECUTE FUNCTION cmdb.workflow_graph_guard();
--> statement-breakpoint
-- TRUNCATE skips row triggers.
CREATE FUNCTION cmdb.workflow_no_truncate() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN
  RAISE EXCEPTION '% cannot be truncated', TG_TABLE_NAME USING ERRCODE = 'insufficient_privilege';
END;
$$;
--> statement-breakpoint
CREATE TRIGGER workflow_versions_no_truncate BEFORE TRUNCATE ON cmdb.workflow_versions
  FOR EACH STATEMENT EXECUTE FUNCTION cmdb.workflow_no_truncate();
--> statement-breakpoint
CREATE TRIGGER workflow_states_no_truncate BEFORE TRUNCATE ON cmdb.workflow_states
  FOR EACH STATEMENT EXECUTE FUNCTION cmdb.workflow_no_truncate();
--> statement-breakpoint
CREATE TRIGGER workflow_transitions_no_truncate BEFORE TRUNCATE ON cmdb.workflow_transitions
  FOR EACH STATEMENT EXECUTE FUNCTION cmdb.workflow_no_truncate();
--> statement-breakpoint
CREATE TRIGGER workflow_transition_fields_no_truncate BEFORE TRUNCATE ON cmdb.workflow_transition_fields
  FOR EACH STATEMENT EXECUTE FUNCTION cmdb.workflow_no_truncate();
--> statement-breakpoint
CREATE TRIGGER workflow_version_attribute_refs_no_truncate BEFORE TRUNCATE ON cmdb.workflow_version_attribute_refs
  FOR EACH STATEMENT EXECUTE FUNCTION cmdb.workflow_no_truncate();
--> statement-breakpoint

-- A definition's key (its export identity) and class never change.
CREATE FUNCTION cmdb.workflow_definitions_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN
  IF NEW.key <> OLD.key OR NEW.class_id <> OLD.class_id THEN
    RAISE EXCEPTION 'workflow_definitions: the key and the class of a workflow never change'
      USING ERRCODE = 'object_not_in_prerequisite_state', CONSTRAINT = 'workflow_definitions_immutable';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER workflow_definitions_guard BEFORE UPDATE OF key, class_id ON cmdb.workflow_definitions
  FOR EACH ROW EXECUTE FUNCTION cmdb.workflow_definitions_guard();
--> statement-breakpoint

-- Events are append-only, for every role: UPDATE, DELETE and TRUNCATE are
-- refused. Restore loads them with the application's triggers off.
CREATE FUNCTION cmdb.workflow_instance_events_append_only() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN
  RAISE EXCEPTION 'workflow_instance_events is append-only (% rejected)', TG_OP
    USING ERRCODE = 'insufficient_privilege';
END;
$$;
--> statement-breakpoint
CREATE TRIGGER workflow_instance_events_append_only BEFORE UPDATE OR DELETE ON cmdb.workflow_instance_events
  FOR EACH ROW EXECUTE FUNCTION cmdb.workflow_instance_events_append_only();
--> statement-breakpoint
CREATE TRIGGER workflow_instance_events_no_truncate BEFORE TRUNCATE ON cmdb.workflow_instance_events
  FOR EACH STATEMENT EXECUTE FUNCTION cmdb.workflow_instance_events_append_only();
--> statement-breakpoint

REVOKE ALL ON FUNCTION cmdb.workflow_versions_guard() FROM PUBLIC;
--> statement-breakpoint
REVOKE ALL ON FUNCTION cmdb.workflow_graph_guard() FROM PUBLIC;
--> statement-breakpoint
REVOKE ALL ON FUNCTION cmdb.workflow_no_truncate() FROM PUBLIC;
--> statement-breakpoint
REVOKE ALL ON FUNCTION cmdb.workflow_definitions_guard() FROM PUBLIC;
--> statement-breakpoint
REVOKE ALL ON FUNCTION cmdb.workflow_instance_events_append_only() FROM PUBLIC;
--> statement-breakpoint

-- Three-role install: 0008's default privileges give the API role DML on the
-- new tables; on the events it keeps SELECT and INSERT only, like audit_log.
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = app_role) AND current_user <> app_role THEN
    EXECUTE format('REVOKE UPDATE, DELETE, TRUNCATE ON cmdb.workflow_instance_events FROM %I', app_role);
  END IF;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- audit_log: the workflow actions (keeps every action up to 0043). Re-added
-- NOT VALID: no scan here; 0047 validates in its own transaction.
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
  'workflow.transition', 'workflow.migrate', 'workflow.force'
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
                 'workflow.publish', 'workflow.start', 'workflow.cancel')
      AND old_value IS NULL AND new_value IS NOT NULL)
  -- Workflow steps that change a CI's state: before and after.
  OR (action IN ('workflow.transition', 'workflow.migrate', 'workflow.force')
      AND old_value IS NOT NULL AND new_value IS NOT NULL)
) NOT VALID;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- prune_audit_log(): the workflow actions join the `changes` scope (they are
-- CI and definition history). The events table is not touched (Q2). Same
-- body as 0043 otherwise; CREATE OR REPLACE keeps the owner and the EXECUTE
-- grants.
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
                              'workflow.transition', 'workflow.migrate', 'workflow.force']
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
