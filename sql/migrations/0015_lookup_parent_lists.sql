-- Parent/child lookup lists (cascading dropdowns, SHAA-268).
--
-- Additive only: every existing list, value and attribute keeps its data and
-- starts without a parent, so nothing changes until an administrator links
-- lists.
--
-- lookup_lists.parent_list_id: a list can depend on another list ("Model"
-- depends on "Manufacturer"). No self-parent, no cycles. A list that is the
-- parent of another list cannot be deleted (RESTRICT).
--
-- lookup_list_values.parent_value_id: the value of the parent list a child
-- value belongs to ("C9300" belongs to "Cisco"). Rules (trigger below):
--   * a value of a list without a parent has no parent value;
--   * a parent value must belong to the list's parent list;
--   * a new value of a child list needs a parent value, and an assigned
--     parent cannot be removed again. The one exception is a value whose
--     parent became stale because its list got another parent list (or
--     none): the API clears those in the same transaction, audited.
-- A value that is the parent of other values cannot be deleted (foreign key).
--
-- ci_attribute_definitions.parent_attribute_id: a lookup field on a child
-- list names the field (on the same class or an ancestor) bound to the parent
-- list. CI writes then only accept a child value whose parent value is the
-- CI's value of that field (checked by the API: the values live in the
-- per-type tables). Optional: a child-list field without it accepts any active
-- value of its list.
--
-- Changing a list's parent (constraint trigger at commit): no value of the
-- list may keep a parent value outside the new parent list, and no field
-- bound to the list may keep a parent field bound to another list.
--
-- Soft delete: unchanged (values and lists are retired with is_active).

ALTER TABLE cmdb.lookup_lists
  ADD COLUMN parent_list_id uuid REFERENCES cmdb.lookup_lists (id) ON DELETE RESTRICT,
  ADD CONSTRAINT lookup_lists_not_own_parent CHECK (parent_list_id IS DISTINCT FROM id);
--> statement-breakpoint
CREATE INDEX lookup_lists_parent_idx ON cmdb.lookup_lists (parent_list_id) WHERE parent_list_id IS NOT NULL;
--> statement-breakpoint

ALTER TABLE cmdb.lookup_list_values
  ADD COLUMN parent_value_id uuid REFERENCES cmdb.lookup_list_values (id),
  ADD CONSTRAINT lookup_list_values_not_own_parent CHECK (parent_value_id IS DISTINCT FROM id);
--> statement-breakpoint
CREATE INDEX lookup_list_values_parent_idx ON cmdb.lookup_list_values (parent_value_id)
  WHERE parent_value_id IS NOT NULL;
--> statement-breakpoint

-- NO ACTION (checked at the end of the statement): purging a type deletes its
-- fields in one statement, parents and children together.
ALTER TABLE cmdb.ci_attribute_definitions
  ADD COLUMN parent_attribute_id uuid REFERENCES cmdb.ci_attribute_definitions (id),
  ADD CONSTRAINT ci_attribute_definitions_not_own_parent CHECK (parent_attribute_id IS DISTINCT FROM id);
--> statement-breakpoint
CREATE INDEX ci_attribute_definitions_parent_idx ON cmdb.ci_attribute_definitions (parent_attribute_id)
  WHERE parent_attribute_id IS NOT NULL;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Lists: no cycles
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.lookup_lists_prevent_cycle() RETURNS trigger
LANGUAGE plpgsql
SET search_path = cmdb, public
AS $$
BEGIN
  IF NEW.parent_list_id IS NOT NULL AND EXISTS (
    WITH RECURSIVE up AS (
      SELECT id, parent_list_id, 0 AS depth FROM lookup_lists WHERE id = NEW.parent_list_id
      UNION ALL
      SELECT l.id, l.parent_list_id, up.depth + 1 FROM lookup_lists l JOIN up ON l.id = up.parent_list_id
      WHERE up.depth < 64
    )
    SELECT 1 FROM up WHERE id = NEW.id
  ) THEN
    RAISE EXCEPTION 'lookup_lists: a list cannot depend on itself or on a list that depends on it'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'lookup_lists_no_cycle';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER lookup_lists_prevent_cycle
  BEFORE INSERT OR UPDATE OF parent_list_id ON cmdb.lookup_lists
  FOR EACH ROW EXECUTE FUNCTION cmdb.lookup_lists_prevent_cycle();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Values: the parent value comes from the parent list
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.lookup_list_values_check_parent() RETURNS trigger
LANGUAGE plpgsql
SET search_path = cmdb, public
AS $$
DECLARE
  parent_list uuid;
BEGIN
  SELECT parent_list_id INTO parent_list FROM lookup_lists WHERE id = NEW.list_id;

  IF NEW.parent_value_id IS NOT NULL THEN
    IF parent_list IS NULL THEN
      RAISE EXCEPTION 'lookup_list_values: the list has no parent list, so its values have no parent value'
        USING ERRCODE = 'check_violation', CONSTRAINT = 'lookup_list_values_parent_list';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM lookup_list_values p WHERE p.id = NEW.parent_value_id AND p.list_id = parent_list) THEN
      RAISE EXCEPTION 'lookup_list_values: the parent value must be a value of the parent list'
        USING ERRCODE = 'check_violation', CONSTRAINT = 'lookup_list_values_parent_list';
    END IF;
  ELSIF parent_list IS NOT NULL AND (
    TG_OP = 'INSERT'
    -- Clearing is allowed only for a parent left over from a previous parent list.
    OR EXISTS (SELECT 1 FROM lookup_list_values p WHERE p.id = OLD.parent_value_id AND p.list_id = parent_list)
  ) THEN
    RAISE EXCEPTION 'lookup_list_values: values of a list with a parent list need a parent value'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'lookup_list_values_parent_required';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER lookup_list_values_check_parent
  BEFORE INSERT OR UPDATE OF parent_value_id ON cmdb.lookup_list_values
  FOR EACH ROW EXECUTE FUNCTION cmdb.lookup_list_values_check_parent();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Fields: the parent field is a lookup field on the parent list, in the lineage
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.ci_attribute_definitions_check_parent() RETURNS trigger
LANGUAGE plpgsql
SET search_path = cmdb, public
AS $$
DECLARE
  parent record;
  parent_list uuid;
BEGIN
  IF NEW.parent_attribute_id IS NULL THEN
    RETURN NEW;
  END IF;
  SELECT parent_list_id INTO parent_list FROM lookup_lists WHERE id = NEW.lookup_list_id;
  IF NEW.data_type <> 'lookup' OR parent_list IS NULL THEN
    RAISE EXCEPTION 'ci_attribute_definitions: only a lookup field on a list with a parent list has a parent field'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_definitions_parent_attribute';
  END IF;
  SELECT class_id, data_type, lookup_list_id INTO parent FROM ci_attribute_definitions WHERE id = NEW.parent_attribute_id;
  IF parent.lookup_list_id IS DISTINCT FROM parent_list THEN
    RAISE EXCEPTION 'ci_attribute_definitions: the parent field must be a lookup field on the parent list'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_definitions_parent_attribute';
  END IF;
  IF NOT ci_class_is_a(NEW.class_id, parent.class_id) THEN
    RAISE EXCEPTION 'ci_attribute_definitions: the parent field must be defined on the same class or an ancestor'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_definitions_parent_attribute';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER ci_attribute_definitions_check_parent
  BEFORE INSERT OR UPDATE OF parent_attribute_id, data_type, lookup_list_id, class_id ON cmdb.ci_attribute_definitions
  FOR EACH ROW EXECUTE FUNCTION cmdb.ci_attribute_definitions_check_parent();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- A list's new parent: nothing may still point into the old one (at commit)
-- ---------------------------------------------------------------------------
CREATE FUNCTION cmdb.lookup_lists_check_children() RETURNS trigger
LANGUAGE plpgsql
SET search_path = cmdb, public
AS $$
DECLARE
  parent_list uuid;
BEGIN
  -- The list as it is at commit, not as this (possibly earlier) update left it.
  SELECT parent_list_id INTO parent_list FROM lookup_lists WHERE id = NEW.id;
  IF NOT FOUND THEN
    RETURN NULL;
  END IF;
  IF EXISTS (
    SELECT 1 FROM lookup_list_values v
    JOIN lookup_list_values p ON p.id = v.parent_value_id
    WHERE v.list_id = NEW.id AND p.list_id IS DISTINCT FROM parent_list
  ) THEN
    RAISE EXCEPTION 'lookup_lists: values of this list still have parent values from another list'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'lookup_lists_parent_values';
  END IF;
  IF EXISTS (
    SELECT 1 FROM ci_attribute_definitions d
    JOIN ci_attribute_definitions p ON p.id = d.parent_attribute_id
    WHERE d.lookup_list_id = NEW.id AND p.lookup_list_id IS DISTINCT FROM parent_list
  ) THEN
    RAISE EXCEPTION 'lookup_lists: fields bound to this list still have a parent field on another list'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'lookup_lists_parent_values';
  END IF;
  RETURN NULL;
END;
$$;
--> statement-breakpoint
CREATE CONSTRAINT TRIGGER lookup_lists_check_children
  AFTER UPDATE OF parent_list_id ON cmdb.lookup_lists
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW EXECUTE FUNCTION cmdb.lookup_lists_check_children();
