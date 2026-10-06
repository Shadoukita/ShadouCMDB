-- Layout windows as tall as the detail page's inline inputs (layout format 4,
-- SHAA-1833, GH#621).
--
-- A free tab's sections are windows with a stored frame. A section without
-- one (a tab on the earlier grid, converted by migrations 0016, 0017 and 0042
-- or saved through the API without frames) got its frame from the API's height
-- estimate: 48 px for the title bar and 48 px per row of fields. Since the
-- detail page edits fields in place (SHAA-1644), a row of fields (label, input
-- and hint) needs about 80 px, so those windows cut their inputs in half behind
-- an inner scrollbar. The estimate is now 88 px for the title bar and the
-- padding, 82 px per row of fields, 28 px per separator and 252 px for the
-- record details (backend ui_settings/document.rs FRAME_METRICS).
--
-- Conversion:
--   * a tab whose framed sections all sit exactly where the old estimate put
--     them from the grid (same x, y, w, h and z, no minH) gets the frames of the
--     new estimate: the same columns and order, taller rows further down;
--   * a tab with any window moved, resized or stacked differently in the
--     layout editor is left exactly as it is, as is every section without a
--     frame (the API places those below the others when it reads the layout);
--   * only frames change: every section, field, separator, note, hidden and
--     read-only field is kept;
--   * this applies to the layout templates and the class layouts in the UI
--     settings document, and to the CIs' own layouts (ci_layout_overrides).
-- A changed settings document gets `"layoutFormat": 4` and is saved as a new
-- settings version, and each changed CI layout gets its version bumped, each
-- with an `update` row in the audit log, actor `migration 0053`, in the shape
-- the API writes. The API converts a document of an older format (an older
-- configuration export, an older version being restored) the same way
-- (UiSettingsDocument::upgraded, resize_default_frames).
--
-- Only JSON documents change; no table or column does.

-- The height of a section's frame from the grid: the old estimate (`legacy`) or the new one.
CREATE OR REPLACE FUNCTION pg_temp.m0053_height(sec jsonb, legacy boolean) RETURNS integer LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE
  kind text := coalesce(sec->>'kind', 'fields');
  cols integer := coalesce((sec->>'columns')::integer, 3);
  nrows integer := 0;
  lines integer := 0;
  col integer;
  w integer;
  f jsonb;
  sep boolean;
  h integer;
BEGIN
  IF kind = 'note' THEN
    h := 144;
  ELSIF kind = 'record' THEN
    h := CASE WHEN legacy THEN 144 ELSE 252 END;
  ELSIF kind <> 'fields' THEN
    h := 320;
  ELSE
    col := cols;
    FOR f IN SELECT e FROM jsonb_array_elements(CASE WHEN jsonb_typeof(sec->'fields') = 'array' THEN sec->'fields' ELSE '[]' END) AS a(e) LOOP
      sep := coalesce((f->>'separator')::boolean, false);
      -- Now a separator is a line of its own, and the next field starts a new row.
      IF sep AND NOT legacy THEN
        lines := lines + 1;
        col := cols;
        CONTINUE;
      END IF;
      w := CASE WHEN sep THEN greatest(cols, 1) ELSE least(greatest(coalesce((f->>'width')::integer, 1), 1), greatest(cols, 1)) END;
      IF col + w > cols THEN
        nrows := nrows + 1;
        col := 0;
      END IF;
      col := col + w;
    END LOOP;
    h := CASE WHEN legacy THEN 48 ELSE 88 END
         + greatest(nrows, coalesce((sec->>'minHeight')::integer, 1), 1) * CASE WHEN legacy THEN 48 ELSE 82 END
         + lines * 28;
  END IF;
  RETURN least(greatest(h, 48), 4000);
END;
$$;
--> statement-breakpoint
-- A tab, with the new frames when every framed section is where the old estimate put it; else as it is.
CREATE OR REPLACE FUNCTION pg_temp.m0053_tab(tab jsonb) RETURNS jsonb LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE
  sec jsonb;
  secs jsonb := '[]';
  col integer := 0;
  w integer;
  z integer := 0;
  old_y integer := 0;
  old_row integer := 0;
  new_y integer := 0;
  new_row integer := 0;
  old_h integer;
  new_h integer;
  changed boolean := false;
BEGIN
  IF jsonb_typeof(tab) IS DISTINCT FROM 'object' OR jsonb_typeof(tab->'sections') IS DISTINCT FROM 'array' THEN
    RETURN tab;
  END IF;
  FOR sec IN SELECT e FROM jsonb_array_elements(tab->'sections') WITH ORDINALITY AS a(e, n) ORDER BY n LOOP
    IF jsonb_typeof(sec) IS DISTINCT FROM 'object' OR jsonb_typeof(sec->'frame') IS DISTINCT FROM 'object' THEN
      secs := secs || jsonb_build_array(sec);
      CONTINUE;
    END IF;
    w := least(greatest(coalesce((sec->>'width')::integer, 12), 1), 12);
    IF col > 0 AND (coalesce((sec->>'newRow')::boolean, false) OR col + w > 12) THEN
      old_y := old_y + old_row + 16;
      new_y := new_y + new_row + 16;
      col := 0;
      old_row := 0;
      new_row := 0;
    END IF;
    old_h := pg_temp.m0053_height(sec, true);
    new_h := pg_temp.m0053_height(sec, false);
    z := z + 1;
    IF sec->'frame' IS DISTINCT FROM jsonb_build_object(
         'x', round(col / 12.0, 4), 'y', least(old_y, 100000), 'w', round(w / 12.0, 4), 'h', old_h, 'z', z) THEN
      -- Arranged in the layout editor (or by hand): left alone.
      RETURN tab;
    END IF;
    changed := changed OR least(old_y, 100000) <> least(new_y, 100000) OR old_h <> new_h;
    secs := secs || jsonb_build_array(sec || jsonb_build_object(
      'frame', (sec->'frame') || jsonb_build_object('y', least(new_y, 100000), 'h', new_h)));
    col := col + w;
    old_row := greatest(old_row, old_h);
    new_row := greatest(new_row, new_h);
  END LOOP;
  IF NOT changed THEN
    RETURN tab;
  END IF;
  RETURN tab || jsonb_build_object('sections', secs);
END;
$$;
--> statement-breakpoint
-- A layout object (tabs, hiddenFields, readOnlyFields), converted.
CREATE OR REPLACE FUNCTION pg_temp.m0053_layout(l jsonb) RETURNS jsonb LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE WHEN jsonb_typeof(l) = 'object' AND jsonb_typeof(l->'tabs') = 'array'
              THEN l || jsonb_build_object('tabs',
                     (SELECT coalesce(jsonb_agg(pg_temp.m0053_tab(t) ORDER BY n), '[]')
                      FROM jsonb_array_elements(l->'tabs') WITH ORDINALITY AS e(t, n)))
              ELSE l END;
$$;
--> statement-breakpoint
DO $$
DECLARE
  current record;
  updated jsonb;
  new_version integer;
  note text := 'Layout windows placed from the grid made as tall as the inline inputs need (layout format 4)';
BEGIN
  SELECT id, version, settings INTO current FROM cmdb.ui_settings FOR UPDATE;
  IF NOT FOUND OR jsonb_typeof(current.settings) IS DISTINCT FROM 'object' THEN
    RETURN;
  END IF;
  updated := current.settings;
  IF jsonb_typeof(updated->'layouts') = 'array' THEN
    updated := updated || jsonb_build_object('layouts',
      (SELECT coalesce(jsonb_agg(pg_temp.m0053_layout(l) ORDER BY n), '[]')
       FROM jsonb_array_elements(updated->'layouts') WITH ORDINALITY AS e(l, n)));
  END IF;
  IF jsonb_typeof(updated->'layoutTemplates') = 'array' THEN
    updated := updated || jsonb_build_object('layoutTemplates',
      (SELECT coalesce(jsonb_agg(
                CASE WHEN jsonb_typeof(t) = 'object' AND jsonb_typeof(t->'layout') = 'object'
                     THEN t || jsonb_build_object('layout', pg_temp.m0053_layout(t->'layout'))
                     ELSE t END ORDER BY n), '[]')
       FROM jsonb_array_elements(updated->'layoutTemplates') WITH ORDINALITY AS e(t, n)));
  END IF;
  -- Nothing to resize: the document stays as it is (the API reads it the same way).
  IF updated = current.settings THEN
    RETURN;
  END IF;
  updated := updated || jsonb_build_object('layoutFormat', 4);

  SELECT max(version) + 1 INTO new_version FROM cmdb.ui_settings_versions;
  INSERT INTO cmdb.ui_settings_versions (version, settings, actor_type, actor_name, comment)
  VALUES (new_version, updated, 'system', 'migration 0053', note);
  UPDATE cmdb.ui_settings
  SET settings = updated, version = new_version,
      updated_at = now(), updated_by_id = NULL, updated_by_name = 'migration 0053';
  INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
  VALUES ('system', 'migration 0053', 'update', 'ui_settings', current.id,
          jsonb_build_object('version', current.version, 'settings', current.settings),
          jsonb_build_object('version', new_version, 'settings', updated, 'comment', note));
END;
$$;
--> statement-breakpoint
-- The CIs' own layouts, each audited like a change through the API (old and new: ciId, templateKey, layout, version).
WITH changed AS (
  SELECT o.ci_id, o.template_key, o.layout AS old_layout, o.version AS old_version,
         pg_temp.m0053_layout(o.layout) AS new_layout
  FROM cmdb.ci_layout_overrides o
  WHERE o.layout IS NOT NULL
  FOR UPDATE
), written AS (
  UPDATE cmdb.ci_layout_overrides o
  SET layout = c.new_layout, version = o.version + 1,
      updated_by_type = 'system', updated_by_id = NULL, updated_by_name = 'migration 0053'
  FROM changed c
  WHERE o.ci_id = c.ci_id AND c.new_layout IS DISTINCT FROM c.old_layout
  RETURNING o.ci_id, o.template_key, o.layout, o.version, c.old_layout, c.old_version
)
INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
SELECT 'system', 'migration 0053', 'update', 'ci_layout_overrides', w.ci_id,
       jsonb_build_object('ciId', w.ci_id, 'templateKey', w.template_key, 'layout', w.old_layout, 'version', w.old_version),
       jsonb_build_object('ciId', w.ci_id, 'templateKey', w.template_key, 'layout', w.layout, 'version', w.version)
FROM written w
ORDER BY w.ci_id;
--> statement-breakpoint
DROP FUNCTION pg_temp.m0053_layout(jsonb);
--> statement-breakpoint
DROP FUNCTION pg_temp.m0053_tab(jsonb);
--> statement-breakpoint
DROP FUNCTION pg_temp.m0053_height(jsonb, boolean);
