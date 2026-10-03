-- Validates the audit_log CHECK constraints that 0046 re-added NOT VALID
-- (SHAA-1422). Its own file, so its own transaction: VALIDATE CONSTRAINT takes
-- only SHARE UPDATE EXCLUSIVE, and the API keeps writing audit entries while
-- the log is scanned. Run in 0046's transaction, the scan would have waited
-- under the ACCESS EXCLUSIVE lock that re-adding the constraints takes until
-- commit. See sql/README.md.

ALTER TABLE cmdb.audit_log VALIDATE CONSTRAINT audit_log_action_valid;
--> statement-breakpoint
ALTER TABLE cmdb.audit_log VALIDATE CONSTRAINT audit_log_values_present;
