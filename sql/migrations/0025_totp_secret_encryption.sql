-- TOTP seeds encrypted at rest (GH#189, design SHAA-484).
--
-- Until now user_totp.secret held the raw 20-byte seed (0013): whoever could
-- read the table, or a backup of it, could compute every user's codes. From
-- now on the server encrypts it with AES-256-GCM under a key kept outside the
-- database (ENCRYPTION_KEY_FILE), bound to its row (the associated data is
-- the user id), and stores nonce(12) || ciphertext(20) || tag(16).
--
-- user_totp.key_id identifies the key that encrypted the row (an HKDF
-- fingerprint of it, not secret). NULL marks a seed written before this
-- migration, still 20 plaintext bytes: `serve` encrypts those at start-up,
-- before it accepts requests, keeping confirmed_at and last_used_step, so
-- nobody enrols again. The server never writes such a row. The same start-up
-- step re-encrypts rows under ENCRYPTION_KEY_PREVIOUS_FILE after a key
-- rotation, and refuses to start when rows carry a key that is not configured.
--
-- The 0013 comment ("stored as is, like the backup file stores it") no longer
-- holds; 0013 itself stays unchanged (applied migrations are checksummed).
--
-- Backups copy the ciphertext and key_id as they are; the key is never in the
-- database, so it is never in a backup either. Soft delete and audit: as 0013.

ALTER TABLE cmdb.user_totp ADD COLUMN key_id integer;
--> statement-breakpoint
ALTER TABLE cmdb.user_totp DROP CONSTRAINT user_totp_secret_length;
--> statement-breakpoint
ALTER TABLE cmdb.user_totp ADD CONSTRAINT user_totp_secret_format CHECK (
  (key_id IS NULL AND octet_length(secret) = 20) OR (key_id IS NOT NULL AND octet_length(secret) = 48));
