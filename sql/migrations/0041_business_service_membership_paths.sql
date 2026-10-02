-- Business service membership changes only through the service (GH#410,
-- SHAA-1389). Membership is meant to change only through
-- /business-services/{id}/members, which keeps the member limit, the configured
-- nesting depth and the members history. Two other paths reached it:
--
--   * Relationship rules on the member type. The 0033 edge trigger ignores
--     them (any class may be a member), but the import mapping took a rule as
--     leave to write member edges itself. The member type now takes no rules;
--     the ones that exist had no effect on what the database accepts and are
--     removed.
--   * Moving a CI into the business service type. 0033 checks nesting when a
--     member edge is written; a member that becomes a service extends the
--     chains above it without any edge being written. The API refuses the move
--     since GH#409; the database now refuses it too. Services are created as
--     services.
--
-- Chains already deeper than the limit (made through either path) are left as
-- they are: nothing is deleted from them, and new edges onto them are refused.
--
-- Each removed rule gets a `delete` row in the audit log (GH#515), written in
-- this transaction so the hash chain continues through it as for any other
-- write: `actorType` system, `actorName` migration 0041, `oldValue` the rule
-- as the API shows it, `newValue` the reason.

INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
SELECT 'system', 'migration 0041', 'delete', 'relationship_type_rules', r.id,
       jsonb_build_object(
         'id', r.id, 'relationshipTypeId', r.relationship_type_id,
         'sourceClassId', r.source_class_id, 'targetClassId', r.target_class_id,
         'createdAt', r.created_at, 'updatedAt', r.updated_at),
       jsonb_build_object(
         'reason', 'The built-in business service membership type takes no rules: any CI can be a member, '
                   'added on the business service',
         'migration', '0041')
FROM cmdb.relationship_type_rules r
JOIN cmdb.relationship_types t ON t.id = r.relationship_type_id
WHERE t.system_role IS NOT NULL
ORDER BY r.created_at, r.id;
--> statement-breakpoint

DELETE FROM cmdb.relationship_type_rules r
USING cmdb.relationship_types t
WHERE t.id = r.relationship_type_id AND t.system_role IS NOT NULL;
--> statement-breakpoint

CREATE FUNCTION cmdb.relationship_type_rules_not_system() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM relationship_types WHERE id = NEW.relationship_type_id AND system_role IS NOT NULL) THEN
    RAISE EXCEPTION 'relationship_type_rules: the built-in business service membership type takes no rules'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'relationship_type_rules_system_type',
            TABLE = 'relationship_type_rules', COLUMN = 'relationship_type_id';
  END IF;
  RETURN NEW;
END;
$$;
--> statement-breakpoint
CREATE TRIGGER relationship_type_rules_not_system
  BEFORE INSERT OR UPDATE OF relationship_type_id ON cmdb.relationship_type_rules
  FOR EACH ROW EXECUTE FUNCTION cmdb.relationship_type_rules_not_system();
--> statement-breakpoint

-- 0034's check, and no CI moves into the business service type. The trigger
-- (0033) fires only when class_id changes.
CREATE OR REPLACE FUNCTION cmdb.configuration_items_keep_service() RETURNS trigger
LANGUAGE plpgsql SET search_path = cmdb, public AS $$
BEGIN
  IF EXISTS (SELECT 1 FROM ci_classes WHERE id = OLD.class_id AND system_role = 'business_service')
     AND (EXISTS (
       SELECT 1 FROM ci_relationships e JOIN relationship_types t ON t.id = e.relationship_type_id
       WHERE e.source_ci_id = OLD.id AND e.deleted_at IS NULL AND t.system_role = 'business_service_member')
     OR EXISTS (SELECT 1 FROM business_service_owners o WHERE o.service_ci_id = OLD.id))
  THEN
    RAISE EXCEPTION 'configuration_items: a business service with members or owners cannot change its type; remove them first'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'configuration_items_service_members',
            TABLE = 'configuration_items', COLUMN = 'class_id';
  END IF;
  IF EXISTS (SELECT 1 FROM ci_classes WHERE id = NEW.class_id AND system_role = 'business_service') THEN
    RAISE EXCEPTION 'configuration_items: a configuration item cannot become a business service; create the service instead'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'configuration_items_service_class',
            TABLE = 'configuration_items', COLUMN = 'class_id';
  END IF;
  RETURN NEW;
END;
$$;
