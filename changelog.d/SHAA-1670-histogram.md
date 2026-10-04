### Added: Change histogram above the inventory table

On screens wider than 820 px, the inventory shows a **Changes** strip above the table for users who may view the audit log (`audit.view`). It charts how many changes were made per hour or per day to the CIs that match the current search and filters, split into updated, created and status changed.

- **Time ranges:** choose 24 hours, 7 days (the default), 30 days or 90 days. Hours are shown in your browser's time zone. Days are UTC days, as the audit log counts them.
- **One day by hour:** click a day's bar, or press Enter on it, to see that day by hour. **Back** returns to the range.
- **Keyboard and screen readers:** the chart is a single tab stop. The left and right arrow keys move between bars, and each bar's counts are read out. **Show values** lists every non-empty hour or day as a table.
- **Remembered per browser:** the chosen range, and whether the strip is open or collapsed.

The counts come from the new `GET /api/v1/configuration-items/change-histogram` endpoint and follow the same permissions as the audit log, so a user only sees counts for CIs of classes they may view. Users without `audit.view` don't see the strip, and the inventory table is unchanged for them.
