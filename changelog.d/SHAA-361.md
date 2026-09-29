### Added: free-placement tabs in the layout format (API)

A layout tab can now set `placement: "free"`. Its sections become windows that can be placed anywhere,
sized freely and stacked over each other: each has a `frame` with `x` and `w` as fractions of the tab
width, `y` and `h` in pixels, a stacking order `z` and an optional `minH` ([layout format]). This entry covers
the format, its validation and the grid ↔ free conversion in the API; placing windows in **Edit layout**
and showing them on the detail page and form are described under "free window placement in Edit
layout".

**Migration notes.** None needed. The change is additive: a tab without `placement` is a grid tab as
before, and the API writes `placement` and `frame` only for free tabs, so stored layouts, saved
versions and configuration exports read and write back unchanged. What API clients should know:

- The API **normalises free tabs on save**: sections without a frame get one from their grid position,
  `z` is renumbered 1..n, `x` and `w` are rounded to 4 decimals, and sections are stored in reading
  order (`y`, then `x`). Compare the saved document, not the one you sent.
- A **grid tab sent with frames is converted back to the grid** (sections ordered by `y`, then `x`,
  widths from `w`, frames dropped). A client that preserves unknown section keys when it edits a free
  tab must also keep the tab's `placement`, or the tab goes back on the grid.
- Frames that do not fit the tab (`x + w` over 1, `minH` over `h`, values out of range) are refused
  with `400 VALIDATION_ERROR` and the path of the value, also in configuration imports.
- The audit trail and configuration export/import carry the new keys unchanged.

[layout format]: docs/data-model.md#detail-and-form-layouts-ui-settings-layout-format-v2
