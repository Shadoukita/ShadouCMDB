-- Record details and relationships become explicit layout sections (layout
-- format 3, SHAA-1643).
--
-- A detail page layout with tabs used to show two things it did not place:
-- the record details (class, created, last changed: a "Record" section last on
-- the first tab) and, when no `relations` section was placed, the relationships
-- after it. Both are now sections like the others (kinds `record` and
-- `relations`), which an administrator can move or remove; a layout shows them
-- only where it places them.
--
-- Conversion, so that every detail page looks as before:
--   * every layout with tabs that does not place a `record` section gets one
--     (key "record", label "Record"), then, unless it places `relations`
--     already, a "Relationships" section (key "relations"), both at the end of
--     its first tab; a key that is taken gets _2, _3...;
--   * this applies to the layout templates and the class layouts in the UI
--     settings document, and to the CIs' own layouts (ci_layout_overrides);
--   * a layout without tabs is left alone: it shows the built-in arrangement,
--     which has both sections.
-- The new sections have no frame: the API places them below the tab's
-- windows when it reads the layout, and stores the frame on the next save.
-- The settings document gets `"layoutFormat": 3`; the API converts a document
-- without it the same way when it is still sent (an older configuration
-- export, an older version being restored). A converted document is saved as
-- a new settings version and each converted CI layout gets its version bumped,
-- each with an `update` row in the audit log, actor `migration 0048`, in the
-- shape the API writes.
--
-- Only JSON documents change; no table or column does.

CREATE OR REPLACE FUNCTION pg_temp.m0048_tabs(tabs jsonb) RETURNS jsonb LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE
  kinds text[];
  keys text[];
  added jsonb := '[]';
  p record;
  candidate text;
  i integer;
  first jsonb;
BEGIN
  IF jsonb_typeof(tabs) IS DISTINCT FROM 'array' OR jsonb_array_length(tabs) = 0
     OR jsonb_typeof(tabs->0) IS DISTINCT FROM 'object' THEN
    RETURN tabs;
  END IF;
  SELECT coalesce(array_agg(s.sec->>'kind'), '{}'), coalesce(array_agg(s.sec->>'key'), '{}')
  INTO kinds, keys
  FROM jsonb_array_elements(tabs) AS t(tab),
       jsonb_array_elements(CASE WHEN jsonb_typeof(t.tab->'sections') = 'array' THEN t.tab->'sections' ELSE '[]' END) AS s(sec);
  FOR p IN SELECT * FROM (VALUES (1, 'record', 'Record'), (2, 'relations', 'Relationships')) AS v(o, kind, label) ORDER BY o LOOP
    CONTINUE WHEN p.kind = ANY(kinds);
    candidate := p.kind;
    i := 1;
    WHILE candidate = ANY(keys) LOOP
      i := i + 1;
      candidate := p.kind || '_' || i;
    END LOOP;
    keys := keys || candidate;
    added := added || jsonb_build_array(jsonb_build_object(
      'key', candidate, 'label', p.label, 'kind', p.kind,
      'columns', 3, 'width', 12, 'fields', '[]'::jsonb, 'collapsed', false));
  END LOOP;
  IF jsonb_array_length(added) = 0 THEN
    RETURN tabs;
  END IF;
  first := tabs->0;
  first := first || jsonb_build_object('sections',
    coalesce(CASE WHEN jsonb_typeof(first->'sections') = 'array' THEN first->'sections' END, '[]') || added);
  RETURN jsonb_set(tabs, '{0}', first);
END;
$$;
--> statement-breakpoint
-- A layout object (tabs, hiddenFields, readOnlyFields), converted.
CREATE OR REPLACE FUNCTION pg_temp.m0048_layout(l jsonb) RETURNS jsonb LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE WHEN jsonb_typeof(l) = 'object' AND jsonb_typeof(l->'tabs') = 'array'
              THEN l || jsonb_build_object('tabs', pg_temp.m0048_tabs(l->'tabs'))
              ELSE l END;
$$;
--> statement-breakpoint
DO $$
DECLARE
  current record;
  updated jsonb;
  new_version integer;
  note text := 'Record details and relationships placed as sections of the layouts (layout format 3)';
BEGIN
  SELECT id, version, settings INTO current FROM cmdb.ui_settings FOR UPDATE;
  IF NOT FOUND OR jsonb_typeof(current.settings) IS DISTINCT FROM 'object' THEN
    RETURN;
  END IF;
  updated := current.settings;
  IF jsonb_typeof(updated->'layouts') = 'array' THEN
    updated := updated || jsonb_build_object('layouts',
      (SELECT coalesce(jsonb_agg(pg_temp.m0048_layout(l) ORDER BY n), '[]')
       FROM jsonb_array_elements(updated->'layouts') WITH ORDINALITY AS e(l, n)));
  END IF;
  IF jsonb_typeof(updated->'layoutTemplates') = 'array' THEN
    updated := updated || jsonb_build_object('layoutTemplates',
      (SELECT coalesce(jsonb_agg(
                CASE WHEN jsonb_typeof(t) = 'object' AND jsonb_typeof(t->'layout') = 'object'
                     THEN t || jsonb_build_object('layout', pg_temp.m0048_layout(t->'layout'))
                     ELSE t END ORDER BY n), '[]')
       FROM jsonb_array_elements(updated->'layoutTemplates') WITH ORDINALITY AS e(t, n)));
  END IF;
  -- Nothing to place: the document stays as it is (the API reads it the same way).
  IF updated = current.settings THEN
    RETURN;
  END IF;
  updated := updated || jsonb_build_object('layoutFormat', 3);

  SELECT max(version) + 1 INTO new_version FROM cmdb.ui_settings_versions;
  INSERT INTO cmdb.ui_settings_versions (version, settings, actor_type, actor_name, comment)
  VALUES (new_version, updated, 'system', 'migration 0048', note);
  UPDATE cmdb.ui_settings
  SET settings = updated, version = new_version,
      updated_at = now(), updated_by_id = NULL, updated_by_name = 'migration 0048';
  INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
  VALUES ('system', 'migration 0048', 'update', 'ui_settings', current.id,
          jsonb_build_object('version', current.version, 'settings', current.settings),
          jsonb_build_object('version', new_version, 'settings', updated, 'comment', note));
END;
$$;
--> statement-breakpoint
-- The CIs' own layouts, each audited like a change through the API (old and new: ciId, templateKey, layout, version).
WITH changed AS (
  SELECT o.ci_id, o.template_key, o.layout AS old_layout, o.version AS old_version,
         pg_temp.m0048_layout(o.layout) AS new_layout
  FROM cmdb.ci_layout_overrides o
  WHERE o.layout IS NOT NULL
  FOR UPDATE
), written AS (
  UPDATE cmdb.ci_layout_overrides o
  SET layout = c.new_layout, version = o.version + 1,
      updated_by_type = 'system', updated_by_id = NULL, updated_by_name = 'migration 0048'
  FROM changed c
  WHERE o.ci_id = c.ci_id AND c.new_layout IS DISTINCT FROM c.old_layout
  RETURNING o.ci_id, o.template_key, o.layout, o.version, c.old_layout, c.old_version
)
INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
SELECT 'system', 'migration 0048', 'update', 'ci_layout_overrides', w.ci_id,
       jsonb_build_object('ciId', w.ci_id, 'templateKey', w.template_key, 'layout', w.old_layout, 'version', w.old_version),
       jsonb_build_object('ciId', w.ci_id, 'templateKey', w.template_key, 'layout', w.layout, 'version', w.version)
FROM written w
ORDER BY w.ci_id;
--> statement-breakpoint
DROP FUNCTION pg_temp.m0048_layout(jsonb);
--> statement-breakpoint
DROP FUNCTION pg_temp.m0048_tabs(jsonb);
