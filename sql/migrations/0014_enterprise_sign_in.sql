-- Enterprise sign-in: OpenID Connect providers and LDAP/Active Directory
-- directories (SHAA-85).
--
-- identity_providers holds both kinds in one table: the settings every kind
-- shares (name, enabled, the sign-in order) plus the columns of its own kind, which the check constraints below
-- require for that kind and forbid for the other. The kind cannot change.
--
-- Secrets (the OIDC client secret, the LDAP service account's password) are
-- stored as is: the server needs them in clear to present them, as it needs
-- the TOTP secrets of 0013. The API never returns them. Whoever can read this
-- table can impersonate ShadouCMDB towards the identity provider, not sign in
-- to ShadouCMDB as someone else.
--
-- Transport security is not optional: an OIDC issuer is https (plain http only
-- for a loopback test issuer), an LDAP directory is ldaps:// or ldap:// with
-- StartTLS, and certificates are always verified (ca_certificate adds a
-- private CA; there is no switch to turn verification off).
--
-- identity_provider_group_mappings: a group the provider reports (an OIDC
-- groups claim value, an LDAP group DN) grants a permission profile. Matched
-- case-insensitively. A user signing in through a provider holds exactly the
-- profiles their groups map to, recomputed at every sign-in; a sign-in whose
-- groups map to nothing is refused.
--
-- users: the first sign-in of an identity creates its account, which has
-- identity_provider_id and external_id (the OIDC subject, the LDAP entry's
-- objectGUID/entryUUID or DN) and no password, so it cannot use the password
-- form. Local accounts keep
-- working next to providers: they are the break-glass way in when the
-- provider is down. A username already taken by another account is never
-- linked to a provider identity (that would hand the account to whoever
-- controls the provider's username claim).
--
-- oidc_login_states: an OIDC sign-in between the redirect to the provider and
-- its callback. The browser holds the random state (an HttpOnly cookie and
-- the state parameter), the table its SHA-256 with the nonce and the PKCE
-- verifier. Ephemeral like sessions: never backed up.
--
-- Soft delete: none. A provider still referenced by users cannot be deleted
-- (disable it instead); deleting a profile drops its mappings.
--
-- Audit: providers and their mappings are 'create'/'update'/'delete' rows
-- with entity_type 'identity_providers' (never a secret). Sign-ins through a
-- provider are the usual login.success/login.failure events with method
-- 'oidc' or 'ldap'.

CREATE TABLE cmdb.identity_providers (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  kind text NOT NULL,
  -- Shown on the sign-in button ("Sign in with ...") and in the audit trail.
  name text NOT NULL,
  is_enabled boolean DEFAULT true NOT NULL,
  -- Button order for OIDC; the order directories are tried in for LDAP.
  sort_order integer DEFAULT 0 NOT NULL,
  -- PEM: extra CA certificates to trust for this provider (private PKI).
  ca_certificate text,

  -- OpenID Connect
  issuer_url text,
  client_id text,
  client_secret text,
  -- Space-separated; openid is always requested.
  scopes text,
  username_claim text,
  groups_claim text,

  -- LDAP / Active Directory
  ldap_url text,
  start_tls boolean,
  bind_dn text,
  bind_password text,
  user_base_dn text,
  -- {username} is replaced by the escaped sign-in name.
  user_filter text,
  username_attribute text,
  display_name_attribute text,
  email_attribute text,
  group_attribute text,

  created_at timestamp with time zone DEFAULT now() NOT NULL,
  updated_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT identity_providers_kind_valid CHECK (kind IN ('oidc', 'ldap')),
  CONSTRAINT identity_providers_name_not_blank CHECK (length(btrim(name)) > 0 AND length(name) <= 200),
  CONSTRAINT identity_providers_ca_certificate_pem CHECK (
    ca_certificate IS NULL OR (ca_certificate LIKE '%-----BEGIN CERTIFICATE-----%' AND length(ca_certificate) <= 65536)),
  CONSTRAINT identity_providers_oidc_settings CHECK (kind <> 'oidc' OR (
    issuer_url IS NOT NULL AND client_id IS NOT NULL AND scopes IS NOT NULL
    AND username_claim IS NOT NULL AND groups_claim IS NOT NULL
    AND ldap_url IS NULL AND start_tls IS NULL AND bind_dn IS NULL AND bind_password IS NULL
    AND user_base_dn IS NULL AND user_filter IS NULL AND username_attribute IS NULL
    AND display_name_attribute IS NULL AND email_attribute IS NULL AND group_attribute IS NULL)),
  CONSTRAINT identity_providers_ldap_settings CHECK (kind <> 'ldap' OR (
    ldap_url IS NOT NULL AND start_tls IS NOT NULL AND user_base_dn IS NOT NULL AND user_filter IS NOT NULL
    AND username_attribute IS NOT NULL AND display_name_attribute IS NOT NULL AND email_attribute IS NOT NULL
    AND group_attribute IS NOT NULL
    AND issuer_url IS NULL AND client_id IS NULL AND client_secret IS NULL AND scopes IS NULL
    AND username_claim IS NULL AND groups_claim IS NULL)),
  -- https, or http to a loopback address (a test issuer on the same host).
  CONSTRAINT identity_providers_issuer_https CHECK (
    issuer_url IS NULL
    OR issuer_url ~ '^https://[^/?#@\s]+(/[^?#\s]*)?$'
    OR issuer_url ~ '^http://(localhost|127\.0\.0\.1|\[::1\])(:[0-9]{1,5})?(/[^?#\s]*)?$'),
  CONSTRAINT identity_providers_ldap_tls CHECK (
    ldap_url IS NULL
    OR (ldap_url ~* '^ldaps://[^/?#@\s]+/?$' AND NOT start_tls)
    OR (ldap_url ~* '^ldap://[^/?#@\s]+/?$' AND start_tls)),
  -- An anonymous search is not offered: a bind DN comes with its password.
  CONSTRAINT identity_providers_bind_pair CHECK ((bind_dn IS NULL) = (bind_password IS NULL)),
  CONSTRAINT identity_providers_user_filter_placeholder CHECK (user_filter IS NULL OR user_filter LIKE '%{username}%')
);
--> statement-breakpoint
CREATE UNIQUE INDEX identity_providers_name_uq ON cmdb.identity_providers (lower(name));
--> statement-breakpoint
CREATE TRIGGER identity_providers_set_updated_at BEFORE UPDATE ON cmdb.identity_providers
  FOR EACH ROW EXECUTE FUNCTION cmdb.set_updated_at();
--> statement-breakpoint

CREATE OR REPLACE FUNCTION cmdb.identity_providers_kind_fixed() RETURNS trigger
LANGUAGE plpgsql
SET search_path = cmdb, public
AS $$
BEGIN
  IF NEW.kind IS DISTINCT FROM OLD.kind THEN
    RAISE EXCEPTION 'identity_providers: the kind of a provider cannot change'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'identity_providers_kind_fixed';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER identity_providers_kind_fixed BEFORE UPDATE OF kind ON cmdb.identity_providers
  FOR EACH ROW EXECUTE FUNCTION cmdb.identity_providers_kind_fixed();
--> statement-breakpoint

CREATE TABLE cmdb.identity_provider_group_mappings (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  provider_id uuid NOT NULL REFERENCES cmdb.identity_providers (id) ON DELETE CASCADE,
  -- As the provider reports it: a group name or id (OIDC), a group DN (LDAP).
  group_name text NOT NULL,
  profile_id uuid NOT NULL REFERENCES cmdb.permission_profiles (id) ON DELETE CASCADE,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT identity_provider_group_mappings_group_not_blank
    CHECK (length(btrim(group_name)) > 0 AND length(group_name) <= 1024)
);
--> statement-breakpoint
CREATE UNIQUE INDEX identity_provider_group_mappings_uq
  ON cmdb.identity_provider_group_mappings (provider_id, lower(group_name), profile_id);
--> statement-breakpoint
CREATE INDEX identity_provider_group_mappings_profile_idx ON cmdb.identity_provider_group_mappings (profile_id);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- users: accounts that belong to a provider
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.users
  ADD COLUMN identity_provider_id uuid REFERENCES cmdb.identity_providers (id) ON DELETE RESTRICT,
  ADD COLUMN external_id text;
--> statement-breakpoint
ALTER TABLE cmdb.users ALTER COLUMN password_hash DROP NOT NULL;
--> statement-breakpoint
ALTER TABLE cmdb.users
  ADD CONSTRAINT users_external_identity_pair CHECK ((identity_provider_id IS NULL) = (external_id IS NULL)),
  ADD CONSTRAINT users_external_id_length CHECK (external_id IS NULL OR length(external_id) BETWEEN 1 AND 1024),
  -- A local account has a password; a provider's account has none.
  ADD CONSTRAINT users_password_iff_local CHECK ((identity_provider_id IS NULL) = (password_hash IS NOT NULL));
--> statement-breakpoint
CREATE UNIQUE INDEX users_external_identity_uq ON cmdb.users (identity_provider_id, external_id)
  WHERE identity_provider_id IS NOT NULL;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- oidc_login_states
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.oidc_login_states (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  state_hash bytea NOT NULL,
  provider_id uuid NOT NULL REFERENCES cmdb.identity_providers (id) ON DELETE CASCADE,
  nonce text NOT NULL,
  code_verifier text NOT NULL,
  -- Where the web UI goes after signing in: a path on this server.
  return_to text,
  expires_at timestamp with time zone NOT NULL,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT oidc_login_states_state_hash_uq UNIQUE (state_hash),
  CONSTRAINT oidc_login_states_state_hash_length CHECK (octet_length(state_hash) = 32),
  CONSTRAINT oidc_login_states_return_to_local CHECK (return_to IS NULL OR (return_to ~ '^/[^/\\]' OR return_to = '/'))
);
--> statement-breakpoint
CREATE INDEX oidc_login_states_expires_idx ON cmdb.oidc_login_states (expires_at);
