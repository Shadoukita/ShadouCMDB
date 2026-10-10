### Changed: Workflow instances, an instance's page and a CI's Workflows tab in the new look, in English and German

The Workflows list now uses the inventory's head band: breadcrumb, title with the instance count, intro and the
Workflow, State, Status and CI type filters, above the "Running now" summary and the table card with numbered
pages. Instance statuses are pills with an icon (Running, Completed, Cancelled). An instance's page uses the
record head band: the workflow's name with its state and status, the CI as a chip, the version and who started
it, and Force state for holders of `workflows.manage`. The CI's Workflows tab, the transition, start, cancel
and force dialogs and the history follow the active language, and German users no longer see English text on
these screens. The static `style` attributes on these screens are gone.

No URL, permission or API changes.
