-- Validates the audit_log CHECK constraints that 0056 re-added NOT VALID
-- (the workflow.approval_refresh action, SHAA-1880). Its own file, so its own
-- transaction: VALIDATE CONSTRAINT takes only SHARE UPDATE EXCLUSIVE, and the
-- API keeps writing audit entries while the log is scanned. See sql/README.md.

ALTER TABLE cmdb.audit_log VALIDATE CONSTRAINT audit_log_action_valid;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log VALIDATE CONSTRAINT audit_log_values_present;
