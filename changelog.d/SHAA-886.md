### Added: the Impact tab, and Criticality in the web UI

Every CI's detail page has a new **Impact** tab (`/cis/<id>/impact`), opened also from the
*Impact analysis* button in the page header, the *Impact* action of each inventory and search
result row, and *Analyse impact* on each CI of the relationship map. It shows which CIs are
affected if the CI fails or changes (*Downstream*), which CIs it depends on (*Upstream*), or both.

- Choose the depth (up to the server's `IMPACT_MAX_DEPTH`), the relationship types to follow and
  whether to include inactive CIs. Every choice is kept in the address, so an analysis can be
  bookmarked, shared, reloaded and walked back with the browser's Back button.
- The **list** view groups the affected CIs by class, criticality or hop distance, sorts by any
  column, links the last hop of each path ("runs on *db-01*"), and shows the whole path on
  *Show path*. The **tree** view shows each CI once under the CI its shortest path comes from,
  with separate trees for the two directions in *Both* mode; it is fully keyboard operable.
- The header gives the count and the critical and high CIs among them, says when a result is
  incomplete (node, relationship or time limit) and when more CIs lie beyond the chosen depth,
  and notes that results include only CIs of classes the user may view whenever their profile
  limits the classes.
- *Export CSV* downloads the current result from the server (recorded in the audit log).
- When no relationship type propagates impact, the tab says so; administrators with *Manage data
  model* get a link to *Data model → Relationship types*.
- *Data model → Relationship types*: a new **Impact propagation** field on each type, worded from
  the type's own labels (a symmetric type offers only *does not propagate* and *both ways*), and
  an *Impact* column in the list.
- **Criticality**: a field on every CI form (after the validity period), a badge on the CI page
  with its text, a *Criticality* filter in the inventory and search, and a *Criticality* column
  the inventory can show (*Columns*). List views in *Customization* cannot store the column yet.
