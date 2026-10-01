-- Business services, part 2 (v0.3.0, SHAA-929, spec SHAA-927 §1.4, §1.6, §6.1):
-- user groups and the owners of a business service.
--
-- user_groups: a named set of users, managed with the users.manage right.
-- Identity data like users, so not part of the configuration export. Names
-- are unique regardless of case, the same rule as usernames (a unique index
-- on lower(name) rather than citext, which would need an extension that a
-- managed PostgreSQL may not offer). Deleting a group removes its
-- memberships and the ownerships it held.
--
-- business_service_owners: per service, two roles (technical, business), each
-- held by users or user groups, one per row, in assignment order (position).
-- The service is a CI of the business service class (checked by trigger). Rows
-- go with the user, the group or the purged CI (ON DELETE CASCADE); a soft-
-- deleted service keeps them, so a restore brings them back. The API limits a
-- role to 10 owners.
--
-- New, empty tables; no existing data changes.

CREATE TABLE cmdb.user_groups (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  name text NOT NULL,
  description text,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  updated_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT user_groups_name_not_blank CHECK (length(btrim(name)) > 0)
);
--> statement-breakpoint
CREATE UNIQUE INDEX user_groups_name_uq ON cmdb.user_groups (lower(name));
--> statement-breakpoint
CREATE TRIGGER user_groups_set_updated_at BEFORE UPDATE ON cmdb.user_groups
  FOR EACH ROW EXECUTE FUNCTION cmdb.set_updated_at();
--> statement-breakpoint

CREATE TABLE cmdb.user_group_members (
  group_id uuid NOT NULL REFERENCES cmdb.user_groups (id) ON DELETE CASCADE,
  user_id uuid NOT NULL REFERENCES cmdb.users (id) ON DELETE CASCADE,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  PRIMARY KEY (group_id, user_id)
);
--> statement-breakpoint
-- The groups of a user ("My services" through a group, user deletion).
CREATE INDEX user_group_members_user_idx ON cmdb.user_group_members (user_id);
--> statement-breakpoint

CREATE TABLE cmdb.business_service_owners (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  service_ci_id uuid NOT NULL REFERENCES cmdb.configuration_items (id) ON DELETE CASCADE,
  role text NOT NULL,
  user_id uuid REFERENCES cmdb.users (id) ON DELETE CASCADE,
  group_id uuid REFERENCES cmdb.user_groups (id) ON DELETE CASCADE,
  position integer NOT NULL,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT business_service_owners_role_valid CHECK (role IN ('technical', 'business')),
  CONSTRAINT business_service_owners_one_principal CHECK (num_nonnulls(user_id, group_id) = 1),
  CONSTRAINT business_service_owners_position_valid CHECK (position >= 0),
  CONSTRAINT business_service_owners_user_uq UNIQUE (service_ci_id, role, user_id),
  CONSTRAINT business_service_owners_group_uq UNIQUE (service_ci_id, role, group_id)
);
--> statement-breakpoint
-- The owners of a service in order; "My services" and the delete counts.
CREATE INDEX business_service_owners_service_idx ON cmdb.business_service_owners (service_ci_id, role, position);
--> statement-breakpoint
CREATE INDEX business_service_owners_user_idx ON cmdb.business_service_owners (user_id) WHERE user_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX business_service_owners_group_idx ON cmdb.business_service_owners (group_id) WHERE group_id IS NOT NULL;
--> statement-breakpoint

CREATE FUNCTION cmdb.business_service_owners_service() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM configuration_items ci JOIN ci_classes c ON c.id = ci.class_id
    WHERE ci.id = NEW.service_ci_id AND c.system_role = 'business_service'
  ) THEN
    RAISE EXCEPTION 'business_service_owners: only a business service has owners'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'business_service_owners_service',
            TABLE = 'business_service_owners', COLUMN = 'service_ci_id';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER business_service_owners_service
  BEFORE INSERT OR UPDATE OF service_ci_id ON cmdb.business_service_owners
  FOR EACH ROW EXECUTE FUNCTION cmdb.business_service_owners_service();
--> statement-breakpoint

-- A service also keeps its type while it has owners (0033 checked members only).
CREATE OR REPLACE FUNCTION cmdb.configuration_items_keep_service() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM ci_classes WHERE id = OLD.class_id AND system_role = 'business_service')
     AND (EXISTS (
       SELECT 1 FROM ci_relationships e JOIN relationship_types t ON t.id = e.relationship_type_id
       WHERE e.source_ci_id = OLD.id AND e.deleted_at IS NULL AND t.system_role = 'business_service_member')
     OR EXISTS (SELECT 1 FROM business_service_owners o WHERE o.service_ci_id = OLD.id))
  THEN
    RAISE EXCEPTION 'configuration_items: a business service with members or owners cannot change its type; remove them first'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'configuration_items_service_members',
            TABLE = 'configuration_items', COLUMN = 'class_id';
  END IF;
  RETURN NEW;
END;
$$;
