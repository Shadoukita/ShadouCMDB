-- E-mail addresses are unique ignoring case and Unicode form (GH#531, SHAA-1523).
--
-- 0044 made account e-mails (users_email_uq) and Person e-mails (uq_<field id
-- hex>) unique on lower(email). Look-alike forms of one address got past it:
-- composed and decomposed characters (NFC/NFD) and compatibility forms such as
-- full-width letters ("ａｌｉｃｅ@example.test"). Both indexes now compare
-- cmdb.email_key(email): NFKC, then lower case. The API stores new addresses
-- in NFKC (accounts, identity provider claims and the Person's Email); stored
-- addresses keep their form, the key covers them.
--
-- cmdb.email_key is IMMUTABLE, as an index expression must be, and calls only
-- pg_catalog functions. Lower case follows Unicode with PostgreSQL's built-in
-- C.UTF-8 collation (pg_c_utf8, PostgreSQL 17 and later) whatever the
-- database's locale; before 17 it follows the database's LC_CTYPE, as lower()
-- did in 0044 (a "C" database lowers ASCII letters only). normalize() needs a
-- UTF8 database; on any other server encoding the key stays lower(email).
-- Both depend on the Unicode version of the PostgreSQL release: after a major
-- version upgrade, REINDEX these two indexes as for any expression index on
-- text (the PostgreSQL release notes say when Unicode changed).
--
-- Addresses that are equal only under the new key stop the upgrade with the
-- list (accounts by username, Persons by CI ident); nothing changes until an
-- administrator gives each its own address.
--
-- The Person's unique index lives in the Person type's table, which belongs
-- to the API role: that part runs as the API role (sql/README.md). When the
-- table is not built yet, the reconcile `shadoucmdb migrate` runs next builds
-- the index with the new expression.
--
-- Rollback: not supported in place, as for every release. Downgrading means
-- restoring the backup taken before the upgrade.

DO $$
BEGIN
  IF current_setting('server_encoding') = 'UTF8'
     AND EXISTS (SELECT FROM pg_collation WHERE collname = 'pg_c_utf8' AND collnamespace = 'pg_catalog'::regnamespace) THEN
    CREATE FUNCTION cmdb.email_key(email text) RETURNS text
      LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
      AS $f$ SELECT pg_catalog.lower(pg_catalog.normalize($1, 'NFKC') COLLATE pg_catalog.pg_c_utf8) $f$;
  ELSIF current_setting('server_encoding') = 'UTF8' THEN
    CREATE FUNCTION cmdb.email_key(email text) RETURNS text
      LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
      AS $f$ SELECT pg_catalog.lower(pg_catalog.normalize($1, 'NFKC')) $f$;
  ELSE
    CREATE FUNCTION cmdb.email_key(email text) RETURNS text
      LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
      AS $f$ SELECT pg_catalog.lower($1) $f$;
  END IF;
END;
$$;
--> statement-breakpoint

COMMENT ON FUNCTION cmdb.email_key(text) IS
  'The comparison key of an e-mail address: NFKC, then lower case (lower case only on a non-UTF8 database). The expression of users_email_uq and of the Person Email unique index (migration 0045).';
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Accounts
-- ---------------------------------------------------------------------------
DO $$
DECLARE
  dups text;
BEGIN
  SELECT string_agg(format('%s (users: %s)', e.emails, e.names), '; ' ORDER BY e.emails) INTO dups
  FROM (
    SELECT string_agg(email, ', ' ORDER BY email) AS emails, string_agg(username, ', ' ORDER BY lower(username)) AS names
    FROM cmdb.users WHERE email IS NOT NULL
    GROUP BY cmdb.email_key(email) HAVING count(*) > 1
  ) e;
  IF dups IS NOT NULL THEN
    RAISE EXCEPTION 'users: e-mail addresses must be unique ignoring case and Unicode form from this release on, and these accounts have addresses that differ only in Unicode form: %. Give each account its own address (Administration > Users) with the previous release, then run `shadoucmdb migrate` again. Nothing was changed.', dups
      USING ERRCODE = 'unique_violation', CONSTRAINT = 'users_email_uq';
  END IF;
END;
$$;
--> statement-breakpoint

DROP INDEX cmdb.users_email_uq;
--> statement-breakpoint
CREATE UNIQUE INDEX users_email_uq ON cmdb.users (cmdb.email_key(email));
--> statement-breakpoint

-- ---------------------------------------------------------------------------
-- Persons (as the API role, which owns the type table)
-- ---------------------------------------------------------------------------
DO $$
DECLARE
  app_role name := COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app');
BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = app_role) AND current_user <> app_role THEN
    PERFORM set_config('role', app_role, true);  -- SET LOCAL ROLE
  END IF;
END;
$$;
--> statement-breakpoint

DO $$
DECLARE
  f record;
  idx text;
  dups text;
BEGIN
  SELECT d.id, d.key AS col, a.key AS area, c.key AS class INTO f
  FROM cmdb.ci_attribute_definitions d
  JOIN cmdb.ci_classes c ON c.id = d.class_id
  JOIN cmdb.areas a ON a.id = c.area_id
  WHERE d.system_role = 'person_email';
  IF NOT FOUND THEN
    RETURN;
  END IF;
  idx := 'uq_' || replace(f.id::text, '-', '');
  IF to_regclass(format('%I.%I', f.area, idx)) IS NULL THEN
    RETURN;  -- not built yet: the reconcile builds it with the new expression
  END IF;
  EXECUTE format(
    'SELECT string_agg(format(%1$L, e.emails, e.idents), %2$L ORDER BY e.emails)
     FROM (
       SELECT string_agg(p.%3$I, %4$L ORDER BY p.%3$I) AS emails, string_agg(ci.ident, %4$L ORDER BY ci.ident) AS idents
       FROM %5$I.%6$I p JOIN cmdb.configuration_items ci ON ci.id = p.id
       WHERE p.%3$I IS NOT NULL
       GROUP BY cmdb.email_key(p.%3$I) HAVING count(*) > 1
     ) e',
    '%s (people: %s)', '; ', f.col, ', ', f.area, f.class)
  INTO dups;
  IF dups IS NOT NULL THEN
    RAISE EXCEPTION 'people: e-mail addresses must be unique ignoring case and Unicode form from this release on, and these people have addresses that differ only in Unicode form: %. Give each person its own address with the previous release, then run `shadoucmdb migrate` again. Nothing was changed.', dups
      USING ERRCODE = 'unique_violation', CONSTRAINT = idx;
  END IF;
  EXECUTE format('DROP INDEX %I.%I', f.area, idx);
  EXECUTE format('CREATE UNIQUE INDEX %I ON %I.%I (cmdb.email_key(%I))', idx, f.area, f.class, f.col);
END;
$$;
--> statement-breakpoint

RESET ROLE;
