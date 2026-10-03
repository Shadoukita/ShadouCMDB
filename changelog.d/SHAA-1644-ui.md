### Changed: A configuration item opens ready to edit

A CI's page shows its fields as inputs straight away; there is no separate **Edit** button any more.
Change a value and a bar at the bottom of the page offers **Save** and **Discard**; with nothing
changed there is nothing to save, and after saving the page stays as it is with the saved values.
Leaving the CI (another page, another CI, closing the tab) with unsaved changes asks first.

Saving works as before: only the changed fields are sent, with the version the page was loaded at,
so a change saved by someone else in the meantime is not overwritten. The page then says so and
offers to load the current version. Fields the API rejects show their message next to the field.

Users without the edit right on the CI's class, deleted CIs, fields the class layout makes read-only,
and a Person's Email that follows a sign-in account show their values read-only in the same place.
