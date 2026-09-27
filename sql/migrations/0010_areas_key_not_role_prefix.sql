-- Area keys may not start with "shadoucmdb_" (GH#62). An area is a schema, and
-- a schema named after a role (shadoucmdb_owner, shadoucmdb_maintenance) comes
-- first on that role's default search_path ("$user", public): an owner session
-- that does not pin its search_path would resolve unqualified names into a
-- schema the API role owns. The API also refuses keys equal to any existing
-- role; this CHECK is the last line of defence for the roles ShadouCMDB creates.

DO $$
DECLARE
  bad text;
BEGIN
  SELECT string_agg(key, ', ') INTO bad FROM cmdb.areas WHERE key ~ '^shadoucmdb_';
  IF bad IS NOT NULL THEN
    RAISE EXCEPTION 'areas % use the reserved prefix "shadoucmdb_"', bad
      USING HINT = 'Purge or rename these areas (and their schemas) before migrating.';
  END IF;
END;
$$;
--> statement-breakpoint
ALTER TABLE cmdb.areas DROP CONSTRAINT areas_key_not_reserved;
--> statement-breakpoint
ALTER TABLE cmdb.areas ADD CONSTRAINT areas_key_not_reserved CHECK (
  key NOT IN ('cmdb', 'public', 'information_schema', 'drizzle') AND key !~ '^(pg_|cmdb_|shadoucmdb_)');
