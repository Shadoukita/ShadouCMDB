-- User groups: an optimistic-concurrency version (v0.3.0, SHAA-931, spec
-- SHAA-927 §4.9).
--
-- PATCH /admin/groups/{id} and PUT /admin/groups/{id}/members carry the
-- version the administrator loaded; a stale one fails with 409
-- VERSION_CONFLICT instead of silently overwriting another administrator's
-- change. Every change to the group or its members increments it (the API
-- does, in the same transaction).
--
-- 0034 created the table empty in this release, so the default fills nothing
-- that matters; ADD COLUMN with a constant default is metadata-only.

ALTER TABLE cmdb.user_groups ADD COLUMN version integer NOT NULL DEFAULT 1;
--> statement-breakpoint
ALTER TABLE cmdb.user_groups ADD CONSTRAINT user_groups_version_positive CHECK (version >= 1);
