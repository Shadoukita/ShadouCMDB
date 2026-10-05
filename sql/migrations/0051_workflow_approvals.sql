-- Approvals on workflow transitions: schema (v0.4.0 slice A1, SHAA-1871;
-- design on SHAA-1869 §3).
--
-- Additive only: new tables, nullable columns with no default on existing
-- tables, and replaced function bodies. No existing row is rewritten and no CI
-- data moves. Nothing changes for an install until an administrator publishes
-- a version with an approval step.
--
--   * Design time: a transition's approval steps (part of the immutable
--     version graph), approver assignments per definition, transition and
--     step, and time-boxed delegations between users.
--   * Run time: approval requests (at most one pending per instance), their
--     steps, the resolved eligibility of an active step, and append-only
--     decisions (one vote per actor and per principal per step).
--   * Instance events gain the approval kinds and three columns; the archive
--     a CI purge writes gains the approvals of each instance.
--   * The approval actions of the audit log. The event kind and audit CHECK
--     constraints are re-added NOT VALID here and validated by 0052 in a
--     transaction of its own (the 0046/0047 pattern, see sql/README.md).

-- ---------------------------------------------------------------------------
-- Design time: the approval policy of a transition. Part of the immutable
-- version graph (workflow_graph_guard below).
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.workflow_transition_approval_steps (
  transition_id         uuid NOT NULL REFERENCES cmdb.workflow_transitions (id) ON DELETE CASCADE,
  step_no               smallint NOT NULL CHECK (step_no BETWEEN 1 AND 5),
  key                   text NOT NULL CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
  name                  text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
  required_approvals    smallint NOT NULL DEFAULT 1 CHECK (required_approvals BETWEEN 1 AND 20),
  -- SLA: the step is overdue this long after it became active. NULL: no due date.
  due_after             interval CHECK (due_after IS NULL OR due_after BETWEEN interval '15 minutes' AND interval '90 days'),
  on_overdue            text NOT NULL DEFAULT 'flag' CHECK (on_overdue IN ('flag', 'reject')),
  -- Separation of duties beyond four-eyes, which is always on.
  -- An approver of an earlier step may not approve this one.
  distinct_from_earlier boolean NOT NULL DEFAULT true,
  -- Transition keys whose actors (on this instance) may not approve.
  exclude_actors_of     text[] NOT NULL DEFAULT '{}',
  -- Decisions need a signed-in session unless set.
  allow_api_tokens      boolean NOT NULL DEFAULT false,
  PRIMARY KEY (transition_id, step_no),
  CONSTRAINT workflow_transition_approval_steps_key_uq UNIQUE (transition_id, key),
  CONSTRAINT workflow_transition_approval_steps_reject_needs_due CHECK (on_overdue = 'flag' OR due_after IS NOT NULL)
);
--> statement-breakpoint

-- Who decides a step: per definition, keyed like transition grants, so
-- staffing changes need no new version. Users, groups and profiles cascade,
-- like business_service_owners; an attribute in use cannot be deleted.
CREATE TABLE cmdb.workflow_approval_assignments (
  id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  definition_id      uuid NOT NULL REFERENCES cmdb.workflow_definitions (id) ON DELETE CASCADE,
  transition_key     text NOT NULL CHECK (transition_key ~ '^[a-z][a-z0-9_]{0,62}$'),
  step_key           text NOT NULL CHECK (step_key ~ '^[a-z][a-z0-9_]{0,62}$'),
  -- escalation: eligible only once the step is overdue.
  role               text NOT NULL DEFAULT 'approver' CHECK (role IN ('approver', 'escalation')),
  source             text NOT NULL CHECK (source IN ('profile', 'group', 'user', 'ci_attribute', 'service_owner')),
  profile_id         uuid REFERENCES cmdb.permission_profiles (id) ON DELETE CASCADE,
  group_id           uuid REFERENCES cmdb.user_groups (id) ON DELETE CASCADE,
  user_id            uuid REFERENCES cmdb.users (id) ON DELETE CASCADE,
  -- A reference attribute of the definition's class whose target is the Person class.
  attribute_id       uuid REFERENCES cmdb.ci_attribute_definitions (id) ON DELETE RESTRICT,
  service_owner_role text CHECK (service_owner_role IN ('technical', 'business')),
  CONSTRAINT workflow_approval_assignments_one_source CHECK (
    num_nonnulls(profile_id, group_id, user_id, attribute_id, service_owner_role) = 1
    AND (source = 'profile')       = (profile_id IS NOT NULL)
    AND (source = 'group')         = (group_id IS NOT NULL)
    AND (source = 'user')          = (user_id IS NOT NULL)
    AND (source = 'ci_attribute')  = (attribute_id IS NOT NULL)
    AND (source = 'service_owner') = (service_owner_role IS NOT NULL))
);
--> statement-breakpoint
-- A service owner source has no id: the nil uuid stands in, so that NULLs,
-- which never collide in a unique index, do not let it be assigned twice.
CREATE UNIQUE INDEX workflow_approval_assignments_uq ON cmdb.workflow_approval_assignments
  (definition_id, transition_key, step_key, role, source,
   coalesce(profile_id, group_id, user_id, attribute_id, '00000000-0000-0000-0000-000000000000'::uuid),
   coalesce(service_owner_role, ''));
--> statement-breakpoint
CREATE INDEX workflow_approval_assignments_profile_idx
  ON cmdb.workflow_approval_assignments (profile_id) WHERE profile_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX workflow_approval_assignments_group_idx
  ON cmdb.workflow_approval_assignments (group_id) WHERE group_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX workflow_approval_assignments_user_idx
  ON cmdb.workflow_approval_assignments (user_id) WHERE user_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX workflow_approval_assignments_attr_idx
  ON cmdb.workflow_approval_assignments (attribute_id) WHERE attribute_id IS NOT NULL;
--> statement-breakpoint

-- Time-boxed delegation: the principal lets the delegate decide in their
-- place. Never deleted, only revoked, so the history stays. A user can be
-- deleted for good (data/auth.rs delete_user): their side becomes NULL and
-- the row keeps their name, so a decision made through it (delegation_id,
-- RESTRICT) neither blocks the delete nor loses who acted for whom. A
-- delegation with a NULL side never qualifies again.
CREATE TABLE cmdb.workflow_approval_delegations (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  principal_id    uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  principal_name  text NOT NULL,
  delegate_id     uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  delegate_name   text NOT NULL,
  -- NULL: every workflow.
  definition_id   uuid REFERENCES cmdb.workflow_definitions (id) ON DELETE CASCADE,
  starts_at       timestamptz NOT NULL,
  ends_at         timestamptz NOT NULL,
  reason          text CHECK (reason IS NULL OR length(reason) <= 500),
  created_at      timestamptz NOT NULL DEFAULT now(),
  created_by_id   uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  created_by_name text NOT NULL,
  revoked_at      timestamptz,
  revoked_by_name text,
  CONSTRAINT workflow_approval_delegations_not_self
    CHECK (principal_id IS NULL OR delegate_id IS NULL OR principal_id <> delegate_id),
  -- An administrator may not delegate someone's approvals to themselves (SHAA-1872 C2).
  CONSTRAINT workflow_approval_delegations_not_creator
    CHECK (created_by_id IS NULL OR created_by_id = principal_id OR created_by_id <> delegate_id),
  CONSTRAINT workflow_approval_delegations_window
    CHECK (ends_at > starts_at AND ends_at - starts_at <= interval '90 days'),
  CONSTRAINT workflow_approval_delegations_revoked CHECK ((revoked_at IS NULL) = (revoked_by_name IS NULL))
);
--> statement-breakpoint
CREATE INDEX workflow_approval_delegations_delegate_idx ON cmdb.workflow_approval_delegations (delegate_id, ends_at)
  WHERE revoked_at IS NULL AND delegate_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX workflow_approval_delegations_principal_idx ON cmdb.workflow_approval_delegations (principal_id, ends_at)
  WHERE principal_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX workflow_approval_delegations_created_by_idx ON cmdb.workflow_approval_delegations (created_by_id)
  WHERE created_by_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX workflow_approval_delegations_definition_idx ON cmdb.workflow_approval_delegations (definition_id)
  WHERE definition_id IS NOT NULL;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Run time.
-- ---------------------------------------------------------------------------

-- One attempt to run an approval-gated transition on one instance. Kept for
-- the life of the CI, then archived with its instance (never pruned).
CREATE TABLE cmdb.workflow_approval_requests (
  id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  instance_id        uuid NOT NULL REFERENCES cmdb.workflow_instances (id) ON DELETE RESTRICT,
  -- The gated transition of the instance's pinned version. Its key identifies the assignments.
  version_id         uuid NOT NULL,
  transition_key     text NOT NULL,
  -- Per instance: 1, 2, … (re-requests).
  request_no         integer NOT NULL CHECK (request_no >= 1),
  status             text NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'withdrawn', 'cancelled')),
  close_reason       text CHECK (close_reason IN ('approved', 'rejected', 'overdue', 'withdrawn',
                                                  'instance_cancelled', 'instance_forced', 'instance_migrated',
                                                  'ci_deleted')),
  current_step_no    smallint NOT NULL DEFAULT 1 CHECK (current_step_no BETWEEN 1 AND 5),
  requested_at       timestamptz NOT NULL DEFAULT now(),
  requested_by_id    uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  requested_by_name  text NOT NULL,
  -- Four-eyes: every identity behind the requesting credential, frozen at request time.
  excluded_user_ids  uuid[] NOT NULL CHECK (cardinality(excluded_user_ids) BETWEEN 1 AND 2),
  -- The token behind the request, when it came through one, and who minted it
  -- (NULL: unknown). No foreign key, like actor ids (SHAA-1872 C3).
  token_id           uuid,
  token_creator_id   uuid,
  comment            text CHECK (comment IS NULL OR length(comment) <= 4000),
  -- The transition fields as submitted, applied on final approval: {attributeKey: value}.
  staged_fields      jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(staged_fields) = 'object'),
  -- The same fields' values when the request was made, so the final approval can detect a stale request.
  field_baseline     jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(field_baseline) = 'object'),
  closed_at          timestamptz,
  closed_by_name     text,
  -- Optimistic concurrency.
  version            integer NOT NULL DEFAULT 1 CHECK (version >= 1),
  CONSTRAINT workflow_approval_requests_transition_fk FOREIGN KEY (version_id, transition_key)
    REFERENCES cmdb.workflow_transitions (version_id, key),
  CONSTRAINT workflow_approval_requests_token CHECK (token_creator_id IS NULL OR token_id IS NOT NULL),
  CONSTRAINT workflow_approval_requests_closed CHECK ((status = 'pending') = (closed_at IS NULL)),
  CONSTRAINT workflow_approval_requests_reason CHECK ((status = 'pending') = (close_reason IS NULL)),
  CONSTRAINT workflow_approval_requests_no_uq UNIQUE (instance_id, request_no)
);
--> statement-breakpoint
-- At most one pending request per instance; also the "awaiting approval" filter.
CREATE UNIQUE INDEX workflow_approval_requests_one_pending
  ON cmdb.workflow_approval_requests (instance_id) WHERE status = 'pending';
--> statement-breakpoint
CREATE INDEX workflow_approval_requests_requester_idx
  ON cmdb.workflow_approval_requests (requested_by_id, requested_at DESC) WHERE requested_by_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX workflow_approval_requests_recent_idx ON cmdb.workflow_approval_requests (requested_at, id);
--> statement-breakpoint
-- The foreign key to the transition: a published transition is immutable, but
-- a draft's is deleted by key.
CREATE INDEX workflow_approval_requests_transition_idx
  ON cmdb.workflow_approval_requests (version_id, transition_key);
--> statement-breakpoint

-- The run-time state of one step of one request.
CREATE TABLE cmdb.workflow_approval_request_steps (
  request_id         uuid NOT NULL REFERENCES cmdb.workflow_approval_requests (id) ON DELETE RESTRICT,
  step_no            smallint NOT NULL CHECK (step_no BETWEEN 1 AND 5),
  step_key           text NOT NULL,
  -- Copied from the version, so history reads alone.
  required_approvals smallint NOT NULL CHECK (required_approvals BETWEEN 1 AND 20),
  status             text NOT NULL CHECK (status IN ('waiting', 'active', 'approved', 'rejected', 'closed')),
  activated_at       timestamptz,
  due_at             timestamptz,
  -- Set once by the SLA sweep.
  overdue_at         timestamptz,
  completed_at       timestamptz,
  -- Distinct users able to decide, at the last resolution.
  eligible_count     integer CHECK (eligible_count IS NULL OR eligible_count >= 0),
  resolved_at        timestamptz,
  PRIMARY KEY (request_id, step_no),
  CONSTRAINT workflow_approval_request_steps_due CHECK (due_at IS NULL OR activated_at IS NOT NULL)
);
--> statement-breakpoint
-- The SLA sweep.
CREATE INDEX workflow_approval_request_steps_due_idx
  ON cmdb.workflow_approval_request_steps (due_at) WHERE status = 'active' AND overdue_at IS NULL AND due_at IS NOT NULL;
--> statement-breakpoint

-- Resolved eligibility of an active step. Profiles and groups are kept as
-- principals, so membership is read live. Sources that depend on the CI
-- (attribute, service owner) and named users are resolved to users. No
-- foreign key on the principal: every decision re-checks it live.
CREATE TABLE cmdb.workflow_approval_eligibility (
  request_id     uuid NOT NULL,
  step_no        smallint NOT NULL,
  role           text NOT NULL CHECK (role IN ('approver', 'escalation')),
  principal_kind text NOT NULL CHECK (principal_kind IN ('user', 'profile', 'group')),
  principal_id   uuid NOT NULL,
  -- How the principal qualified, for the decision record: {source, label}.
  via            jsonb NOT NULL CHECK (jsonb_typeof(via) = 'object'),
  PRIMARY KEY (request_id, step_no, role, principal_kind, principal_id),
  CONSTRAINT workflow_approval_eligibility_step_fk FOREIGN KEY (request_id, step_no)
    REFERENCES cmdb.workflow_approval_request_steps (request_id, step_no) ON DELETE CASCADE
);
--> statement-breakpoint
CREATE INDEX workflow_approval_eligibility_principal_idx
  ON cmdb.workflow_approval_eligibility (principal_kind, principal_id, request_id);
--> statement-breakpoint

-- One approve or reject by one user on one step. Append-only, like the
-- events (trigger below, and no UPDATE/DELETE for the API role).
CREATE TABLE cmdb.workflow_approval_decisions (
  id                bigint PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
  request_id        uuid NOT NULL,
  step_no           smallint NOT NULL,
  decision          text NOT NULL CHECK (decision IN ('approve', 'reject')),
  -- No foreign key: a decision outlives its user (the name below).
  actor_id          uuid NOT NULL,
  actor_name        text NOT NULL,
  credential        text NOT NULL CHECK (credential IN ('session', 'token')),
  -- The token a decision was made with, and who minted it. No foreign key, like actor_id (SHAA-1872 C3).
  token_id          uuid,
  token_creator_id  uuid,
  -- The principal, when acting through a delegation.
  on_behalf_of_id   uuid,
  on_behalf_of_name text,
  delegation_id     uuid REFERENCES cmdb.workflow_approval_delegations (id) ON DELETE RESTRICT,
  -- The eligibility row(s) that qualified the principal.
  via               jsonb NOT NULL CHECK (jsonb_typeof(via) IN ('object', 'array')),
  comment           text CHECK (comment IS NULL OR length(comment) <= 4000),
  decided_at        timestamptz NOT NULL DEFAULT now(),
  -- Joins audit_log.request_id.
  http_request_id   text,
  CONSTRAINT workflow_approval_decisions_step_fk FOREIGN KEY (request_id, step_no)
    REFERENCES cmdb.workflow_approval_request_steps (request_id, step_no) ON DELETE RESTRICT,
  CONSTRAINT workflow_approval_decisions_token CHECK (
    (credential = 'token') = (token_id IS NOT NULL) AND (token_creator_id IS NULL OR token_id IS NOT NULL)),
  CONSTRAINT workflow_approval_decisions_delegated CHECK (
    (on_behalf_of_id IS NULL) = (delegation_id IS NULL)
    AND (on_behalf_of_id IS NULL) = (on_behalf_of_name IS NULL))
);
--> statement-breakpoint
-- One vote per person per step, whether cast in person or for someone.
CREATE UNIQUE INDEX workflow_approval_decisions_actor_uq
  ON cmdb.workflow_approval_decisions (request_id, step_no, actor_id);
--> statement-breakpoint
CREATE UNIQUE INDEX workflow_approval_decisions_principal_uq
  ON cmdb.workflow_approval_decisions (request_id, step_no, coalesce(on_behalf_of_id, actor_id));
--> statement-breakpoint
CREATE INDEX workflow_approval_decisions_delegation_idx
  ON cmdb.workflow_approval_decisions (delegation_id) WHERE delegation_id IS NOT NULL;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Immutability. A published version's approval steps are as immutable as its
-- transitions: the graph guard resolves their version through transition_id,
-- as for transition fields. Same body as 0046 otherwise.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION cmdb.workflow_graph_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
DECLARE
  rows_version uuid[];
  v uuid;
  st text;
BEGIN
  IF TG_TABLE_NAME IN ('workflow_transition_fields', 'workflow_transition_approval_steps') THEN
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
CREATE TRIGGER workflow_transition_approval_steps_guard
  BEFORE INSERT OR UPDATE OR DELETE ON cmdb.workflow_transition_approval_steps
  FOR EACH ROW EXECUTE FUNCTION cmdb.workflow_graph_guard();
--> statement-breakpoint
CREATE TRIGGER workflow_transition_approval_steps_no_truncate BEFORE TRUNCATE ON cmdb.workflow_transition_approval_steps
  FOR EACH STATEMENT EXECUTE FUNCTION cmdb.workflow_no_truncate();
--> statement-breakpoint

-- Events and decisions are append-only: UPDATE and TRUNCATE are always
-- refused, DELETE passes only for the CI purge archive (0050's escape: the
-- variable is set by the archive trigger, which runs as the table owner).
-- Same body as 0050, with the table named from the trigger.
CREATE OR REPLACE FUNCTION cmdb.workflow_instance_events_append_only() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN
  IF TG_OP = 'DELETE' AND current_setting('shadoucmdb.workflow_archive', true) = 'on'
     AND current_user = (SELECT r.rolname FROM pg_class c JOIN pg_roles r ON r.oid = c.relowner WHERE c.oid = TG_RELID)
  THEN
    RETURN OLD;
  END IF;
  RAISE EXCEPTION '% is append-only (% rejected)', TG_TABLE_NAME, TG_OP
    USING ERRCODE = 'insufficient_privilege';
END;
$$;
--> statement-breakpoint
CREATE TRIGGER workflow_approval_decisions_append_only BEFORE UPDATE OR DELETE ON cmdb.workflow_approval_decisions
  FOR EACH ROW EXECUTE FUNCTION cmdb.workflow_instance_events_append_only();
--> statement-breakpoint
CREATE TRIGGER workflow_approval_decisions_no_truncate BEFORE TRUNCATE ON cmdb.workflow_approval_decisions
  FOR EACH STATEMENT EXECUTE FUNCTION cmdb.workflow_instance_events_append_only();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Instance events: the approval kinds. Nullable columns with no default are
-- metadata-only; the CHECK constraints are re-added NOT VALID (no scan of a
-- table that may hold tens of millions of rows) and validated by 0052.
-- No foreign key to the request: the archive deletes requests before events.
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.workflow_instance_events
  ADD COLUMN approval_request_id uuid,
  ADD COLUMN approval_step_no smallint,
  ADD COLUMN on_behalf_of_name text;
--> statement-breakpoint
ALTER TABLE cmdb.workflow_instance_events DROP CONSTRAINT workflow_instance_events_kind_check;
--> statement-breakpoint
ALTER TABLE cmdb.workflow_instance_events ADD CONSTRAINT workflow_instance_events_kind_check CHECK (kind IN (
  'start', 'transition', 'cancel', 'migrate', 'force',
  'approval_request', 'approval_decision', 'approval_withdraw', 'approval_close', 'approval_overdue'
)) NOT VALID;
--> statement-breakpoint
ALTER TABLE cmdb.workflow_instance_events ADD CONSTRAINT workflow_instance_events_approval
  CHECK (kind NOT LIKE 'approval\_%' OR approval_request_id IS NOT NULL) NOT VALID;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- The archive a CI purge writes: the approvals of each instance. Rows
-- archived before this release keep NULL.
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.workflow_instance_archive
  ADD COLUMN approvals jsonb CHECK (approvals IS NULL OR jsonb_typeof(approvals) = 'array');
--> statement-breakpoint

-- Before a CI row goes: archive its instances with their events and their
-- approval requests (steps and decisions), then delete them. Same as 0050
-- otherwise; CREATE OR REPLACE keeps the owner, the trigger and the revoked
-- EXECUTE. Pending requests are archived as they are: a purge only runs on
-- CIs that were soft-deleted, which closed them.
CREATE OR REPLACE FUNCTION cmdb.configuration_items_archive_workflows() RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
  ids uuid[];
  request_ids uuid[];
BEGIN
  SELECT array_agg(wi.id) INTO ids FROM cmdb.workflow_instances wi WHERE wi.ci_id = OLD.id;
  IF ids IS NULL THEN
    RETURN OLD;
  END IF;
  SELECT array_agg(r.id) INTO request_ids FROM cmdb.workflow_approval_requests r WHERE r.instance_id = ANY (ids);
  INSERT INTO cmdb.workflow_instance_archive
    (instance_id, ci_id, ci_ident, ci_label, class_key, definition_id, definition_key, version_no, state_key,
     status, started_at, started_by_name, last_transition_at, ended_at, events, approvals, request_id)
  SELECT wi.id, OLD.id, OLD.ident, OLD.label, c.key, d.id, d.key, v.version_no, s.key,
         wi.status, wi.started_at, wi.started_by_name, wi.last_transition_at, wi.ended_at,
         coalesce((SELECT jsonb_agg(jsonb_build_object(
                     'id', e.id, 'kind', e.kind, 'transitionKey', e.transition_key,
                     'fromStateKey', e.from_state_key, 'toStateKey', e.to_state_key,
                     'fromVersionNo', e.from_version_no, 'toVersionNo', e.to_version_no,
                     'occurredAt', e.occurred_at, 'actorType', e.actor_type, 'actorId', e.actor_id,
                     'actorName', e.actor_name, 'comment', e.comment, 'fieldChanges', e.field_changes,
                     'requestId', e.request_id, 'approvalRequestId', e.approval_request_id,
                     'approvalStepNo', e.approval_step_no, 'onBehalfOfName', e.on_behalf_of_name) ORDER BY e.id)
                   FROM cmdb.workflow_instance_events e WHERE e.instance_id = wi.id), '[]'::jsonb),
         -- Oldest first; each request with its steps, each step with its decisions.
         coalesce((SELECT jsonb_agg(jsonb_build_object(
                     'id', r.id, 'requestNo', r.request_no, 'transitionKey', r.transition_key,
                     'status', r.status, 'closeReason', r.close_reason, 'currentStepNo', r.current_step_no,
                     'requestedAt', r.requested_at, 'requestedById', r.requested_by_id,
                     'requestedByName', r.requested_by_name, 'excludedUserIds', to_jsonb(r.excluded_user_ids),
                     'tokenId', r.token_id, 'tokenCreatorId', r.token_creator_id,
                     'comment', r.comment, 'stagedFields', r.staged_fields, 'fieldBaseline', r.field_baseline,
                     'closedAt', r.closed_at, 'closedByName', r.closed_by_name,
                     'steps', coalesce((SELECT jsonb_agg(jsonb_build_object(
                                'stepNo', st.step_no, 'stepKey', st.step_key,
                                'requiredApprovals', st.required_approvals, 'status', st.status,
                                'activatedAt', st.activated_at, 'dueAt', st.due_at, 'overdueAt', st.overdue_at,
                                'completedAt', st.completed_at, 'eligibleCount', st.eligible_count,
                                'decisions', coalesce((SELECT jsonb_agg(jsonb_build_object(
                                     'id', dc.id, 'decision', dc.decision, 'actorId', dc.actor_id,
                                     'actorName', dc.actor_name, 'credential', dc.credential,
                                     'tokenId', dc.token_id, 'tokenCreatorId', dc.token_creator_id,
                                     'onBehalfOfId', dc.on_behalf_of_id, 'onBehalfOfName', dc.on_behalf_of_name,
                                     'delegationId', dc.delegation_id, 'via', dc.via, 'comment', dc.comment,
                                     'decidedAt', dc.decided_at, 'requestId', dc.http_request_id) ORDER BY dc.id)
                                   FROM cmdb.workflow_approval_decisions dc
                                   WHERE dc.request_id = st.request_id AND dc.step_no = st.step_no), '[]'::jsonb))
                              ORDER BY st.step_no)
                            FROM cmdb.workflow_approval_request_steps st WHERE st.request_id = r.id), '[]'::jsonb))
                   ORDER BY r.request_no)
                   FROM cmdb.workflow_approval_requests r WHERE r.instance_id = wi.id), '[]'::jsonb),
         NULLIF(current_setting('shadoucmdb.request_id', true), '')
  FROM cmdb.workflow_instances wi
  JOIN cmdb.workflow_definitions d ON d.id = wi.definition_id
  JOIN cmdb.workflow_versions v ON v.id = wi.version_id
  JOIN cmdb.workflow_states s ON s.id = wi.current_state_id
  JOIN cmdb.ci_classes c ON c.id = OLD.class_id
  WHERE wi.id = ANY (ids);
  PERFORM set_config('shadoucmdb.workflow_archive', 'on', true);
  IF request_ids IS NOT NULL THEN
    DELETE FROM cmdb.workflow_approval_decisions WHERE request_id = ANY (request_ids);
    DELETE FROM cmdb.workflow_approval_eligibility WHERE request_id = ANY (request_ids);
    DELETE FROM cmdb.workflow_approval_request_steps WHERE request_id = ANY (request_ids);
    DELETE FROM cmdb.workflow_approval_requests WHERE id = ANY (request_ids);
  END IF;
  DELETE FROM cmdb.workflow_instance_events WHERE instance_id = ANY (ids);
  PERFORM set_config('shadoucmdb.workflow_archive', '', true);
  DELETE FROM cmdb.workflow_instances WHERE id = ANY (ids);
  RETURN OLD;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Three-role install: 0008's default privileges give the API role DML on the
-- new tables; on the decisions it keeps SELECT and INSERT only, like the events.
-- ---------------------------------------------------------------------------
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = app_role) AND current_user <> app_role THEN
    EXECUTE format('REVOKE UPDATE, DELETE, TRUNCATE ON cmdb.workflow_approval_decisions FROM %I', app_role);
  END IF;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- audit_log: the approval actions (keeps every action up to 0046). Re-added
-- NOT VALID: no scan here; 0052 validates in its own transaction.
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
  'workflow.approval_request', 'workflow.approval_decide', 'workflow.approval_close', 'workflow.approval_overdue'
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
                 'workflow.approval_overdue')
      AND old_value IS NULL AND new_value IS NOT NULL)
  -- Workflow steps that change a CI's state: before and after.
  OR (action IN ('workflow.transition', 'workflow.migrate', 'workflow.force')
      AND old_value IS NOT NULL AND new_value IS NOT NULL)
) NOT VALID;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- prune_audit_log(): the approval actions join the `changes` scope (they are
-- CI history). The approval tables are never touched. Same body as 0046
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
    WHEN 'changes' THEN ARRAY['create', 'update', 'delete', 'restore', 'schema_change.refused', 'import.commit', 'export',
                              'workflow.publish', 'workflow.start', 'workflow.cancel',
                              'workflow.transition', 'workflow.migrate', 'workflow.force',
                              'workflow.approval_request', 'workflow.approval_decide', 'workflow.approval_close',
                              'workflow.approval_overdue']
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
