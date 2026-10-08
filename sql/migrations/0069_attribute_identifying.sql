-- Identifying fields, left out when a CI is cloned (SHAA-2460).
--
-- Cloning a CI copies its values into a new record. Some values belong to one
-- CI only (serial number, asset tag, MAC address, ...) and must not travel to
-- the copy. `is_identifying` marks such a field: the clone dialog leaves it
-- empty. It is a hint for clients; writes do not enforce uniqueness.
--
-- Every existing field starts as not identifying; administrators mark the
-- ones their data model needs. Only the metadata row changes; no type table
-- does.

ALTER TABLE cmdb.ci_attribute_definitions ADD COLUMN is_identifying boolean NOT NULL DEFAULT false;
--> statement-breakpoint
COMMENT ON COLUMN cmdb.ci_attribute_definitions.is_identifying IS
  'Identifies one CI: not copied when a CI is cloned. Uniqueness is not enforced on writes.';
