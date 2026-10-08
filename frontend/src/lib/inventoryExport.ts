// The inventory's CSV export (GET /configuration-items/export, gap G9): the list's query without its page,
// with the columns the list shows. Kept free of browser globals so the unit tests can run it.
import type { paths } from "../api/schema";

export type InventoryExportQuery = NonNullable<paths["/api/v1/configuration-items/export"]["get"]["parameters"]["query"]>;
export type ExportDelimiter = NonNullable<InventoryExportQuery["delimiter"]>;

/** List parameters the export does not take: the page, and the dashboard's data-quality drill-down. */
const LIST_ONLY = ["limit", "offset", "quality", "endOfLifeWithinDays"] as const;

/**
 * True while the list is narrowed by a filter the export cannot apply (a data-quality check): the file would hold
 * CIs the list leaves out, so the export is offered only without it.
 */
export function exportUnsupported(listQuery: Record<string, unknown>): boolean {
  return listQuery.quality !== undefined && listQuery.quality !== null && listQuery.quality !== "";
}

/** The export parameters for the list query and its visible columns, in the order shown. */
export function inventoryExportQuery(
  listQuery: Record<string, unknown>,
  columns: readonly string[],
  delimiter: ExportDelimiter,
): InventoryExportQuery {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(listQuery)) {
    if (v === undefined || v === null || v === "" || (LIST_ONLY as readonly string[]).includes(k)) continue;
    out[k] = v;
  }
  if (columns.length > 0) out.columns = columns.join(",");
  if (delimiter !== "comma") out.delimiter = delimiter;
  return out as InventoryExportQuery;
}

/** The file name when the server's is unreadable (another origin): inventory-server-20261008-1432.csv. */
export function exportFileName(classKey: string | undefined, stamp: string): string {
  const scope = classKey ? classKey.replace(/[^A-Za-z0-9_-]+/g, "-") : "all";
  return `inventory-${scope}-${stamp}.csv`;
}
