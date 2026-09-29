### Added: resize sections by dragging and place them side by side in the layout editors

The form designer (**Customization › Detail and form layout**) and the layout editor window (**Edit
layout** on a CI) now edit the 12-column grid directly ([editing a layout on the CI page]):

- **Resize a section** by dragging its right edge; it snaps to the 12 columns and shows e.g. "6 / 12"
  while you drag. Between two sections in a row, the left edge of the second moves the border between
  them.
- **Place a section beside another** by dragging the grip on its top edge onto the other section's left
  or right edge (a bar shows where it goes). It takes the columns the row leaves free, or half of the
  other section when the row is full. The **+** on a section's right edge adds a new section next to it;
  **+ Section** below it adds one underneath.
- **Fields** can be resized on grids of up to 12 columns, with the same live guide.
- **Preview width:** the preview frame has a visible grip on its right edge to drag it to any width;
  the Full width / Laptop / Tablet / Phone buttons are shortcuts.
- **Keyboard:** on a section's grip, Alt+← / Alt+→ resize it and Alt+↑ / Alt+↓ move it; the section's
  toolbar and the designer's **Properties** set the width (1–12 / 12), **Start a new row** and the
  columns. The preview grip takes ← / →, Home and End. In the layout editor, one drag is one undo step.

On the CI detail page and the form, a section's field grid now narrows with the section's own width, so
a half-width section on a wide screen uses two columns instead of squeezing three.

**Upgrade:** nothing to do. No API or schema change; layouts saved before look the same.

[editing a layout on the CI page]: docs/data-model.md#editing-a-layout-on-the-ci-page
