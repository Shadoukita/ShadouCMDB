-- Who created an API token, as a user id (GH#145, SHAA-393).
--
-- An administrator with users.manage can mint a token owned by another user
-- (POST /api/v1/admin/api-tokens with userId). api_tokens.created_by only
-- keeps the creator's name for display, so after a compromised administrator's
-- password reset the tokens it minted for other accounts could neither be
-- found reliably nor revoked with the reset. created_by_user_id records the
-- creating user; an administrator's password reset revokes the working tokens
-- it created, and GET /api/v1/admin/api-tokens?createdBy=<id> lists them.
--
-- NULL when the token was created by the CLI or another non-user actor, when
-- the creating user was deleted (ON DELETE SET NULL: the token belongs to its
-- owner, not its creator, and keeps working), or when the creator of an older
-- token cannot be established.
--
-- Backfill: from the token's create row in the audit log, whose actor_id is
-- the creating user's id. Tokens whose create row was purged by audit
-- retention, or whose creator no longer exists, stay NULL. The display name in
-- created_by is not used: a username can be renamed or reused, and a wrong
-- attribution is worse than none.
--
-- Soft delete and audit: unchanged (the column is part of the token's audited
-- values from now on).

ALTER TABLE cmdb.api_tokens
  ADD COLUMN created_by_user_id uuid REFERENCES cmdb.users (id) ON DELETE SET NULL;
--> statement-breakpoint
UPDATE cmdb.api_tokens t
SET created_by_user_id = u.id
FROM cmdb.audit_log a
JOIN cmdb.users u ON u.id::text = a.actor_id
WHERE a.entity_type = 'api_tokens'
  AND a.action = 'create'
  AND a.actor_type = 'user'
  AND a.entity_id = t.id;
--> statement-breakpoint
-- The createdBy filter, the revocation on password reset and the SET NULL
-- when a user is deleted.
CREATE INDEX api_tokens_created_by_user_idx ON cmdb.api_tokens (created_by_user_id)
  WHERE created_by_user_id IS NOT NULL;
