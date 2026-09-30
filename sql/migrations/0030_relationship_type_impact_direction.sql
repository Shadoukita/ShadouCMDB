-- Impact analysis (v0.3.0, SHAA-885): which relationship types propagate impact, and which way.
--
-- relationship_types.impact_direction says how impact flows across an edge
-- `source -forward_label-> target`:
--   none              the type does not propagate impact (the default)
--   target_to_source  when the target fails, the source is affected (app runs_on server)
--   source_to_target  when the source fails, the target is affected (hosts, supplies_power_to)
--   both              impact flows both ways (clustered peers)
-- A non-directional type (connected_to) has no source or target side, so it
-- can only be none or both.
--
-- Existing types become none, except the starter template's runs_on,
-- depends_on and located_in while their labels are still the template's: an
-- administrator who repurposed one of those keys keeps it at none. New
-- installations get the same values from the template.
--
-- ADD COLUMN with a constant default is metadata-only; no index is needed:
-- the traversal uses the partial indexes on (source_ci_id, relationship_type_id)
-- and (target_ci_id, relationship_type_id) from 0001.

ALTER TABLE cmdb.relationship_types
  ADD COLUMN impact_direction text NOT NULL DEFAULT 'none',
  ADD CONSTRAINT relationship_types_impact_direction_valid
    CHECK (impact_direction IN ('none', 'target_to_source', 'source_to_target', 'both')),
  ADD CONSTRAINT relationship_types_impact_nondirectional
    CHECK (is_directional OR impact_direction IN ('none', 'both'));
--> statement-breakpoint

-- The updated_at trigger would stamp every seeded row; this is not an edit.
ALTER TABLE cmdb.relationship_types DISABLE TRIGGER relationship_types_set_updated_at;
--> statement-breakpoint
UPDATE cmdb.relationship_types t
SET impact_direction = 'target_to_source'
FROM (VALUES ('runs_on', 'runs on', 'hosts'),
             ('depends_on', 'depends on', 'is required by'),
             ('located_in', 'is located in', 'contains')) AS template (key, forward_label, reverse_label)
WHERE t.key = template.key
  AND t.forward_label = template.forward_label
  AND t.reverse_label = template.reverse_label
  AND t.is_directional;
--> statement-breakpoint
ALTER TABLE cmdb.relationship_types ENABLE TRIGGER relationship_types_set_updated_at;
