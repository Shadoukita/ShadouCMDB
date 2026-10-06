-- The `_start` grant (GH#666, SHAA-2110): starting a workflow again on a CI
-- where an instance of it completed or was cancelled sets the state field back
-- to the initial state, so it needs `workflows.manage` or this grant, as
-- cancelling needs `_cancel`. The grants table is small (definitions times
-- transitions times profiles), so the CHECK is re-added and validated here.

ALTER TABLE cmdb.workflow_transition_grants DROP CONSTRAINT workflow_transition_grants_transition_key_check;
--> statement-breakpoint
ALTER TABLE cmdb.workflow_transition_grants ADD CONSTRAINT workflow_transition_grants_transition_key_check
  CHECK (transition_key ~ '^(_cancel|_start|[a-z][a-z0-9_]{0,62})$');
