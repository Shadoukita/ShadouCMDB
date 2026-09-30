### Added: Bulk import switch, the `cis.import` right and the import tables

The groundwork for importing CIs from CSV and Excel files (the upload and the wizard follow in
later changes of this release):

- **Switch, off by default.** `GET /api/v1/imports/settings` reports whether bulk import is on and
  its limits; `PUT /api/v1/imports/settings` turns it on or off (Administrator, session only,
  audited as an `update` of `import_settings`).
- **New global right `cis.import`** ("Import configuration items from CSV and Excel files"). It is
  granted to **no** profile on upgrade; the Administrator profile holds it implicitly. Importing
  also needs the class rights for every row.
- **New server settings:** `IMPORT_ALLOWED` (set `false` to keep import off whatever an
  administrator sets), `IMPORT_MAX_FILE_MB`, `IMPORT_MAX_ROWS`, `IMPORT_MAX_STORED_MB`,
  `IMPORT_UPLOAD_TIMEOUT_SECS` and `IMPORT_WORKERS`. See `.env.example`.
- **Audit:** two new events, `import.commit` (kept with the change history, `prune-audit --scope
  changes`) and `import.report_read` (kept with the authentication events, scope `auth`). Import
  job and saved-mapping entries of a class the reader may not view are left out of the audit log
  and its totals.
- **Backups** leave out uploaded import files, their per-row problems and idempotency keys
  (`import_job_files`, `import_job_issues`, `import_idempotency_keys`). Job records, saved mappings
  and the switch are backed up, and `restore` marks unfinished jobs as expired.

**Upgrade:** migration 0029 adds the tables and re-checks the `audit_log` constraints once. On a very
large audit log, `shadoucmdb migrate` takes longer than usual for this step; audit writes wait
until it finishes. Nothing to do otherwise: import stays off until you turn it on and grant
`cis.import`.
