-- Data-quality fields of a type (SHAA-2351): which field holds a CI's owner
-- and which its end of life, for the "Needs attention" checks
-- (GET /api/v1/configuration-items/data-quality).
--
--   * ci_classes.owner_attribute_id: a text, enum, lookup or reference field
--     of the type or an ancestor. A CI of the type without a value there has
--     no owner.
--   * ci_classes.end_of_life_attribute_id: a date or datetime field of the
--     type or an ancestor. Its value is the CI's end of life.
--   * NULL on a type means its parent's setting applies (resolved at query
--     time), so one setting on a root type covers every subtype; a type with
--     no setting in its lineage is left out of that check.
--   * The same lineage rule as the title attribute (0016): a field outside the
--     lineage is refused, and a type that moves away from the field it named
--     loses the setting (the API then clears stale settings of its subtypes).
--     Deleting the field clears the setting (ON DELETE SET NULL).
--
-- Existing installs: a type that defines a field keyed "owner" (the lookup
-- field 0016 made from the old owner column) gets it as its owner field, and a
-- type that defines a date or datetime field keyed "end_of_life",
-- "end_of_life_date", "eol" or "eol_date" gets it as its end-of-life field.
-- Each such type gets an `update` row in the audit log (actor "migration
-- 0068"). updated_at is left alone, as for the backfills of 0016. No CI data
-- changes.

ALTER TABLE cmdb.ci_classes
  ADD COLUMN owner_attribute_id uuid REFERENCES cmdb.ci_attribute_definitions (id) ON DELETE SET NULL,
  ADD COLUMN end_of_life_attribute_id uuid REFERENCES cmdb.ci_attribute_definitions (id) ON DELETE SET NULL;
--> statement-breakpoint
-- For the foreign keys (a field delete looks up the types naming it).
CREATE INDEX ci_classes_owner_attribute_idx ON cmdb.ci_classes (owner_attribute_id)
  WHERE owner_attribute_id IS NOT NULL;
--> statement-breakpoint
CREATE INDEX ci_classes_end_of_life_attribute_idx ON cmdb.ci_classes (end_of_life_attribute_id)
  WHERE end_of_life_attribute_id IS NOT NULL;
--> statement-breakpoint

-- Data types an owner field may have. The API checks the same list.
CREATE FUNCTION cmdb.owner_data_type(p_data_type text) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
  SELECT p_data_type IN ('text', 'enum', 'lookup', 'reference');
$$;
--> statement-breakpoint
-- Data types an end-of-life field may have. The API checks the same list.
CREATE FUNCTION cmdb.end_of_life_data_type(p_data_type text) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
  SELECT p_data_type IN ('date', 'datetime');
$$;
--> statement-breakpoint

CREATE FUNCTION cmdb.ci_classes_quality_attributes() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
DECLARE
  def record;
BEGIN
  IF NEW.owner_attribute_id IS NOT NULL THEN
    SELECT class_id, data_type INTO def FROM cmdb.ci_attribute_definitions WHERE id = NEW.owner_attribute_id;
    -- The lineage as it will be: the row itself is not (or not yet so) in the table.
    IF NOT (def.class_id = NEW.id OR (NEW.parent_id IS NOT NULL AND cmdb.ci_class_is_a(NEW.parent_id, def.class_id))) THEN
      IF TG_OP = 'UPDATE' AND NEW.owner_attribute_id IS NOT DISTINCT FROM OLD.owner_attribute_id THEN
        NEW.owner_attribute_id := NULL;
      ELSE
        RAISE EXCEPTION 'ci_classes: the owner field must be defined on class % or one of its ancestors', NEW.key
          USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_owner_attribute_in_lineage';
      END IF;
    ELSIF NOT cmdb.owner_data_type(def.data_type) THEN
      RAISE EXCEPTION 'ci_classes: a % field cannot be the owner field of class %', def.data_type, NEW.key
        USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_owner_attribute_type';
    END IF;
  END IF;
  IF NEW.end_of_life_attribute_id IS NOT NULL THEN
    SELECT class_id, data_type INTO def FROM cmdb.ci_attribute_definitions WHERE id = NEW.end_of_life_attribute_id;
    IF NOT (def.class_id = NEW.id OR (NEW.parent_id IS NOT NULL AND cmdb.ci_class_is_a(NEW.parent_id, def.class_id))) THEN
      IF TG_OP = 'UPDATE' AND NEW.end_of_life_attribute_id IS NOT DISTINCT FROM OLD.end_of_life_attribute_id THEN
        NEW.end_of_life_attribute_id := NULL;
      ELSE
        RAISE EXCEPTION 'ci_classes: the end-of-life field must be defined on class % or one of its ancestors', NEW.key
          USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_end_of_life_attribute_in_lineage';
      END IF;
    ELSIF NOT cmdb.end_of_life_data_type(def.data_type) THEN
      RAISE EXCEPTION 'ci_classes: a % field cannot be the end-of-life field of class %', def.data_type, NEW.key
        USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_end_of_life_attribute_type';
    END IF;
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER ci_classes_quality_attributes
  BEFORE INSERT OR UPDATE OF owner_attribute_id, end_of_life_attribute_id, parent_id ON cmdb.ci_classes
  FOR EACH ROW EXECUTE FUNCTION cmdb.ci_classes_quality_attributes();
--> statement-breakpoint

-- Backfill: the type's own field of the conventional key (keys are unique in a lineage).
ALTER TABLE cmdb.ci_classes DISABLE TRIGGER ci_classes_set_updated_at;
--> statement-breakpoint
WITH found AS (
  SELECT c.id, c.key,
         (SELECT d.id FROM cmdb.ci_attribute_definitions d
          WHERE d.class_id = c.id AND d.key = 'owner' AND cmdb.owner_data_type(d.data_type)) AS owner_id,
         (SELECT d.id FROM cmdb.ci_attribute_definitions d
          WHERE d.class_id = c.id AND d.key IN ('end_of_life', 'end_of_life_date', 'eol', 'eol_date')
            AND cmdb.end_of_life_data_type(d.data_type)
          ORDER BY array_position(ARRAY['end_of_life', 'end_of_life_date', 'eol', 'eol_date'], d.key) LIMIT 1) AS eol_id
  FROM cmdb.ci_classes c
),
written AS (
  UPDATE cmdb.ci_classes c
  SET owner_attribute_id = f.owner_id, end_of_life_attribute_id = f.eol_id
  FROM found f
  WHERE c.id = f.id AND (f.owner_id IS NOT NULL OR f.eol_id IS NOT NULL)
  RETURNING c.id, c.key, c.owner_attribute_id, c.end_of_life_attribute_id
)
INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
SELECT 'system', 'migration 0068', 'update', 'ci_classes', w.id,
       jsonb_build_object('key', w.key, 'ownerAttributeId', NULL, 'endOfLifeAttributeId', NULL),
       jsonb_build_object('key', w.key, 'ownerAttributeId', w.owner_attribute_id,
                          'endOfLifeAttributeId', w.end_of_life_attribute_id,
                          'comment', 'Data-quality fields set from the conventional field keys')
FROM written w
ORDER BY w.key;
--> statement-breakpoint
ALTER TABLE cmdb.ci_classes ENABLE TRIGGER ci_classes_set_updated_at;
