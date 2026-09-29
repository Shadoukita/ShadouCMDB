### Added: sections side by side and a finer grid in detail and form layouts

Each tab of a class layout is now a grid of 12 columns ([layout format]). A section has a `width` (1–12,
default 12, the full width) and sections fill the grid row by row, so two sections of width 6 sit side
by side; `newRow: true` starts a new row early, and the optional `minHeight` keeps a section at least
that many field rows tall. A section's own field grid (`columns`) and field widths go up to 12, for
finer sizes than the earlier 1–4. Sizes are fractions of the width, never pixels: below the tablet
breakpoint (820 px) sections stack at the full width. Both layout editors set these by
dragging (see "resize sections by dragging and place them side by side in the layout editors").

**Upgrade:** nothing to do. Layouts and exports saved before stay valid and look the same (every
section is full width); there is no migration.

**API (additive):** `UiLayoutSection` gets `width` (always returned, default 12), `newRow` and
`minHeight` (returned only when set); the maximum of `columns` and `UiLayoutField.width` is now 12, and
a field's width must still fit its section's columns (`400` with the path otherwise). API clients that
rebuild a layout from its known keys should keep the new ones, or saving drops them.

[layout format]: docs/data-model.md#detail-and-form-layouts-ui-settings-layout-format-v2
