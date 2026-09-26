-- UI settings and branding assets (SHAA-33).
--
-- One settings document applies to every user: branding, navigation,
-- dashboard widgets, list views and detail/form layouts per CI class. It is a
-- JSON document validated by the API (the schema lives in the OpenAPI spec as
-- `UiSettingsDocument`), because its shape is presentation, not data the
-- database reasons about. Classes and attributes are referenced by key, so a
-- document survives export/import between installs.
--
-- Versioning: ui_settings holds the current document and its version number;
-- every saved version is kept in ui_settings_versions (append-only), so an
-- administrator can see and restore earlier layouts. Writes use optimistic
-- concurrency on `version`. Each change is also written to audit_log.
--
-- Logo and favicon are stored as bytes in ui_assets (small: the API limits
-- them to 512 KiB and 128 KiB), so the application needs no shared file
-- storage and backups of the database include them.
--
-- Soft delete: none. Settings are replaced, not deleted; an asset is removed
-- by deleting its row (the audit log keeps its metadata).

CREATE TABLE ui_settings (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  -- Exactly one row.
  singleton boolean DEFAULT true NOT NULL,
  version integer NOT NULL,
  settings jsonb NOT NULL,
  updated_at timestamp with time zone DEFAULT now() NOT NULL,
  updated_by_id text,
  updated_by_name text,
  CONSTRAINT ui_settings_singleton CHECK (singleton),
  CONSTRAINT ui_settings_singleton_uq UNIQUE (singleton),
  CONSTRAINT ui_settings_version_positive CHECK (version >= 1),
  CONSTRAINT ui_settings_settings_object CHECK (jsonb_typeof(settings) = 'object')
);
--> statement-breakpoint

CREATE TABLE ui_settings_versions (
  version integer PRIMARY KEY NOT NULL,
  settings jsonb NOT NULL,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  actor_type text NOT NULL,
  actor_id text,
  actor_name text,
  -- Optional note from the administrator ("Q3 menu clean-up").
  comment text,
  CONSTRAINT ui_settings_versions_version_positive CHECK (version >= 1),
  CONSTRAINT ui_settings_versions_settings_object CHECK (jsonb_typeof(settings) = 'object'),
  CONSTRAINT ui_settings_versions_actor_type_valid CHECK (actor_type IN ('system', 'user', 'api_client', 'import')),
  CONSTRAINT ui_settings_versions_comment_length CHECK (comment IS NULL OR length(comment) <= 500)
);
--> statement-breakpoint
CREATE INDEX ui_settings_versions_created_idx ON ui_settings_versions (created_at DESC);
--> statement-breakpoint

-- History is append-only, like audit_log.
CREATE OR REPLACE FUNCTION ui_settings_versions_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'ui_settings_versions is append-only (% rejected)', TG_OP
    USING ERRCODE = 'check_violation', CONSTRAINT = 'ui_settings_versions_append_only';
END;
$$;
--> statement-breakpoint
CREATE TRIGGER ui_settings_versions_append_only
  BEFORE UPDATE OR DELETE ON ui_settings_versions
  FOR EACH ROW EXECUTE FUNCTION ui_settings_versions_append_only();
--> statement-breakpoint

-- The current version must be one that history holds.
ALTER TABLE ui_settings
  ADD CONSTRAINT ui_settings_version_fk FOREIGN KEY (version) REFERENCES ui_settings_versions (version)
  DEFERRABLE INITIALLY DEFERRED;
--> statement-breakpoint

-- Version 1: the empty document (every section at its default).
INSERT INTO ui_settings_versions (version, settings, actor_type, actor_name, comment)
VALUES (1, '{}', 'system', 'migration', 'Defaults');
--> statement-breakpoint
INSERT INTO ui_settings (version, settings, updated_by_name) VALUES (1, '{}', 'migration');
--> statement-breakpoint

CREATE TABLE ui_assets (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  kind text NOT NULL,
  content_type text NOT NULL,
  data bytea NOT NULL,
  -- Hex SHA-256 of data: the ETag and the cache-busting part of the URL.
  sha256 text NOT NULL,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  updated_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT ui_assets_kind_uq UNIQUE (kind),
  CONSTRAINT ui_assets_kind_valid CHECK (kind IN ('logo', 'favicon')),
  CONSTRAINT ui_assets_content_type_valid CHECK (content_type IN (
    'image/png', 'image/jpeg', 'image/webp', 'image/svg+xml', 'image/x-icon')),
  CONSTRAINT ui_assets_size CHECK (
    octet_length(data) > 0
    AND octet_length(data) <= CASE kind WHEN 'logo' THEN 524288 ELSE 131072 END),
  CONSTRAINT ui_assets_sha256_format CHECK (sha256 ~ '^[0-9a-f]{64}$')
);
--> statement-breakpoint
CREATE TRIGGER ui_assets_set_updated_at BEFORE UPDATE ON ui_assets
  FOR EACH ROW EXECUTE FUNCTION set_updated_at();
