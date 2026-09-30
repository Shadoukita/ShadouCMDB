### Added: a layout editor on the real CI page

Users with **customization.manage** get an **Edit layout** button on the CI detail page and on the CI
form (edit and new). It opens the layout editor in a separate browser window, one per class
(a second click brings the open window to the front), while the page it came from stays as it is. The
editor shows the real page, framed and with a sticky bar that names the class: the change applies to
every CI of that class. The page keeps showing the
CI's real values while tabs are added (**+ Tab** at the end of the tab bar), sections are added between
and after sections (**+ Section**), tabs and sections are renamed by clicking their name, fields are
dragged between sections and onto tabs and resized by their right edge, and a toolbar on each field and
section moves, collapses, sets columns, hides and removes. Hidden fields are listed in a tray to show
them again. The bar has undo and redo (Ctrl+Z, Ctrl+Shift+Z), desktop, tablet and phone widths,
**Reset to built-in layout**, **Save layout** with an optional note, **Discard** and **Done** (closes
the window); leaving or closing the window with unsaved changes asks first. Every drag has a keyboard
equivalent. After a save, the other open windows of the web UI show the new layout without a reload.
When a popup blocker refuses the window, the editor opens in the same tab and says so.

Saving creates a new UI settings version exactly as **Customization** does (same API, permission check,
history and audit trail). If someone else saved in the meantime, the save is refused with a message
and a **Load the latest version** button. The designer in **Administration › Customization › Detail and
form layout** stays available and gains **Open on a CI** (the class's first CI, or an empty form of the
class when it has none), in the same editor window. The editor's URLs (`/cis/<id>/layout-editor`,
`/cis/<id>/edit/layout-editor`, `/cis/new/layout-editor?classId=…`) show the normal page to users
without the permission. No API or database change.
