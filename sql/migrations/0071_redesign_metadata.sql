-- Metadata for the redesigned inventory and CI pages (SHAA-2357; gaps G8, G15
-- and G16 of the SHAA-1670 design).
--
-- G8  ci_classes.subtitle_attribute_id: the field whose value the UI shows
--     under a CI's name (a server's model). Any field of the class or an
--     ancestor, of any data type; null shows the class name. Not stored per
--     CI: the inventory and the CI already carry every field value. A move to
--     a parent that does not provide it clears it, as for the title (0016).
-- G15 relationship_types.category: an optional group heading ("Location",
--     "Network & power") the CI page groups relationships by. Free text an
--     administrator sets on the type; types with the same text form a group.
-- G16 cmdb.ci_last_changes: who last changed each CI, and when, without
--     reading the audit log. One row per CI, written only by a trigger on
--     cmdb.audit_log from the newest create, update, delete or restore entry
--     on the CI, so it always names the actor that entry names. Read events
--     (export) and workflow events do not count. Audit retention does not
--     touch it.
--
-- Additive: two nullable columns, one table, two triggers. The starter
-- template's located_in and connected_to get a category while their labels
-- are still the template's; the backfill copies each CI's newest such audit
-- entry (a CI whose entries were pruned gets no row until its next change).

-- ---------------------------------------------------------------------------
-- G8: subtitle attribute
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.ci_classes
  ADD COLUMN subtitle_attribute_id uuid REFERENCES cmdb.ci_attribute_definitions (id) ON DELETE SET NULL;
--> statement-breakpoint
CREATE INDEX ci_classes_subtitle_attribute_idx ON cmdb.ci_classes (subtitle_attribute_id)
  WHERE subtitle_attribute_id IS NOT NULL;
--> statement-breakpoint

-- The subtitle attribute must be a field of the class or one of its ancestors.
-- When a class moves to a parent that does not provide it, it is cleared (the
-- API then gives the moved type and its subtypes the new parent's).
CREATE FUNCTION cmdb.ci_classes_subtitle_attribute() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
DECLARE
  def_class uuid;
BEGIN
  IF NEW.subtitle_attribute_id IS NULL THEN
    RETURN NEW;
  END IF;
  SELECT class_id INTO def_class FROM cmdb.ci_attribute_definitions WHERE id = NEW.subtitle_attribute_id;
  -- An unknown id is left to the foreign key.
  IF def_class IS NULL THEN
    RETURN NEW;
  END IF;
  -- The lineage as it will be: the row itself is not (or not yet so) in the table.
  IF NOT (def_class = NEW.id OR (NEW.parent_id IS NOT NULL AND cmdb.ci_class_is_a(NEW.parent_id, def_class))) THEN
    IF TG_OP = 'UPDATE' AND NEW.subtitle_attribute_id IS NOT DISTINCT FROM OLD.subtitle_attribute_id THEN
      NEW.subtitle_attribute_id := NULL;
      RETURN NEW;
    END IF;
    RAISE EXCEPTION 'ci_classes: the subtitle attribute must be defined on class % or one of its ancestors', NEW.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_classes_subtitle_attribute_in_lineage';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER ci_classes_subtitle_attribute
  BEFORE INSERT OR UPDATE OF subtitle_attribute_id, parent_id ON cmdb.ci_classes
  FOR EACH ROW EXECUTE FUNCTION cmdb.ci_classes_subtitle_attribute();
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- G15: relationship type category
-- ---------------------------------------------------------------------------
ALTER TABLE cmdb.relationship_types
  ADD COLUMN category text,
  ADD CONSTRAINT relationship_types_category_valid
    CHECK (category IS NULL OR (category = btrim(category) AND category <> '' AND char_length(category) <= 100));
--> statement-breakpoint

-- The updated_at trigger would stamp every seeded row; this is not an edit.
ALTER TABLE cmdb.relationship_types DISABLE TRIGGER relationship_types_set_updated_at;
--> statement-breakpoint
UPDATE cmdb.relationship_types t
SET category = template.category
FROM (VALUES ('located_in', 'is located in', 'contains', true, 'Location'),
             ('connected_to', 'is connected to', 'is connected to', false, 'Network & power'))
     AS template (key, forward_label, reverse_label, is_directional, category)
WHERE t.key = template.key
  AND t.forward_label = template.forward_label
  AND t.reverse_label = template.reverse_label
  AND t.is_directional = template.is_directional
  AND t.category IS NULL;
--> statement-breakpoint
ALTER TABLE cmdb.relationship_types ENABLE TRIGGER relationship_types_set_updated_at;
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- G16: last change per CI, from the audit log
-- ---------------------------------------------------------------------------
CREATE TABLE cmdb.ci_last_changes (
  ci_id      uuid PRIMARY KEY REFERENCES cmdb.configuration_items (id) ON DELETE CASCADE,
  -- The audit entry the row copies (audit_log.id); newer entries replace it.
  audit_id   bigint NOT NULL,
  changed_at timestamptz NOT NULL,
  action     text NOT NULL,
  actor_type text NOT NULL,
  actor_id   text,
  actor_name text,
  CONSTRAINT ci_last_changes_action_valid CHECK (action IN ('create', 'update', 'delete', 'restore')),
  CONSTRAINT ci_last_changes_actor_type_valid CHECK (actor_type IN ('system', 'user', 'api_client', 'import'))
);
--> statement-breakpoint
COMMENT ON TABLE cmdb.ci_last_changes IS
  'Newest create/update/delete/restore audit entry per CI (actor and time). Written by the audit_log trigger only.';
--> statement-breakpoint

-- After each INSERT statement on the audit log: the newest data change per CI
-- in the statement replaces an older one. A CI already gone (a type purge
-- records its delete entries) is skipped. SECURITY DEFINER, because the API
-- role may only read the table; it runs only from the trigger (EXECUTE is
-- revoked from PUBLIC).
CREATE FUNCTION cmdb.audit_log_track_ci_changes() RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
  INSERT INTO cmdb.ci_last_changes AS l (ci_id, audit_id, changed_at, action, actor_type, actor_id, actor_name)
  SELECT DISTINCT ON (a.entity_id) a.entity_id, a.id, a.occurred_at, a.action, a.actor_type, a.actor_id, a.actor_name
  FROM added a
  WHERE a.entity_type = 'configuration_items'
    AND a.action IN ('create', 'update', 'delete', 'restore')
    AND EXISTS (SELECT FROM cmdb.configuration_items ci WHERE ci.id = a.entity_id)
  ORDER BY a.entity_id, a.id DESC
  ON CONFLICT (ci_id) DO UPDATE
    SET audit_id = EXCLUDED.audit_id, changed_at = EXCLUDED.changed_at, action = EXCLUDED.action,
        actor_type = EXCLUDED.actor_type, actor_id = EXCLUDED.actor_id, actor_name = EXCLUDED.actor_name
    WHERE l.audit_id < EXCLUDED.audit_id;
  RETURN NULL;
END;
$$;
--> statement-breakpoint
REVOKE ALL ON FUNCTION cmdb.audit_log_track_ci_changes() FROM PUBLIC;
--> statement-breakpoint
CREATE TRIGGER audit_log_track_ci_changes
  AFTER INSERT ON cmdb.audit_log REFERENCING NEW TABLE AS added
  FOR EACH STATEMENT EXECUTE FUNCTION cmdb.audit_log_track_ci_changes();
--> statement-breakpoint

-- Existing CIs: their newest such entry still in the log.
INSERT INTO cmdb.ci_last_changes (ci_id, audit_id, changed_at, action, actor_type, actor_id, actor_name)
SELECT DISTINCT ON (a.entity_id) a.entity_id, a.id, a.occurred_at, a.action, a.actor_type, a.actor_id, a.actor_name
FROM cmdb.audit_log a
JOIN cmdb.configuration_items ci ON ci.id = a.entity_id
WHERE a.entity_type = 'configuration_items'
  AND a.action IN ('create', 'update', 'delete', 'restore')
ORDER BY a.entity_id, a.id DESC;
--> statement-breakpoint

-- The API role reads the table; rows arrive through the trigger above.
INSERT INTO cmdb.api_role_privileges (object, object_type, privileges)
  VALUES ('cmdb.ci_last_changes', 'table', '{SELECT}');
--> statement-breakpoint
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
BEGIN
  -- Not on a single-role install, where the API role owns cmdb.
  IF to_regrole(app_role) IS NOT NULL AND current_user <> app_role
     AND (SELECT nspowner FROM pg_namespace WHERE nspname = 'cmdb') <> to_regrole(app_role) THEN
    PERFORM cmdb.apply_api_role_grants(app_role);
  END IF;
END $$;
