### Changed: CI record relationships panel and CI picker

The **Relationships** panel of a CI has been tidied up, and the CI picker now looks the same wherever it is used.

- **Direction:** the "This CI…" column starts with an arrow icon: outgoing, incoming, or both ways for a relationship type without a direction. Screen readers announce the direction by name. The separate Direction column is gone.
- **Related CI:** each related CI shows its class icon, and hostnames appear in the data font. A deleted CI has a **Deleted** badge.
- **Remove** is now a small button at the end of the row instead of a red link.
- **Add form:** the CI and relationship fields have the same width, and the notes field takes up the rest of the row.
- **CI picker** (Add relationship, reference fields, workflow conditions): a chosen CI now appears as a filled field with a clear button (×) instead of bold text with a **Change** button. Clearing it puts the cursor back in the search box.
- The empty option of a dropdown, enum or yes/no field now reads **Not set** instead of "— not set —".

### Changed: CI history as an event stream

The **History** panel of a CI now reads as an event stream.

- **Columns:** time, actor, source, event and change. Times are in UTC (the local time is in the tooltip), so they can be compared with server logs.
- **Source:** each entry shows where the change came from: **UI** (a signed-in user), **API** (an API token), **Import** or **System**.
- **Events** have readable names in English and German, with one colour per kind: created and restored in green, deleted in red, workflow steps in blue. The raw action stays in the tooltip.
- **Paging:** the history is paged on the server, 50 entries per page, instead of showing only the latest 50.
- Entries whose details are withheld from you now say **Details withheld** instead of "No visible field changes".
- The **Audit trail** panel uses the same UTC times and event names.
