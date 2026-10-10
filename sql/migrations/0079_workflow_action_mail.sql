-- Workflow actions by e-mail (v0.4.0 design SHAA-2725 §4.6 and §6, slice S4):
-- the lookups of the e-mail fan-out.
--
--   * A bulk request's runs are fanned out together, and a later run of the
--     same action and request (another request with the same X-Request-Id)
--     folds into the message still waiting for its recipient: runs by action
--     and request id.
--   * A recipient's waiting message (a bulk lead or the hour's digest) is
--     found by its recipient key among the pending deliveries only, a small
--     set however long the delivered and skipped rows are kept.
--
-- Both tables are new in v0.4.0 (0073), so the indexes are built in place.

CREATE INDEX workflow_action_runs_request_idx ON cmdb.workflow_action_runs (action_id, http_request_id)
  WHERE http_request_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX workflow_action_deliveries_pending_key_idx ON cmdb.workflow_action_deliveries (recipient_key)
  WHERE status = 'pending';
