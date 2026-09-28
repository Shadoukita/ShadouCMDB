-- Upgrade check for migration 0016 (barebone CI core), part 1 of 2.
--
-- Run BEFORE upgrading to a release with migration 0016, on a database at
-- migration 0009 to 0015, as the role that runs the migrations
-- (MIGRATION_DATABASE_URL, or DATABASE_URL on a single-role install):
--   psql "$MIGRATION_DATABASE_URL" -f sql/checks/core_ci_upgrade_1_before.sql
--
-- Copies the value of every fixed CI column (name, status_id, environment_id,
-- owner_id, location_id, hostname, ip_address, serial_number, notes) of every
-- CI, deleted ones included, as text into public.core_ci_upgrade_before, by the
-- field key migration 0016 moves it to. The migration does not touch public, so
-- the table survives the upgrade. Part 2 compares it with the class fields and
-- drops it.

SET search_path = cmdb, public;

CREATE TABLE public.core_ci_upgrade_before AS
SELECT ci.id AS ci_id, v.field, v.value
FROM configuration_items ci
CROSS JOIN LATERAL (VALUES
  ('name', ci.name),
  ('status', ci.status_id::text),
  ('environment', ci.environment_id::text),
  ('owner', ci.owner_id::text),
  ('location', ci.location_id::text),
  ('hostname', ci.hostname),
  ('ip_address', ci.ip_address::text),
  ('serial_number', ci.serial_number),
  ('notes', ci.notes)
) AS v(field, value)
WHERE v.value IS NOT NULL;

SELECT field, count(*) AS values_recorded FROM public.core_ci_upgrade_before GROUP BY field ORDER BY field;
