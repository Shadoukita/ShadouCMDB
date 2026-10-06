-- The approvals list's `view=decided` (SHAA-1880, slice A3b): the requests a
-- user approved or rejected a step of, in person or for someone. The unique
-- indexes of 0051 lead with request_id, so they cannot find a user's
-- decisions. Decisions are append-only and kept for the life of the CI, so
-- this table only grows.

CREATE INDEX IF NOT EXISTS workflow_approval_decisions_actor_idx
  ON cmdb.workflow_approval_decisions (actor_id, request_id);
--> statement-breakpoint
CREATE INDEX IF NOT EXISTS workflow_approval_decisions_on_behalf_idx
  ON cmdb.workflow_approval_decisions (on_behalf_of_id, request_id) WHERE on_behalf_of_id IS NOT NULL;
