-- Layout templates and per-CI layouts (SHAA-1472).
--
-- Detail page and form layouts become named templates in the UI settings
-- document (`layoutTemplates[]`: key, name, description, layout). Each class
-- layout (`layouts[]`) now names its default template (`templateKey`) instead
-- of holding the tabs itself; a CI can use another template or a layout of its
-- own (cmdb.ci_layout_overrides).
--
-- Conversion of the stored settings (the API converts a document without
-- templates the same way when it is still sent, e.g. by an older configuration
-- export, and when an older version is restored):
--   * every class layout with tabs, hidden or read-only fields becomes a
--     template named "<class name> layout" (the class key when the class does
--     not exist), keyed by the class key (with _2, _3... when taken), and the
--     class uses it as its default: the detail page and form look as before;
--   * a class layout without any of them uses the Standard template;
--   * the Standard template (key "standard", no tabs: the built-in
--     arrangement) is added; classes without a layout use it.
-- The converted document is saved as a new settings version (the previous one
-- stays in the history) with an `update` row in the audit log, actor
-- `migration 0042`, in the shape the API writes. Settings without layouts are
-- left as they are; the API adds the Standard template on the next save.
--
-- ci_layout_overrides: at most one row per CI (the primary key), holding
-- either a template key or a layout (never both). Template keys refer into the
-- settings document, not a table, so they are not foreign keys: the API
-- refuses to remove a template a live CI uses. CIs are soft-deleted and keep
-- their row, so a deleted CI still renders as it did; the row goes only with
-- the CI itself (ON DELETE CASCADE). Writes are audited by the API (entity
-- type ci_layout_overrides, entity id the CI's id). Not part of configuration
-- export/import, which holds no CI data.

CREATE TABLE cmdb.ci_layout_overrides (
  ci_id           uuid PRIMARY KEY REFERENCES cmdb.configuration_items (id) ON DELETE CASCADE,
  template_key    text CHECK (template_key ~ '^[a-z][a-z0-9_]{0,62}$'),
  layout          jsonb CHECK (jsonb_typeof(layout) = 'object' AND pg_column_size(layout) <= 524288),
  version         integer NOT NULL DEFAULT 1 CHECK (version >= 1),
  created_at      timestamptz NOT NULL DEFAULT now(),
  updated_at      timestamptz NOT NULL DEFAULT now(),
  updated_by_type text NOT NULL CHECK (updated_by_type IN ('system', 'user', 'api_client', 'import')),
  updated_by_id   text,
  updated_by_name text,
  CONSTRAINT ci_layout_overrides_one_source CHECK ((template_key IS NULL) <> (layout IS NULL))
);
--> statement-breakpoint
-- Usage per template (GET /ui-settings/layout-templates/usage, and the check before a template is removed).
CREATE INDEX ci_layout_overrides_template_idx ON cmdb.ci_layout_overrides (template_key) WHERE template_key IS NOT NULL;
--> statement-breakpoint
CREATE TRIGGER ci_layout_overrides_set_updated_at BEFORE UPDATE ON cmdb.ci_layout_overrides
  FOR EACH ROW EXECUTE FUNCTION cmdb.set_updated_at();
--> statement-breakpoint

DO $$
DECLARE
  current record;
  l jsonb;
  n bigint;
  layouts jsonb := '[]';
  templates jsonb := '[]';
  keys text[] := ARRAY['standard'];
  names text[] := ARRAY[]::text[];
  class_key text;
  class_name text;
  candidate text;
  suffix text;
  i integer;
  updated jsonb;
  new_version integer;
BEGIN
  SELECT id, version, settings INTO current FROM cmdb.ui_settings FOR UPDATE;
  IF NOT FOUND OR jsonb_typeof(current.settings->'layouts') IS DISTINCT FROM 'array'
     OR jsonb_array_length(current.settings->'layouts') = 0
     OR current.settings ? 'layoutTemplates' THEN
    RETURN;
  END IF;

  FOR l, n IN SELECT e, o FROM jsonb_array_elements(current.settings->'layouts') WITH ORDINALITY AS a(e, o) ORDER BY o LOOP
    IF jsonb_typeof(l) IS DISTINCT FROM 'object' OR l ? 'templateKey' THEN
      layouts := layouts || jsonb_build_array(l);
      CONTINUE;
    END IF;
    IF coalesce(jsonb_array_length(CASE WHEN jsonb_typeof(l->'tabs') = 'array' THEN l->'tabs' END), 0) = 0
       AND coalesce(jsonb_array_length(CASE WHEN jsonb_typeof(l->'hiddenFields') = 'array' THEN l->'hiddenFields' END), 0) = 0
       AND coalesce(jsonb_array_length(CASE WHEN jsonb_typeof(l->'readOnlyFields') = 'array' THEN l->'readOnlyFields' END), 0) = 0 THEN
      layouts := layouts || jsonb_build_array(
        (l - 'tabs' - 'hiddenFields' - 'readOnlyFields' - 'panels') || jsonb_build_object('templateKey', 'standard'));
      CONTINUE;
    END IF;

    class_key := l->>'classKey';
    -- Key: the class key, else with _2, _3... (at most 63 characters).
    candidate := class_key;
    i := 1;
    WHILE candidate = ANY(keys) LOOP
      i := i + 1;
      suffix := '_' || i;
      candidate := left(class_key, 63 - length(suffix)) || suffix;
    END LOOP;
    keys := keys || candidate;

    SELECT c.name INTO class_name FROM cmdb.ci_classes c WHERE c.key = class_key;
    class_name := btrim(coalesce(class_name, class_key));
    -- Name: "<class name> layout", else "... layout (2)"; at most 100 characters, unique ignoring case.
    i := 1;
    LOOP
      suffix := CASE WHEN i = 1 THEN ' layout' ELSE ' layout (' || i || ')' END;
      EXIT WHEN NOT (lower(rtrim(left(class_name, 100 - length(suffix))) || suffix) = ANY(names));
      i := i + 1;
    END LOOP;
    names := names || lower(rtrim(left(class_name, 100 - length(suffix))) || suffix);

    templates := templates || jsonb_build_array(jsonb_build_object(
      'key', candidate,
      'name', rtrim(left(class_name, 100 - length(suffix))) || suffix,
      'layout', jsonb_build_object(
        'tabs', coalesce(l->'tabs', '[]'),
        'hiddenFields', coalesce(l->'hiddenFields', '[]'),
        'readOnlyFields', coalesce(l->'readOnlyFields', '[]'))));
    layouts := layouts || jsonb_build_array(
      (l - 'tabs' - 'hiddenFields' - 'readOnlyFields' - 'panels') || jsonb_build_object('templateKey', candidate));
  END LOOP;

  -- "Standard" cannot be taken: every new name ends in "layout" or "layout (n)".
  templates := jsonb_build_array(jsonb_build_object(
      'key', 'standard', 'name', 'Standard',
      'layout', jsonb_build_object('tabs', '[]'::jsonb, 'hiddenFields', '[]'::jsonb, 'readOnlyFields', '[]'::jsonb)))
    || templates;
  updated := current.settings || jsonb_build_object('layouts', layouts, 'layoutTemplates', templates);

  SELECT max(version) + 1 INTO new_version FROM cmdb.ui_settings_versions;
  INSERT INTO cmdb.ui_settings_versions (version, settings, actor_type, actor_name, comment)
  VALUES (new_version, updated, 'system', 'migration 0042',
          'Class layouts converted to layout templates, each class using its own as the default');
  UPDATE cmdb.ui_settings
  SET settings = updated, version = new_version,
      updated_at = now(), updated_by_id = NULL, updated_by_name = 'migration 0042';
  INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
  VALUES ('system', 'migration 0042', 'update', 'ui_settings', current.id,
          jsonb_build_object('version', current.version, 'settings', current.settings),
          jsonb_build_object('version', new_version, 'settings', updated,
                             'comment', 'Class layouts converted to layout templates, each class using its own as the default'));
END;
$$;
