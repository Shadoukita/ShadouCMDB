-- Users, login sessions and permission profiles (SHAA-28).
--
-- There are no fixed roles: an administrator defines permission profiles
-- (named sets of global and per-CI-class permissions) and assigns any number of
-- them to each user. One built-in profile, "Administrator", holds every
-- permission implicitly; it cannot be deleted or changed, and the last active
-- user holding it cannot lose it (deferred trigger below).
--
-- Soft delete: none of these tables use it. Users are disabled with
-- is_active = false (the normal way to remove access); a hard delete is allowed
-- because audit_log keeps actor_id and actor_name as plain text.

-- ---------------------------------------------------------------------------
-- users
-- ---------------------------------------------------------------------------
CREATE TABLE users (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  username text NOT NULL,
  display_name text NOT NULL,
  email text,
  -- PHC string ($argon2id$v=19$m=...,t=...,p=...$salt$hash). Never leaves the API.
  password_hash text NOT NULL,
  is_active boolean DEFAULT true NOT NULL,
  password_changed_at timestamp with time zone DEFAULT now() NOT NULL,
  last_login_at timestamp with time zone,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  updated_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT users_username_format CHECK (username ~ '^[A-Za-z0-9][A-Za-z0-9._@-]{0,63}$'),
  CONSTRAINT users_display_name_not_blank CHECK (length(btrim(display_name)) > 0),
  CONSTRAINT users_email_format CHECK (email IS NULL OR email ~ '^[^@\s]+@[^@\s]+$'),
  CONSTRAINT users_password_hash_argon2id CHECK (password_hash LIKE '$argon2id$%')
);
--> statement-breakpoint
-- Usernames are unique regardless of case; login matches case-insensitively.
CREATE UNIQUE INDEX users_username_uq ON users (lower(username));
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- permission profiles
-- ---------------------------------------------------------------------------
CREATE TABLE permission_profiles (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  name text NOT NULL,
  description text,
  -- The Administrator profile: every permission, not editable, not deletable.
  is_builtin boolean DEFAULT false NOT NULL,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  updated_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT permission_profiles_name_not_blank CHECK (length(btrim(name)) > 0)
);
--> statement-breakpoint
CREATE UNIQUE INDEX permission_profiles_name_uq ON permission_profiles (lower(name));
--> statement-breakpoint
-- At most one built-in profile.
CREATE UNIQUE INDEX permission_profiles_one_builtin ON permission_profiles (is_builtin) WHERE is_builtin;
--> statement-breakpoint

-- Global (not class-scoped) permissions. The set is defined by the code, so a
-- check constraint rather than a lookup table.
CREATE TABLE permission_profile_global_permissions (
  profile_id uuid NOT NULL REFERENCES permission_profiles (id) ON DELETE CASCADE,
  permission text NOT NULL,
  PRIMARY KEY (profile_id, permission),
  CONSTRAINT permission_profile_global_permissions_valid CHECK (permission IN (
    'users.manage', 'profiles.manage', 'datamodel.manage', 'customization.manage',
    'config.export_import', 'audit.view'))
);
--> statement-breakpoint

-- Per-class permissions. class_id NULL is the "all classes" wildcard. A grant
-- applies to exactly that class (not its subclasses). Every row grants view:
-- create/edit/delete without view make no sense, and a row granting nothing is
-- left out.
CREATE TABLE permission_profile_class_permissions (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  profile_id uuid NOT NULL REFERENCES permission_profiles (id) ON DELETE CASCADE,
  class_id uuid REFERENCES ci_classes (id) ON DELETE CASCADE,
  can_view boolean DEFAULT false NOT NULL,
  can_create boolean DEFAULT false NOT NULL,
  can_edit boolean DEFAULT false NOT NULL,
  can_delete boolean DEFAULT false NOT NULL,
  CONSTRAINT permission_profile_class_permissions_view_required CHECK (can_view)
);
--> statement-breakpoint
CREATE UNIQUE INDEX permission_profile_class_permissions_class_uq
  ON permission_profile_class_permissions (profile_id, class_id) WHERE class_id IS NOT NULL;
--> statement-breakpoint
CREATE UNIQUE INDEX permission_profile_class_permissions_wildcard_uq
  ON permission_profile_class_permissions (profile_id) WHERE class_id IS NULL;
--> statement-breakpoint
CREATE INDEX permission_profile_class_permissions_class_idx
  ON permission_profile_class_permissions (class_id) WHERE class_id IS NOT NULL;
--> statement-breakpoint

CREATE TABLE user_permission_profiles (
  user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  profile_id uuid NOT NULL REFERENCES permission_profiles (id) ON DELETE CASCADE,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  PRIMARY KEY (user_id, profile_id)
);
--> statement-breakpoint
CREATE INDEX user_permission_profiles_profile_idx ON user_permission_profiles (profile_id);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- sessions (server-side; the cookie carries a random token, the table its SHA-256)
-- ---------------------------------------------------------------------------
CREATE TABLE sessions (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  token_hash bytea NOT NULL,
  user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  -- Must be echoed in X-CSRF-Token on every state-changing request.
  csrf_token text NOT NULL,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  last_seen_at timestamp with time zone DEFAULT now() NOT NULL,
  -- Absolute lifetime; the idle timeout is applied against last_seen_at.
  expires_at timestamp with time zone NOT NULL,
  user_agent text,
  CONSTRAINT sessions_token_hash_uq UNIQUE (token_hash),
  CONSTRAINT sessions_token_hash_length CHECK (octet_length(token_hash) = 32)
);
--> statement-breakpoint
CREATE INDEX sessions_user_idx ON sessions (user_id);
--> statement-breakpoint
CREATE INDEX sessions_expires_idx ON sessions (expires_at);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- updated_at maintenance (function from 0002)
-- ---------------------------------------------------------------------------
CREATE TRIGGER users_set_updated_at BEFORE UPDATE ON users
  FOR EACH ROW EXECUTE FUNCTION set_updated_at();
--> statement-breakpoint
CREATE TRIGGER permission_profiles_set_updated_at BEFORE UPDATE ON permission_profiles
  FOR EACH ROW EXECUTE FUNCTION set_updated_at();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- The built-in Administrator profile
-- ---------------------------------------------------------------------------
INSERT INTO permission_profiles (name, description, is_builtin)
VALUES ('Administrator', 'Built-in: every permission, including all CI classes. Cannot be changed or deleted.', true);
--> statement-breakpoint

CREATE OR REPLACE FUNCTION permission_profiles_protect_builtin() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    IF OLD.is_builtin THEN
      RAISE EXCEPTION 'permission_profiles: the built-in Administrator profile cannot be deleted'
        USING ERRCODE = 'check_violation', CONSTRAINT = 'permission_profiles_builtin_protected';
    END IF;
    RETURN OLD;
  END IF;
  IF OLD.is_builtin OR NEW.is_builtin THEN
    RAISE EXCEPTION 'permission_profiles: the built-in Administrator profile cannot be changed'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'permission_profiles_builtin_protected';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER permission_profiles_protect_builtin
  BEFORE UPDATE OR DELETE ON permission_profiles
  FOR EACH ROW EXECUTE FUNCTION permission_profiles_protect_builtin();
--> statement-breakpoint

-- The built-in profile's permissions are implicit; it has no permission rows.
CREATE OR REPLACE FUNCTION permission_rows_not_builtin() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM permission_profiles WHERE id = NEW.profile_id AND is_builtin) THEN
    RAISE EXCEPTION 'permission_profiles: the built-in Administrator profile cannot be changed'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'permission_profiles_builtin_protected';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER permission_profile_global_permissions_not_builtin
  BEFORE INSERT OR UPDATE ON permission_profile_global_permissions
  FOR EACH ROW EXECUTE FUNCTION permission_rows_not_builtin();
--> statement-breakpoint
CREATE TRIGGER permission_profile_class_permissions_not_builtin
  BEFORE INSERT OR UPDATE ON permission_profile_class_permissions
  FOR EACH ROW EXECUTE FUNCTION permission_rows_not_builtin();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Never leave zero active users holding the Administrator profile
-- ---------------------------------------------------------------------------
-- Checked at commit (deferred), so replacing a user's profiles (delete + insert)
-- in one transaction is fine. Deleting a user cascades to
-- user_permission_profiles, which fires the check there. The advisory lock serialises concurrent changes,
-- so two administrators cannot disable each other at the same time.
CREATE OR REPLACE FUNCTION users_keep_one_administrator() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  -- Only changes that can take the Administrator profile away from someone matter.
  IF TG_TABLE_NAME = 'users' THEN
    IF NOT (OLD.is_active AND NOT NEW.is_active) THEN
      RETURN NULL;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM user_permission_profiles up JOIN permission_profiles p ON p.id = up.profile_id
                   WHERE up.user_id = OLD.id AND p.is_builtin) THEN
      RETURN NULL;
    END IF;
  ELSE
    IF NOT EXISTS (SELECT 1 FROM permission_profiles WHERE id = OLD.profile_id AND is_builtin) THEN
      RETURN NULL;
    END IF;
  END IF;
  PERFORM pg_advisory_xact_lock(hashtext('shadoucmdb.administrators'));
  IF NOT EXISTS (
    SELECT 1
    FROM users u
    JOIN user_permission_profiles up ON up.user_id = u.id
    JOIN permission_profiles p ON p.id = up.profile_id AND p.is_builtin
    WHERE u.is_active
  ) THEN
    RAISE EXCEPTION 'users: this change would leave no active user with the Administrator profile'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'users_last_administrator';
  END IF;
  RETURN NULL;
END;
$$;
--> statement-breakpoint
CREATE CONSTRAINT TRIGGER users_keep_one_administrator
  AFTER UPDATE OF is_active ON users
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW EXECUTE FUNCTION users_keep_one_administrator();
--> statement-breakpoint
CREATE CONSTRAINT TRIGGER user_permission_profiles_keep_one_administrator
  AFTER DELETE ON user_permission_profiles
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW EXECUTE FUNCTION users_keep_one_administrator();
