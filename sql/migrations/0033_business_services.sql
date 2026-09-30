-- Business services, part 1 (v0.3.0, SHAA-929, spec SHAA-927 §1 and §6.1).
--
-- A business service is a CI of one built-in class, marked
-- ci_classes.system_role = 'business_service'. Its members are ordinary
-- ci_relationships rows of one built-in relationship type, marked
-- relationship_types.system_role = 'business_service_member', with the
-- service as source and the member as target. The type propagates impact
-- target_to_source: when a member fails, its services are affected.
--
-- The service class:
--   * An installation whose starter class "service" is still the template's
--     (key service, no parent, active, no subtypes, a service_tier field)
--     adopts it: only system_role is set. Same id, table, CIs, values and
--     grants; updated_at is left alone.
--   * Anywhere else a new type "Business service" is created: key
--     business_service (business_service_2, … if taken), with a required
--     "name" field as its title, in a new area "Business services" (key
--     business_services, suffixed if taken). A new installation has no area
--     yet, and a type cannot move between areas later, so it gets its own.
--     The area's schema and the type's table are created by the reconcile
--     that `shadoucmdb migrate` runs right after the migrations.
--
-- The API finds the class and the type by their role, never by key.
--
-- Protections (the API refuses first; these are the backstop for psql,
-- restore scripts and any path the API misses):
--   * the service class cannot be deleted (so not purged either), archived,
--     made abstract or given a parent: ci_classes_system_protected;
--   * no type may have the service class as parent:
--     ci_classes_system_no_subclass (column parent_id);
--   * the member type cannot be deleted: relationship_types_system_protected;
--     its key, direction, impact direction and active flag are fixed:
--     relationship_types_system_fixed (with the column);
--   * no row's system role can change after this migration.
--   * a service CI that includes members cannot change type:
--     configuration_items_service_members.
--
-- Membership (ci_relationships of the member type), checked on insert, on a
-- change of type or endpoints, and on restore (deleted_at back to NULL):
--   * the source is a CI of the service class: membership_source;
--   * the source is not the target: membership_self;
--   * when the target is itself a service, under one transaction-level
--     advisory lock (so two concurrent inserts cannot jointly close a loop):
--     the target must not already include the source, directly or through
--     nesting (membership_cycle), and the longest chain of services
--     including services through the new edge must stay within the nesting
--     limit (membership_nesting_depth). A chain A includes B includes C is
--     2 levels deep. The limit is 8, or lower when the transaction sets
--     shadoucmdb.business_service_max_nesting (the API passes
--     BUSINESS_SERVICE_MAX_NESTING that way). The walks follow live member
--     edges between services only, bounded by the limit.
--   The check reads committed rows after taking the lock, which holds under
--   READ COMMITTED, the isolation level the API uses.
--   Member edges are exempt from relationship_type_rules: any class may be a
--   member.
--
-- No new index: ci_relationships_target_idx (target_ci_id,
-- relationship_type_id) serves "services containing CI X" and
-- ci_relationships_source_idx (source_ci_id, relationship_type_id) the member
-- lists and the walks.
--
-- ADD COLUMN without a default is metadata-only; the inserts touch single rows.
--
-- Rollback: not supported in place, as for every release. Downgrading to
-- 0.2.x means restoring the backup taken before the upgrade.

-- ---------------------------------------------------------------------------
-- System roles
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.ci_classes
  ADD COLUMN system_role text,
  ADD CONSTRAINT ci_classes_system_role_valid CHECK (system_role IN ('business_service')),
  ADD CONSTRAINT ci_classes_system_role_uq UNIQUE (system_role);
--> statement-breakpoint
ALTER TABLE cmdb.relationship_types
  ADD COLUMN system_role text,
  ADD CONSTRAINT relationship_types_system_role_valid CHECK (system_role IN ('business_service_member')),
  ADD CONSTRAINT relationship_types_system_role_uq UNIQUE (system_role);
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- The service class: adopt the template's "service", or create one
-- ---------------------------------------------------------------------------
DO $$
DECLARE
  adopt uuid;
  class_key text := 'business_service';
  area_key text := 'business_services';
  n integer := 1;
  new_area uuid;
  new_class uuid;
  name_field uuid;
BEGIN
  SELECT c.id INTO adopt
  FROM cmdb.ci_classes c
  WHERE c.key = 'service'
    AND c.parent_id IS NULL
    AND c.is_active
    AND NOT EXISTS (SELECT 1 FROM cmdb.ci_classes s WHERE s.parent_id = c.id)
    AND EXISTS (SELECT 1 FROM cmdb.ci_attribute_definitions d WHERE d.class_id = c.id AND d.key = 'service_tier');

  IF adopt IS NOT NULL THEN
    -- Not an edit: the class keeps its updated_at.
    ALTER TABLE cmdb.ci_classes DISABLE TRIGGER ci_classes_set_updated_at;
    UPDATE cmdb.ci_classes SET system_role = 'business_service' WHERE id = adopt;
    ALTER TABLE cmdb.ci_classes ENABLE TRIGGER ci_classes_set_updated_at;
    RETURN;
  END IF;

  WHILE EXISTS (SELECT 1 FROM cmdb.areas WHERE key = area_key)
     OR EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = area_key) LOOP
    n := n + 1;
    area_key := 'business_services_' || n;
  END LOOP;
  INSERT INTO cmdb.areas (key, name, description, sort_order)
  VALUES (area_key, 'Business services',
          'Business services and the configuration items they include',
          COALESCE((SELECT max(sort_order) + 10 FROM cmdb.areas), 0))
  RETURNING id INTO new_area;

  n := 1;
  WHILE EXISTS (SELECT 1 FROM cmdb.ci_classes WHERE key = class_key) LOOP
    n := n + 1;
    class_key := 'business_service_' || n;
  END LOOP;
  INSERT INTO cmdb.ci_classes (key, name, description, area_id, color, system_role)
  VALUES (class_key, 'Business service',
          'A service delivered to the business, and the configuration items it includes',
          new_area, '#cf222e', 'business_service')
  RETURNING id INTO new_class;

  INSERT INTO cmdb.ci_attribute_definitions (class_id, key, label, data_type, is_required, validation, sort_order)
  VALUES (new_class, 'name', 'Name', 'text', true, '{"maxLength": 200, "pattern": "\\S"}', 0)
  RETURNING id INTO name_field;
  UPDATE cmdb.ci_classes SET title_attribute_id = name_field WHERE id = new_class;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- The member relationship type, keyed business_service_member (…_2 if taken)
-- ---------------------------------------------------------------------------
DO $$
DECLARE
  type_key text := 'business_service_member';
  n integer := 1;
BEGIN
  WHILE EXISTS (SELECT 1 FROM cmdb.relationship_types WHERE key = type_key) LOOP
    n := n + 1;
    type_key := 'business_service_member_' || n;
  END LOOP;
  INSERT INTO cmdb.relationship_types
    (key, name, description, forward_label, reverse_label, is_directional, impact_direction, system_role, sort_order)
  VALUES (type_key, 'Service member',
          'A business service includes this configuration item. Managed on the business service.',
          'includes', 'is part of', true, 'target_to_source', 'business_service_member',
          COALESCE((SELECT max(sort_order) + 10 FROM cmdb.relationship_types), 0));
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Protections: the service class
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.ci_classes_keep_system() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    RAISE EXCEPTION 'ci_classes: the % type is the built-in business service type and cannot be deleted', OLD.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_protected';
  END IF;
  IF TG_OP = 'INSERT' AND NEW.system_role IS NOT NULL THEN
    RAISE EXCEPTION 'ci_classes: a new type cannot take a system role'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_protected';
  END IF;
  IF TG_OP = 'UPDATE' THEN
    IF NEW.system_role IS DISTINCT FROM OLD.system_role THEN
      RAISE EXCEPTION 'ci_classes: a type''s system role cannot change'
        USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_protected';
    END IF;
    IF OLD.system_role IS NOT NULL THEN
      IF NOT NEW.is_active THEN
        RAISE EXCEPTION 'ci_classes: the % type is the built-in business service type and cannot be archived', OLD.key
          USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_protected';
      END IF;
      IF NEW.is_abstract THEN
        RAISE EXCEPTION 'ci_classes: the % type is the built-in business service type and cannot be abstract', OLD.key
          USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_protected';
      END IF;
      IF NEW.parent_id IS NOT NULL THEN
        RAISE EXCEPTION 'ci_classes: the % type is the built-in business service type and cannot have a parent type', OLD.key
          USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_protected';
      END IF;
    END IF;
  END IF;
  IF NEW.parent_id IS NOT NULL
     AND EXISTS (SELECT 1 FROM ci_classes p WHERE p.id = NEW.parent_id AND p.system_role IS NOT NULL) THEN
    RAISE EXCEPTION 'ci_classes: the built-in business service type cannot have subtypes'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_system_no_subclass',
            TABLE = 'ci_classes', COLUMN = 'parent_id';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER ci_classes_keep_system
  BEFORE DELETE ON cmdb.ci_classes
  FOR EACH ROW WHEN (OLD.system_role IS NOT NULL)
  EXECUTE FUNCTION cmdb.ci_classes_keep_system();
--> statement-breakpoint
CREATE TRIGGER ci_classes_keep_system_update
  BEFORE UPDATE OF system_role, is_active, is_abstract, parent_id ON cmdb.ci_classes
  FOR EACH ROW EXECUTE FUNCTION cmdb.ci_classes_keep_system();
--> statement-breakpoint
CREATE TRIGGER ci_classes_keep_system_insert
  BEFORE INSERT ON cmdb.ci_classes
  FOR EACH ROW WHEN (NEW.parent_id IS NOT NULL OR NEW.system_role IS NOT NULL)
  EXECUTE FUNCTION cmdb.ci_classes_keep_system();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Protections: the member type
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.relationship_types_keep_system() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
DECLARE
  fixed text;
BEGIN
  IF TG_OP = 'DELETE' THEN
    RAISE EXCEPTION 'relationship_types: the % type is the built-in business service membership and cannot be deleted', OLD.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'relationship_types_system_protected';
  END IF;
  IF TG_OP = 'INSERT' THEN
    RAISE EXCEPTION 'relationship_types: a new relationship type cannot take a system role'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'relationship_types_system_protected';
  END IF;
  IF NEW.system_role IS DISTINCT FROM OLD.system_role THEN
    RAISE EXCEPTION 'relationship_types: a relationship type''s system role cannot change'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'relationship_types_system_protected';
  END IF;
  IF OLD.system_role IS NOT NULL THEN
    fixed := CASE
      WHEN NEW.key IS DISTINCT FROM OLD.key THEN 'key'
      WHEN NEW.is_directional IS DISTINCT FROM OLD.is_directional THEN 'is_directional'
      WHEN NEW.impact_direction IS DISTINCT FROM OLD.impact_direction THEN 'impact_direction'
      WHEN NEW.is_active IS DISTINCT FROM OLD.is_active THEN 'is_active'
    END;
    IF fixed IS NOT NULL THEN
      RAISE EXCEPTION 'relationship_types: % of the built-in business service membership type cannot change', fixed
        USING ERRCODE = 'check_violation', CONSTRAINT = 'relationship_types_system_fixed',
              TABLE = 'relationship_types', COLUMN = fixed;
    END IF;
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER relationship_types_keep_system
  BEFORE DELETE ON cmdb.relationship_types
  FOR EACH ROW WHEN (OLD.system_role IS NOT NULL)
  EXECUTE FUNCTION cmdb.relationship_types_keep_system();
--> statement-breakpoint
CREATE TRIGGER relationship_types_keep_system_update
  BEFORE UPDATE OF system_role, key, is_directional, impact_direction, is_active ON cmdb.relationship_types
  FOR EACH ROW EXECUTE FUNCTION cmdb.relationship_types_keep_system();
--> statement-breakpoint
CREATE TRIGGER relationship_types_keep_system_insert
  BEFORE INSERT ON cmdb.relationship_types
  FOR EACH ROW WHEN (NEW.system_role IS NOT NULL)
  EXECUTE FUNCTION cmdb.relationship_types_keep_system();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Member edges: any class may be a member, so relationship_type_rules do not
-- apply to the member type. Everything else is unchanged from 0012.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION cmdb.ci_relationships_validate() RETURNS trigger
LANGUAGE plpgsql
SET search_path = cmdb, public
AS $$
DECLARE
  rt record;
  src record;
  tgt record;
BEGIN
  -- Only live edges are validated; soft-deleting an edge is always allowed.
  IF NEW.deleted_at IS NOT NULL THEN
    RETURN NEW;
  END IF;

  SELECT key, is_directional, is_active, system_role INTO rt FROM relationship_types WHERE id = NEW.relationship_type_id;
  SELECT class_id, deleted_at INTO src FROM configuration_items WHERE id = NEW.source_ci_id;
  SELECT class_id, deleted_at INTO tgt FROM configuration_items WHERE id = NEW.target_ci_id;

  IF TG_OP = 'INSERT' AND NOT rt.is_active THEN
    RAISE EXCEPTION 'ci_relationships: relationship type % is inactive', rt.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_relationships_type_active';
  END IF;

  IF src.deleted_at IS NOT NULL THEN
    RAISE EXCEPTION 'ci_relationships: cannot link a deleted CI (the source CI is deleted)'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_relationships_live_endpoints',
            TABLE = 'ci_relationships', COLUMN = 'source_ci_id';
  END IF;
  IF tgt.deleted_at IS NOT NULL THEN
    RAISE EXCEPTION 'ci_relationships: cannot link a deleted CI (the target CI is deleted)'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_relationships_live_endpoints',
            TABLE = 'ci_relationships', COLUMN = 'target_ci_id';
  END IF;

  IF rt.system_role IS DISTINCT FROM 'business_service_member' AND NOT EXISTS (
    SELECT 1 FROM relationship_type_rules r
    WHERE r.relationship_type_id = NEW.relationship_type_id
      AND ci_class_is_a(src.class_id, r.source_class_id)
      AND ci_class_is_a(tgt.class_id, r.target_class_id)
  ) AND NOT (NOT rt.is_directional AND EXISTS (
    SELECT 1 FROM relationship_type_rules r
    WHERE r.relationship_type_id = NEW.relationship_type_id
      AND ci_class_is_a(tgt.class_id, r.source_class_id)
      AND ci_class_is_a(src.class_id, r.target_class_id)
  )) THEN
    RAISE EXCEPTION 'ci_relationships: % is not allowed between these CI classes', rt.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_relationships_endpoint_rule';
  END IF;

  IF NOT rt.is_directional THEN
    -- Serialise concurrent inserts of the same unordered pair, then check the reverse edge.
    PERFORM pg_advisory_xact_lock(hashtextextended(
      NEW.relationship_type_id::text || least(NEW.source_ci_id, NEW.target_ci_id)::text
        || greatest(NEW.source_ci_id, NEW.target_ci_id)::text, 0));
    IF EXISTS (
      SELECT 1 FROM ci_relationships e
      WHERE e.relationship_type_id = NEW.relationship_type_id
        AND e.source_ci_id = NEW.target_ci_id
        AND e.target_ci_id = NEW.source_ci_id
        AND e.deleted_at IS NULL
        AND e.id <> NEW.id
    ) THEN
      RAISE EXCEPTION 'ci_relationships: % already exists between these CIs', rt.key
        USING ERRCODE = 'unique_violation', CONSTRAINT = 'ci_relationships_live_edge_uq';
    END IF;
  END IF;

  RETURN NEW;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Membership: source class, self, cycle and nesting depth
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.ci_relationships_membership() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
DECLARE
  member_type uuid;
  service_class uuid;
  max_nesting integer := 8;
  requested text;
  above integer;
  below integer;
BEGIN
  SELECT id INTO member_type FROM relationship_types WHERE system_role = 'business_service_member';
  IF NEW.relationship_type_id IS DISTINCT FROM member_type THEN
    RETURN NEW;
  END IF;
  -- An update that neither moves nor revives a live member edge needs no check.
  IF TG_OP = 'UPDATE' AND NEW.source_ci_id = OLD.source_ci_id AND NEW.target_ci_id = OLD.target_ci_id
     AND NEW.relationship_type_id = OLD.relationship_type_id
     AND (NEW.deleted_at IS NOT NULL OR OLD.deleted_at IS NULL) THEN
    RETURN NEW;
  END IF;

  SELECT id INTO service_class FROM ci_classes WHERE system_role = 'business_service';
  IF NOT EXISTS (SELECT 1 FROM configuration_items WHERE id = NEW.source_ci_id AND class_id = service_class) THEN
    RAISE EXCEPTION 'ci_relationships: only a business service can include members'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'membership_source',
            TABLE = 'ci_relationships', COLUMN = 'source_ci_id';
  END IF;
  IF NEW.source_ci_id = NEW.target_ci_id THEN
    RAISE EXCEPTION 'ci_relationships: a business service cannot be a member of itself'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'membership_self',
            TABLE = 'ci_relationships', COLUMN = 'target_ci_id';
  END IF;
  -- A deleted edge joins no chain; it is checked again when it is restored.
  IF NEW.deleted_at IS NOT NULL
     OR NOT EXISTS (SELECT 1 FROM configuration_items WHERE id = NEW.target_ci_id AND class_id = service_class) THEN
    RETURN NEW;
  END IF;

  -- Service-in-service edges are added one at a time across all sessions, and
  -- the walks below read what committed before the lock was granted.
  PERFORM pg_advisory_xact_lock(hashtext('shadoucmdb:business-service-membership'));

  requested := current_setting('shadoucmdb.business_service_max_nesting', true);
  IF requested ~ '^[0-9]{1,2}$' THEN
    max_nesting := least(greatest(requested::integer, 1), 8);
  END IF;

  -- Down from the member: the services it includes, and theirs. Reaching the
  -- new edge's source means the edge would close a loop. The walks go one
  -- level past the ceiling of 8 (no stored chain is longer), and UNION keeps
  -- them to one row per service and depth however the services share members.
  WITH RECURSIVE down (ci, depth) AS (
    SELECT NEW.target_ci_id, 0
    UNION
    SELECT e.target_ci_id, d.depth + 1
    FROM down d
    JOIN ci_relationships e ON e.source_ci_id = d.ci AND e.relationship_type_id = member_type AND e.deleted_at IS NULL
    JOIN configuration_items m ON m.id = e.target_ci_id AND m.class_id = service_class
    WHERE d.depth < 9 AND d.ci <> NEW.source_ci_id
  )
  SELECT CASE WHEN bool_or(ci = NEW.source_ci_id) THEN -1 ELSE max(depth) END INTO below FROM down;
  IF below = -1 THEN
    RAISE EXCEPTION 'ci_relationships: the member already includes this business service; adding it would create a loop'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'membership_cycle',
            TABLE = 'ci_relationships', COLUMN = 'target_ci_id';
  END IF;

  -- Up from the service: the services that include it, and theirs.
  WITH RECURSIVE up (ci, depth) AS (
    SELECT NEW.source_ci_id, 0
    UNION
    SELECT e.source_ci_id, u.depth + 1
    FROM up u
    JOIN ci_relationships e ON e.target_ci_id = u.ci AND e.relationship_type_id = member_type AND e.deleted_at IS NULL
    WHERE u.depth < 9
  )
  SELECT max(depth) INTO above FROM up;

  IF above + 1 + below > max_nesting THEN
    RAISE EXCEPTION 'ci_relationships: business services can be nested at most % levels deep', max_nesting
      USING ERRCODE = 'check_violation', CONSTRAINT = 'membership_nesting_depth',
            TABLE = 'ci_relationships', COLUMN = 'target_ci_id';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER ci_relationships_membership
  BEFORE INSERT OR UPDATE OF source_ci_id, target_ci_id, relationship_type_id, deleted_at ON cmdb.ci_relationships
  FOR EACH ROW EXECUTE FUNCTION cmdb.ci_relationships_membership();
--> statement-breakpoint

-- A service keeps its type while it includes members (the members' source
-- must stay a service).
CREATE FUNCTION cmdb.configuration_items_keep_service() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM ci_classes WHERE id = OLD.class_id AND system_role = 'business_service')
     AND EXISTS (
       SELECT 1 FROM ci_relationships e JOIN relationship_types t ON t.id = e.relationship_type_id
       WHERE e.source_ci_id = OLD.id AND e.deleted_at IS NULL AND t.system_role = 'business_service_member')
  THEN
    RAISE EXCEPTION 'configuration_items: a business service that includes members cannot change its type; remove its members first'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'configuration_items_service_members',
            TABLE = 'configuration_items', COLUMN = 'class_id';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER configuration_items_keep_service
  BEFORE UPDATE OF class_id ON cmdb.configuration_items
  FOR EACH ROW WHEN (NEW.class_id IS DISTINCT FROM OLD.class_id)
  EXECUTE FUNCTION cmdb.configuration_items_keep_service();
