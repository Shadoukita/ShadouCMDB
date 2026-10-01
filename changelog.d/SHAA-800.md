### Added: Bulk import wizard in the web UI

Operators with the new **Bulk import** permission (`cis.import`) can import configuration items from a
CSV or Excel (`.xlsx`) file under **Inventory › Bulk import** (`/imports`), in four steps:

1. **Upload.** The file is read on the server; the wizard shows the row and column count, a preview of
   the first 20 rows, and lets the operator change the sheet, encoding, delimiter and header row.
2. **Map columns.** The target class, the mode (create, update, or both), the attribute that finds
   existing CIs, and one row per file column that maps it to an attribute, the ident, the validity
   dates or a relationship. The choices come from the class's attributes and relationship rules, so
   new classes need no change. Columns are matched automatically by key and name. A mapping can be
   saved and is applied automatically to files with the same column names.
3. **Check.** A dry run of every row, with the counts, a sample of the planned changes, the row
   problems (filterable by column, problem and severity) and a downloadable error report. Rows with
   errors can be skipped after a confirmation that states how many rows are imported and skipped.
4. **Import.** Progress, a **Stop import** that finishes the current batch of at most 500 rows, and the
   result with links to the inventory and the audit log.

Every step survives a reload and can be shared as a link. **Upload a corrected file** keeps the class
and mapping. Recent imports are listed on `/imports`; administrators can show every user's imports.

Import is off after installation. An administrator turns it on under **Administration › Import** and
grants **Bulk import** in a permission profile. The server setting `IMPORT_ALLOWED=false` locks it off.

Also in this release, contrast fixes in the dark theme (error text, red buttons, the hover colour of
primary buttons) and for the CI counts in the sidebar.

**Upgrade:** nothing to do. Bulk import stays off until an administrator turns it on.
