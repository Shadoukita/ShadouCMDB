-- Data model administration (SHAA-31).
--
-- Additive only: existing installs keep every class, attribute, lookup and CI.
-- Business content (classes, attributes, relationship types and rules,
-- statuses, environments, locations) is no longer loaded by `shadoucmdb seed`;
-- it ships as the "IT infrastructure" starter template, which an administrator
-- installs from the API. Installing it on a database that already holds the
-- old seed is a no-op, because rows are matched by key.
--
-- New here:
--   * ci_classes.color and ci_classes.sort_order (menus, pickers, badges);
--   * ci_attribute_definitions.help_text and .default_value;
--   * admin-defined lookup lists (lookup_lists, lookup_list_values) and the
--     attribute data type "lookup", whose values live in
--     ci_attribute_values.value_lookup_id with a real foreign key.
--
-- Soft delete: none of the new tables use it. Lists and list values are retired
-- with is_active = false; a value stored on a CI cannot be deleted (RESTRICT).

-- ---------------------------------------------------------------------------
-- CI classes: colour and explicit order
-- ---------------------------------------------------------------------------
ALTER TABLE ci_classes
  ADD COLUMN color text,
  ADD COLUMN sort_order integer DEFAULT 0 NOT NULL,
  ADD CONSTRAINT ci_classes_color_format CHECK (color IS NULL OR color ~ '^#[0-9a-fA-F]{6}$');
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Admin-defined lookup lists
-- ---------------------------------------------------------------------------
CREATE TABLE lookup_lists (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  key text NOT NULL,
  name text NOT NULL,
  description text,
  sort_order integer DEFAULT 0 NOT NULL,
  is_active boolean DEFAULT true NOT NULL,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  updated_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT lookup_lists_key_unique UNIQUE (key),
  CONSTRAINT lookup_lists_key_format CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
  CONSTRAINT lookup_lists_name_not_blank CHECK (length(btrim(name)) > 0)
);
--> statement-breakpoint
-- Values go with their list (CASCADE). A list that an attribute uses cannot be
-- deleted (RESTRICT below), so a cascade never removes a value stored on a CI.
CREATE TABLE lookup_list_values (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  list_id uuid NOT NULL REFERENCES lookup_lists (id) ON DELETE CASCADE,
  key text NOT NULL,
  name text NOT NULL,
  description text,
  color text,
  sort_order integer DEFAULT 0 NOT NULL,
  is_active boolean DEFAULT true NOT NULL,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  updated_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT lookup_list_values_list_key_uq UNIQUE (list_id, key),
  CONSTRAINT lookup_list_values_key_format CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
  CONSTRAINT lookup_list_values_name_not_blank CHECK (length(btrim(name)) > 0),
  CONSTRAINT lookup_list_values_color_format CHECK (color IS NULL OR color ~ '^#[0-9a-fA-F]{6}$')
);
--> statement-breakpoint
CREATE TRIGGER lookup_lists_set_updated_at BEFORE UPDATE ON lookup_lists
  FOR EACH ROW EXECUTE FUNCTION set_updated_at();
--> statement-breakpoint
CREATE TRIGGER lookup_list_values_set_updated_at BEFORE UPDATE ON lookup_list_values
  FOR EACH ROW EXECUTE FUNCTION set_updated_at();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Attribute definitions: help text, default value, lookup data type
-- ---------------------------------------------------------------------------
ALTER TABLE ci_attribute_definitions
  ADD COLUMN help_text text,
  -- JSON literal in the same shape the CI API accepts for this attribute.
  -- Applied when a CI is created without a value for it.
  ADD COLUMN default_value jsonb,
  ADD COLUMN lookup_list_id uuid REFERENCES lookup_lists (id) ON DELETE RESTRICT,
  DROP CONSTRAINT ci_attribute_definitions_data_type_valid,
  ADD CONSTRAINT ci_attribute_definitions_data_type_valid CHECK (data_type IN (
    'text', 'number', 'integer', 'boolean', 'enum', 'date', 'datetime', 'ip', 'cidr', 'reference', 'lookup')),
  ADD CONSTRAINT ci_attribute_definitions_lookup_list CHECK ((data_type = 'lookup') = (lookup_list_id IS NOT NULL)),
  ADD CONSTRAINT ci_attribute_definitions_default_value CHECK (
    default_value IS NULL OR (jsonb_typeof(default_value) <> 'null' AND data_type <> 'reference'));
--> statement-breakpoint
CREATE INDEX ci_attribute_definitions_lookup_list_idx ON ci_attribute_definitions (lookup_list_id)
  WHERE lookup_list_id IS NOT NULL;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Attribute values: a column for lookup values
-- ---------------------------------------------------------------------------
ALTER TABLE ci_attribute_values
  ADD COLUMN value_lookup_id uuid REFERENCES lookup_list_values (id) ON DELETE RESTRICT,
  DROP CONSTRAINT ci_attribute_values_exactly_one_value,
  ADD CONSTRAINT ci_attribute_values_exactly_one_value CHECK (num_nonnulls(
    value_text, value_number, value_boolean, value_date, value_datetime,
    value_ip, value_cidr, value_ref_ci_id, value_lookup_id) = 1);
--> statement-breakpoint
CREATE INDEX ci_attribute_values_lookup_idx ON ci_attribute_values (value_lookup_id)
  WHERE value_lookup_id IS NOT NULL;
--> statement-breakpoint

-- Same rules as 0002, plus: a lookup value must come from the attribute's list.
CREATE OR REPLACE FUNCTION ci_attribute_values_validate() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  def record;
  ci_class uuid;
  ref_class uuid;
  expected text;
  populated text;
BEGIN
  SELECT d.key, d.class_id, d.data_type, d.enum_values, d.reference_class_id, d.lookup_list_id
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
    WHEN 'lookup' THEN 'value_lookup_id'
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
    WHEN NEW.value_lookup_id IS NOT NULL THEN 'value_lookup_id'
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

  IF def.data_type = 'lookup' AND NOT EXISTS (
    SELECT 1 FROM lookup_list_values lv WHERE lv.id = NEW.value_lookup_id AND lv.list_id = def.lookup_list_id
  ) THEN
    RAISE EXCEPTION 'ci_attribute_values: value % is not in the list of attribute %', NEW.value_lookup_id, def.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_values_lookup_list';
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

-- A list value cannot move to another list while CIs store it.
CREATE OR REPLACE FUNCTION lookup_list_values_keep_list() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.list_id IS DISTINCT FROM OLD.list_id THEN
    RAISE EXCEPTION 'lookup_list_values: a value cannot move to another list'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'lookup_list_values_list_immutable';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER lookup_list_values_keep_list BEFORE UPDATE OF list_id ON lookup_list_values
  FOR EACH ROW EXECUTE FUNCTION lookup_list_values_keep_list();
