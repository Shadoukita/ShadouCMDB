-- How far an OIDC provider is trusted to have used a second factor (GH#131, SHAA-397).
--
-- A permission profile with require_mfa exempted every OIDC account, on the
-- assumption that the provider runs the second factor. Nothing checked it: an
-- IdP whose policy for this client is password-only gave a require_mfa
-- profile (up to Administrator) on one factor, silently.
--
-- identity_providers.mfa_assurance (OIDC only):
--   'verify'         the exemption holds only when the signed ID token proves
--                    MFA (amr, or acr against required_acr); otherwise a user
--                    holding a require_mfa profile is refused at sign-in.
--   'trust_provider' the administrator states the IdP enforces MFA for this
--                    client; the token is not checked (the old behaviour, now
--                    an explicit and audited choice).
-- identity_providers.required_acr: with 'verify', the ID token's acr must be
-- one of these (and the authorization request asks for them in acr_values).
-- Empty: amr decides. Always empty with 'trust_provider'. LDAP: both NULL.
--
-- sessions.provider_mfa: the sign-in that opened the session proved MFA under
-- 'verify'. The per-request gate reads it together with the provider's current
-- setting, so switching a provider from trust to verify affects live sessions.
--
-- Upgrade: existing OIDC providers become 'trust_provider', which keeps their
-- behaviour and locks nobody out; the release note asks administrators to
-- review them. There is no column default: providers created from now on get
-- their value from the API, which defaults to 'verify'. Existing sessions need
-- no backfill (false is right: every existing provider is 'trust_provider').
--
-- Audit: changes to the two provider columns are part of the provider's
-- audited values (identity_providers update rows). Soft delete: unchanged.

ALTER TABLE cmdb.identity_providers
  ADD COLUMN mfa_assurance text,
  ADD COLUMN required_acr text[];
--> statement-breakpoint
UPDATE cmdb.identity_providers SET mfa_assurance = 'trust_provider', required_acr = '{}' WHERE kind = 'oidc';
--> statement-breakpoint
ALTER TABLE cmdb.identity_providers
  ADD CONSTRAINT identity_providers_mfa_assurance CHECK (
    CASE kind
      WHEN 'oidc' THEN mfa_assurance IN ('verify', 'trust_provider') AND required_acr IS NOT NULL
        AND (mfa_assurance = 'verify' OR cardinality(required_acr) = 0)
      ELSE mfa_assurance IS NULL AND required_acr IS NULL
    END),
  -- At most 10 values of 1 to 200 printable ASCII characters without spaces
  -- (joined with a newline, which no value may contain).
  ADD CONSTRAINT identity_providers_required_acr_values CHECK (
    required_acr IS NULL OR cardinality(required_acr) = 0
    OR (array_ndims(required_acr) = 1 AND array_position(required_acr, NULL) IS NULL
        AND array_to_string(required_acr, E'\n') ~ '^[!-~]{1,200}(\n[!-~]{1,200}){0,9}$'));
--> statement-breakpoint
ALTER TABLE cmdb.sessions ADD COLUMN provider_mfa boolean NOT NULL DEFAULT false;
