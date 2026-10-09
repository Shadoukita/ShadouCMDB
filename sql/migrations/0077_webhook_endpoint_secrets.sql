-- Webhook endpoints: the sealed columns as the other sealed tables have them
-- (v0.4.0 design SHAA-2725 §5.4, slice S5; GH#836).
--
-- 0073 created cmdb.webhook_endpoints before anything wrote it. Its key-id
-- columns were bytea; every other sealed table stores the id of the key that
-- sealed a value as integer (secrets::KeyId, user_totp.key_id,
-- identity_providers.secrets_key_id), and the start-up key check, the
-- re-encryption under a new key, the backup header and reset-undecryptable
-- read them as such. The ciphertext columns accepted any bytes, and the URL
-- could carry credentials (https://user:password@host/), which would then be
-- shown by the API and written to the audit log; receiver credentials belong
-- in the sealed auth header.
--
-- Nothing has written an endpoint yet (the API arrives with this release), so
-- the type change rewrites no data. Should a table hold rows anyway (written
-- by hand), the migration stops rather than guess what they hold.

DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM cmdb.webhook_endpoints) THEN
    RAISE EXCEPTION 'cmdb.webhook_endpoints holds % row(s) written before webhook endpoints could be created through the API; they cannot be converted. Delete them (DELETE FROM cmdb.webhook_endpoints, after removing the workflow actions that use them) and run the migration again',
      (SELECT count(*) FROM cmdb.webhook_endpoints);
  END IF;
END
$$;
--> statement-breakpoint

ALTER TABLE cmdb.webhook_endpoints
  ALTER COLUMN secret_key_id TYPE integer USING NULL,
  ALTER COLUMN previous_secret_key_id TYPE integer USING NULL,
  ALTER COLUMN auth_header_key_id TYPE integer USING NULL;
--> statement-breakpoint

-- A sealed value is a 12-byte nonce, the ciphertext and a 16-byte tag
-- (secrets::OVERHEAD = 28). The signing secrets are 32 random bytes, so at
-- least 16 bytes of plaintext is a floor no real secret falls under; a header
-- value only has to be non-empty.
ALTER TABLE cmdb.webhook_endpoints
  ADD CONSTRAINT webhook_endpoints_secret_sealed CHECK (octet_length(secret_ciphertext) >= 28 + 16),
  ADD CONSTRAINT webhook_endpoints_previous_secret_sealed
    CHECK (previous_secret_ciphertext IS NULL OR octet_length(previous_secret_ciphertext) >= 28 + 16),
  ADD CONSTRAINT webhook_endpoints_auth_header_sealed
    CHECK (auth_header_ciphertext IS NULL OR octet_length(auth_header_ciphertext) > 28),
  -- No user:password@ in the authority.
  ADD CONSTRAINT webhook_endpoints_url_userinfo CHECK (url !~ '^https?://[^/?#]*@');
--> statement-breakpoint

-- The delivery workers claim per endpoint (in flight, per minute) and hold or
-- release an endpoint's queue when it is suspended or resumed.
CREATE INDEX workflow_action_deliveries_endpoint_queue_idx ON cmdb.workflow_action_deliveries (endpoint_id, next_attempt_at, id)
  WHERE status IN ('pending', 'held') AND endpoint_id IS NOT NULL;
