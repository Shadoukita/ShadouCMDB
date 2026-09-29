### Changed: *Data model › Lookups* is read only

Since migration 0016 the status, environment, owner and location of a CI are values of the lookup
lists under **Administration › Data model › Dropdowns**. The older statuses, environments, locations
and owners tables under **Lookups** are kept for history only, so values added there never appeared
on a CI form ([data model tables]). The web UI now shows these tables read only (no add, edit, reorder, archive
or delete), and each tab links to the Dropdowns list that replaced it. Maintain CI statuses,
environments, owners and locations under **Dropdowns**. The API refuses writes to the old tables in
this release as well (see the breaking API change on legacy statuses, environments, locations and
owners).

[data model tables]: docs/data-model.md#tables
