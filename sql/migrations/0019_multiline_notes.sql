-- Multi-line text attributes (GH#109, SHAA-302).
--
-- Text attributes gain the validation rule "multiline": true, which tells the
-- forms to edit the value in a text area and keep its line breaks. Migration
-- 0016 turned the fixed column configuration_items.notes into a plain text
-- field "notes" (or "notes_<n>" where the key was taken) on every class that
-- held notes; those fields get the flag here, since their values were always
-- free text with line breaks.
--
-- The fields are found by provenance, not by label: a definition 0016 created
-- was inserted in the same transaction that recorded the "migration 0016"
-- schema change (so its created_at is that change's occurred_at), and that
-- change added the field's column to its class table. A field an administrator
-- created later, even one called "notes", is left alone. Fields that are no
-- longer text are skipped. Each change is written to the audit log.
--
-- A fresh install has no such fields; its IT infrastructure template creates
-- "notes" with the flag set. Values are not touched.

WITH migrated AS (
  SELECT d.id, d.validation AS old_validation
  FROM cmdb.ci_attribute_definitions d
  JOIN cmdb.ci_classes c ON c.id = d.class_id
  JOIN cmdb.areas a ON a.id = c.area_id
  JOIN cmdb.schema_changes s
    ON s.actor_type = 'system' AND s.actor_name = 'migration 0016'
   AND d.created_at = s.occurred_at
   AND format('ALTER TABLE %I.%I ADD COLUMN %I text', a.key, c.key, d.key) = ANY (s.statements)
  WHERE d.key ~ '^notes(_[0-9]+)?$'
    AND d.data_type = 'text'
    AND NOT coalesce(d.validation @> '{"multiline": true}', false)
),
updated AS (
  UPDATE cmdb.ci_attribute_definitions d
  SET validation = coalesce(d.validation, '{}'::jsonb) || '{"multiline": true}'::jsonb
  FROM migrated m
  WHERE d.id = m.id
  RETURNING d.id, d.validation
)
INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
SELECT 'system', 'migration 0019', 'update', 'ci_attribute_definitions', u.id,
       jsonb_build_object('validation', m.old_validation), jsonb_build_object('validation', u.validation)
FROM updated u JOIN migrated m ON m.id = u.id;
