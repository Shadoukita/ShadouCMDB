-- Upgrade check for migration 0016 (barebone CI core), part 2 of 2.
--
-- Run AFTER `shadoucmdb migrate`, on the database part 1 ran on:
--   psql "$MIGRATION_DATABASE_URL" -f sql/checks/core_ci_upgrade_2_after.sql
--
-- Reads every recorded value back from the class fields of the CI's lineage
-- (the field keeps its key, or gets a suffix such as hostname_2 when the key was
-- taken) and checks that each CI's label is its former name. Expect missing = 0
-- and wrong_labels = 0; the rows behind any difference are listed. Drops the
-- scratch table at the end.

SET search_path = cmdb, public;

CREATE TEMP TABLE core_ci_upgrade_after (ci_id uuid, field text, value text);

DO $$
DECLARE
  f record;
BEGIN
  FOR f IN
    SELECT d.id, a.key AS area, c.key AS type, d.key AS column_name,
           substring(d.key FROM '^(name|status|environment|owner|location|hostname|ip_address|serial_number|notes)(?:_[0-9]+)?$') AS field
    FROM cmdb.ci_attribute_definitions d
    JOIN cmdb.ci_classes c ON c.id = d.class_id
    JOIN cmdb.areas a ON a.id = c.area_id
    WHERE d.key ~ '^(name|status|environment|owner|location|hostname|ip_address|serial_number|notes)(_[0-9]+)?$'
  LOOP
    EXECUTE format('INSERT INTO core_ci_upgrade_after SELECT id, %L, %I::text FROM %I.%I WHERE %I IS NOT NULL',
                   f.field, f.column_name, f.area, f.type, f.column_name);
  END LOOP;
END;
$$;

SELECT (SELECT count(*) FROM public.core_ci_upgrade_before) AS values_before,
       (SELECT count(*) FROM (SELECT * FROM public.core_ci_upgrade_before EXCEPT SELECT * FROM core_ci_upgrade_after) m) AS missing,
       (SELECT count(*) FROM public.core_ci_upgrade_before b JOIN configuration_items ci ON ci.id = b.ci_id
         WHERE b.field = 'name' AND ci.label IS DISTINCT FROM b.value) AS wrong_labels;

SELECT 'missing' AS problem, m.ci_id, m.field, m.value
FROM (SELECT * FROM public.core_ci_upgrade_before EXCEPT SELECT * FROM core_ci_upgrade_after) m
UNION ALL
SELECT 'wrong label', b.ci_id, 'label', ci.label
FROM public.core_ci_upgrade_before b JOIN configuration_items ci ON ci.id = b.ci_id
WHERE b.field = 'name' AND ci.label IS DISTINCT FROM b.value
LIMIT 50;

DROP TABLE public.core_ci_upgrade_before;
