### Fixed: database role guidance for reporting and backups

Earlier setup guidance advised granting a reporting role `SELECT ON ALL TABLES IN SCHEMA public`
and making `shadoucmdb_app` own schema `public`. That no longer matches the product: system tables
live in schema `cmdb`, and read-only reporting uses the `cmdb_reporting` role on the area views.

**Action on upgrade:** none in the product. If your reporting role was set up from the earlier
guidance, replace it with `cmdb_reporting`. Take `pg_dump` backups as `shadoucmdb_owner`, not the
API role.
