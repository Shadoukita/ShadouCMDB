-- Expected fields for the completeness metric (SHAA-2350).
--
-- A record is complete when every field that matters for it holds a value.
-- Required fields are enforced (NOT NULL in the type table once every CI has a
-- value), so they rarely tell anything; most fields that operations relies on
-- (owner, location, serial number, ...) are optional on purpose, because a CI
-- can be registered before they are known. `is_expected` marks such a field:
-- writes never require it, but a live CI without a value counts as incomplete
-- in GET /api/v1/configuration-items/completeness.
--
-- Every existing field starts as not expected, so completeness is measured on
-- the required fields until an administrator marks expected ones. Only the
-- metadata row changes; no type table does.

ALTER TABLE cmdb.ci_attribute_definitions ADD COLUMN is_expected boolean NOT NULL DEFAULT false;
--> statement-breakpoint
COMMENT ON COLUMN cmdb.ci_attribute_definitions.is_expected IS
  'Counts towards completeness: a live CI without a value is incomplete. Not enforced on writes.';
