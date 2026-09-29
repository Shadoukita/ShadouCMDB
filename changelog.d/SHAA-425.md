### Fixed: inventory rows open their CI when a list view leaves out the Label column

A list view without the **Label** column (removed under **Administration › Customization › List
views**, or saved that way by an earlier version) left the inventory without any link to open a CI,
and CIs with empty visible columns could not be told apart. The inventory now shows **Label** as the
first column of such a view, linked to the CI, and the list-view editor says so. Stored list views
are not changed.
