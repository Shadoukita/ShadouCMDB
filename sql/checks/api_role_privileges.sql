-- ShadouCMDB: what the API role may do outside the area schemas (GH#713).
--
-- One line per privilege the API role holds, directly or through PUBLIC, on
-- the database, its schemas, tables, views, sequences, columns and routines,
-- and per default privilege granted to it. Objects the API role owns (the
-- area schemas and their type tables and views) are left out: they differ
-- with the data model. Two installs of the same release must print the same
-- lines, however they were set up: a fresh three-role install, and a
-- single-role install split with sql/bootstrap/10_split_roles.sql. The
-- upgrade workflow and a backend test compare the two.
--
-- The API role is shadoucmdb.app_role if set, else shadoucmdb_app:
--   psql "postgres://admin@db.example.internal:5432/shadoucmdb" -XAt -f sql/checks/api_role_privileges.sql
WITH app AS (
  SELECT oid FROM pg_roles
  WHERE rolname = COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app')
),
schemas AS (
  SELECT n.oid, n.nspname, n.nspacl, n.nspowner FROM pg_namespace n
  WHERE n.nspowner <> (SELECT oid FROM app) AND n.nspname NOT LIKE 'pg\_%' AND n.nspname <> 'information_schema'
),
privileges AS (
  SELECT 'database' AS kind, '' AS name, a.privilege_type, a.grantee, a.is_grantable
  FROM pg_database d, aclexplode(COALESCE(d.datacl, acldefault('d', d.datdba))) a
  WHERE d.datname = current_database()
  UNION ALL
  SELECT 'schema', s.nspname, a.privilege_type, a.grantee, a.is_grantable
  FROM schemas s, aclexplode(COALESCE(s.nspacl, acldefault('n', s.nspowner))) a
  UNION ALL
  SELECT CASE c.relkind WHEN 'S' THEN 'sequence' WHEN 'v' THEN 'view' WHEN 'm' THEN 'view' ELSE 'table' END,
         format('%I.%I', s.nspname, c.relname), a.privilege_type, a.grantee, a.is_grantable
  FROM pg_class c JOIN schemas s ON s.oid = c.relnamespace,
       aclexplode(COALESCE(c.relacl, acldefault((CASE c.relkind WHEN 'S' THEN 's' ELSE 'r' END)::"char", c.relowner))) a
  WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S') AND c.relowner <> (SELECT oid FROM app)
  UNION ALL
  SELECT 'column', format('%I.%I.%I', s.nspname, c.relname, att.attname), a.privilege_type, a.grantee, a.is_grantable
  FROM pg_attribute att JOIN pg_class c ON c.oid = att.attrelid JOIN schemas s ON s.oid = c.relnamespace,
       aclexplode(att.attacl) a
  WHERE att.attacl IS NOT NULL AND c.relowner <> (SELECT oid FROM app)
  UNION ALL
  SELECT 'routine', p.oid::regprocedure::text, a.privilege_type, a.grantee, a.is_grantable
  FROM pg_proc p JOIN schemas s ON s.oid = p.pronamespace,
       aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) a
  WHERE p.proowner <> (SELECT oid FROM app)
    -- Extension functions (pg_trgm) come with the extension.
    AND NOT EXISTS (SELECT FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e')
  UNION ALL
  SELECT 'default ' || CASE d.defaclobjtype WHEN 'r' THEN 'tables' WHEN 'S' THEN 'sequences' WHEN 'f' THEN 'routines'
                                            WHEN 'T' THEN 'types' ELSE 'schemas' END,
         COALESCE(s.nspname, ''), a.privilege_type, a.grantee, a.is_grantable
  FROM pg_default_acl d LEFT JOIN pg_namespace s ON s.oid = d.defaclnamespace, aclexplode(d.defaclacl) a
)
SELECT kind || ' ' || name || ': ' || privilege_type
       || CASE WHEN grantee = 0 THEN ' (PUBLIC)' ELSE '' END
       || CASE WHEN is_grantable THEN ' WITH GRANT OPTION' ELSE '' END AS privilege
FROM privileges
WHERE grantee = 0 OR grantee = (SELECT oid FROM app)
ORDER BY 1;
