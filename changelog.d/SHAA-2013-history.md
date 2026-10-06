### Changed: CI history as an event stream

The **History** panel of a CI now reads as an event stream.

- **Columns:** time, actor, source, event and change. Times are in UTC (the local time is in the tooltip), so they can be compared with server logs.
- **Source:** each entry shows where the change came from: **UI** (a signed-in user), **API** (an API token), **Import** or **System**.
- **Filter by source:** chips above the stream show only the entries from the chosen sources. The filter runs on the server, so paging and the entry count match it. **Show all** clears it.
- **Events** have readable names in English and German, with one colour per kind: created and restored in green, deleted in red, workflow steps in blue. The raw action stays in the tooltip.
- **Paging:** the history is paged on the server, 50 entries per page, instead of showing only the latest 50.
- Entries whose details are withheld from you now say **Details withheld** instead of "No visible field changes".
- The **Audit trail** panel uses the same UTC times and event names.
