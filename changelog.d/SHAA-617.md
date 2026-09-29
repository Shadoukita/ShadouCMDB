### Added: Column chooser and search filters

The inventory toolbar has a **Columns** button. Operators pick the columns of the list they are
looking at, in their order, from the built-in fields and, when the list shows one class, that class's
attributes. The choice is part of the address (`columns=label,ident,attributes.os`), so a reload, a
bookmark or a shared link shows the same columns. Without a choice the list shows the class's list
view columns (Administration › Customization › List views), or the default columns; **Reset to default
columns** returns to them.

The search page gets filters for class, validity and deleted CIs. They are kept in the address too, and
**Open as filterable inventory** carries them over. The inventory also honours an `ipWithin` filter in
the address and shows it in the toolbar.

Opening a class list from the menu when its list view has default filters now queries the list once,
with the filters, instead of first without them.

### Fixed: Primary button contrast in the dark theme

Primary buttons in the dark theme use a slightly deeper blue, so their white text meets the WCAG AA
contrast ratio (4.8:1, was 4.1:1).
