-- OIDC client secrets and LDAP bind passwords encrypted at rest (GH#199,
-- design SHAA-490 Part A).
--
-- Until now identity_providers.client_secret and .bind_password held the
-- secrets as text (0014), so a read of the table, a pg_dump or a
-- `shadoucmdb backup` file revealed the credentials ShadouCMDB presents to
-- the identity provider. From now on the server encrypts them like the TOTP
-- seeds of 0025: AES-256-GCM under the key in ENCRYPTION_KEY_FILE (its own
-- HKDF subkey), bound to the provider and the column (the associated data is
-- the provider id and the column name), stored as
-- nonce(12) || ciphertext || tag(16) in the new bytea columns.
--
-- secrets_key_id identifies the key of the row's ciphertext (an HKDF
-- fingerprint, not secret). Only one secret column applies to a kind, so one
-- key id per row covers both. A row is in one of three states:
--   * no secret at all (an OIDC public client, an anonymous directory
--     search): every secret column and secrets_key_id NULL; needs no key;
--   * written before this migration: the secret in the text column,
--     secrets_key_id NULL. `serve` encrypts those at start-up, before it
--     accepts requests, and never writes one;
--   * encrypted: the _enc column and secrets_key_id set, the text columns NULL.
-- The same start-up step re-encrypts rows under ENCRYPTION_KEY_PREVIOUS_FILE
-- after a key rotation and refuses to start when rows carry a key that is not
-- configured ("shadoucmdb identity-providers reset-undecryptable" gives such
-- providers up: disabled, secret cleared, audited).
--
-- The text columns stay for one release, so an instance of the previous
-- release can still read and write them during a rolling upgrade; a later
-- release drops them. The 0014 comment ("stored as is") no longer holds; 0014
-- itself stays unchanged (applied migrations are checksummed).
--
-- Backups copy the ciphertext as it is; the key is never in the database.
-- Backups taken before this release still hold the plaintext secrets.
-- Soft delete: none, as 0014. Audit: unchanged, the audit rows carry the API
-- representation (clientSecretSet / bindPasswordSet), never a secret.

ALTER TABLE cmdb.identity_providers
  ADD COLUMN client_secret_enc bytea,
  ADD COLUMN bind_password_enc bytea,
  ADD COLUMN secrets_key_id integer;
--> statement-breakpoint
-- The bind pair now counts the encrypted form too.
ALTER TABLE cmdb.identity_providers DROP CONSTRAINT identity_providers_bind_pair;
--> statement-breakpoint
ALTER TABLE cmdb.identity_providers ADD CONSTRAINT identity_providers_bind_pair
  CHECK ((bind_dn IS NULL) = (bind_password IS NULL AND bind_password_enc IS NULL));
--> statement-breakpoint
-- The kind checks of 0014 cover the text columns; this covers the new ones.
ALTER TABLE cmdb.identity_providers ADD CONSTRAINT identity_providers_secret_kind CHECK (
  (kind = 'oidc' OR client_secret_enc IS NULL) AND (kind = 'ldap' OR bind_password_enc IS NULL));
--> statement-breakpoint
-- All plaintext (key id NULL) or all encrypted, never both.
ALTER TABLE cmdb.identity_providers ADD CONSTRAINT identity_providers_secrets_form CHECK (
  CASE WHEN secrets_key_id IS NULL
       THEN client_secret_enc IS NULL AND bind_password_enc IS NULL
       ELSE client_secret IS NULL AND bind_password IS NULL
            AND (client_secret_enc IS NOT NULL OR bind_password_enc IS NOT NULL) END);
--> statement-breakpoint
-- 1..4096 characters of up to 4 bytes each, plus the 28 bytes of nonce and tag.
ALTER TABLE cmdb.identity_providers ADD CONSTRAINT identity_providers_secrets_length CHECK (
  (client_secret_enc IS NULL OR octet_length(client_secret_enc) BETWEEN 29 AND 16412)
  AND (bind_password_enc IS NULL OR octet_length(bind_password_enc) BETWEEN 29 AND 16412));
