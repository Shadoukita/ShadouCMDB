-- Workflow actions: the loop breakers of the enqueue trigger (v0.4.0 design
-- SHAA-2725 §4.6 and §7.4, slice S3).
--
-- A run is still written for every matching action, never dropped, but as
-- `suppressed` with a reason when:
--
--   queue_full     the workers flagged the queue overloaded (unchanged, 0073);
--   echo_loop      the event is a transition caused by one of our own
--                  deliveries (X-ShadouCMDB-Cause, caused_by_delivery_id) and so
--                  were the 9 transitions of the instance before it: a
--                  receiver that transitions the CI back on every update;
--   instance_rate  the instance already had runs for
--                  WORKFLOW_ACTIONS_MAX_PER_INSTANCE_PER_HOUR events in the
--                  last hour (default 50), counted per minute in
--                  workflow_action_instance_rate: the check reads at most 60
--                  rows whatever the limit, also while a loop is hot.
--
-- The trigger cannot read the server's environment, so the per-instance limit
-- is a column of the queue state row, which the workers write with the
-- overload flag on every tick. The workers audit suppressed runs
-- (`workflow.action_suppressed`, at most once a minute per definition and
-- reason) and mark them done with `completed_at`; the partial index finds the
-- ones still to audit.

ALTER TABLE cmdb.workflow_action_queue_state
  ADD COLUMN max_per_instance_per_hour integer NOT NULL DEFAULT 50
    CHECK (max_per_instance_per_hour BETWEEN 1 AND 10000);
--> statement-breakpoint

-- Events that enqueued runs, per instance and minute; the workers delete rows
-- older than an hour.
CREATE TABLE cmdb.workflow_action_instance_rate (
  instance_id uuid NOT NULL REFERENCES cmdb.workflow_instances (id) ON DELETE CASCADE,
  minute      timestamptz NOT NULL,
  events      integer NOT NULL CHECK (events > 0),
  PRIMARY KEY (instance_id, minute)
);
--> statement-breakpoint

-- The workers' housekeeping: leases that ran out, and retention of finished deliveries.
CREATE INDEX workflow_action_runs_lease_idx ON cmdb.workflow_action_runs (lease_until) WHERE status = 'fanning_out';
--> statement-breakpoint
CREATE INDEX workflow_action_deliveries_lease_idx ON cmdb.workflow_action_deliveries (lease_until)
  WHERE status = 'sending';
--> statement-breakpoint
CREATE INDEX workflow_action_deliveries_done_idx ON cmdb.workflow_action_deliveries (created_at)
  WHERE status IN ('delivered', 'skipped');
--> statement-breakpoint
-- Pending deliveries past WORKFLOW_ACTIONS_MAX_AGE_HOURS.
CREATE INDEX workflow_action_deliveries_pending_age_idx ON cmdb.workflow_action_deliveries (created_at)
  WHERE status IN ('pending', 'held');
--> statement-breakpoint
CREATE INDEX workflow_action_runs_suppressed_idx ON cmdb.workflow_action_runs (definition_id, status_reason)
  WHERE status = 'suppressed' AND completed_at IS NULL;
--> statement-breakpoint

-- Same as 0073 up to the matching actions; then the status of their runs.
CREATE OR REPLACE FUNCTION cmdb.workflow_actions_enqueue() RETURNS trigger
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
  per_hour integer;
  matched uuid[];
  run_status text := 'pending';
  reason text;
  recent integer;
  echoed integer;
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

  SELECT array_agg(a.id) INTO matched
    FROM cmdb.workflow_actions a
   WHERE a.definition_id = definition AND a.enabled AND a.trigger = ANY (triggers)
     AND CASE WHEN a.trigger IN ('instance_cancelled', 'instance_forced') THEN a.transition_key IS NULL
              ELSE a.transition_key = tkey END
     AND (a.trigger <> 'approval_closed' OR jsonb_typeof(a.settings -> 'statuses') IS DISTINCT FROM 'array'
          OR (a.settings -> 'statuses') ? closed_status);
  IF matched IS NULL THEN
    RETURN NULL;
  END IF;

  SELECT coalesce(bool_or(q.overloaded), false), coalesce(min(q.max_per_instance_per_hour), 50)
    INTO overloaded, per_hour FROM cmdb.workflow_action_queue_state q;
  IF overloaded THEN
    run_status := 'suppressed';
    reason := 'queue_full';
  END IF;
  -- Echo loop: this transition and the 9 before it all caused by deliveries.
  IF reason IS NULL AND NEW.kind = 'transition' AND NEW.caused_by_delivery_id IS NOT NULL THEN
    SELECT count(*), count(e.caused_by_delivery_id) INTO recent, echoed
      FROM (SELECT ev.caused_by_delivery_id FROM cmdb.workflow_instance_events ev
             WHERE ev.instance_id = NEW.instance_id AND ev.kind = 'transition'
             ORDER BY ev.id DESC LIMIT 10) e;
    IF recent = 10 AND echoed = 10 THEN
      run_status := 'suppressed';
      reason := 'echo_loop';
    END IF;
  END IF;
  -- Per instance: the events that enqueued in this minute and the 59 before it.
  IF reason IS NULL THEN
    SELECT coalesce(sum(r.events), 0) INTO recent FROM cmdb.workflow_action_instance_rate r
     WHERE r.instance_id = NEW.instance_id AND r.minute > date_trunc('minute', now()) - interval '1 hour';
    IF recent >= per_hour THEN
      run_status := 'suppressed';
      reason := 'instance_rate';
    END IF;
  END IF;
  INSERT INTO cmdb.workflow_action_instance_rate AS r (instance_id, minute, events)
  VALUES (NEW.instance_id, date_trunc('minute', now()), 1)
  ON CONFLICT (instance_id, minute) DO UPDATE SET events = r.events + 1;

  INSERT INTO cmdb.workflow_action_runs
    (event_id, action_id, action_key, kind, definition_id, instance_id, ci_id, http_request_id, status, status_reason)
  SELECT NEW.id, a.id, a.key, a.kind, a.definition_id, NEW.instance_id, ci, NEW.request_id, run_status, reason
    FROM cmdb.workflow_actions a
   WHERE a.id = ANY (matched)
  ON CONFLICT (event_id, action_id) DO NOTHING;
  RETURN NULL;
END;
$$;
