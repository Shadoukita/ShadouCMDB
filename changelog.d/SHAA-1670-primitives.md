### Changed: Consistent buttons, forms, tables, menus, dialogs and messages in the web UI

The web UI's shared building blocks now follow one design across every page:

- **Buttons:** slightly taller (30 px), with one secondary style. Disabled buttons are greyed out
  instead of faded, so their label stays readable. Small buttons are never below the 24 px minimum
  target size.
- **Forms:** selects draw the same chevron in every browser and theme, checkboxes and radio buttons
  use the primary colour, and a field error shows an icon next to its message.
- **Tables:** column headers are set as small capitals, rows use lighter separators, and identifiers
  and figures line up.
- **Menus and lists:** the row menu, the View menu and the search and owner pickers share one item
  style. A pointer hover no longer shows the keyboard focus ring.
- **Dialogs:** a divider separates the title from the content, and the default buttons are
  translated. The delete dialog no longer shows "Working…" while it checks what a row is used by.
- **Messages:** every message has an icon and a coloured edge for its tone. Confirmations after a
  save are now shown as success messages instead of neutral notes.
- **Empty states** show an icon above their title.

Nothing moves on the page and no workflow changes.
