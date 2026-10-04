### Fixed: Business services open ready to edit (GH#588)

A business service's *Overview* now shows its fields as inputs, like every other CI since
SHAA-1644: there is no separate **Edit** button. Once something was changed a bar offers **Save**
and **Discard**, the page stays where it is after saving, and leaving the service with unsaved
changes asks first. Users without the edit right on the service class see the values read-only.
