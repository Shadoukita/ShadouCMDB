### Added: Bulk import maps columns, checks every row in a dry run, and commits in chunks

An uploaded import can now be finished through the API:

- **Mapping.** `PUT /api/v1/imports/{id}/mapping` maps each column to an attribute, the ident,
  the validity dates or a relationship type, chooses the mode (`create_only`, `update_only` or
  `create_or_update`, with a key column that finds existing CIs) and is checked against the file and the data
  model, with every problem reported at once. `GET /api/v1/imports/{id}/mapping-suggestion`
  proposes a mapping from the column names, a saved mapping, attribute keys and labels.
- **Saved mappings.** `/api/v1/import-mappings` stores mappings per class, shared with the holders
  of `cis.import` who can view the class. Names are unique per class, ignoring case. Only the
  creator or an administrator changes or deletes one. At most 500 per instance.
- **Dry run.** `POST /api/v1/imports/{id}/dry-run` checks every row through the same validation
  as the CI API, without writing, and reports the counts (create, update, unchanged, error), a
  preview and the problems (`GET /api/v1/imports/{id}/issues`, paged and filtered).
- **Commit.** `POST /api/v1/imports/{id}/commit` writes the rows in transactions of 500. A row that
  fails only at the database is counted as failed and the rest of its chunk is written; with
  `skipErrorRows` the rows with dry-run errors are left out. A commit whose dry run is older than
  24 hours, or whose data model changed since, is refused (`dry_run_stale`). A server restart
  resumes a commit where it stopped, without writing a row twice. Relationships are only added,
  never removed.
- **Error report.** `GET /api/v1/imports/{id}/error-report` downloads a CSV with one line per
  problem and the row's original values. Cells that a spreadsheet would run as a formula are
  neutralised. A download by an administrator other than the job's owner is audited as
  `import.report_read`.
- **Audit.** Each written CI and relationship is audited as the job's owner with actor type
  `import` and request id `import:<jobId>`; unchanged rows write nothing. Each commit ends with one
  `import.commit` event, also when it fails or is cancelled.

### Changed: A deadlock or serialisation failure returns `503 SERVER_BUSY`

A request that loses a database deadlock or serialisation race used to fail with a generic `500`.
It now returns `503` with the code `SERVER_BUSY` and `Retry-After: 1`, on every route. Clients can
retry the request unchanged.

**Upgrade:** nothing to do.
