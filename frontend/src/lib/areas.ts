import type { NavArea } from "./uiSettings";

/**
 * Items (classes, or tree nodes of classes) under their area, in the areas' tab
 * order; the item order is kept within an area. Items whose area is unknown
 * (areas not loaded yet) come last under a null area. For <optgroup> pickers.
 */
export function groupByArea<T>(items: readonly T[], areaIdOf: (item: T) => string | undefined, areas: readonly NavArea[]): { area: NavArea | null; items: T[] }[] {
  const sorted = [...areas].sort((a, b) => a.sortOrder - b.sortOrder);
  const out = sorted.map((area) => ({ area: area as NavArea | null, items: items.filter((i) => areaIdOf(i) === area.id) }));
  const known = new Set(areas.map((a) => a.id));
  const rest = items.filter((i) => !known.has(areaIdOf(i) ?? ""));
  if (rest.length) out.push({ area: null, items: rest });
  return out.filter((g) => g.items.length > 0);
}
