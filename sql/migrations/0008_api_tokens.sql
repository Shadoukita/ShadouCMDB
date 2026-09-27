-- API tokens for scripts and services (SHAA-81).
--
-- A token belongs to a user (its owner, the actor in the audit log) and is
-- scoped to one permission profile. Its effective permissions are the
-- intersection of the owner's current permissions and the profile's, so a
-- token never grants more than its owner holds, and taking a right away from
-- the owner takes it away from their tokens too. It stops working when it
-- expires, is revoked, its owner is disabled, or its profile is deleted.
--
-- Only the SHA-256 of the secret is stored (the secret carries 256 random
-- bits, so a fast hash is enough, as for sessions); the secret is shown once,
-- in the response that creates the token. token_prefix is the first
-- characters of the secret, kept so an operator can match a token found in a
-- script or a log against its row.
--
-- Soft delete: revocation sets revoked_at and keeps the row, so the token's
-- history stays readable next to its audit rows. Deleting the owner deletes
-- their tokens (the API audits each one first).
--
-- Audit: create and revoke are 'create' and 'update' rows with entity_type
-- 'api_tokens'; every request made with a token, accepted or not, is a
-- 'token.use' row (details in new_value; never the secret or its hash).

CREATE TABLE api_tokens (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  name text NOT NULL,
  user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  -- NULL once the profile is deleted: the token then has no scope and is refused.
  profile_id uuid REFERENCES permission_profiles (id) ON DELETE SET NULL,
  token_hash bytea NOT NULL,
  token_prefix text NOT NULL,
  expires_at timestamp with time zone NOT NULL,
  revoked_at timestamp with time zone,
  -- Who revoked it (audit actor name), for the list view.
  revoked_by text,
  last_used_at timestamp with time zone,
  -- Evidence only, like sessions.ip_address: never an input to an access decision.
  last_used_ip inet,
  created_by text,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT api_tokens_name_not_blank CHECK (length(btrim(name)) > 0 AND length(name) <= 200),
  CONSTRAINT api_tokens_token_hash_uq UNIQUE (token_hash),
  CONSTRAINT api_tokens_token_hash_length CHECK (octet_length(token_hash) = 32),
  CONSTRAINT api_tokens_expiry_after_creation CHECK (expires_at > created_at),
  CONSTRAINT api_tokens_revoked_consistent CHECK ((revoked_at IS NULL) = (revoked_by IS NULL))
);
--> statement-breakpoint
CREATE INDEX api_tokens_user_idx ON api_tokens (user_id);
--> statement-breakpoint
CREATE INDEX api_tokens_profile_idx ON api_tokens (profile_id) WHERE profile_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX api_tokens_created_idx ON api_tokens (created_at DESC, id);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- audit_log: the token.use action (keeps 0007's audit.purge)
-- ---------------------------------------------------------------------------
ALTER TABLE audit_log DROP CONSTRAINT audit_log_action_valid;
--> statement-breakpoint
ALTER TABLE audit_log ADD CONSTRAINT audit_log_action_valid CHECK (action IN (
  'create', 'update', 'delete', 'restore',
  'login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke',
  'audit.purge', 'token.use'
));
--> statement-breakpoint
ALTER TABLE audit_log DROP CONSTRAINT audit_log_values_present;
--> statement-breakpoint
ALTER TABLE audit_log ADD CONSTRAINT audit_log_values_present CHECK (
  (action = 'create' AND old_value IS NULL AND new_value IS NOT NULL)
  OR (action = 'update' AND old_value IS NOT NULL AND new_value IS NOT NULL)
  OR (action IN ('delete', 'restore') AND old_value IS NOT NULL)
  -- Events, not changes: the details are in new_value.
  OR (action IN ('login.success', 'login.failure', 'login.locked', 'logout', 'session.revoke', 'audit.purge', 'token.use')
      AND old_value IS NULL AND new_value IS NOT NULL)
);
