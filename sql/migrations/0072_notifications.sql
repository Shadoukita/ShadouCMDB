-- In-app notifications (SHAA-2356): per user, read in the web UI only.
--
-- Nothing here leaves the server; there is no e-mail, webhook or push. The
-- rows are written by triggers in the transaction of the event they report,
-- so every code path that runs a transition, closes an approval request or
-- ends an import notifies, and a rolled-back action notifies no one:
--
--   approval_requested  a step of an approval request becomes active (the
--                       request is made, or the step before reached its
--                       quorum): every active user eligible as an approver of
--                       that step, less the requester and the requesting
--                       token's creator (excluded_user_ids).
--   approval_closed     a pending request closes, except when its requester
--                       withdrew it or closed it otherwise themselves: the
--                       requester.
--   workflow_transition someone runs a transition on, cancels or forces an
--                       instance: the user who started it, unless they did it
--                       themselves or the transition was their own approved
--                       request (approval_closed says so).
--   import_finished     an import job ends completed, completed with errors or
--                       failed: the user who created it.
--
-- Access is checked when read: the API lists a notification about a CI only
-- while the reader may view the CI's class. Rows go with their user (GDPR) and
-- with their CI when it is purged; the API deletes them after
-- NOTIFICATION_RETENTION_DAYS. A restore loads this table with its triggers
-- off, like every table, and the triggers fire for nothing it loads.
CREATE TABLE cmdb.notifications (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id     uuid NOT NULL REFERENCES cmdb.users (id) ON DELETE CASCADE,
  kind        text NOT NULL CHECK (kind IN (
                'approval_requested', 'approval_closed', 'workflow_transition', 'import_finished')),
  -- What to open: an approval request, a workflow instance or an import job.
  -- No foreign key: the request or job may go first (CI purge, import record
  -- retention); the UI then says it is no longer available.
  entity_type text NOT NULL CHECK (entity_type IN ('workflow_approval_requests', 'workflow_instances', 'import_jobs')),
  entity_id   uuid NOT NULL,
  -- The CI it is about, for the class check on read; NULL: not about a CI.
  ci_id       uuid REFERENCES cmdb.configuration_items (id) ON DELETE CASCADE,
  -- Display values as they were at the event (labels, names, states).
  data        jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(data) = 'object'),
  -- One notification per user and event, whatever fires twice.
  dedupe_key  text NOT NULL CHECK (length(dedupe_key) BETWEEN 1 AND 200),
  created_at  timestamptz NOT NULL DEFAULT now(),
  read_at     timestamptz,
  CONSTRAINT notifications_dedupe_uq UNIQUE (user_id, dedupe_key)
);
--> statement-breakpoint
COMMENT ON TABLE cmdb.notifications IS
  'In-app notifications per user (SHAA-2356); written by triggers, read and marked read through the API.';
--> statement-breakpoint
-- The list, newest first.
CREATE INDEX notifications_user_idx ON cmdb.notifications (user_id, created_at DESC, id);
--> statement-breakpoint
-- The bell's unread count.
CREATE INDEX notifications_unread_idx ON cmdb.notifications (user_id) WHERE read_at IS NULL;
--> statement-breakpoint
-- Retention.
CREATE INDEX notifications_created_idx ON cmdb.notifications (created_at);
--> statement-breakpoint
-- The foreign key's cascade on a CI purge.
CREATE INDEX notifications_ci_idx ON cmdb.notifications (ci_id) WHERE ci_id IS NOT NULL;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- approval_requested. A deferred constraint trigger: the step's eligibility
-- rows are written after the request row (and an older request's next step is
-- resolved after it activates), so it runs at commit and reads the request as
-- it is then. A request that closed in the same transaction notifies no one.
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.notify_approval_requested() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
DECLARE
  r record;
BEGIN
  SELECT req.id, req.request_no, req.transition_key, req.excluded_user_ids, req.requested_by_name,
         req.current_step_no, st.step_key, st.due_at, i.ci_id, i.id AS instance_id,
         ci.label AS ci_label, ci.ident AS ci_ident, d.name AS definition_name,
         tr.name AS transition_name, ps.name AS step_name
    INTO r
    FROM cmdb.workflow_approval_requests req
    JOIN cmdb.workflow_approval_request_steps st
      ON st.request_id = req.id AND st.step_no = req.current_step_no AND st.status = 'active'
    JOIN cmdb.workflow_instances i ON i.id = req.instance_id
    JOIN cmdb.configuration_items ci ON ci.id = i.ci_id
    JOIN cmdb.workflow_definitions d ON d.id = i.definition_id
    LEFT JOIN cmdb.workflow_transitions tr ON tr.version_id = req.version_id AND tr.key = req.transition_key
    LEFT JOIN cmdb.workflow_transition_approval_steps ps ON ps.transition_id = tr.id AND ps.step_no = req.current_step_no
   WHERE req.id = NEW.id AND req.status = 'pending';
  IF NOT FOUND THEN
    RETURN NULL;
  END IF;
  INSERT INTO cmdb.notifications (user_id, kind, entity_type, entity_id, ci_id, data, dedupe_key)
  SELECT u.id, 'approval_requested', 'workflow_approval_requests', r.id, r.ci_id,
         jsonb_build_object(
           'instanceId', r.instance_id, 'ciId', r.ci_id, 'ciLabel', r.ci_label, 'ciIdent', r.ci_ident,
           'definitionName', r.definition_name, 'transitionKey', r.transition_key,
           'transitionName', r.transition_name, 'requestNo', r.request_no, 'stepKey', r.step_key,
           'stepName', r.step_name, 'dueAt', r.due_at, 'requestedByName', r.requested_by_name),
         'approval_requested:' || r.id || ':' || r.current_step_no
    FROM cmdb.users u
   WHERE u.is_active AND NOT (u.id = ANY (r.excluded_user_ids))
     AND u.id IN (
       SELECT e.principal_id FROM cmdb.workflow_approval_eligibility e
        WHERE e.request_id = r.id AND e.step_no = r.current_step_no AND e.role = 'approver'
          AND e.principal_kind = 'user'
       UNION
       SELECT m.user_id FROM cmdb.workflow_approval_eligibility e
         JOIN cmdb.user_permission_profiles m ON m.profile_id = e.principal_id
        WHERE e.request_id = r.id AND e.step_no = r.current_step_no AND e.role = 'approver'
          AND e.principal_kind = 'profile'
       UNION
       SELECT m.user_id FROM cmdb.workflow_approval_eligibility e
         JOIN cmdb.user_group_members m ON m.group_id = e.principal_id
        WHERE e.request_id = r.id AND e.step_no = r.current_step_no AND e.role = 'approver'
          AND e.principal_kind = 'group')
  ON CONFLICT (user_id, dedupe_key) DO NOTHING;
  RETURN NULL;
END $$;
--> statement-breakpoint
CREATE CONSTRAINT TRIGGER workflow_approval_requests_notify_new
  AFTER INSERT ON cmdb.workflow_approval_requests
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW EXECUTE FUNCTION cmdb.notify_approval_requested();
--> statement-breakpoint
CREATE CONSTRAINT TRIGGER workflow_approval_requests_notify_step
  AFTER UPDATE OF current_step_no ON cmdb.workflow_approval_requests
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW WHEN (OLD.current_step_no IS DISTINCT FROM NEW.current_step_no)
  EXECUTE FUNCTION cmdb.notify_approval_requested();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- approval_closed.
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.notify_approval_closed() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN
  INSERT INTO cmdb.notifications (user_id, kind, entity_type, entity_id, ci_id, data, dedupe_key)
  SELECT u.id, 'approval_closed', 'workflow_approval_requests', NEW.id, i.ci_id,
         jsonb_build_object(
           'instanceId', i.id, 'ciId', i.ci_id, 'ciLabel', ci.label, 'ciIdent', ci.ident,
           'definitionName', d.name, 'transitionKey', NEW.transition_key, 'transitionName', tr.name,
           'requestNo', NEW.request_no, 'status', NEW.status, 'closeReason', NEW.close_reason,
           'closedByName', NEW.closed_by_name),
         'approval_closed:' || NEW.id
    FROM cmdb.users u
    JOIN cmdb.workflow_instances i ON i.id = NEW.instance_id
    JOIN cmdb.configuration_items ci ON ci.id = i.ci_id
    JOIN cmdb.workflow_definitions d ON d.id = i.definition_id
    LEFT JOIN cmdb.workflow_transitions tr ON tr.version_id = NEW.version_id AND tr.key = NEW.transition_key
   WHERE u.id = NEW.requested_by_id AND u.is_active
  ON CONFLICT (user_id, dedupe_key) DO NOTHING;
  RETURN NULL;
END $$;
--> statement-breakpoint
-- closed_by_name and requested_by_name are both the acting user's username.
-- A manager's cancel is status cancelled, reason withdrawn, and notifies.
CREATE TRIGGER workflow_approval_requests_notify_closed
  AFTER UPDATE OF status ON cmdb.workflow_approval_requests
  FOR EACH ROW WHEN (OLD.status = 'pending' AND NEW.status NOT IN ('pending', 'withdrawn')
                     AND NEW.closed_by_name IS DISTINCT FROM NEW.requested_by_name)
  EXECUTE FUNCTION cmdb.notify_approval_closed();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- workflow_transition.
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.notify_workflow_event() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN
  INSERT INTO cmdb.notifications (user_id, kind, entity_type, entity_id, ci_id, data, dedupe_key)
  SELECT u.id, 'workflow_transition', 'workflow_instances', i.id, i.ci_id,
         jsonb_build_object(
           'instanceId', i.id, 'ciId', i.ci_id, 'ciLabel', ci.label, 'ciIdent', ci.ident,
           'definitionName', d.name, 'event', NEW.kind, 'transitionKey', NEW.transition_key,
           'transitionName', tr.name, 'fromStateKey', NEW.from_state_key, 'fromStateName', fs.name,
           'toStateKey', NEW.to_state_key, 'toStateName', ts.name, 'actorName', NEW.actor_name),
         'workflow_event:' || NEW.id
    FROM cmdb.workflow_instances i
    JOIN cmdb.users u ON u.id = i.started_by_id AND u.is_active
    JOIN cmdb.configuration_items ci ON ci.id = i.ci_id
    JOIN cmdb.workflow_definitions d ON d.id = i.definition_id
    LEFT JOIN cmdb.workflow_transitions tr ON tr.version_id = i.version_id AND tr.key = NEW.transition_key
    LEFT JOIN cmdb.workflow_states fs ON fs.version_id = i.version_id AND fs.key = NEW.from_state_key
    LEFT JOIN cmdb.workflow_states ts ON ts.version_id = i.version_id AND ts.key = NEW.to_state_key
   WHERE i.id = NEW.instance_id
     AND NOT (NEW.actor_type IN ('user', 'api_client') AND NEW.actor_id = i.started_by_id::text)
     AND NOT EXISTS (SELECT 1 FROM cmdb.workflow_approval_requests ar
                      WHERE ar.id = NEW.approval_request_id AND ar.requested_by_id = i.started_by_id)
  ON CONFLICT (user_id, dedupe_key) DO NOTHING;
  RETURN NULL;
END $$;
--> statement-breakpoint
CREATE TRIGGER workflow_instance_events_notify
  AFTER INSERT ON cmdb.workflow_instance_events
  FOR EACH ROW WHEN (NEW.kind IN ('transition', 'cancel', 'force'))
  EXECUTE FUNCTION cmdb.notify_workflow_event();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- import_finished. Cancelled (the user did it) and expired (housekeeping) are
-- left out.
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.notify_import_finished() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN
  INSERT INTO cmdb.notifications (user_id, kind, entity_type, entity_id, data, dedupe_key)
  SELECT u.id, 'import_finished', 'import_jobs', NEW.id,
         jsonb_build_object('fileName', NEW.file_name, 'classKey', NEW.class_key, 'status', NEW.status,
                            'errorCode', NEW.error->>'code'),
         'import_finished:' || NEW.id
    FROM cmdb.users u
   WHERE u.id = NEW.created_by_id AND u.is_active
  ON CONFLICT (user_id, dedupe_key) DO NOTHING;
  RETURN NULL;
END $$;
--> statement-breakpoint
CREATE TRIGGER import_jobs_notify
  AFTER UPDATE OF status ON cmdb.import_jobs
  FOR EACH ROW WHEN (OLD.status IS DISTINCT FROM NEW.status
                     AND NEW.status IN ('completed', 'completed_with_errors', 'failed'))
  EXECUTE FUNCTION cmdb.notify_import_finished();
