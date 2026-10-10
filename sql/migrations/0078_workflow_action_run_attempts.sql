-- Workflow actions: a run's fan-out gives up (GH#846, SHAA-2881).
--
-- Deliveries count their attempts and die after WORKFLOW_ACTIONS_MAX_ATTEMPTS
-- or WORKFLOW_ACTIONS_MAX_AGE_HOURS; runs did not. A run whose fan-out always
-- failed went back to `pending` each time its lease ran out and was claimed
-- again, every minute, for ever, without an audit entry.
--
-- `attempts` counts the claims of a run. The workers' housekeeping now
-- cancels a run whose lease ran out on its last attempt (`fan_out_failed`) and
-- a run still waiting past the maximum age (`expired`), with a
-- `workflow.action_dead` audit entry (actor system). Existing runs start at 0.
-- The lease index from 0075 serves both the lease scan and the backlog count.

ALTER TABLE cmdb.workflow_action_runs
  ADD COLUMN attempts smallint NOT NULL DEFAULT 0 CHECK (attempts >= 0);
