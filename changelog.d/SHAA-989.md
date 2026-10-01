### Changed: Import row problems and error reports stay readable while import is off

While bulk import is turned off, the owner of an import and administrators can still list its
row problems (`GET /api/v1/imports/{id}/issues`) and download its error report
(`GET /api/v1/imports/{id}/error-report`), for example to keep the report before deleting the
import. Before, both answered `403 import_disabled`. Who may read a job is unchanged. Uploading,
mapping, file options, dry run and commit are still refused with `403 import_disabled`.
