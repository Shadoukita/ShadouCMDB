-- Retire the Application field "criticality" (SHAA-969, GH#354).
--
-- The IT infrastructure starter template gave the Application class an enum
-- field "criticality" (low, medium, high, critical). Since 0031 every CI has a
-- core Criticality, so an Application showed two. The template no longer has
-- the field; this migration moves existing values onto the core field and
-- archives the class field.
--
-- Which field: key "criticality" on the class keyed "application", active, of
-- type enum, text or lookup. A field of another class, or of another type, is
-- an administrator's own and is left alone.
--
-- Per CI with a value (the class and its subclasses):
--   * the core field is empty and the value matches a value of the
--     criticality list (its key, or its name ignoring case and surrounding
--     spaces; a lookup field by the key or name of its value; active values
--     only): the core field is set and the change is audited on the CI;
--   * the core field already holds that value: nothing to do;
--   * the core field holds another value: the core value wins;
--   * no match: the core field stays empty.
-- Nothing is deleted. The field is archived (is_active = false): its column
-- and every value stay readable, forms hide it and it takes no new values. Its
-- audit entry reports the counts and the values that did not map, so an
-- administrator can follow up; un-archiving the field restores it as it was.
-- A required field stops being NOT NULL at the reconcile that
-- `shadoucmdb migrate` runs after the migrations (an archived field is never
-- required).

DO $$
DECLARE
  att record;
  crit_list uuid;
  source_sql text;
  mapped bigint;
  already bigint;
  conflicting bigint;
  unmapped bigint;
  unmapped_values jsonb;
BEGIN
  SELECT id INTO crit_list FROM cmdb.lookup_lists WHERE system_role = 'criticality';
  CREATE TEMP TABLE m0035 (ci_id uuid, raw text, raw_name text) ON COMMIT DROP;
  CREATE TEMP TABLE m0035_plan (ci_id uuid, raw text, class_id uuid, current_id uuid, target_id uuid) ON COMMIT DROP;

  FOR att IN
    SELECT d.id, d.data_type, c.key AS class_key, a.key AS area_key
    FROM cmdb.ci_attribute_definitions d
    JOIN cmdb.ci_classes c ON c.id = d.class_id
    JOIN cmdb.areas a ON a.id = c.area_id
    WHERE c.key = 'application' AND d.key = 'criticality' AND d.is_active
      AND d.data_type IN ('enum', 'text', 'lookup')
  LOOP
    IF NOT EXISTS (
      SELECT FROM information_schema.columns
      WHERE table_schema = att.area_key AND table_name = att.class_key AND column_name = 'criticality'
    ) THEN
      RAISE NOTICE '0035: %.% has no column "criticality"; field left as it is', att.area_key, att.class_key;
      CONTINUE;
    END IF;

    -- (CI, value as text, name to match as well) for every CI holding a value
    IF att.data_type = 'lookup' THEN
      source_sql := format(
        'SELECT t.id, lv.key, lv.name FROM %I.%I t JOIN cmdb.lookup_list_values lv ON lv.id = t.criticality',
        att.area_key, att.class_key);
    ELSE
      source_sql := format(
        'SELECT t.id, t.criticality::text, t.criticality::text FROM %I.%I t WHERE t.criticality IS NOT NULL',
        att.area_key, att.class_key);
    END IF;

    TRUNCATE m0035, m0035_plan;
    EXECUTE 'INSERT INTO m0035 (ci_id, raw, raw_name) ' || source_sql;

    INSERT INTO m0035_plan (ci_id, raw, class_id, current_id, target_id)
    SELECT m.ci_id, m.raw, ci.class_id, ci.criticality_value_id AS current_id,
           (SELECT v.id FROM cmdb.lookup_list_values v
            WHERE v.list_id = crit_list AND v.is_active
              AND (v.key = lower(btrim(m.raw)) OR lower(v.name) IN (lower(btrim(m.raw)), lower(btrim(m.raw_name))))
            ORDER BY (v.key = lower(btrim(m.raw))) DESC, v.sort_order, v.key
            LIMIT 1) AS target_id
    FROM m0035 m JOIN cmdb.configuration_items ci ON ci.id = m.ci_id;

    WITH moved AS (
      UPDATE cmdb.configuration_items ci
      SET criticality_value_id = p.target_id, version = ci.version + 1, updated_at = now()
      FROM m0035_plan p
      WHERE ci.id = p.ci_id AND p.current_id IS NULL AND p.target_id IS NOT NULL
      RETURNING ci.id, ci.class_id, p.raw, p.target_id
    )
    INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
    SELECT 'system', 'migration 0035', 'update', 'configuration_items', mv.id,
           jsonb_build_object('classId', mv.class_id, 'criticality', NULL),
           jsonb_build_object(
             'classId', mv.class_id,
             'criticality', jsonb_build_object('id', v.id, 'key', v.key, 'name', v.name),
             'from', jsonb_build_object('field', 'attributes.criticality', 'value', mv.raw))
    FROM moved mv JOIN cmdb.lookup_list_values v ON v.id = mv.target_id;
    GET DIAGNOSTICS mapped = ROW_COUNT;

    SELECT count(*) FILTER (WHERE current_id IS NOT NULL AND current_id = target_id),
           count(*) FILTER (WHERE current_id IS NOT NULL AND target_id IS NOT NULL AND current_id <> target_id),
           count(*) FILTER (WHERE target_id IS NULL),
           coalesce(jsonb_agg(DISTINCT raw) FILTER (WHERE target_id IS NULL), '[]'::jsonb)
      INTO already, conflicting, unmapped, unmapped_values
    FROM m0035_plan;

    UPDATE cmdb.ci_attribute_definitions SET is_active = false, updated_at = now() WHERE id = att.id;
    INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
    VALUES ('system', 'migration 0035', 'update', 'ci_attribute_definitions', att.id,
            jsonb_build_object('isActive', true),
            jsonb_build_object(
              'isActive', false,
              'reason', 'Superseded by the core Criticality field of every CI',
              'criticalitySet', mapped,
              'alreadySet', already,
              'keptCoreValue', conflicting,
              'notMapped', unmapped,
              'notMappedValues', unmapped_values));
    RAISE NOTICE '0035: %.criticality archived; core criticality set on % CIs, already set on %, other core value kept on %, not mapped on % (values: %)',
      att.class_key, mapped, already, conflicting, unmapped, unmapped_values;
  END LOOP;
  DROP TABLE m0035, m0035_plan;
END;
$$;
