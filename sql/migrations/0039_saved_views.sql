-- Saved views (SHAA-578 spec, SHAA-616).
--
-- Adds the global right `views.share` (D4), granted to every profile that
-- already holds `customization.manage` so nobody loses the ability to curate
-- shared lists, the saved views themselves and each user's default view per
-- inventory list. The file is additive: it rewrites no data and takes no long
-- lock (permission_profile_global_permissions is tiny).
--
-- A view stores a query, never data or rights (D2). Classes, attributes and
-- lookup values are referenced by key inside `definition`, never by id (D3),
-- so views survive renames and travel with the config export. A future
-- migration that renames keys or field references must rewrite
-- `saved_views.definition` and `saved_view_defaults.home` too (see
-- sql/README.md).
--
-- Personal views (`owner_id` set) are user data: they go with their owner
-- (ON DELETE CASCADE) and are never exported. Shared views (`owner_id` NULL)
-- are configuration: they outlive the user who created them, who stays named in
-- `created_by_name` / `updated_by_name` (the ui_settings pattern). Deleting a
-- view is a hard delete; changes to shared views are in the audit log.

-- ---------------------------------------------------------------------------
-- Global right (D4): the full list of 0029 plus views.share.
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.permission_profile_global_permissions
  DROP CONSTRAINT permission_profile_global_permissions_valid;
--> statement-breakpoint
ALTER TABLE cmdb.permission_profile_global_permissions
  ADD CONSTRAINT permission_profile_global_permissions_valid CHECK (permission IN (
    'users.manage', 'profiles.manage', 'datamodel.manage', 'customization.manage',
    'config.export_import', 'audit.view',
    'cis.import',
    'views.share'));
--> statement-breakpoint
INSERT INTO cmdb.permission_profile_global_permissions (profile_id, permission)
  SELECT profile_id, 'views.share' FROM cmdb.permission_profile_global_permissions
  WHERE permission = 'customization.manage'
  ON CONFLICT DO NOTHING;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Views. The API holds a definition to 16 KiB of compact JSON; the CHECK is a
-- backstop with room for jsonb's per-element overhead.
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.saved_views (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  -- NULL: shared with every user (organisation-wide). Personal views go with their owner.
  owner_id        uuid REFERENCES cmdb.users (id) ON DELETE CASCADE,
  context         text NOT NULL CHECK (context IN ('inventory', 'search')),
  name            text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
  description     text CHECK (description IS NULL OR length(description) <= 500),
  definition      jsonb NOT NULL CHECK (jsonb_typeof(definition) = 'object' AND pg_column_size(definition) <= 32768),
  version         integer NOT NULL DEFAULT 1 CHECK (version >= 1),
  created_at      timestamptz NOT NULL DEFAULT now(),
  created_by_id   uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  created_by_name text NOT NULL,
  updated_at      timestamptz NOT NULL DEFAULT now(),
  updated_by_id   uuid REFERENCES cmdb.users (id) ON DELETE SET NULL,
  updated_by_name text NOT NULL
);
--> statement-breakpoint
-- Names are unique ignoring case: per owner and context, and per context among shared views.
CREATE UNIQUE INDEX saved_views_personal_name_uq
  ON cmdb.saved_views (owner_id, context, lower(name)) WHERE owner_id IS NOT NULL;
--> statement-breakpoint
CREATE UNIQUE INDEX saved_views_shared_name_uq
  ON cmdb.saved_views (context, lower(name)) WHERE owner_id IS NULL;
--> statement-breakpoint
-- GET /saved-views: the caller's views and the shared ones (owner_id IS NULL).
CREATE INDEX saved_views_owner_idx ON cmdb.saved_views (owner_id, context);
--> statement-breakpoint
CREATE TRIGGER saved_views_set_updated_at BEFORE UPDATE ON cmdb.saved_views
  FOR EACH ROW EXECUTE FUNCTION cmdb.set_updated_at();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Each user's default view per inventory list (D5, D7: inventory only). `home`
-- is '' for the unscoped inventory, else a class key (not a foreign key, D3):
-- a default whose class was purged is simply never matched.
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.saved_view_defaults (
  user_id   uuid NOT NULL REFERENCES cmdb.users (id) ON DELETE CASCADE,
  context   text NOT NULL CHECK (context = 'inventory'),
  home      text NOT NULL CHECK (home = '' OR home ~ '^[a-z][a-z0-9_]{0,62}$'),
  view_id   uuid NOT NULL REFERENCES cmdb.saved_views (id) ON DELETE CASCADE,
  PRIMARY KEY (user_id, context, home)
);
--> statement-breakpoint
CREATE INDEX saved_view_defaults_view_idx ON cmdb.saved_view_defaults (view_id);
