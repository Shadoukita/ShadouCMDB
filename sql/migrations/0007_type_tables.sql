-- Real database tables per type, part 2 (SHAA-56): one table per CI class,
-- one column per attribute, and the values moved out of ci_attribute_values.
--
-- Class table inheritance: cmdb.configuration_items stays the registry of every
-- asset (name, class, status, environment, owner, location, version,
-- timestamps). Each class gets a table in its area's schema whose primary key
-- is the registry id:
--
--   CREATE TABLE infrastruktur.server (
--     id uuid PRIMARY KEY REFERENCES cmdb.configuration_items (id) ON DELETE CASCADE,
--     cpu_cores bigint, memory_gb numeric, os_family text CHECK (...), ...);
--
-- A CI has one row in the table of its class and of every ancestor class (a
-- server has a row in infrastruktur.hardware for the inherited fields and one in
-- infrastruktur.server for its own).
--
-- Column types: text/enum -> text (enum with a CHECK on the allowed values),
-- number -> numeric, integer -> bigint, boolean -> boolean, date -> date,
-- datetime -> timestamptz, ip -> inet, cidr -> cidr, reference -> uuid with a
-- foreign key to the registry (NO ACTION), lookup -> uuid with a foreign key to
-- cmdb.lookup_list_values. A required, active field is NOT NULL.
--
-- Constraint and index names derive from the attribute id, so they never
-- depend on user text: ck_<id>_<hash of the enum values>, fk_<id>, ix_<id>.
-- The run-time DDL engine (backend/src/schema) builds exactly the same objects.
--
-- The move is verified: for every attribute the number of non-null values in
-- its column must equal its number of rows in ci_attribute_values, and the
-- totals must match, or the migration fails and nothing changes. Then
-- ci_attribute_values and its trigger are dropped. Every statement run is
-- recorded in cmdb.schema_changes.
--
-- Reporting views (<area>.v_<type>) are created by `shadoucmdb migrate` right
-- after the migrations, by the same engine that maintains them afterwards.

DO $$
DECLARE
  area record;
  cls record;
  att record;
  stmts text[] := ARRAY[]::text[];
  stmt text;
  pg_type text;
  value_column text;
  hex text;
  moved bigint;
  expected bigint;
  total_moved bigint := 0;
  total_expected bigint;
  nulls bigint;
  notes jsonb := '[]'::jsonb;
BEGIN
  FOR area IN SELECT key FROM cmdb.areas ORDER BY sort_order, key LOOP
    stmt := format('CREATE SCHEMA %I', area.key);
    EXECUTE stmt;
    stmts := stmts || stmt;
  END LOOP;

  FOR cls IN
    SELECT c.id, c.key, a.key AS area FROM cmdb.ci_classes c JOIN cmdb.areas a ON a.id = c.area_id ORDER BY a.key, c.key
  LOOP
    stmt := format('CREATE TABLE %I.%I (id uuid PRIMARY KEY REFERENCES cmdb.configuration_items (id) ON DELETE CASCADE)',
                   cls.area, cls.key);
    EXECUTE stmt;
    stmts := stmts || stmt;
    -- One row for every CI (deleted ones too) of this class or a descendant.
    EXECUTE format('INSERT INTO %I.%I (id) SELECT ci.id FROM cmdb.configuration_items ci WHERE cmdb.ci_class_is_a(ci.class_id, $1)',
                   cls.area, cls.key) USING cls.id;

    FOR att IN
      SELECT d.id, d.key, d.data_type, d.enum_values, d.is_required, d.is_active
      FROM cmdb.ci_attribute_definitions d WHERE d.class_id = cls.id ORDER BY d.sort_order, d.key
    LOOP
      IF att.key = 'id' THEN
        RAISE EXCEPTION 'attribute "id" of class % would collide with the primary key of its table', cls.key;
      END IF;
      pg_type := CASE att.data_type
        WHEN 'text' THEN 'text' WHEN 'enum' THEN 'text' WHEN 'number' THEN 'numeric' WHEN 'integer' THEN 'bigint'
        WHEN 'boolean' THEN 'boolean' WHEN 'date' THEN 'date' WHEN 'datetime' THEN 'timestamptz'
        WHEN 'ip' THEN 'inet' WHEN 'cidr' THEN 'cidr' WHEN 'reference' THEN 'uuid' WHEN 'lookup' THEN 'uuid' END;
      value_column := CASE att.data_type
        WHEN 'text' THEN 'value_text' WHEN 'enum' THEN 'value_text' WHEN 'number' THEN 'value_number'
        WHEN 'integer' THEN 'value_number' WHEN 'boolean' THEN 'value_boolean' WHEN 'date' THEN 'value_date'
        WHEN 'datetime' THEN 'value_datetime' WHEN 'ip' THEN 'value_ip' WHEN 'cidr' THEN 'value_cidr'
        WHEN 'reference' THEN 'value_ref_ci_id' WHEN 'lookup' THEN 'value_lookup_id' END;
      hex := replace(att.id::text, '-', '');

      stmt := format('ALTER TABLE %I.%I ADD COLUMN %I %s', cls.area, cls.key, att.key, pg_type);
      EXECUTE stmt;
      stmts := stmts || stmt;

      EXECUTE format('UPDATE %I.%I t SET %I = v.%I::%s FROM cmdb.ci_attribute_values v WHERE v.ci_id = t.id AND v.attribute_id = $1',
                     cls.area, cls.key, att.key, value_column, pg_type) USING att.id;
      GET DIAGNOSTICS moved = ROW_COUNT;
      SELECT count(*) INTO expected FROM cmdb.ci_attribute_values WHERE attribute_id = att.id;
      IF moved <> expected THEN
        RAISE EXCEPTION 'moving %.% failed: % values in ci_attribute_values, % written to %.%',
          cls.key, att.key, expected, moved, cls.area, cls.key;
      END IF;
      total_moved := total_moved + moved;

      IF att.data_type = 'enum' THEN
        stmt := format('ALTER TABLE %I.%I ADD CONSTRAINT %I CHECK (%I = ANY (%L::text[]))', cls.area, cls.key,
          'ck_' || hex || '_' || left(encode(sha256(convert_to(
            (SELECT string_agg(v, E'\n' ORDER BY n) FROM jsonb_array_elements_text(att.enum_values) WITH ORDINALITY AS e(v, n)),
            'UTF8')), 'hex'), 12),
          att.key, ARRAY(SELECT jsonb_array_elements_text(att.enum_values)));
        EXECUTE stmt;
        stmts := stmts || stmt;
      ELSIF att.data_type IN ('reference', 'lookup') THEN
        -- NO ACTION for references: checked at the end of the statement, so a purge can
        -- delete CIs of a type that reference each other.
        stmt := format('ALTER TABLE %I.%I ADD CONSTRAINT %I FOREIGN KEY (%I) REFERENCES %s',
          cls.area, cls.key, 'fk_' || hex, att.key,
          CASE att.data_type WHEN 'reference' THEN 'cmdb.configuration_items (id) ON DELETE NO ACTION'
                             ELSE 'cmdb.lookup_list_values (id) ON DELETE RESTRICT' END);
        EXECUTE stmt;
        stmts := stmts || stmt;
        stmt := format('CREATE INDEX %I ON %I.%I (%I)', 'ix_' || hex, cls.area, cls.key, att.key);
        EXECUTE stmt;
        stmts := stmts || stmt;
      END IF;

      IF att.is_required AND att.is_active THEN
        EXECUTE format('SELECT count(*) FROM %I.%I WHERE %I IS NULL', cls.area, cls.key, att.key) INTO nulls;
        IF nulls = 0 THEN
          stmt := format('ALTER TABLE %I.%I ALTER COLUMN %I SET NOT NULL', cls.area, cls.key, att.key);
          EXECUTE stmt;
          stmts := stmts || stmt;
        ELSE
          -- Before this migration "required" was only enforced on live CIs.
          notes := notes || jsonb_build_object('kind', 'not_null_skipped', 'rows', nulls, 'message',
            format('%s.%s stays nullable: %s CIs (deleted ones included) have no value', cls.key, att.key, nulls));
          RAISE NOTICE '%.% stays nullable: % CIs have no value', cls.key, att.key, nulls;
        END IF;
      END IF;
    END LOOP;
  END LOOP;

  SELECT count(*) INTO total_expected FROM cmdb.ci_attribute_values;
  IF total_moved <> total_expected THEN
    RAISE EXCEPTION 'moving attribute values failed: % in ci_attribute_values, % moved', total_expected, total_moved;
  END IF;

  -- The generic value table and its validation are replaced by the typed columns.
  DROP TABLE cmdb.ci_attribute_values;
  stmts := stmts || 'DROP TABLE cmdb.ci_attribute_values'::text;
  DROP FUNCTION cmdb.ci_attribute_values_validate();

  IF cardinality(stmts) > 1 THEN
    INSERT INTO cmdb.schema_changes (actor_type, actor_name, summary, statements, impact)
    VALUES ('system', 'migration 0007',
            format('Migration 0007: %s attribute values moved into per-type tables', total_moved),
            stmts,
            jsonb_build_array(jsonb_build_object('kind', 'data_moved', 'rows', total_moved,
              'message', format('%s values moved from ci_attribute_values and verified', total_moved))) || notes);
  END IF;
END;
$$;
--> statement-breakpoint

-- Same checks as before, minus the attribute values: a CI's class change now
-- moves its rows between type tables in the API, in the same transaction.
CREATE OR REPLACE FUNCTION cmdb.configuration_items_validate() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
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
  RETURN NEW;
END;
$$;
--> statement-breakpoint

-- Assets storing a lookup list value, over every lookup field of its list.
CREATE OR REPLACE FUNCTION cmdb.lookup_value_count(p_value_id uuid) RETURNS bigint
LANGUAGE plpgsql STABLE SET search_path = cmdb, public AS $$
DECLARE
  f record;
  n bigint;
  total bigint := 0;
BEGIN
  FOR f IN
    SELECT a.key AS area, c.key AS type, d.key AS field
    FROM cmdb.ci_attribute_definitions d
    JOIN cmdb.ci_classes c ON c.id = d.class_id
    JOIN cmdb.areas a ON a.id = c.area_id
    JOIN cmdb.lookup_list_values v ON v.list_id = d.lookup_list_id
    WHERE v.id = p_value_id AND d.data_type = 'lookup'
  LOOP
    IF EXISTS (
      SELECT 1 FROM pg_attribute att
      WHERE att.attrelid = to_regclass(format('%I.%I', f.area, f.type)) AND att.attname = f.field AND NOT att.attisdropped
    ) THEN
      EXECUTE format('SELECT count(*) FROM %I.%I WHERE %I = $1', f.area, f.type, f.field) INTO n USING p_value_id;
      total := total + n;
    END IF;
  END LOOP;
  RETURN total;
END;
$$;
