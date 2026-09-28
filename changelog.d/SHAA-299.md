### Added: layout sections can hold a note or a built-in panel (API)

A layout section has a new optional `kind` ([SHAA-299]): `fields` (the default, a grid of fields as
before), `note` (static text written by an administrator in `text`, at most 4,000 characters, plain
text or limited Markdown; the web UI never renders raw HTML from it), or one of the detail page's
built-in panels `relations`, `history` and `audit`, which can then be placed in any tab. Each panel can
be placed once per layout; a panel a layout does not place keeps its usual position. The API refuses a
panel placed twice, `fields` or `text` on a section of the wrong kind and an empty or over-long note
(`400` with the path). The editor for these sections follows in the web UI.

**Upgrade:** nothing to do. The change is additive to layout format v2: saved layouts, earlier settings
versions and configuration exports stay valid and are returned unchanged (`kind` is only written for
sections that are not `fields`).

[SHAA-299]: docs/data-model.md#detail-and-form-layouts-ui-settings-layout-format-v2
