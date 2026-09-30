### Removed: the read-only Lookups tabs for the former statuses, environments, locations and owners

Since migration 0016, CIs take their status, environment, location and owner from the lookup lists `status`,
`environment`, `location` and `owner`, edited under *Administration › Data model › Dropdowns*. The *Lookups*
section still showed the former tables as read-only tabs. They held frozen copies of values that are in the lookup
lists, so the section is gone ([SHAA-809]).

- **Bookmarks:** `/admin/lookups` and the old tab addresses (`/admin/lookups/statuses`, `environments`,
  `locations`, `owners`) now open Dropdowns. `/admin/lookups/lists?list=…` still opens that list.
- **Export / import:** the page describes the export as carrying the lookup lists. When an import of an older file
  (0.1.0-rc.1) converts or skips the former sections, the dry run and the applied import both list the warning for
  each section.
- The audit log keeps its labels for rows of the former tables, and the read API (`GET /api/v1/statuses`,
  `environments`, `locations`, `owners`) is unchanged.

**Upgrade:** nothing to do.

[SHAA-809]: docs/data-model.md
