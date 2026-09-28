-- Layout format v2 in the UI settings (form designer, SHAA-271).
--
-- A class layout (settings.layouts[]) was a list of panels, each an ordered
-- list of field names. It is now tabs -> sections -> fields on a grid:
--   { "tabs": [ { "key", "label", "sections": [
--       { "key", "label", "columns": 1-4, "collapsed",
--         "fields": [ { "field": "attributes.cpu_cores", "width": 1-4 } ] } ] } ] }
--
-- Conversion (the API converts the old format the same way when it is still
-- sent, e.g. by an older configuration export):
--   * the panels become the sections of one tab "General" (key "general"),
--     in their order, keeping key, label, collapsed and the field order;
--   * each section gets 3 columns and each field a width of 1;
--   * a layout without panels gets no tabs (everything keeps its default place);
--   * ident, validFrom and validUntil can no longer be hidden: they are taken
--     out of hiddenFields (they were shown on the form anyway when required).
-- Nothing else in the document changes. The converted document is saved as a
-- new settings version, so the previous one stays in the history; that older
-- version can still be restored (the API converts it again).
--
-- Only the settings document changes; no table or column does.

CREATE OR REPLACE FUNCTION pg_temp.m0017_layout(l jsonb) RETURNS jsonb LANGUAGE sql IMMUTABLE AS $$
  SELECT (l - 'panels')
    || CASE WHEN jsonb_typeof(l->'panels') = 'array' AND jsonb_array_length(l->'panels') > 0 THEN
         jsonb_build_object('tabs', jsonb_build_array(jsonb_build_object(
           'key', 'general', 'label', 'General',
           'sections', (SELECT jsonb_agg(
               jsonb_build_object('key', p->'key', 'label', p->'label', 'columns', 3,
                 'fields', (SELECT coalesce(jsonb_agg(jsonb_build_object('field', f, 'width', 1) ORDER BY fn), '[]')
                            FROM jsonb_array_elements(coalesce(p->'fields', '[]')) WITH ORDINALITY AS g(f, fn)),
                 'collapsed', coalesce(p->'collapsed', 'false'))
               ORDER BY pn)
             FROM jsonb_array_elements(l->'panels') WITH ORDINALITY AS q(p, pn)))))
       ELSE '{}' END
    || CASE WHEN jsonb_typeof(l->'hiddenFields') = 'array' THEN jsonb_build_object('hiddenFields',
         (SELECT coalesce(jsonb_agg(f ORDER BY fn), '[]')
          FROM jsonb_array_elements(l->'hiddenFields') WITH ORDINALITY AS h(f, fn)
          WHERE f #>> '{}' NOT IN ('ident', 'validFrom', 'validUntil')))
       ELSE '{}' END;
$$;
--> statement-breakpoint
DO $$
DECLARE
  current record;
  updated jsonb;
BEGIN
  SELECT version, settings INTO current FROM cmdb.ui_settings FOR UPDATE;
  IF NOT FOUND OR jsonb_typeof(current.settings->'layouts') IS DISTINCT FROM 'array' THEN
    RETURN;
  END IF;
  updated := current.settings || jsonb_build_object('layouts',
    (SELECT coalesce(jsonb_agg(CASE WHEN jsonb_typeof(l) = 'object' THEN pg_temp.m0017_layout(l) ELSE l END ORDER BY n), '[]')
     FROM jsonb_array_elements(current.settings->'layouts') WITH ORDINALITY AS e(l, n)));
  IF updated IS DISTINCT FROM current.settings THEN
    INSERT INTO cmdb.ui_settings_versions (version, settings, actor_type, actor_name, comment)
    SELECT max(version) + 1, updated, 'system', 'migration 0017',
           'Detail and form layouts converted to tabs and sections (layout format v2)'
    FROM cmdb.ui_settings_versions;
    UPDATE cmdb.ui_settings
    SET settings = updated, version = (SELECT max(version) FROM cmdb.ui_settings_versions),
        updated_at = now(), updated_by_id = NULL, updated_by_name = 'migration 0017';
  END IF;
END;
$$;
--> statement-breakpoint

DROP FUNCTION pg_temp.m0017_layout(jsonb);
