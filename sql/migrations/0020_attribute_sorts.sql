-- Inventory sorts on attributes (GH#112, SHAA-335).
--
-- Migration 0016 moved hostname, ip_address, serial_number and status into
-- class attributes. The inventory list could then sort only by core fields,
-- so 0016 rewrote stored sorts on hostname, ipAddress, serialNumber and
-- statusName to "label". The list now sorts by "attributes.<key>"; this puts
-- those sorts back, as a new settings version.
--
-- The original sort is read from the settings version 0016 replaced. A list
-- view's defaultSort, or the sort of a saved-search widget on exactly one
-- class, is restored when
--   * it is still the label sort 0016 wrote (nobody has changed it since),
--   * the version before 0016 sorted it by one of those fields, and
--   * the class has the attribute 0016 made of that column, found by
--     provenance as in 0019 (so a key 0016 suffixed, "hostname_2", is found;
--     an attribute an administrator created is not).
-- Anything else is left alone. A fresh install, or one whose settings never
-- sorted by these fields, is unchanged.

-- The attribute 0016 made of the column behind an old sort field, in the
-- lineage of the class (NULL: none, or not a sort 0016 rewrote).
CREATE OR REPLACE FUNCTION pg_temp.m0020_key(class_key text, old_field text) RETURNS text LANGUAGE sql STABLE AS $$
  SELECT d.key
  FROM (VALUES ('hostname', 'hostname', 'text'), ('ipAddress', 'ip_address', 'inet'),
               ('serialNumber', 'serial_number', 'text'), ('statusName', 'status', 'uuid')) AS m(field, key, pg_type)
  JOIN cmdb.ci_classes vc ON vc.key = class_key
  JOIN cmdb.ci_attribute_definitions d
    ON cmdb.ci_class_is_a(vc.id, d.class_id) AND d.key ~ ('^' || m.key || '(_[0-9]+)?$') AND d.data_type <> 'reference'
  JOIN cmdb.ci_classes c ON c.id = d.class_id
  JOIN cmdb.areas a ON a.id = c.area_id
  JOIN cmdb.schema_changes s
    ON s.actor_type = 'system' AND s.actor_name = 'migration 0016'
   AND d.created_at = s.occurred_at
   AND format('ALTER TABLE %I.%I ADD COLUMN %I %s', a.key, c.key, d.key, m.pg_type) = ANY (s.statements)
  WHERE m.field = old_field
  ORDER BY d.key
  LIMIT 1;
$$;
--> statement-breakpoint
-- The sort to store: the current one, or the attribute sort it was before 0016.
CREATE OR REPLACE FUNCTION pg_temp.m0020_sort(cur jsonb, at_0016 jsonb, before jsonb, class_key text)
RETURNS jsonb LANGUAGE sql STABLE AS $$
  SELECT CASE WHEN jsonb_typeof(cur) = 'object' AND cur->>'field' = 'label' AND cur = at_0016
                   AND jsonb_typeof(before) = 'object' AND k.key IS NOT NULL
              THEN jsonb_set(cur, '{field}', to_jsonb('attributes.' || k.key))
              ELSE cur END
  FROM (SELECT CASE WHEN jsonb_typeof(before) = 'object' THEN pg_temp.m0020_key(class_key, before->>'field') END AS key) k;
$$;
--> statement-breakpoint
-- The first element of an array whose property equals the value.
CREATE OR REPLACE FUNCTION pg_temp.m0020_find(arr jsonb, prop text, val text) RETURNS jsonb LANGUAGE sql IMMUTABLE AS $$
  SELECT x FROM jsonb_array_elements(CASE WHEN jsonb_typeof(arr) = 'array' THEN arr ELSE '[]' END) WITH ORDINALITY AS e(x, n)
  WHERE x->>prop = val ORDER BY n LIMIT 1;
$$;
--> statement-breakpoint
CREATE OR REPLACE FUNCTION pg_temp.m0020_settings(s jsonb, at_0016 jsonb, before jsonb) RETURNS jsonb LANGUAGE sql STABLE AS $$
  SELECT s
    || CASE WHEN jsonb_typeof(s->'listViews') = 'array' THEN jsonb_build_object('listViews',
         (SELECT coalesce(jsonb_agg(
            CASE WHEN jsonb_typeof(v->'defaultSort') = 'object' THEN
              jsonb_set(v, '{defaultSort}', pg_temp.m0020_sort(v->'defaultSort',
                pg_temp.m0020_find(at_0016->'listViews', 'classKey', v->>'classKey')->'defaultSort',
                pg_temp.m0020_find(before->'listViews', 'classKey', v->>'classKey')->'defaultSort',
                v->>'classKey'))
            ELSE v END
            ORDER BY n), '[]')
          FROM jsonb_array_elements(s->'listViews') WITH ORDINALITY AS e(v, n)))
       ELSE '{}' END
    || CASE WHEN jsonb_typeof(s->'dashboard'->'widgets') = 'array' THEN jsonb_build_object('dashboard',
         (s->'dashboard') || jsonb_build_object('widgets',
           (SELECT coalesce(jsonb_agg(
              CASE WHEN jsonb_typeof(w->'search'->'sort') = 'object'
                        AND jsonb_typeof(w->'search'->'classKeys') = 'array'
                        AND jsonb_array_length(w->'search'->'classKeys') = 1 THEN
                jsonb_set(w, '{search,sort}', pg_temp.m0020_sort(w->'search'->'sort',
                  pg_temp.m0020_find(at_0016->'dashboard'->'widgets', 'id', w->>'id')->'search'->'sort',
                  pg_temp.m0020_find(before->'dashboard'->'widgets', 'id', w->>'id')->'search'->'sort',
                  w->'search'->'classKeys'->>0))
              ELSE w END
              ORDER BY n), '[]')
            FROM jsonb_array_elements(s->'dashboard'->'widgets') WITH ORDINALITY AS e(w, n))))
       ELSE '{}' END;
$$;
--> statement-breakpoint
DO $$
DECLARE
  v_0016 integer;
  at_0016 jsonb;
  before jsonb;
  current record;
  updated jsonb;
BEGIN
  SELECT version, settings INTO v_0016, at_0016 FROM cmdb.ui_settings_versions
  WHERE actor_type = 'system' AND actor_name = 'migration 0016';
  IF NOT FOUND THEN
    RETURN;
  END IF;
  SELECT settings INTO before FROM cmdb.ui_settings_versions WHERE version = v_0016 - 1;
  IF NOT FOUND THEN
    RETURN;
  END IF;
  SELECT version, settings INTO current FROM cmdb.ui_settings FOR UPDATE;
  IF NOT FOUND THEN
    RETURN;
  END IF;
  updated := pg_temp.m0020_settings(current.settings, at_0016, before);
  IF updated IS DISTINCT FROM current.settings THEN
    INSERT INTO cmdb.ui_settings_versions (version, settings, actor_type, actor_name, comment)
    SELECT max(version) + 1, updated, 'system', 'migration 0020',
           'Inventory sorts on hostname, IP address, serial number and status restored (attributes.<key>)'
    FROM cmdb.ui_settings_versions;
    UPDATE cmdb.ui_settings
    SET settings = updated, version = (SELECT max(version) FROM cmdb.ui_settings_versions),
        updated_at = now(), updated_by_id = NULL, updated_by_name = 'migration 0020';
  END IF;
END;
$$;
--> statement-breakpoint

-- The helpers live in this session's temporary schema; the connection may run more migrations.
DROP FUNCTION pg_temp.m0020_settings(jsonb, jsonb, jsonb);
DROP FUNCTION pg_temp.m0020_find(jsonb, text, text);
DROP FUNCTION pg_temp.m0020_sort(jsonb, jsonb, jsonb, text);
DROP FUNCTION pg_temp.m0020_key(text, text);
