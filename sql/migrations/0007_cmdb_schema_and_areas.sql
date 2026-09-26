-- Real database tables per type, part 1 (SHAA-56): the system schema, areas
-- and the schema change history.
--
-- From here on an administrator's data model is real DDL: an *area* ("Bestand")
-- is a PostgreSQL schema (`bestand`), a *type* (CI class, "Netzwerk") is a table
-- in it (`bestand.netzwerk`) and a *field* (attribute) is a typed column. The
-- application creates and alters those objects at run time through its DDL
-- engine (backend/src/schema), never from raw user text.
--
-- So that admin-made schemas can never collide with the application's own
-- tables, every system table and function moves from `public` into the `cmdb`
-- schema. The application connects with search_path = cmdb, public, so its
-- queries are unchanged; the sqlx bookkeeping table stays in public.
--
-- New here:
--   * cmdb.areas: the menu tabs, one PostgreSQL schema each;
--   * ci_classes.area_id (every type belongs to one area);
--   * cmdb.schema_changes: append-only history of every DDL statement the
--     application ran (who, when, the exact SQL, and its impact on data);
--   * technical names (area key, type key, attribute key, the area of a type)
--     are immutable at the database level: reports and integrations query them;
--   * helpers for DBAs and the API: cmdb.type_table(), cmdb.attribute_value_count().
--
-- Existing installs: the CI classes they already have are placed in an area
-- "Infrastruktur" (schema `infrastruktur`), the area the IT infrastructure
-- starter template installs into. Migration 0008 then builds their tables and
-- moves the values out of ci_attribute_values.
--
-- Soft delete: areas are archived with is_active = false (the API's DELETE);
-- the separate purge drops the schema. Nothing else here is deleted.

CREATE SCHEMA IF NOT EXISTS cmdb;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Move the system tables and functions (indexes, sequences, constraints and
-- triggers move with their tables)
-- ---------------------------------------------------------------------------
DO $$
DECLARE
  t text;
BEGIN
  FOREACH t IN ARRAY ARRAY[
    'statuses', 'environments', 'locations', 'owners',
    'ci_classes', 'ci_attribute_definitions', 'ci_attribute_values', 'configuration_items',
    'relationship_types', 'relationship_type_rules', 'ci_relationships', 'audit_log',
    'users', 'permission_profiles', 'permission_profile_global_permissions',
    'permission_profile_class_permissions', 'user_permission_profiles', 'sessions',
    'lookup_lists', 'lookup_list_values', 'ui_settings', 'ui_settings_versions', 'ui_assets'
  ] LOOP
    EXECUTE format('ALTER TABLE public.%I SET SCHEMA cmdb', t);
  END LOOP;
END;
$$;
--> statement-breakpoint

-- Trigger functions resolve table names through search_path; pin it so they
-- behave the same for the application, a DBA's psql session and a report.
DO $$
DECLARE
  f text;
BEGIN
  FOREACH f IN ARRAY ARRAY[
    'set_updated_at', 'ci_classes_prevent_cycle', 'locations_prevent_cycle',
    'configuration_items_validate', 'ci_attribute_values_validate', 'ci_relationships_validate',
    'audit_log_append_only', 'permission_profiles_protect_builtin', 'permission_rows_not_builtin',
    'users_keep_one_administrator', 'lookup_list_values_keep_list', 'ui_settings_versions_append_only'
  ] LOOP
    EXECUTE format('ALTER FUNCTION public.%I() SET SCHEMA cmdb', f);
    EXECUTE format('ALTER FUNCTION cmdb.%I() SET search_path = cmdb, public', f);
  END LOOP;
END;
$$;
--> statement-breakpoint

-- The lineage helpers are SQL functions the planner inlines; a SET clause would
-- prevent that, so they are recreated with schema-qualified names instead.
DROP FUNCTION public.ci_class_is_a(uuid, uuid);
--> statement-breakpoint
ALTER FUNCTION public.ci_class_lineage(uuid) SET SCHEMA cmdb;
--> statement-breakpoint
CREATE OR REPLACE FUNCTION cmdb.ci_class_lineage(p_class_id uuid)
RETURNS TABLE (class_id uuid, depth integer)
LANGUAGE sql STABLE AS $$
  WITH RECURSIVE up AS (
    SELECT c.id, c.parent_id, 0 AS depth FROM cmdb.ci_classes c WHERE c.id = p_class_id
    UNION ALL
    SELECT c.id, c.parent_id, up.depth + 1
    FROM cmdb.ci_classes c JOIN up ON c.id = up.parent_id
    WHERE up.depth < 64
  )
  SELECT id, depth FROM up;
$$;
--> statement-breakpoint
CREATE FUNCTION cmdb.ci_class_is_a(p_class_id uuid, p_ancestor_id uuid)
RETURNS boolean
LANGUAGE sql STABLE AS $$
  SELECT EXISTS (SELECT 1 FROM cmdb.ci_class_lineage(p_class_id) l WHERE l.class_id = p_ancestor_id);
$$;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Areas
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.areas (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  -- The PostgreSQL schema name. Immutable (trigger below).
  key text NOT NULL,
  name text NOT NULL,
  description text,
  icon text,
  color text,
  sort_order integer DEFAULT 0 NOT NULL,
  -- Archived areas keep their schema and data; the API hides them.
  is_active boolean DEFAULT true NOT NULL,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  updated_at timestamp with time zone DEFAULT now() NOT NULL,
  CONSTRAINT areas_key_unique UNIQUE (key),
  CONSTRAINT areas_key_format CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
  -- System schemas. The API gives the reason; this is the last line of defence.
  CONSTRAINT areas_key_not_reserved CHECK (
    key NOT IN ('cmdb', 'public', 'information_schema', 'drizzle') AND key !~ '^(pg_|cmdb_)'),
  CONSTRAINT areas_name_not_blank CHECK (length(btrim(name)) > 0),
  CONSTRAINT areas_color_format CHECK (color IS NULL OR color ~ '^#[0-9a-fA-F]{6}$')
);
--> statement-breakpoint
CREATE TRIGGER areas_set_updated_at BEFORE UPDATE ON cmdb.areas
  FOR EACH ROW EXECUTE FUNCTION cmdb.set_updated_at();
--> statement-breakpoint

INSERT INTO cmdb.areas (key, name, description, sort_order)
SELECT 'infrastruktur', 'Infrastruktur', 'IT infrastructure: hardware, virtual machines, applications, databases and services', 0
WHERE EXISTS (SELECT 1 FROM cmdb.ci_classes);
--> statement-breakpoint

ALTER TABLE cmdb.ci_classes ADD COLUMN area_id uuid REFERENCES cmdb.areas (id) ON DELETE RESTRICT;
--> statement-breakpoint
UPDATE cmdb.ci_classes SET area_id = (SELECT id FROM cmdb.areas WHERE key = 'infrastruktur');
--> statement-breakpoint
ALTER TABLE cmdb.ci_classes ALTER COLUMN area_id SET NOT NULL;
--> statement-breakpoint
CREATE INDEX ci_classes_area_idx ON cmdb.ci_classes (area_id);
--> statement-breakpoint

-- A type's key is its table name and "v_<key>" its reporting view (63 characters
-- at most). NOT VALID: checked for new types only, an older key keeps working
-- (the engine then skips its view and says so).
ALTER TABLE cmdb.ci_classes
  ADD CONSTRAINT ci_classes_key_table_name CHECK (length(key) <= 61 AND key !~ '^v_') NOT VALID;
--> statement-breakpoint
-- An attribute's key is its column name; "id" is the primary key of every type table.
ALTER TABLE cmdb.ci_attribute_definitions
  ADD CONSTRAINT ci_attribute_definitions_key_column_name CHECK (key <> 'id') NOT VALID;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Technical names are immutable
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION cmdb.technical_names_immutable() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
BEGIN
  IF NEW.key IS DISTINCT FROM OLD.key THEN
    RAISE EXCEPTION '%: the technical name "%" cannot be changed; rename the display name instead', TG_TABLE_NAME, OLD.key
      USING ERRCODE = 'check_violation', CONSTRAINT = TG_TABLE_NAME || '_key_immutable';
  END IF;
  -- Nested: PL/pgSQL does not short-circuit, and each table has only one of these columns.
  IF TG_TABLE_NAME = 'ci_classes' THEN
    IF NEW.area_id IS DISTINCT FROM OLD.area_id THEN
      RAISE EXCEPTION 'ci_classes: a type cannot move to another area (its table lives in the area''s schema)'
        USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_area_immutable';
    END IF;
  ELSIF TG_TABLE_NAME = 'ci_attribute_definitions' THEN
    IF NEW.class_id IS DISTINCT FROM OLD.class_id THEN
      RAISE EXCEPTION 'ci_attribute_definitions: a field cannot move to another type'
        USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_attribute_definitions_class_immutable';
    END IF;
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER areas_technical_names_immutable BEFORE UPDATE ON cmdb.areas
  FOR EACH ROW EXECUTE FUNCTION cmdb.technical_names_immutable();
--> statement-breakpoint
CREATE TRIGGER ci_classes_technical_names_immutable BEFORE UPDATE ON cmdb.ci_classes
  FOR EACH ROW EXECUTE FUNCTION cmdb.technical_names_immutable();
--> statement-breakpoint
CREATE TRIGGER ci_attribute_definitions_technical_names_immutable BEFORE UPDATE ON cmdb.ci_attribute_definitions
  FOR EACH ROW EXECUTE FUNCTION cmdb.technical_names_immutable();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Schema change history
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.schema_changes (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  occurred_at timestamp with time zone DEFAULT now() NOT NULL,
  actor_type text NOT NULL,
  actor_id text,
  actor_name text,
  request_id text,
  -- One line, e.g. "Create type bestand.netzwerk".
  summary text NOT NULL,
  -- Exactly what ran, in order, in one transaction.
  statements text[] NOT NULL,
  -- [{ "statement": 0, "kind": "rewrite", "rows": 1204, "message": "..." }, ...]
  impact jsonb DEFAULT '[]' NOT NULL,
  CONSTRAINT schema_changes_actor_type_valid CHECK (actor_type IN ('system', 'user', 'api_client', 'import')),
  CONSTRAINT schema_changes_statements_present CHECK (cardinality(statements) > 0),
  CONSTRAINT schema_changes_impact_array CHECK (jsonb_typeof(impact) = 'array')
);
--> statement-breakpoint
CREATE INDEX schema_changes_occurred_idx ON cmdb.schema_changes (occurred_at DESC, id);
--> statement-breakpoint
CREATE OR REPLACE FUNCTION cmdb.schema_changes_append_only() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
BEGIN
  RAISE EXCEPTION 'schema_changes is append-only (% rejected)', TG_OP
    USING ERRCODE = 'insufficient_privilege';
END;
$$;
--> statement-breakpoint
CREATE TRIGGER schema_changes_append_only
  BEFORE UPDATE OR DELETE ON cmdb.schema_changes
  FOR EACH ROW EXECUTE FUNCTION cmdb.schema_changes_append_only();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Helpers (identifiers come from validated keys and are quoted with %I)
-- ---------------------------------------------------------------------------
-- "bestand.netzwerk": the table of a type, quoted where needed.
CREATE OR REPLACE FUNCTION cmdb.type_table(p_class_id uuid) RETURNS text
LANGUAGE sql STABLE SET search_path = cmdb, public AS $$
  SELECT format('%I.%I', a.key, c.key) FROM cmdb.ci_classes c JOIN cmdb.areas a ON a.id = c.area_id WHERE c.id = p_class_id;
$$;
--> statement-breakpoint
-- Assets holding a value for a field (0 while its column does not exist yet).
CREATE OR REPLACE FUNCTION cmdb.attribute_value_count(p_attribute_id uuid) RETURNS bigint
LANGUAGE plpgsql STABLE SET search_path = cmdb, public AS $$
DECLARE
  target record;
  n bigint;
BEGIN
  SELECT a.key AS area, c.key AS type, d.key AS field INTO target
  FROM cmdb.ci_attribute_definitions d
  JOIN cmdb.ci_classes c ON c.id = d.class_id
  JOIN cmdb.areas a ON a.id = c.area_id
  WHERE d.id = p_attribute_id;
  IF NOT FOUND OR NOT EXISTS (
    SELECT 1 FROM pg_attribute att
    WHERE att.attrelid = to_regclass(format('%I.%I', target.area, target.type))
      AND att.attname = target.field AND NOT att.attisdropped
  ) THEN
    RETURN 0;
  END IF;
  EXECUTE format('SELECT count(*) FROM %I.%I WHERE %I IS NOT NULL', target.area, target.type, target.field) INTO n;
  RETURN n;
END;
$$;
--> statement-breakpoint
-- Would this text convert to that type? The DDL engine dry-runs a field type
-- change with it and refuses the change if any stored value would not convert.
CREATE OR REPLACE FUNCTION cmdb.value_castable(p_value text, p_type regtype) RETURNS boolean
LANGUAGE plpgsql STABLE SET search_path = cmdb, public AS $$
BEGIN
  IF p_value IS NULL THEN
    RETURN true;
  END IF;
  IF current_setting('server_version_num')::integer >= 160000 THEN
    RETURN pg_input_is_valid(p_value, format_type(p_type, NULL));
  END IF;
  EXECUTE format('SELECT %L::%s', p_value, format_type(p_type, NULL));
  RETURN true;
EXCEPTION WHEN data_exception THEN
  RETURN false;
END;
$$;
