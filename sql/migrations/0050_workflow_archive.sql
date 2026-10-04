-- Workflow history outlives its CI (v0.4.0 slice S6, SHAA-1427; design on
-- SHAA-1411 §3.2, §8.2, Q2).
--
-- Instances and their events are kept for the life of the CI (ON DELETE
-- RESTRICT, append-only events). A CI row is only ever deleted for good by a
-- type purge; until now a purged CI with workflow history made the purge fail
-- on the RESTRICT. From here on, deleting a CI row moves its instances, each
-- with all of its events, into cmdb.workflow_instance_archive in the same
-- transaction, and only then removes them. The archive is append-only like
-- the events and is not touched by audit retention (prune_audit_log() never
-- reads it), so the business record survives both the CI and the audit
-- window.
--
-- Additive only: a new table, a trigger on configuration_items and a guarded
-- DELETE path on workflow_instance_events. No existing row is rewritten.

CREATE TABLE cmdb.workflow_instance_archive (
  -- The archived instance's id (no foreign key: the instance row is gone).
  instance_id        uuid PRIMARY KEY,
  -- The deleted CI, as it was named when it went.
  ci_id              uuid NOT NULL,
  ci_ident           text NOT NULL,
  ci_label           text NOT NULL,
  class_key          text NOT NULL,
  definition_id      uuid NOT NULL,
  definition_key     text NOT NULL,
  version_no         integer NOT NULL,
  state_key          text NOT NULL,
  -- The instance's status when its CI was deleted.
  status             text NOT NULL CHECK (status IN ('active', 'completed', 'cancelled')),
  started_at         timestamptz NOT NULL,
  started_by_name    text NOT NULL,
  last_transition_at timestamptz NOT NULL,
  ended_at           timestamptz,
  -- Every event of the instance, oldest first, in the shape of
  -- GET /workflow-instances/{id}/events.
  events             jsonb NOT NULL CHECK (jsonb_typeof(events) = 'array'),
  archived_at        timestamptz NOT NULL DEFAULT now(),
  -- The request that deleted the CI (shadoucmdb.request_id), joining the
  -- CI's `delete` audit row.
  request_id         text
);
--> statement-breakpoint
CREATE INDEX workflow_instance_archive_ci_idx ON cmdb.workflow_instance_archive (ci_id, archived_at);
--> statement-breakpoint
CREATE INDEX workflow_instance_archive_recent_idx ON cmdb.workflow_instance_archive (archived_at, instance_id);
--> statement-breakpoint
CREATE INDEX workflow_instance_archive_definition_idx
  ON cmdb.workflow_instance_archive (lower(definition_key), archived_at);
--> statement-breakpoint

-- Append-only, for every role.
CREATE FUNCTION cmdb.workflow_instance_archive_append_only() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN
  RAISE EXCEPTION 'workflow_instance_archive is append-only (% rejected)', TG_OP
    USING ERRCODE = 'insufficient_privilege';
END;
$$;
--> statement-breakpoint
CREATE TRIGGER workflow_instance_archive_append_only BEFORE UPDATE OR DELETE ON cmdb.workflow_instance_archive
  FOR EACH ROW EXECUTE FUNCTION cmdb.workflow_instance_archive_append_only();
--> statement-breakpoint
CREATE TRIGGER workflow_instance_archive_no_truncate BEFORE TRUNCATE ON cmdb.workflow_instance_archive
  FOR EACH STATEMENT EXECUTE FUNCTION cmdb.workflow_instance_archive_append_only();
--> statement-breakpoint

-- Events stay append-only. A DELETE passes only while the archive trigger
-- below runs: it sets shadoucmdb.workflow_archive for its own statement and
-- runs as the table owner, after it copied the events. Any other role that
-- sets the variable is still rejected (the 0007 pattern of audit_log).
-- UPDATE and TRUNCATE are always rejected.
CREATE OR REPLACE FUNCTION cmdb.workflow_instance_events_append_only() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN
  IF TG_OP = 'DELETE' AND current_setting('shadoucmdb.workflow_archive', true) = 'on'
     AND current_user = (SELECT r.rolname FROM pg_class c JOIN pg_roles r ON r.oid = c.relowner WHERE c.oid = TG_RELID)
  THEN
    RETURN OLD;
  END IF;
  RAISE EXCEPTION 'workflow_instance_events is append-only (% rejected)', TG_OP
    USING ERRCODE = 'insufficient_privilege';
END;
$$;
--> statement-breakpoint

-- Before a CI row goes: archive its instances with their events, then delete
-- them. SECURITY DEFINER, because the API role may not delete events; it
-- runs only from the trigger (EXECUTE is revoked from PUBLIC).
CREATE FUNCTION cmdb.configuration_items_archive_workflows() RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
  ids uuid[];
BEGIN
  SELECT array_agg(wi.id) INTO ids FROM cmdb.workflow_instances wi WHERE wi.ci_id = OLD.id;
  IF ids IS NULL THEN
    RETURN OLD;
  END IF;
  INSERT INTO cmdb.workflow_instance_archive
    (instance_id, ci_id, ci_ident, ci_label, class_key, definition_id, definition_key, version_no, state_key,
     status, started_at, started_by_name, last_transition_at, ended_at, events, request_id)
  SELECT wi.id, OLD.id, OLD.ident, OLD.label, c.key, d.id, d.key, v.version_no, s.key,
         wi.status, wi.started_at, wi.started_by_name, wi.last_transition_at, wi.ended_at,
         coalesce((SELECT jsonb_agg(jsonb_build_object(
                     'id', e.id, 'kind', e.kind, 'transitionKey', e.transition_key,
                     'fromStateKey', e.from_state_key, 'toStateKey', e.to_state_key,
                     'fromVersionNo', e.from_version_no, 'toVersionNo', e.to_version_no,
                     'occurredAt', e.occurred_at, 'actorType', e.actor_type, 'actorId', e.actor_id,
                     'actorName', e.actor_name, 'comment', e.comment, 'fieldChanges', e.field_changes,
                     'requestId', e.request_id) ORDER BY e.id)
                   FROM cmdb.workflow_instance_events e WHERE e.instance_id = wi.id), '[]'::jsonb),
         NULLIF(current_setting('shadoucmdb.request_id', true), '')
  FROM cmdb.workflow_instances wi
  JOIN cmdb.workflow_definitions d ON d.id = wi.definition_id
  JOIN cmdb.workflow_versions v ON v.id = wi.version_id
  JOIN cmdb.workflow_states s ON s.id = wi.current_state_id
  JOIN cmdb.ci_classes c ON c.id = OLD.class_id
  WHERE wi.id = ANY (ids);
  PERFORM set_config('shadoucmdb.workflow_archive', 'on', true);
  DELETE FROM cmdb.workflow_instance_events WHERE instance_id = ANY (ids);
  PERFORM set_config('shadoucmdb.workflow_archive', '', true);
  DELETE FROM cmdb.workflow_instances WHERE id = ANY (ids);
  RETURN OLD;
END;
$$;
--> statement-breakpoint
REVOKE ALL ON FUNCTION cmdb.configuration_items_archive_workflows() FROM PUBLIC;
--> statement-breakpoint
REVOKE ALL ON FUNCTION cmdb.workflow_instance_archive_append_only() FROM PUBLIC;
--> statement-breakpoint
CREATE TRIGGER configuration_items_archive_workflows BEFORE DELETE ON cmdb.configuration_items
  FOR EACH ROW EXECUTE FUNCTION cmdb.configuration_items_archive_workflows();
--> statement-breakpoint

-- Three-role install: 0008's default privileges give the API role DML on the
-- new table; it keeps SELECT only (rows arrive through the trigger above).
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = app_role) AND current_user <> app_role THEN
    EXECUTE format('REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON cmdb.workflow_instance_archive FROM %I', app_role);
  END IF;
END;
$$;
