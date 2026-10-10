-- Workflow actions: the operations API (v0.4.0 design SHAA-2725 §4.4 and
-- §11.2, slice S3b): the deliveries of a workflow, their retry and discard.
--
-- A manual retry gives a dead or held delivery fresh attempts. Its age for
-- WORKFLOW_ACTIONS_MAX_AGE_HOURS then counts from the retry, not from its
-- creation: otherwise a delivery retried a day after it was queued would be
-- dead as `expired` again by the next housekeeping tick, without one attempt.
-- `created_at` keeps telling when the event queued it (lists, retention).
--
-- A discard is `dead` with status_reason `discarded`: no new status, so the
-- 0073 checks and every index on `status` stay as they are.
--
-- Additive: a nullable column without a default (catalogue only, no rewrite)
-- and two indexes.

ALTER TABLE cmdb.workflow_action_deliveries ADD COLUMN retried_at timestamptz;
--> statement-breakpoint

-- The max-age sweep of the workers, from the retry when there was one (replaces 0075's).
DROP INDEX cmdb.workflow_action_deliveries_pending_age_idx;
--> statement-breakpoint
CREATE INDEX workflow_action_deliveries_pending_age_idx
  ON cmdb.workflow_action_deliveries ((coalesce(retried_at, created_at)))
  WHERE status IN ('pending', 'held');
--> statement-breakpoint

-- The deliveries list and the summary of one workflow start from its runs.
CREATE INDEX workflow_action_runs_definition_idx ON cmdb.workflow_action_runs (definition_id, created_at);
