### Changed: Spacing on the shared scale, styled error messages and even gaps between layout sections

- Paddings, margins and gaps use the spacing scale everywhere. The design token check (`npm run lint` in
  `frontend/`) now also reports spacing literals. A few optical offsets that centre a dot or a checkbox
  on a line stay, each with its reason. Some controls move by 1 or 2 px: the dashboard period switch, the
  dashboard mini bars, multi-select filters (their caret gutter now matches the other selects), wrapped
  table cells and the database change preview.
- An error message outside a form field (for example in the query bar's error list, the member picker's
  tray or the import column mapping) now has the danger colour and the alert icon, like field errors.
- Sections of a detail or form layout are spaced by the layout grid alone: 16 px between rows and columns.
  Before, rows had 28 px and columns 12 px.
- Customization and the user picker no longer use `style` attributes, which the served Content Security
  Policy (`style-src 'self'`) can drop.

No URL, permission, API or stored-layout changes.
