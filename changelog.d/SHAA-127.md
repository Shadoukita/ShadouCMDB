### Fixed: database roles and the database may have any name

The migrations granted to the fixed names `shadoucmdb_app` and `shadoucmdb_maintenance`, so an
install with its own role names stopped at migration 0008
([#42](https://github.com/Shadoukita/ShadouCMDB/issues/42)). `migrate` now grants to the users of
`DATABASE_URL` (API) and `MAINTENANCE_DATABASE_URL` (maintenance), prints both, and stops if one
does not exist. **Set both when you run `migrate`**, `restore`, `factory-reset` or
`decommission`. The bootstrap scripts take `-v owner_role=… -v app_role=… -v maintenance_role=…
-v db_name=…`; the defaults are unchanged. Nothing changes for installs with the default names. A
database migrated by a pre-release build since v0.1.0-rc.1 gets its migration records updated by
the next `migrate`, and its backups still restore.
