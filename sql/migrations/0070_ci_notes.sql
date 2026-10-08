-- CI notes (SHAA-2355): a stream of short, signed, timestamped texts on a
-- configuration item (the Notes tab of the CI detail page). A note is not an
-- attribute: it has an author and a time, is never part of the CI's version,
-- and is changed only by its author within the edit window of the policy.
--
-- The file is additive: two new tables and one settings row; no data is
-- rewritten and no lock is taken on an existing table beyond the foreign keys.
--
-- Notes are user data. They go with their CI when the CI row is removed (a
-- soft-deleted CI keeps them, readable for history) and stay when their author
-- is deleted, who stays named in `author_name` (the saved_views pattern).
-- Deleting a note is a hard delete: the audit log keeps the note as it was.
-- The API role gets the default rights (SELECT, INSERT, UPDATE, DELETE) on
-- the notes, and on the policy row all but DELETE.

CREATE TABLE cmdb.ci_notes (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  ci_id       uuid NOT NULL REFERENCES cmdb.configuration_items (id) ON DELETE CASCADE,
  -- Plain text; the API trims it and holds it to 10,000 characters.
  body        text NOT NULL CHECK (length(btrim(body)) BETWEEN 1 AND 10000),
  author_id   uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  author_name text NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  -- NULL until the author first changes the text.
  edited_at   timestamptz CHECK (edited_at IS NULL OR edited_at >= created_at),
  version     integer NOT NULL DEFAULT 1 CHECK (version >= 1)
);
--> statement-breakpoint
-- The note stream of one CI, newest first (GET /configuration-items/{id}/notes).
CREATE INDEX ci_notes_ci_created_idx ON cmdb.ci_notes (ci_id, created_at DESC, id DESC);
--> statement-breakpoint
-- The retention sweep: notes older than the retention period, oldest first.
CREATE INDEX ci_notes_created_idx ON cmdb.ci_notes (created_at);
--> statement-breakpoint
CREATE INDEX ci_notes_author_idx ON cmdb.ci_notes (author_id) WHERE author_id IS NOT NULL;
--> statement-breakpoint

-- The policy, exactly one row:
--   edit_window_minutes: how long after posting the author may change or
--     delete their note; NULL: no limit, 0: never. Default 24 hours.
--   retention_days: notes older than this are deleted by the server's
--     retention sweep; NULL (the default): kept until deleted. At least 30.
CREATE TABLE cmdb.ci_note_settings (
  id                  boolean PRIMARY KEY DEFAULT true CHECK (id),
  edit_window_minutes integer DEFAULT 1440 CHECK (edit_window_minutes BETWEEN 0 AND 525600),
  retention_days      integer CHECK (retention_days BETWEEN 30 AND 36500),
  updated_at          timestamptz NOT NULL DEFAULT now(),
  updated_by_id       uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  updated_by_name     text
);
--> statement-breakpoint
INSERT INTO cmdb.ci_note_settings DEFAULT VALUES;
--> statement-breakpoint
CREATE TRIGGER ci_note_settings_set_updated_at BEFORE UPDATE ON cmdb.ci_note_settings
  FOR EACH ROW EXECUTE FUNCTION cmdb.set_updated_at();
--> statement-breakpoint
-- The API role reads and changes the policy row but never removes it.
INSERT INTO cmdb.api_role_privileges (object, object_type, privileges)
  VALUES ('cmdb.ci_note_settings', 'table', '{SELECT,INSERT,UPDATE}');
--> statement-breakpoint
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
BEGIN
  -- Not on a single-role install, where the API role owns cmdb.
  IF to_regrole(app_role) IS NOT NULL AND current_user <> app_role
     AND (SELECT nspowner FROM pg_namespace WHERE nspname = 'cmdb') <> to_regrole(app_role) THEN
    PERFORM cmdb.apply_api_role_grants(app_role);
  END IF;
END $$;
