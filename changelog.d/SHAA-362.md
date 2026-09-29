### Added: free window placement in Edit layout

A tab of a class layout can now be switched from **Grid** to **Free** in the bar of **Edit layout** on a
CI page ([SHAA-362]). On a free tab every section, note and panel is a window: drag it anywhere by its
title bar, resize it from any edge or corner, and let windows overlap. A readout shows the position and
size while a window moves. Edges snap to the other windows and to an 8 px guide grid; hold **Alt**, or
turn **Snap off** in the bar, to place a window to the pixel.

- **Layers.** Pressing on a window brings it to the front. The bar, each window's toolbar and its
  right-click menu offer **Bring to front**, **Bring forward**, **Send backward** and **Send to back**.
  The saved order is what every user sees.
- **Keyboard.** On a window's grip, the arrow keys move it by 8 px (Shift: 64 px, Alt: 1 px),
  Ctrl+arrows resize it, Ctrl+PageUp / Ctrl+PageDown move it one layer (with Shift: to the front or the
  back), and Shift+F10 opens the layers menu.
- Switching to Free keeps every section where it is on screen. Switching back to Grid orders the
  sections by position and takes their widths from the windows, as the API does. Undo, redo and Discard
  cover every move, resize, layer change and switch. Save stores a new settings version as before.
- The detail page and the form show a free tab as saved: each window scrolls its own content, and a
  collapsed one shows only its title bar. Below 820 px of width (tablets and phones) and in print, the
  windows stack at the full width in reading order. Fields the layout does not place follow below the
  lowest window.

Grid stays the default, and grid tabs look and work as before. **Administration › Customization ›
Detail and form layout** previews a free tab on the grid and says so; positions and layers are edited
with Edit layout. No API or database change: the format is the one added in [SHAA-361].

[SHAA-362]: docs/data-model.md#editing-a-layout-on-the-ci-page
[SHAA-361]: docs/data-model.md#detail-and-form-layouts-ui-settings-layout-format-v2
