-- Stateless OIDC sign-in start (GH#122, SHAA-339).
--
-- Starting an OIDC sign-in is anonymous. It used to insert a row into
-- oidc_login_states, under a cap shared by every client, so one client could
-- fill the table and keep everybody else from signing in. The pending sign-in
-- (provider, state, nonce, PKCE verifier, return path, expiry) now travels in
-- the shadoucmdb_oidc cookie itself, sealed with AES-256-GCM under a server
-- key, and oidc_login_states is dropped. Sign-ins in progress while this
-- migration runs end with "expired" once; the user starts again.
--
-- server_keys: secrets the server generates for itself, one row per purpose
-- ('oidc_state' seals the cookie above). The first API process that needs a
-- key inserts it (ON CONFLICT DO NOTHING) and every process then reads the
-- same row, so all replicas behind a load balancer share it without any
-- operator configuration and it survives restarts. key_id is the byte in
-- front of every sealed value, so a later automated rotation can keep the
-- previous key for a while.
--
-- The API role may read and add keys, never change or delete them. Rotating
-- by hand: as the migration (owner) role
--   DELETE FROM cmdb.server_keys WHERE purpose = 'oidc_state';
-- then restart the API processes; they generate a new key, and sign-ins in
-- progress end with "expired".
--
-- Never backed up (backup/restore excludes the table): a restored database
-- gets a fresh key. Whoever can read this table can forge sign-in state
-- cookies, which gives them nothing: they can read the providers' client
-- secrets already, and the provider still has to vouch for the user.
--
-- Soft delete: none. Audit: none (no user action changes it).

CREATE TABLE cmdb.server_keys (
  purpose text PRIMARY KEY,
  -- One byte on the wire.
  key_id smallint NOT NULL,
  secret bytea NOT NULL,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT server_keys_purpose_not_blank CHECK (length(btrim(purpose)) > 0 AND length(purpose) <= 100),
  CONSTRAINT server_keys_key_id_byte CHECK (key_id BETWEEN 0 AND 255),
  CONSTRAINT server_keys_secret_length CHECK (octet_length(secret) = 32)
);
--> statement-breakpoint
REVOKE ALL ON cmdb.server_keys FROM PUBLIC;
--> statement-breakpoint
-- Migration 0008's default privileges give the API role DML on new cmdb tables;
-- it only reads keys and adds missing ones.
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = app_role) AND current_user <> app_role THEN
    EXECUTE format('REVOKE ALL ON cmdb.server_keys FROM %I', app_role);
    EXECUTE format('GRANT SELECT, INSERT ON cmdb.server_keys TO %I', app_role);
  END IF;
END;
$$;
--> statement-breakpoint

DROP TABLE cmdb.oidc_login_states;
