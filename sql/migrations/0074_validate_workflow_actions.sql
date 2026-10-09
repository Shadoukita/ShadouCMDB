-- Validates the audit_log CHECK constraints that 0073 re-added NOT VALID (the
-- workflow action, webhook and mail actions, SHAA-2731). Its own file, so its
-- own transaction: VALIDATE CONSTRAINT takes only SHARE UPDATE EXCLUSIVE, and
-- the API keeps writing audit entries while the log is scanned. See sql/README.md.

ALTER TABLE cmdb.audit_log VALIDATE CONSTRAINT audit_log_action_valid;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log VALIDATE CONSTRAINT audit_log_values_present;
