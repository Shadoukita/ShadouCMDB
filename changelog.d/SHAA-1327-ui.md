### Added: Saved views in the web UI

The inventory (`/cis`) and the search page (`/search`) have a **View** menu at the start of their
toolbar. It lists **My views** and **Shared views**, and offers Save view, Save as new view, Rename,
Set as my default (inventory only), Copy to my views, Share a copy (with `views.share`), Delete, Copy
link and Manage views.

- **The URL stays the source of truth.** Opening a view writes all its filters, the sort, the columns
  and the page size into the address next to `view=<id>`, so reload, bookmarks and shared links show
  the same list. A link with only `view=<id>` follows later edits of the view; a colleague who cannot
  open a personal view still gets the same list from the full link.
- **Defaults:** opening a class list, or the whole inventory, applies your default view for it. The
  list is requested once, with the view's state. Search views cannot be a default.
- A **Modified** marker with **Save** and **Revert** appears when the list differs from the view.
- A view whose classes, attributes or lookup values changed says what was left out (readers of a
  shared view see only a count) and offers **Save to fix** to whoever may change it. A view whose
  filter no longer exists is shown disabled and is never applied.
- Delete confirmations name the view and, for a shared view, how many users have it as their default.
