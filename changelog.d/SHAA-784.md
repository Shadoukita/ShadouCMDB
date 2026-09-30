### Removed: the configuration export no longer carries the deprecated statuses, environments, locations and owners

Since migration 0016, CIs take their status, environment, owner and location from the lookup lists `status`,
`environment`, `owner` and `location`. The former tables are kept, read-only, for history. Until now the
configuration export (`GET /api/v1/admin/config/export`) still copied those tables into `lookups.statuses`,
`environments`, `locations` and `owners`, and an import wrote them back into the deprecated tables. The lists that
CIs actually use were not changed (SHAA-784).

- **Export:** those four sections are no longer written. The values are in `lookups.lists`, as before.
- **Import of older files:** files that still carry the sections keep importing. A file from 0.1.0-rc.1, whose
  only copy of these values is in these sections, gets them as the lookup lists `status`, `environment`, `location`
  and `owner`, converted as migration 0016 converts the tables. Three things are not carried over: a location's
  tree and type, and a status's `isOperational`. If the file's own lists already hold the values (exports made
  after 0016), the section is skipped. Either way the dry run and the import list a warning for the section. The
  deprecated tables are never written.
- **`POST`, `PATCH` and `DELETE` on `/statuses`, `/environments`, `/locations` and `/owners`** still answer
  `410 GONE`, but no longer require `datamodel.manage`. Every signed-in caller now gets the `410` and its pointer
  to `/lookup-list-values`, where a caller without the permission used to get `403`. The read routes are
  unchanged.
- The descriptions of the starter template install and of `GET /api/v1/ui-settings` now name lookup lists
  instead of the former tables.

**Upgrade:** nothing to do. Your data stays where it is, and migration 0016 already copied the deprecated tables
into the lookup lists. Scripts that read `lookups.statuses`, `environments`, `locations` or `owners` from an export
must read `lookups.lists` instead.
