### Changed (breaking API change): legacy statuses, environments, locations and owners are read-only

Since migration `0016_core_ci_model`, CIs take their status, environment, location and owner from the
lookup lists `status`, `environment`, `location` and `owner` (same ids), so a row written to the old
tables never showed up on a CI ([GH#111]). Their write endpoints now refuse the change instead of
accepting it silently:

- `POST /api/v1/{statuses,environments,locations,owners}` and `PATCH`/`DELETE` on `…/{id}` answer
  **`410 GONE`** (new error code `GONE`) and change nothing. The message names the replacement. The
  usual checks still come first: `401` without a session or token, `403` without `datamodel.manage`
  or the CSRF token.
- `GET` on the list, `…/{id}` and `…/{id}/usage` is unchanged, for history and reports. It stays
  deprecated and will be removed in a later release.
- Configuration import/export still carries these rows, so exports from earlier versions import as
  before.

**Upgrade:** integrations that create or change these rows must write lookup list values instead:
find the list with `GET /api/v1/lookup-lists?q=status` (the key can differ if `status` was already
taken before 0016), then `POST /api/v1/lookup-list-values` with its `listId`, or `PATCH`
`/api/v1/lookup-list-values/{id}` with the id the old row had. No schema change.

[GH#111]: https://github.com/Shadoukita/ShadouCMDB/issues/111
