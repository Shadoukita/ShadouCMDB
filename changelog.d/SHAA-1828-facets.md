### Added: Facet panel beside the inventory table

The inventory now has a **Facets** panel to the left of the table. It lists the values of the class, the criticality and every lookup list held in a lookup attribute (status, environment, location and so on), each with how many CIs match.

- **Counts that add up:** each facet is counted with every other filter applied but its own left out, so a count tells you how many CIs ticking that value adds. The counts follow the search, the query bar, the toolbar filters and the chips, and only cover CIs in classes you may view.
- **Tick to filter:** ticking a value sets the same filter as the toolbar and the query bar, so it shows in both, is kept in the URL and survives a reload, and can be saved in a view. Ticking several values of one facet shows CIs with any of them.
- **Long lists:** each facet shows its eight largest values first; **Show more** lists the rest.
- **Remembered per browser:** collapsed facets, and whether the panel is shown. The panel is open by default on screens wider than 820 px and closed on narrower ones, where it sits above the table.

The counts come from `GET /api/v1/configuration-items/facets`. A business-service facet is not shown yet, because the inventory's filters and saved views have no business-service filter.
