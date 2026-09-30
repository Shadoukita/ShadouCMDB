### Added: Bulk import uploads CSV and Excel files and reads them safely

Users with `cis.import` can upload a CSV or `.xlsx` file while bulk import is switched on
(`POST /api/v1/imports`); a worker in the server analyses it (sheets, encoding, delimiter, columns
with sample values, row count), and `GET /api/v1/imports/{id}` shows the result. Mapping, dry run
and commit follow in a later change of this release.

- The file is sent as the request body (`Content-Type: text/csv` or the XLSX type), with its name
  percent-encoded in the `X-File-Name` header. It is stored in the database in 1 MiB chunks and
  never written to the server's disk; an upload holds no database connection while waiting for the
  client. Files are deleted 24 hours after the job's last activity.
- Workbooks are checked before any spreadsheet library reads them: macro-enabled, password-protected
  and binary workbooks, `.xls` files, zip bombs, DOCTYPEs and malformed ZIP structures are refused
  with a reason the UI shows. Formulas are never evaluated; their stored results are read.
- Limits per user: one import running, 20 unfinished imports, 30 uploads an hour. The server-wide
  limits are the `IMPORT_*` settings.
- `GET /api/v1/imports/template?classKey=` downloads an empty CSV with a class's column names.
- New error code `IDEMPOTENCY_KEY_REUSED` (422). The descriptions of `415` and `429` in the API
  document are now general (they also cover the import upload).
- Deleting a user deletes their unfinished imports and uploaded files.

**Upgrade:** nothing to do. The import workers start with the server (`IMPORT_WORKERS`, default
1); add them to your `DATABASE_POOL_MAX` sizing.
