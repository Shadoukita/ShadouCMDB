-- Integrity rules that a declarative schema cannot express: hierarchy cycles,
-- inheritance-aware attribute/relationship validation, append-only audit, and
-- updated_at maintenance. Hand-written; drizzle-kit does not model triggers.
-- Errors use SQLSTATE 23514 (check_violation) or 23505 (unique_violation) so the
-- API can map them to validation errors the same way as native constraints.

-- ---------------------------------------------------------------------------
-- updated_at maintenance
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION set_updated_at() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  NEW.updated_at := now();
  RETURN NEW;
END;
$$;
--> statement-breakpoint
DO $$
DECLARE
  t text;
BEGIN
  FOREACH t IN ARRAY ARRAY[
    'statuses', 'environments', 'locations', 'owners', 'ci_classes',
    'ci_attribute_definitions', 'configuration_items', 'ci_attribute_values',
    'relationship_types', 'relationship_type_rules', 'ci_relationships'
  ] LOOP
    EXECUTE format(
      'CREATE TRIGGER %I BEFORE UPDATE ON %I FOR EACH ROW EXECUTE FUNCTION set_updated_at()',
      t || '_set_updated_at', t);
  END LOOP;
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Class hierarchy helpers (also useful to the API for inherited attributes)
-- ---------------------------------------------------------------------------
-- The class itself plus every ancestor, nearest first.
CREATE OR REPLACE FUNCTION ci_class_lineage(p_class_id uuid)
RETURNS TABLE (class_id uuid, depth integer)
LANGUAGE sql STABLE AS $$
  WITH RECURSIVE up AS (
    SELECT c.id, c.parent_id, 0 AS depth FROM ci_classes c WHERE c.id = p_class_id
    UNION ALL
    SELECT c.id, c.parent_id, up.depth + 1
    FROM ci_classes c JOIN up ON c.id = up.parent_id
    WHERE up.depth < 64
  )
  SELECT id, depth FROM up;
$$;
--> statement-breakpoint
CREATE OR REPLACE FUNCTION ci_class_is_a(p_class_id uuid, p_ancestor_id uuid)
RETURNS boolean
LANGUAGE sql STABLE AS $$
  SELECT EXISTS (SELECT 1 FROM ci_class_lineage(p_class_id) l WHERE l.class_id = p_ancestor_id);
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- No cycles in the class and location trees
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION ci_classes_prevent_cycle() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.parent_id IS NOT NULL AND ci_class_is_a(NEW.parent_id, NEW.id) THEN
    RAISE EXCEPTION 'ci_classes: setting parent % on % would create a cycle', NEW.parent_id, NEW.id
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_no_cycle';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER ci_classes_prevent_cycle
  BEFORE INSERT OR UPDATE OF parent_id ON ci_classes
  FOR EACH ROW EXECUTE FUNCTION ci_classes_prevent_cycle();
--> statement-breakpoint
CREATE OR REPLACE FUNCTION locations_prevent_cycle() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.parent_id IS NOT NULL AND EXISTS (
    WITH RECURSIVE up AS (
      SELECT id, parent_id, 0 AS depth FROM locations WHERE id = NEW.parent_id
      UNION ALL
      SELECT l.id, l.parent_id, up.depth + 1 FROM locations l JOIN up ON l.id = up.parent_id
      WHERE up.depth < 64
    )
    SELECT 1 FROM up WHERE id = NEW.id
  ) THEN
    RAISE EXCEPTION 'locations: setting parent % on % would create a cycle', NEW.parent_id, NEW.id
      USING ERRCODE = 'check_violation', CONSTRAINT = 'locations_no_cycle';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER locations_prevent_cycle
  BEFORE INSERT OR UPDATE OF parent_id ON locations
  FOR EACH ROW EXECUTE FUNCTION locations_prevent_cycle();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Configuration items: concrete, active class; class change keeps values valid
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION configuration_items_validate() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  cls record;
BEGIN
  IF TG_OP = 'INSERT' OR NEW.class_id IS DISTINCT FROM OLD.class_id THEN
    SELECT is_abstract, is_active, key INTO cls FROM ci_classes WHERE id = NEW.class_id;
    IF cls.is_abstract THEN
      RAISE EXCEPTION 'configuration_items: class % is abstract and cannot hold CIs', cls.key
        USING ERRCODE = 'check_violation', CONSTRAINT = 'configuration_items_class_concrete';
    END IF;
    IF NOT cls.is_active THEN
      RAISE EXCEPTION 'configuration_items: class % is inactive', cls.key
        USING ERRCODE = 'check_violation', CONSTRAINT = 'configuration_items_class_active';
    END IF;
  END IF;

  IF TG_OP = 'UPDATE' AND NEW.class_id IS DISTINCT FROM OLD.class_id AND EXISTS (
    SELECT 1
    FROM ci_attribute_values v
    JOIN ci_attribute_definitions d ON d.id = v.attribute_id
    WHERE v.ci_id = NEW.id AND NOT ci_class_is_a(NEW.class_id, d.class_id)
  ) THEN
    RAISE EXCEPTION 'configuration_items: CI % has attribute values not defined on the new class; remove them first', NEW.id
      USING ERRCODE = 'check_violation', CONSTRAINT = 'configuration_items_class_change_attributes';
  END IF;

  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER configuration_items_validate
  BEFORE INSERT OR UPDATE OF class_id ON configuration_items
  FOR EACH ROW EXECUTE FUNCTION configuration_items_validate();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Attribute values: right class lineage, right value column, legal value
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION ci_attribute_values_validate() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  def record;
  ci_class uuid;
  ref_class uuid;
  expected text;
  populated text;
BEGIN
  SELECT d.key, d.class_id, d.data_type, d.enum_values, d.reference_class_id
    INTO def FROM ci_attribute_definitions d WHERE d.id = NEW.attribute_id;
  SELECT class_id INTO ci_class FROM configuration_items WHERE id = NEW.ci_id;

  IF NOT ci_class_is_a(ci_class, def.class_id) THEN
    RAISE EXCEPTION 'ci_attribute_values: attribute % is not defined on the class of CI % or its ancestors', def.key, NEW.ci_id
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_values_attribute_in_class';
  END IF;

  expected := CASE def.data_type
    WHEN 'text' THEN 'value_text'
    WHEN 'enum' THEN 'value_text'
    WHEN 'number' THEN 'value_number'
    WHEN 'integer' THEN 'value_number'
    WHEN 'boolean' THEN 'value_boolean'
    WHEN 'date' THEN 'value_date'
    WHEN 'datetime' THEN 'value_datetime'
    WHEN 'ip' THEN 'value_ip'
    WHEN 'cidr' THEN 'value_cidr'
    WHEN 'reference' THEN 'value_ref_ci_id'
  END;
  populated := CASE
    WHEN NEW.value_text IS NOT NULL THEN 'value_text'
    WHEN NEW.value_number IS NOT NULL THEN 'value_number'
    WHEN NEW.value_boolean IS NOT NULL THEN 'value_boolean'
    WHEN NEW.value_date IS NOT NULL THEN 'value_date'
    WHEN NEW.value_datetime IS NOT NULL THEN 'value_datetime'
    WHEN NEW.value_ip IS NOT NULL THEN 'value_ip'
    WHEN NEW.value_cidr IS NOT NULL THEN 'value_cidr'
    WHEN NEW.value_ref_ci_id IS NOT NULL THEN 'value_ref_ci_id'
  END;
  IF populated IS DISTINCT FROM expected THEN
    RAISE EXCEPTION 'ci_attribute_values: attribute % is of type % and must be stored in %, got %',
      def.key, def.data_type, expected, populated
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_values_type_match';
  END IF;

  IF def.data_type = 'integer' AND NEW.value_number <> trunc(NEW.value_number) THEN
    RAISE EXCEPTION 'ci_attribute_values: attribute % must be an integer', def.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_values_integer';
  END IF;

  IF def.data_type = 'enum' AND NOT (def.enum_values ? NEW.value_text) THEN
    RAISE EXCEPTION 'ci_attribute_values: % is not an allowed value for attribute %', NEW.value_text, def.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_values_enum';
  END IF;

  IF def.data_type = 'reference' THEN
    SELECT class_id INTO ref_class FROM configuration_items WHERE id = NEW.value_ref_ci_id;
    IF NOT ci_class_is_a(ref_class, def.reference_class_id) THEN
      RAISE EXCEPTION 'ci_attribute_values: attribute % must reference a CI of the configured class', def.key
        USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_values_reference_class';
    END IF;
    IF NEW.value_ref_ci_id = NEW.ci_id THEN
      RAISE EXCEPTION 'ci_attribute_values: attribute % cannot reference its own CI', def.key
        USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_values_reference_self';
    END IF;
  END IF;

  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER ci_attribute_values_validate
  BEFORE INSERT OR UPDATE ON ci_attribute_values
  FOR EACH ROW EXECUTE FUNCTION ci_attribute_values_validate();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Relationships: legal endpoint classes, live endpoints, symmetric duplicates
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION ci_relationships_validate() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  rt record;
  src record;
  tgt record;
BEGIN
  -- Only live edges are validated; soft-deleting an edge is always allowed.
  IF NEW.deleted_at IS NOT NULL THEN
    RETURN NEW;
  END IF;

  SELECT key, is_directional, is_active INTO rt FROM relationship_types WHERE id = NEW.relationship_type_id;
  SELECT class_id, deleted_at INTO src FROM configuration_items WHERE id = NEW.source_ci_id;
  SELECT class_id, deleted_at INTO tgt FROM configuration_items WHERE id = NEW.target_ci_id;

  IF TG_OP = 'INSERT' AND NOT rt.is_active THEN
    RAISE EXCEPTION 'ci_relationships: relationship type % is inactive', rt.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_relationships_type_active';
  END IF;

  IF src.deleted_at IS NOT NULL OR tgt.deleted_at IS NOT NULL THEN
    RAISE EXCEPTION 'ci_relationships: cannot link a deleted CI'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_relationships_live_endpoints';
  END IF;

  IF NOT EXISTS (
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
CREATE TRIGGER ci_relationships_validate
  BEFORE INSERT OR UPDATE OF relationship_type_id, source_ci_id, target_ci_id, deleted_at ON ci_relationships
  FOR EACH ROW EXECUTE FUNCTION ci_relationships_validate();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- audit_log is append-only
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION audit_log_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'audit_log is append-only (% rejected)', TG_OP
    USING ERRCODE = 'insufficient_privilege';
END;
$$;
--> statement-breakpoint
CREATE TRIGGER audit_log_append_only
  BEFORE UPDATE OR DELETE ON audit_log
  FOR EACH ROW EXECUTE FUNCTION audit_log_append_only();
