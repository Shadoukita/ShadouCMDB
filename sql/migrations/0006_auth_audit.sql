-- Authentication events in audit_log, and the client IP of each session (SHAA-38).
--
-- Sign-in success and failure, the login lock, sign-out and every session the
-- API ends (user disabled or deleted, password reset or changed, a new sign-in
-- replacing the browser's old session) are audit_log rows with
-- entity_type = 'sessions'. They carry no old value: new_value holds the
-- event's details (user, IP address, user agent, reason). No password, session
-- token, token hash or CSRF token is ever written.
--
-- Not reversible once an authentication row exists: audit_log is append-only,
-- and restoring the old audit_log_action_valid check would fail against those rows.

-- ---------------------------------------------------------------------------
-- sessions.ip_address
-- ---------------------------------------------------------------------------
-- The client address as seen by the API (first X-Forwarded-For hop, else
-- Forwarded: for=, else the TCP peer). Evidence for the audit trail only:
-- forwarded headers are client-controlled unless a proxy overwrites them, so
-- nothing may base an access decision on this value. NULL for sessions opened
-- before this migration.
ALTER TABLE sessions ADD COLUMN ip_address inet;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- audit_log: authentication actions
-- ---------------------------------------------------------------------------
ALTER TABLE audit_log DROP CONSTRAINT audit_log_action_valid;
--> statement-breakpoint
ALTER TABLE audit_log ADD CONSTRAINT audit_log_action_valid CHECK (action IN (
  'create', 'update', 'delete', 'restore',
  'login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke'
));
--> statement-breakpoint
ALTER TABLE audit_log DROP CONSTRAINT audit_log_values_present;
--> statement-breakpoint
ALTER TABLE audit_log ADD CONSTRAINT audit_log_values_present CHECK (
  (action = 'create' AND old_value IS NULL AND new_value IS NOT NULL)
  OR (action = 'update' AND old_value IS NOT NULL AND new_value IS NOT NULL)
  OR (action IN ('delete', 'restore') AND old_value IS NOT NULL)
  -- Events, not changes: the details are in new_value.
  OR (action IN ('login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke')
      AND old_value IS NULL AND new_value IS NOT NULL)
);
