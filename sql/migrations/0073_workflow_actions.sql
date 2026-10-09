-- Workflow actions and notifications (v0.4.0 design SHAA-2725, slice S1).
--
-- Design-time: attribute actions in the immutable version graph
-- (workflow_transition_set_attributes); notification actions on the definition
-- (workflow_actions, workflow_action_recipients), keyed by transition like
-- grants and approvers; webhook endpoints and the administrator's allowlist.
--
-- Run-time: a transactional outbox. Every engine path writes a
-- workflow_instance_events row in the transaction that changes the instance;
-- a trigger on that table enqueues one run per matching notification action
-- in the same transaction, so a rolled-back transition enqueues nothing and
-- nothing external happens inside it. Workers (a later slice) fan runs out
-- into deliveries and send them after commit.
--
-- Additive: no row is rewritten. Events written before this migration never
-- fire an action (the trigger only sees new inserts; nothing is backfilled).
-- Restore and factory reset are generic over cmdb tables; `restore` cancels
-- what was in flight and suspends every endpoint after loading (maintenance/restore.rs).

-- ---------------------------------------------------------------------------
-- Global right: the full list of 0046 plus webhooks.manage. The Administrator
-- profile holds it implicitly; no other profile is given it.
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
    'workflows.manage',
    'webhooks.manage'));
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Users: the language of what the server writes to them (e-mail). NULL until
-- the web UI sets it; MAIL_DEFAULT_LOCALE applies then. Metadata-only.
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.users
  ADD COLUMN locale text CONSTRAINT users_locale_valid CHECK (locale IS NULL OR locale IN ('en', 'de'));
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Attribute actions: part of the version graph, immutable once published.
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.workflow_transition_set_attributes (
  transition_id uuid NOT NULL REFERENCES cmdb.workflow_transitions (id) ON DELETE CASCADE,
  position      smallint NOT NULL CHECK (position BETWEEN 1 AND 20),
  attribute_id  uuid NOT NULL REFERENCES cmdb.ci_attribute_definitions (id) ON DELETE RESTRICT,
  value_from    text NOT NULL CHECK (value_from IN ('literal', 'now', 'today', 'actor', 'clear')),
  -- Literal only; validated at publish and again at apply time.
  value         jsonb CHECK (value IS NULL OR pg_column_size(value) <= 8192),
  PRIMARY KEY (transition_id, position),
  CONSTRAINT workflow_transition_set_attributes_attr_uq UNIQUE (transition_id, attribute_id),
  CONSTRAINT workflow_transition_set_attributes_literal CHECK ((value_from = 'literal') = (value IS NOT NULL))
);
--> statement-breakpoint
-- The RESTRICT check when a field is deleted.
CREATE INDEX workflow_transition_set_attributes_attr_idx ON cmdb.workflow_transition_set_attributes (attribute_id);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Webhooks: the administrator's allowlist (inside the operator's ceiling,
-- WEBHOOK_ALLOWED_HOSTS) and the endpoints. Empty allowlist: no endpoint can
-- be saved.
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.webhook_allowed_hosts (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  -- An exact host (IDNA form), `*.` and a domain (one label or more, never a
  -- bare `*`), or an IP literal.
  host_pattern    text NOT NULL CHECK (host_pattern ~ '^(\*\.)?[a-z0-9.-]{1,253}$|^[0-9a-f:.]{2,45}$'),
  -- NULL: the scheme's default port.
  port            integer CHECK (port BETWEEN 1 AND 65535),
  allow_http      boolean NOT NULL DEFAULT false,
  comment         text CHECK (comment IS NULL OR length(comment) <= 500),
  created_at      timestamptz NOT NULL DEFAULT now(),
  created_by_name text NOT NULL
);
--> statement-breakpoint
-- One entry per host and port; NULL counts as one port value.
CREATE UNIQUE INDEX webhook_allowed_hosts_uq ON cmdb.webhook_allowed_hosts (host_pattern, coalesce(port, 0));
--> statement-breakpoint

CREATE TABLE cmdb.webhook_endpoints (
  id                         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  key                        text NOT NULL UNIQUE CHECK (key ~ '^[a-z][a-z0-9_-]{0,62}$'),
  name                       text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
  url                        text NOT NULL CHECK (length(url) <= 2048 AND url ~ '^https?://'),
  status                     text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'paused', 'suspended')),
  -- breaker, host_not_allowed, restored, secret_required, ...
  suspended_reason           text CHECK (suspended_reason IS NULL OR length(suspended_reason) <= 100),
  payload_version            smallint NOT NULL DEFAULT 1 CHECK (payload_version >= 1),
  timeout_ms                 integer NOT NULL DEFAULT 10000 CHECK (timeout_ms BETWEEN 1000 AND 30000),
  max_per_minute             integer NOT NULL DEFAULT 120 CHECK (max_per_minute BETWEEN 1 AND 6000),
  max_in_flight              smallint NOT NULL DEFAULT 2 CHECK (max_in_flight BETWEEN 1 AND 16),
  -- Sealed like the identity providers' secrets (secrets/sealed.rs): ciphertext
  -- and the id of the key that sealed it. Never readable through the API.
  secret_ciphertext          bytea NOT NULL,
  secret_key_id              bytea NOT NULL,
  -- The secret before the last rotation, valid until previous_secret_until.
  previous_secret_ciphertext bytea,
  previous_secret_key_id     bytea,
  previous_secret_until      timestamptz,
  -- An optional static header for the receiver (for example Authorization),
  -- its value sealed the same way.
  auth_header_name           text CHECK (auth_header_name IS NULL OR auth_header_name ~ '^[A-Za-z0-9-]{1,64}$'),
  auth_header_ciphertext     bytea,
  auth_header_key_id         bytea,
  consecutive_failures       integer NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
  last_success_at            timestamptz,
  last_failure_at            timestamptz,
  created_at                 timestamptz NOT NULL DEFAULT now(),
  updated_at                 timestamptz NOT NULL DEFAULT now(),
  -- Optimistic concurrency.
  version                    integer NOT NULL DEFAULT 1 CHECK (version >= 1),
  CONSTRAINT webhook_endpoints_prev_secret CHECK (
    (previous_secret_ciphertext IS NULL) = (previous_secret_until IS NULL)
    AND (previous_secret_ciphertext IS NULL) = (previous_secret_key_id IS NULL)),
  CONSTRAINT webhook_endpoints_auth_header CHECK (
    (auth_header_name IS NULL) = (auth_header_ciphertext IS NULL)
    AND (auth_header_ciphertext IS NULL) = (auth_header_key_id IS NULL)),
  CONSTRAINT webhook_endpoints_suspended CHECK ((status = 'suspended') = (suspended_reason IS NOT NULL))
);
--> statement-breakpoint
CREATE TRIGGER webhook_endpoints_set_updated_at BEFORE UPDATE ON cmdb.webhook_endpoints
  FOR EACH ROW EXECUTE FUNCTION cmdb.set_updated_at();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Notification actions: on the definition, mutable and audited, no republish.
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.workflow_actions (
  id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  definition_id  uuid NOT NULL REFERENCES cmdb.workflow_definitions (id) ON DELETE CASCADE,
  key            text NOT NULL CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
  name           text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
  kind           text NOT NULL CHECK (kind IN ('email', 'webhook', 'inbox')),
  trigger        text NOT NULL CHECK (trigger IN ('transition', 'approval_requested', 'approval_step', 'approval_closed',
                                                  'approval_overdue', 'instance_cancelled', 'instance_forced')),
  -- The transition it fires on (the request's transition for approval
  -- triggers); NULL for the instance triggers. A key the current version
  -- lacks is kept for instances pinned to an older one.
  transition_key text CHECK (transition_key IS NULL OR transition_key ~ '^[a-z][a-z0-9_]{0,62}$'),
  enabled        boolean NOT NULL DEFAULT true,
  position       smallint NOT NULL DEFAULT 1 CHECK (position BETWEEN 1 AND 1000),
  endpoint_id    uuid REFERENCES cmdb.webhook_endpoints (id) ON DELETE RESTRICT,
  -- Kind-specific, validated by the API against a schema per kind: content
  -- level, subject and intro per locale, excludeActor, `statuses` (the
  -- approval_closed filter, read by the enqueue trigger), the overdue reason,
  -- includeAttributes. Never secrets, never a user's address.
  settings       jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(settings) = 'object' AND pg_column_size(settings) <= 16384),
  CONSTRAINT workflow_actions_key_uq UNIQUE (definition_id, key),
  CONSTRAINT workflow_actions_endpoint CHECK ((kind = 'webhook') = (endpoint_id IS NOT NULL)),
  CONSTRAINT workflow_actions_transition CHECK (
    (trigger IN ('instance_cancelled', 'instance_forced')) = (transition_key IS NULL))
);
--> statement-breakpoint
-- The enqueue trigger's lookup.
CREATE INDEX workflow_actions_match_idx ON cmdb.workflow_actions (definition_id, trigger, transition_key) WHERE enabled;
--> statement-breakpoint
-- The RESTRICT check when an endpoint is deleted.
CREATE INDEX workflow_actions_endpoint_idx ON cmdb.workflow_actions (endpoint_id) WHERE endpoint_id IS NOT NULL;
--> statement-breakpoint

-- At most 10 actions per (definition, trigger, transition). The definition row
-- is locked first, so two concurrent saves cannot both pass the count.
CREATE FUNCTION cmdb.workflow_actions_limit() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
DECLARE
  n integer;
BEGIN
  PERFORM 1 FROM cmdb.workflow_definitions d WHERE d.id = NEW.definition_id FOR NO KEY UPDATE;
  SELECT count(*) INTO n FROM cmdb.workflow_actions a
   WHERE a.definition_id = NEW.definition_id AND a.trigger = NEW.trigger
     AND a.transition_key IS NOT DISTINCT FROM NEW.transition_key AND a.id <> NEW.id;
  IF n >= 10 THEN
    RAISE EXCEPTION 'workflow_actions: at most 10 actions per trigger and transition (%, %)',
      NEW.trigger, coalesce(NEW.transition_key, '-')
      USING ERRCODE = 'check_violation', CONSTRAINT = 'workflow_actions_per_trigger';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER workflow_actions_limit BEFORE INSERT OR UPDATE OF definition_id, trigger, transition_key
  ON cmdb.workflow_actions FOR EACH ROW EXECUTE FUNCTION cmdb.workflow_actions_limit();
--> statement-breakpoint

-- Who an e-mail or inbox action reaches: exactly one source per row, resolved
-- at send time. Users, groups and profiles cascade, as approver assignments do.
CREATE TABLE cmdb.workflow_action_recipients (
  action_id          uuid NOT NULL REFERENCES cmdb.workflow_actions (id) ON DELETE CASCADE,
  position           smallint NOT NULL CHECK (position BETWEEN 1 AND 20),
  source             text NOT NULL CHECK (source IN ('profile', 'group', 'user', 'ci_owner', 'ci_attribute',
                                                     'service_owner', 'participant', 'address')),
  profile_id         uuid REFERENCES cmdb.permission_profiles (id) ON DELETE CASCADE,
  group_id           uuid REFERENCES cmdb.user_groups (id) ON DELETE CASCADE,
  user_id            uuid REFERENCES cmdb.users (id) ON DELETE CASCADE,
  -- A reference attribute to Person of the class or an ancestor.
  attribute_id       uuid REFERENCES cmdb.ci_attribute_definitions (id) ON DELETE RESTRICT,
  service_owner_role text CHECK (service_owner_role IN ('technical', 'business')),
  participant        text CHECK (participant IN ('actor', 'starter', 'requester', 'approvers')),
  -- A fixed address (a distribution list): configuration, not a user's data.
  address            text CHECK (address IS NULL OR (length(address) <= 254 AND address ~ '^[^@\s]+@[^@\s]+$')),
  PRIMARY KEY (action_id, position),
  CONSTRAINT workflow_action_recipients_one_source CHECK (
    num_nonnulls(profile_id, group_id, user_id, attribute_id, service_owner_role, participant, address) <= 1
    AND (source = 'ci_owner')
        = (num_nonnulls(profile_id, group_id, user_id, attribute_id, service_owner_role, participant, address) = 0)
    AND (source = 'profile') = (profile_id IS NOT NULL)
    AND (source = 'group') = (group_id IS NOT NULL)
    AND (source = 'user') = (user_id IS NOT NULL)
    AND (source = 'ci_attribute') = (attribute_id IS NOT NULL)
    AND (source = 'service_owner') = (service_owner_role IS NOT NULL)
    AND (source = 'participant') = (participant IS NOT NULL)
    AND (source = 'address') = (address IS NOT NULL))
);
--> statement-breakpoint
-- The cascades and the RESTRICT check.
CREATE INDEX workflow_action_recipients_profile_idx ON cmdb.workflow_action_recipients (profile_id) WHERE profile_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX workflow_action_recipients_group_idx ON cmdb.workflow_action_recipients (group_id) WHERE group_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX workflow_action_recipients_user_idx ON cmdb.workflow_action_recipients (user_id) WHERE user_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX workflow_action_recipients_attr_idx ON cmdb.workflow_action_recipients (attribute_id) WHERE attribute_id IS NOT NULL;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- The outbox. Operational data, not the record: the events and the audit log
-- are. Runs and deliveries are pruned after WORKFLOW_ACTIONS_RETENTION_DAYS
-- and go with their CI when it is purged.
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.workflow_action_runs (
  id              bigint PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
  -- workflow_instance_events.id. No foreign key: events go to the archive with their CI.
  event_id        bigint NOT NULL,
  action_id       uuid REFERENCES cmdb.workflow_actions (id) ON DELETE SET NULL,
  -- Kept when the action is deleted.
  action_key      text NOT NULL,
  kind            text NOT NULL CHECK (kind IN ('email', 'webhook', 'inbox')),
  definition_id   uuid NOT NULL,
  instance_id     uuid NOT NULL,
  ci_id           uuid NOT NULL,
  -- The event's request id: the runs of one bulk request are fanned out together.
  http_request_id text,
  -- Reserved for a hop counter should actions ever start transitions; always 0.
  depth           smallint NOT NULL DEFAULT 0 CHECK (depth = 0),
  status          text NOT NULL CHECK (status IN ('pending', 'fanning_out', 'fanned_out', 'suppressed', 'cancelled')),
  -- queue_full, instance_rate, echo_loop, restored, ...
  status_reason   text CHECK (status_reason IS NULL OR length(status_reason) <= 100),
  lease_owner     text,
  lease_until     timestamptz,
  created_at      timestamptz NOT NULL DEFAULT now(),
  completed_at    timestamptz,
  CONSTRAINT workflow_action_runs_once UNIQUE (event_id, action_id),
  CONSTRAINT workflow_action_runs_reason CHECK (status NOT IN ('suppressed', 'cancelled') OR status_reason IS NOT NULL),
  CONSTRAINT workflow_action_runs_lease CHECK ((status = 'fanning_out') = (lease_until IS NOT NULL))
);
--> statement-breakpoint
CREATE INDEX workflow_action_runs_ready_idx ON cmdb.workflow_action_runs (created_at, id) WHERE status = 'pending';
--> statement-breakpoint
-- The per-instance limit and the instance's run list.
CREATE INDEX workflow_action_runs_instance_idx ON cmdb.workflow_action_runs (instance_id, created_at);
--> statement-breakpoint
-- A CI purge.
CREATE INDEX workflow_action_runs_ci_idx ON cmdb.workflow_action_runs (ci_id);
--> statement-breakpoint
-- Retention.
CREATE INDEX workflow_action_runs_created_idx ON cmdb.workflow_action_runs (created_at);
--> statement-breakpoint

CREATE TABLE cmdb.workflow_action_deliveries (
  -- Sent as the idempotency key (Idempotency-Key, X-ShadouCMDB-Delivery, Message-ID).
  id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  run_id           bigint NOT NULL REFERENCES cmdb.workflow_action_runs (id) ON DELETE CASCADE,
  -- user:<uuid> | addr:<lower(address)> | endpoint:<uuid>. A user's address is
  -- not stored: it is read from users.email at send time.
  recipient_key    text NOT NULL CHECK (length(recipient_key) BETWEEN 1 AND 300),
  user_id          uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  endpoint_id      uuid REFERENCES cmdb.webhook_endpoints (id) ON DELETE SET NULL,
  status           text NOT NULL CHECK (status IN ('pending', 'sending', 'held', 'delivered', 'skipped', 'dead')),
  -- no_view, inactive, no_email, throttled_digest, mail_off, expired, restored, ...
  status_reason    text CHECK (status_reason IS NULL OR length(status_reason) <= 200),
  attempts         smallint NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  next_attempt_at  timestamptz NOT NULL DEFAULT now(),
  lease_owner      text,
  lease_until      timestamptz,
  -- Fencing: an outcome is written only by the worker holding this epoch.
  lease_epoch      integer NOT NULL DEFAULT 0,
  last_status_code integer,
  last_error       text CHECK (last_error IS NULL OR length(last_error) <= 1024),
  -- Throttled into this digest delivery.
  digest_id        uuid,
  created_at       timestamptz NOT NULL DEFAULT now(),
  completed_at     timestamptz,
  CONSTRAINT workflow_action_deliveries_once UNIQUE (run_id, recipient_key),
  CONSTRAINT workflow_action_deliveries_sending CHECK ((status = 'sending') = (lease_until IS NOT NULL)),
  CONSTRAINT workflow_action_deliveries_reason CHECK (status NOT IN ('skipped', 'dead') OR status_reason IS NOT NULL)
);
--> statement-breakpoint
CREATE INDEX workflow_action_deliveries_ready_idx ON cmdb.workflow_action_deliveries (next_attempt_at, id) WHERE status = 'pending';
--> statement-breakpoint
CREATE INDEX workflow_action_deliveries_inflight_idx ON cmdb.workflow_action_deliveries (endpoint_id) WHERE status = 'sending';
--> statement-breakpoint
CREATE INDEX workflow_action_deliveries_dead_idx ON cmdb.workflow_action_deliveries (created_at) WHERE status = 'dead';
--> statement-breakpoint
CREATE INDEX workflow_action_deliveries_user_idx ON cmdb.workflow_action_deliveries (user_id, created_at) WHERE user_id IS NOT NULL;
--> statement-breakpoint
-- The endpoint's held deliveries and the SET NULL when it is deleted.
CREATE INDEX workflow_action_deliveries_endpoint_idx ON cmdb.workflow_action_deliveries (endpoint_id, status) WHERE endpoint_id IS NOT NULL;
--> statement-breakpoint

-- Per-recipient, per-endpoint and digest windows, shared by every API process.
CREATE TABLE cmdb.workflow_action_rate_windows (
  -- mail:user:<uuid> | endpoint:<uuid> | ...
  scope        text NOT NULL CHECK (length(scope) BETWEEN 1 AND 300),
  window_start timestamptz NOT NULL,
  count        integer NOT NULL CHECK (count >= 0),
  PRIMARY KEY (scope, window_start)
);
--> statement-breakpoint

-- One row: the overload flag the enqueue trigger reads, refreshed by the
-- workers, so the hot path never counts the queue.
CREATE TABLE cmdb.workflow_action_queue_state (
  id         boolean PRIMARY KEY DEFAULT true CHECK (id),
  overloaded boolean NOT NULL DEFAULT false,
  backlog    integer NOT NULL DEFAULT 0 CHECK (backlog >= 0),
  checked_at timestamptz NOT NULL DEFAULT now()
);
--> statement-breakpoint
INSERT INTO cmdb.workflow_action_queue_state (id) VALUES (true);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Instance events: the delivery a transition request echoed back
-- (X-ShadouCMDB-Cause), for the echo-loop breaker. Nullable without a
-- default: metadata-only, no scan of the table.
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.workflow_instance_events ADD COLUMN caused_by_delivery_id uuid;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Enqueue. A deferred constraint trigger: it runs at commit, still inside the
-- event's transaction (a rollback takes its runs with it), and so reads the
-- approval request as the transaction leaves it. An approval decision's event
-- is written before the next step is activated or the request is rejected,
-- and a request closes after its last event; a plain AFTER INSERT trigger
-- would not see either. Events of kinds no trigger maps are skipped by WHEN.
--
--   transition         -> transition (key: the event's transition)
--   approval_request   -> approval_requested, approval_step (step 1)
--   approval_decision  -> approval_step when it activated the next step
--   approval_overdue   -> approval_overdue (the reason filter is applied at fan-out)
--   cancel / force     -> instance_cancelled / instance_forced
--   the request's last event, once it is closed (approval_close,
--   approval_withdraw, a rejecting decision, or the transition of a final
--   approval) -> approval_closed, with the action's `statuses` filter
--
-- Approval triggers match on the request's transition key. The run holds ids
-- only: no recipient, no CI data, no payload. While the queue is flagged
-- overloaded, runs are written `suppressed` / `queue_full`, never dropped.
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.workflow_actions_enqueue() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
DECLARE
  definition uuid;
  ci uuid;
  request_key text;
  request_status text;
  triggers text[] := '{}';
  tkey text;
  closed_status text;
  overloaded boolean;
BEGIN
  SELECT wi.definition_id, wi.ci_id INTO definition, ci FROM cmdb.workflow_instances wi WHERE wi.id = NEW.instance_id;
  -- The common case: the workflow has no enabled action.
  IF definition IS NULL OR NOT EXISTS (SELECT 1 FROM cmdb.workflow_actions a
                                        WHERE a.definition_id = definition AND a.enabled) THEN
    RETURN NULL;
  END IF;

  IF NEW.approval_request_id IS NOT NULL THEN
    SELECT r.transition_key, r.status INTO request_key, request_status FROM cmdb.workflow_approval_requests r
     WHERE r.id = NEW.approval_request_id;
    IF request_status <> 'pending'
       AND NOT EXISTS (SELECT 1 FROM cmdb.workflow_instance_events e
                        WHERE e.instance_id = NEW.instance_id AND e.id > NEW.id
                          AND e.approval_request_id = NEW.approval_request_id) THEN
      triggers := triggers || 'approval_closed'::text;
      closed_status := request_status;
    END IF;
  END IF;

  CASE NEW.kind
    WHEN 'transition' THEN
      triggers := triggers || 'transition'::text;
    WHEN 'approval_request' THEN
      triggers := triggers || ARRAY['approval_requested', 'approval_step'];
    WHEN 'approval_decision' THEN
      IF EXISTS (SELECT 1 FROM cmdb.workflow_approval_request_steps st
                  WHERE st.request_id = NEW.approval_request_id AND st.step_no = NEW.approval_step_no + 1
                    AND st.activated_at IS NOT NULL) THEN
        triggers := triggers || 'approval_step'::text;
      END IF;
    WHEN 'approval_overdue' THEN
      triggers := triggers || 'approval_overdue'::text;
    WHEN 'cancel' THEN
      triggers := triggers || 'instance_cancelled'::text;
    WHEN 'force' THEN
      triggers := triggers || 'instance_forced'::text;
    ELSE
      NULL;
  END CASE;
  IF cardinality(triggers) = 0 THEN
    RETURN NULL;
  END IF;
  tkey := coalesce(NEW.transition_key, request_key);

  SELECT coalesce(bool_or(q.overloaded), false) INTO overloaded FROM cmdb.workflow_action_queue_state q;
  INSERT INTO cmdb.workflow_action_runs
    (event_id, action_id, action_key, kind, definition_id, instance_id, ci_id, http_request_id, status, status_reason)
  SELECT NEW.id, a.id, a.key, a.kind, a.definition_id, NEW.instance_id, ci, NEW.request_id,
         CASE WHEN overloaded THEN 'suppressed' ELSE 'pending' END,
         CASE WHEN overloaded THEN 'queue_full' END
    FROM cmdb.workflow_actions a
   WHERE a.definition_id = definition AND a.enabled AND a.trigger = ANY (triggers)
     AND CASE WHEN a.trigger IN ('instance_cancelled', 'instance_forced') THEN a.transition_key IS NULL
              ELSE a.transition_key = tkey END
     AND (a.trigger <> 'approval_closed' OR jsonb_typeof(a.settings -> 'statuses') IS DISTINCT FROM 'array'
          OR (a.settings -> 'statuses') ? closed_status)
  ON CONFLICT (event_id, action_id) DO NOTHING;
  RETURN NULL;
END;
$$;
--> statement-breakpoint
CREATE CONSTRAINT TRIGGER workflow_instance_events_enqueue_actions
  AFTER INSERT ON cmdb.workflow_instance_events
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW WHEN (NEW.kind IN ('transition', 'approval_request', 'approval_decision', 'approval_withdraw',
                                  'approval_close', 'approval_overdue', 'cancel', 'force'))
  EXECUTE FUNCTION cmdb.workflow_actions_enqueue();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- In-app notifications: a configured inbox action, and the notice to
-- webhooks.manage holders that an endpoint was suspended. Small table:
-- re-added and validated in place.
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.notifications DROP CONSTRAINT notifications_kind_check;
--> statement-breakpoint
ALTER TABLE cmdb.notifications ADD CONSTRAINT notifications_kind_check CHECK (kind IN (
  'approval_requested', 'approval_closed', 'workflow_transition', 'import_finished',
  'workflow_action', 'webhook_suspended'));
--> statement-breakpoint
ALTER TABLE cmdb.notifications DROP CONSTRAINT notifications_entity_type_check;
--> statement-breakpoint
ALTER TABLE cmdb.notifications ADD CONSTRAINT notifications_entity_type_check CHECK (entity_type IN (
  'workflow_approval_requests', 'workflow_instances', 'import_jobs', 'webhook_endpoints'));
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Immutability. A published version's attribute actions are as immutable as
-- its transition fields: the graph guard resolves their version through
-- transition_id. Same body as 0051 otherwise.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION cmdb.workflow_graph_guard() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
DECLARE
  rows_version uuid[];
  v uuid;
  st text;
BEGIN
  IF TG_TABLE_NAME IN ('workflow_transition_fields', 'workflow_transition_approval_steps',
                       'workflow_transition_set_attributes') THEN
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
CREATE TRIGGER workflow_transition_set_attributes_guard
  BEFORE INSERT OR UPDATE OR DELETE ON cmdb.workflow_transition_set_attributes
  FOR EACH ROW EXECUTE FUNCTION cmdb.workflow_graph_guard();
--> statement-breakpoint
CREATE TRIGGER workflow_transition_set_attributes_no_truncate BEFORE TRUNCATE ON cmdb.workflow_transition_set_attributes
  FOR EACH STATEMENT EXECUTE FUNCTION cmdb.workflow_no_truncate();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Before a CI row goes: its action runs and their deliveries are deleted
-- (operational data, no archive copy), then its instances are archived as in
-- 0051, the events with the delivery that caused them. CREATE OR REPLACE
-- keeps the owner, the trigger and the revoked EXECUTE.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION cmdb.configuration_items_archive_workflows() RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
  ids uuid[];
  request_ids uuid[];
BEGIN
  DELETE FROM cmdb.workflow_action_runs WHERE ci_id = OLD.id;
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
                     'approvalStepNo', e.approval_step_no, 'onBehalfOfName', e.on_behalf_of_name,
                     'causedByDeliveryId', e.caused_by_delivery_id) ORDER BY e.id)
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
-- audit_log: the new actions (keeps every action up to 0056). Re-added NOT
-- VALID; 0074 validates them in its own transaction (see sql/README.md).
-- entity_type has no database check: webhook_endpoints, webhook_allowed_hosts
-- and workflow_action_deliveries are Rust EntityType variants only.
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
  'mail.test'
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
                 'workflow.action_suppressed', 'mail.test')
      AND old_value IS NULL AND new_value IS NOT NULL)
  -- Workflow steps that change a CI's state, and an approval request's
  -- approvers re-resolved: before and after.
  OR (action IN ('workflow.transition', 'workflow.migrate', 'workflow.force', 'workflow.approval_refresh')
      AND old_value IS NOT NULL AND new_value IS NOT NULL)
) NOT VALID;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- prune_audit_log(): the new actions join the `changes` scope. Same body as
-- 0060 otherwise; CREATE OR REPLACE keeps the owner and the EXECUTE grants.
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
                              'workflow.action_suppressed', 'mail.test']
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
