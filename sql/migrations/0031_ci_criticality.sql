-- Criticality: an optional core field on every CI (v0.3.0, SHAA-885, decision D1).
--
-- Impact analysis groups affected CIs by how critical they are, and business
-- services will map their tier onto the same scale. Like ident and the
-- validity period it is a core column of configuration_items, not a class
-- attribute, so every class has it without per-class form work.
--
-- The values are a lookup list, so administrators can rename them, reorder
-- them and add more. It is a *system* list: lookup_lists.system_role marks it,
-- the API finds it by that role (not by its key, which may differ when a list
-- keyed "criticality" already exists) and refuses to delete it. A value is
-- ranked by its position in the list (sort_order, then key): 1 is the most
-- critical. A value in use cannot be deleted (ON DELETE RESTRICT).

-- ---------------------------------------------------------------------------
-- System lookup lists
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.lookup_lists
  ADD COLUMN system_role text,
  ADD CONSTRAINT lookup_lists_system_role_valid CHECK (system_role IN ('criticality')),
  ADD CONSTRAINT lookup_lists_system_role_uq UNIQUE (system_role);
--> statement-breakpoint

-- A system list stays: the API refuses the delete, this is the backstop for
-- every other client (psql, a restore script).
CREATE FUNCTION cmdb.lookup_lists_keep_system() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN
    RAISE EXCEPTION 'lookup_lists: the % list is a system list and cannot be deleted', OLD.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'lookup_lists_system_protected';
  END IF;
  IF NEW.system_role IS DISTINCT FROM OLD.system_role THEN
    RAISE EXCEPTION 'lookup_lists: a list''s system role cannot change'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'lookup_lists_system_protected';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER lookup_lists_keep_system
  BEFORE DELETE ON cmdb.lookup_lists
  FOR EACH ROW WHEN (OLD.system_role IS NOT NULL)
  EXECUTE FUNCTION cmdb.lookup_lists_keep_system();
--> statement-breakpoint
CREATE TRIGGER lookup_lists_keep_system_role
  BEFORE UPDATE OF system_role ON cmdb.lookup_lists
  FOR EACH ROW EXECUTE FUNCTION cmdb.lookup_lists_keep_system();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- The criticality list, keyed "criticality" (criticality_2, … if taken)
-- ---------------------------------------------------------------------------
DO $$
DECLARE
  list_key text := 'criticality';
  n integer := 1;
  list_id uuid;
BEGIN
  WHILE EXISTS (SELECT 1 FROM cmdb.lookup_lists WHERE key = list_key) LOOP
    n := n + 1;
    list_key := 'criticality_' || n;
  END LOOP;
  INSERT INTO cmdb.lookup_lists (key, name, description, system_role)
  VALUES (list_key, 'Criticality',
          'How critical a CI is to the business; impact analysis groups affected CIs by it. First is most critical.',
          'criticality')
  RETURNING id INTO list_id;
  INSERT INTO cmdb.lookup_list_values (list_id, key, name, sort_order)
  VALUES (list_id, 'critical', 'Critical', 10),
         (list_id, 'high', 'High', 20),
         (list_id, 'medium', 'Medium', 30),
         (list_id, 'low', 'Low', 40);
END;
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- configuration_items.criticality_value_id
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.configuration_items
  ADD COLUMN criticality_value_id uuid
    CONSTRAINT configuration_items_criticality_value_id_fkey
    REFERENCES cmdb.lookup_list_values (id) ON DELETE RESTRICT;
--> statement-breakpoint

CREATE FUNCTION cmdb.configuration_items_criticality_valid() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
BEGIN
  IF NEW.criticality_value_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM cmdb.lookup_list_values v JOIN cmdb.lookup_lists l ON l.id = v.list_id
    WHERE v.id = NEW.criticality_value_id AND l.system_role = 'criticality'
  ) THEN
    RAISE EXCEPTION 'configuration_items: % is not a value of the criticality list', NEW.criticality_value_id
      USING ERRCODE = 'check_violation', CONSTRAINT = 'configuration_items_criticality_list';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER configuration_items_criticality_valid
  BEFORE INSERT OR UPDATE OF criticality_value_id ON cmdb.configuration_items
  FOR EACH ROW WHEN (NEW.criticality_value_id IS NOT NULL)
  EXECUTE FUNCTION cmdb.configuration_items_criticality_valid();
--> statement-breakpoint

-- CI list filter by criticality (most CIs hold none).
CREATE INDEX configuration_items_criticality_idx ON cmdb.configuration_items (criticality_value_id)
  WHERE criticality_value_id IS NOT NULL;
