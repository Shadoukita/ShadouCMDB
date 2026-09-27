-- Upgrade check for migration 0009 (per-type tables), part 2 of 2.
--
-- Run AFTER `shadoucmdb migrate`, on the database part 1 ran on:
--   psql "$MIGRATION_DATABASE_URL" -f sql/checks/eav_upgrade_2_after.sql
--
-- Reads every field value back from the type tables and compares it with what
-- part 1 recorded. Expect missing = 0 and unexpected = 0; the rows behind any
-- difference are listed. Drops the scratch tables at the end.

SET search_path = cmdb, public;

CREATE TEMP TABLE eav_upgrade_after (ci_id uuid, attribute_id uuid, value text);

DO $$
DECLARE
  f record;
BEGIN
  FOR f IN
    SELECT d.id, a.key AS area, c.key AS type, d.key AS field
    FROM cmdb.ci_attribute_definitions d
    JOIN cmdb.ci_classes c ON c.id = d.class_id
    JOIN cmdb.areas a ON a.id = c.area_id
  LOOP
    EXECUTE format('INSERT INTO eav_upgrade_after SELECT id, %L::uuid, %I::text FROM %I.%I WHERE %I IS NOT NULL',
                   f.id, f.field, f.area, f.type, f.field);
  END LOOP;
END;
$$;

SELECT (SELECT count(*) FROM public.eav_upgrade_before) AS values_before,
       (SELECT count(*) FROM eav_upgrade_after) AS values_after,
       (SELECT count(*) FROM (SELECT * FROM public.eav_upgrade_before EXCEPT SELECT * FROM eav_upgrade_after) m) AS missing,
       (SELECT count(*) FROM (SELECT * FROM eav_upgrade_after EXCEPT SELECT * FROM public.eav_upgrade_before) u) AS unexpected;

SELECT 'missing' AS problem, * FROM (SELECT * FROM public.eav_upgrade_before EXCEPT SELECT * FROM eav_upgrade_after) m
UNION ALL
SELECT 'unexpected', * FROM (SELECT * FROM eav_upgrade_after EXCEPT SELECT * FROM public.eav_upgrade_before) u
LIMIT 50;

DROP TABLE public.eav_upgrade_before;
