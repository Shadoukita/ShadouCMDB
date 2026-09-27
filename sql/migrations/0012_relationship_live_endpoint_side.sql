-- Linking a deleted CI names the side that is deleted (SHAA-125, GH#44).
--
-- ci_relationships_validate() raised ci_relationships_live_endpoints without
-- saying which endpoint was deleted, so the API always blamed sourceCiId. The
-- constraint name (the machine-readable error code) stays the same; the error
-- now carries the offending column (source_ci_id or target_ci_id), which the
-- API turns into the field name. When both are deleted the source is reported.
--
-- Everything else in the function is unchanged from 0002 (moved to cmdb and
-- search_path pinned in 0008). CREATE OR REPLACE drops SET clauses, so the
-- search_path is set again here.

CREATE OR REPLACE FUNCTION cmdb.ci_relationships_validate() RETURNS trigger
LANGUAGE plpgsql
SET search_path = cmdb, public
AS $$
DECLARE
  rt record;
  src record;
  tgt record;
BEGIN
  -- Only live edges are validated; soft-deleting an edge is always allowed.
  IF NEW.deleted_at IS NOT NULL THEN
    RETURN NEW;
  END IF;

  SELECT key, is_directional, is_active INTO rt FROM relationship_types WHERE id = NEW.relationship_type_id;
  SELECT class_id, deleted_at INTO src FROM configuration_items WHERE id = NEW.source_ci_id;
  SELECT class_id, deleted_at INTO tgt FROM configuration_items WHERE id = NEW.target_ci_id;

  IF TG_OP = 'INSERT' AND NOT rt.is_active THEN
    RAISE EXCEPTION 'ci_relationships: relationship type % is inactive', rt.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_relationships_type_active';
  END IF;

  IF src.deleted_at IS NOT NULL THEN
    RAISE EXCEPTION 'ci_relationships: cannot link a deleted CI (the source CI is deleted)'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_relationships_live_endpoints',
            TABLE = 'ci_relationships', COLUMN = 'source_ci_id';
  END IF;
  IF tgt.deleted_at IS NOT NULL THEN
    RAISE EXCEPTION 'ci_relationships: cannot link a deleted CI (the target CI is deleted)'
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_relationships_live_endpoints',
            TABLE = 'ci_relationships', COLUMN = 'target_ci_id';
  END IF;

  IF NOT EXISTS (
    SELECT 1 FROM relationship_type_rules r
    WHERE r.relationship_type_id = NEW.relationship_type_id
      AND ci_class_is_a(src.class_id, r.source_class_id)
      AND ci_class_is_a(tgt.class_id, r.target_class_id)
  ) AND NOT (NOT rt.is_directional AND EXISTS (
    SELECT 1 FROM relationship_type_rules r
    WHERE r.relationship_type_id = NEW.relationship_type_id
      AND ci_class_is_a(tgt.class_id, r.source_class_id)
      AND ci_class_is_a(src.class_id, r.target_class_id)
  )) THEN
    RAISE EXCEPTION 'ci_relationships: % is not allowed between these CI classes', rt.key
      USING ERRCODE = 'check_violation', CONSTRAINT = 'ci_relationships_endpoint_rule';
  END IF;

  IF NOT rt.is_directional THEN
    -- Serialise concurrent inserts of the same unordered pair, then check the reverse edge.
    PERFORM pg_advisory_xact_lock(hashtextextended(
      NEW.relationship_type_id::text || least(NEW.source_ci_id, NEW.target_ci_id)::text
        || greatest(NEW.source_ci_id, NEW.target_ci_id)::text, 0));
    IF EXISTS (
      SELECT 1 FROM ci_relationships e
      WHERE e.relationship_type_id = NEW.relationship_type_id
        AND e.source_ci_id = NEW.target_ci_id
        AND e.target_ci_id = NEW.source_ci_id
        AND e.deleted_at IS NULL
        AND e.id <> NEW.id
    ) THEN
      RAISE EXCEPTION 'ci_relationships: % already exists between these CIs', rt.key
        USING ERRCODE = 'unique_violation', CONSTRAINT = 'ci_relationships_live_edge_uq';
    END IF;
  END IF;

  RETURN NEW;
END;
$$;
