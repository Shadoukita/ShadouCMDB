/**
 * Numbered pagination and the criticality meter of the inventory (design document §0, step 12c).
 */

/** A page button, or a gap ("…") between two runs of pages. */
export type PageItem = number | "gap";

/**
 * The page buttons for `page` of `pages` (both 1-based): the first and last page, the current page with one
 * neighbour on each side, and the first or last three while the current page is near that end. A gap stands
 * for two or more hidden pages; a single hidden page is shown instead of a gap.
 */
export function pageItems(page: number, pages: number): PageItem[] {
  if (pages <= 0) return [];
  const p = Math.min(Math.max(1, page), pages);
  const shown = new Set<number>([1, pages, p - 1, p, p + 1]);
  if (p <= 3) [2, 3].forEach((n) => shown.add(n));
  if (p >= pages - 2) [pages - 1, pages - 2].forEach((n) => shown.add(n));
  const list = [...shown].filter((n) => n >= 1 && n <= pages).sort((a, b) => a - b);
  const out: PageItem[] = [];
  for (const n of list) {
    const prev = out.length ? (out[out.length - 1] as number) : 0;
    if (out.length && n - prev === 2) out.push(prev + 1);
    else if (out.length && n - prev > 2) out.push("gap");
    out.push(n);
  }
  return out;
}

/** Bars filled for a criticality rank (1 = most critical, fills all) on a meter of `levels` bars. */
export function meterFill(rank: number, levels: number): number {
  if (levels <= 0) return 0;
  return Math.min(levels, Math.max(1, levels - rank + 1));
}
