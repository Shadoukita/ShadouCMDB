-- Validates the CHECK constraints that 0051 added NOT VALID (SHAA-1871): the
-- event kinds and the approval request link on workflow_instance_events, and
-- the audit actions. Its own file, so its own transaction: VALIDATE
-- CONSTRAINT takes only SHARE UPDATE EXCLUSIVE, and the API keeps writing
-- events and audit entries while the tables are scanned. See sql/README.md.

ALTER TABLE cmdb.workflow_instance_events VALIDATE CONSTRAINT workflow_instance_events_kind_check;
--> statement-breakpoint
ALTER TABLE cmdb.workflow_instance_events VALIDATE CONSTRAINT workflow_instance_events_approval;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log VALIDATE CONSTRAINT audit_log_action_valid;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log VALIDATE CONSTRAINT audit_log_values_present;
