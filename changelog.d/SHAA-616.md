### Added: Saved views (API and storage)

Operators can save the state of the inventory list (classes, filters, sort, columns, page size) and of
the search page (search term and filters) as a named **view**, personal or shared with every user, and
pick a personal default view per inventory list. This release adds the API; the Views menu in the web
UI follows.

- **New endpoints** under `/api/v1/saved-views`: list, get, create, change, delete, copy ("save as",
  "copy to my views", "share a copy") and `PUT /api/v1/saved-views/defaults` to set or clear a default.
  All need a signed-in browser session; API tokens get `403`.
- **A view never widens what a user sees.** It stores a query by key, not data or rights: running it is
  an ordinary list or search request, filtered by the user's class permissions as always. When a class,
  attribute or lookup value a view uses is archived or deleted, the view comes back `degraded` (only
  what narrows or presents results was dropped, e.g. a column) or `unavailable` (a filter would
  disappear, so the view is not applied at all). The stored view is never rewritten.
- **Personal views** are visible to their owner only, administrators included, and are deleted with
  the user account. **Shared views** are visible to every user who may view at least one of their
  classes; class keys a user may not view are left out of what they get.
- **New global right `views.share`** ("Create, edit and delete views shared with all users"). On
  upgrade it is granted to every permission profile that holds `customization.manage`, so nobody
  loses the ability to curate shared lists. The Administrator profile holds it implicitly.
- **Audit:** creating, changing and deleting a shared view is recorded (entity type `saved_views`).
  Personal views and default views are not recorded. Readers of the audit log whose profile limits the
  classes they may view do not get the keys of other classes in those entries.
- **Limits:** 200 personal views per user and 500 shared views per instance (both contexts
  together), a definition of at most 16 KiB, 100 classes, 100 lookup values and 50 columns per view,
  names of 1 to 100 characters, unique per owner and context (shared views: per context), ignoring case.
- **Configuration export format 6** adds the section `savedViews` with the **shared** views only;
  personal views and defaults are user data and never exported. Exporting and importing the section
  needs `views.share` in addition to `config.export_import`. Import merges views by context and name
  and never deletes; a dry run warns about classes, attributes and lookup values the target lacks.
  Files of format 1 to 5 still import. An older release refuses a format 6 file ("formatVersion: Too
  big").
- **Backups** include saved views and defaults; a factory reset removes them.

**Upgrade:** migration 0039 adds two tables, widens the permission constraint and grants
`views.share` as described above. It rewrites no data and takes no long lock. Nothing to do otherwise.
