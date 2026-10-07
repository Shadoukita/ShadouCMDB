-- GH#664 (SHAA-2108): a requester could choose the approver of a step whose
-- approvers come from a CI field (`ci_attribute`) by setting that field to a
-- colleague just before requesting. Every step's approvers are now resolved
-- when the request is made, and an approver source is dropped when the
-- field's current value was set by someone excluded from the request (or by
-- an API token or import that recorded no user). The dropped sources are kept
-- on the step with the reason and the audit change that set the field, so
-- the request shows why the field's approvers may not decide it.
--
-- Each element: {source, label, reason, message, fieldLastChanged: {actorType,
-- actorId, actorName, changedAt}}. Existing steps had nothing dropped.

ALTER TABLE cmdb.workflow_approval_request_steps
  ADD COLUMN IF NOT EXISTS dropped_sources jsonb NOT NULL DEFAULT '[]'::jsonb
    CONSTRAINT workflow_approval_request_steps_dropped_sources CHECK (jsonb_typeof(dropped_sources) = 'array');
