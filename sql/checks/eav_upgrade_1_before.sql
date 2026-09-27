-- Upgrade check for migration 0009 (per-type tables), part 1 of 2.
--
-- Run BEFORE upgrading to a release with migration 0009, on a database still at
-- migration 0005, 0006 or 0007, as the role that runs the migrations
-- (MIGRATION_DATABASE_URL, or DATABASE_URL on a single-role install):
--   psql "$MIGRATION_DATABASE_URL" -f sql/checks/eav_upgrade_1_before.sql
--
-- Copies every stored attribute value, as the text of the column type it will
-- have in its type table, into public.eav_upgrade_before. Migration 0008 moves
-- only the system tables (a fixed list) out of public, so this table is left
-- alone by the upgrade.
-- Part 2 compares it with the type tables and drops it.

SET search_path = cmdb, public;

CREATE TABLE public.eav_upgrade_before AS
SELECT v.ci_id,
       v.attribute_id,
       CASE d.data_type
         WHEN 'text' THEN v.value_text
         WHEN 'enum' THEN v.value_text
         WHEN 'number' THEN v.value_number::numeric::text
         WHEN 'integer' THEN v.value_number::bigint::text
         WHEN 'boolean' THEN v.value_boolean::text
         WHEN 'date' THEN v.value_date::text
         WHEN 'datetime' THEN v.value_datetime::text
         WHEN 'ip' THEN v.value_ip::text
         WHEN 'cidr' THEN v.value_cidr::text
         WHEN 'reference' THEN v.value_ref_ci_id::text
         WHEN 'lookup' THEN v.value_lookup_id::text
       END AS value
FROM ci_attribute_values v
JOIN ci_attribute_definitions d ON d.id = v.attribute_id;

SELECT count(*) AS values_recorded FROM public.eav_upgrade_before;
