### Added: visual form designer; layouts get tabs, sections and a field grid

**Administration › Customization › Detail and form layout** is now a visual designer. It
shows the class's form as it will look and lets administrators drag fields between sections and tabs,
resize them on the section's field grid (up to 12 columns, see "sections side by side and a finer grid"), add, rename, reorder and remove tabs and sections, hide fields and
make them read-only, and resize the preview (or pick laptop, tablet or phone width) to check smaller
screens. Every action also works from the keyboard. Ident, valid from and valid until can be moved but
not hidden. The CI form and the detail page show the layout's tabs; the detail page's relationship map
and history follow them.

**Upgrade:** migration `0017_layout_tabs` converts saved layouts to the new format (layout format v2)
as a new settings version: the old panels become the sections of one **General** tab, in the same order,
so forms and detail pages look as before apart from the field grid. Hidden ident and validity fields are
shown again. Nothing else in the settings changes, and the previous version stays in
**Customization › History**.

**API:** `UiClassLayout` has `tabs[]` → `sections[]` (`columns`) → `fields[]` (`{ field, width }`) instead
of `panels[]`, and the API validates them (unique tab and section keys, a field placed once, widths
within the section's columns, core fields not hidden; `400` with the path otherwise). `panels[]` is
deprecated: still accepted from older exports and API clients and converted to one General tab, but
never returned. New issue code `core_field_hidden`.
