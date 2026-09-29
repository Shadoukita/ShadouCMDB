-- API tokens follow their owner's two-factor requirement (GH#200, SHAA-495).
--
-- require_mfa was enforced on sessions only: a token created before a profile
-- of its owner required MFA kept working after the owner's sessions were gated
-- or ended. A token is now refused at use when the owner falls under the same
-- MFA_REQUIRED rule as a session (backend/src/data/auth.rs) and the token was
-- not created from a session that proved a second factor. The rule is
-- evaluated per request, so it follows the policy both ways; nothing is
-- revoked when the policy changes.
--
-- sessions.mfa_verified (was provider_mfa, 0023): the session proved a second
-- factor. Set for an OIDC sign-in that proved MFA under 'verify' (as before),
-- a local or directory sign-in through /auth/login/mfa (authenticator or
-- recovery code), and a session in which TOTP enrolment was confirmed.
--
-- api_tokens.mfa_verified: the creating session's mfa_verified, set once at
-- creation. When an administrator creates a token for another owner, the
-- administrator's session counts. A token created without a session (the CLI)
-- gets true: the operator has host access.
--
-- Backfill (best effort; the creating session of an older token is unknown):
-- a token whose creator (or, when unknown, its owner) had a confirmed
-- authenticator when it was created is trusted, as is a session opened after
-- its user confirmed one: that user had to pass the second factor to sign in.
-- Every other existing token starts false, which only matters when its owner
-- falls under require_mfa (fail closed). `shadoucmdb migrate` prints how many
-- working tokens that refuses.
--
-- Audit: the column is part of the token's audited values from now on.
-- Soft delete: unchanged.

ALTER TABLE cmdb.sessions RENAME COLUMN provider_mfa TO mfa_verified;
--> statement-breakpoint
ALTER TABLE cmdb.api_tokens ADD COLUMN mfa_verified boolean NOT NULL DEFAULT false;
--> statement-breakpoint
UPDATE cmdb.api_tokens t SET mfa_verified = true
WHERE EXISTS (SELECT 1 FROM cmdb.user_totp x
              WHERE x.user_id = COALESCE(t.created_by_user_id, t.user_id)
                AND x.confirmed_at IS NOT NULL AND x.confirmed_at <= t.created_at);
--> statement-breakpoint
UPDATE cmdb.sessions s SET mfa_verified = true
WHERE NOT s.mfa_verified
  AND EXISTS (SELECT 1 FROM cmdb.user_totp x
              WHERE x.user_id = s.user_id AND x.confirmed_at IS NOT NULL AND x.confirmed_at <= s.created_at);
