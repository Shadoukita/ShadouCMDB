/** The form section an attribute without groupName is shown in. */
export const DEFAULT_SECTION = "Other";

/**
 * Groups attribute definitions by groupName (the form section), keeping the API's
 * sortOrder inside and across groups: sections appear in the order of their first
 * attribute. The CI form, the detail page and the attribute editor all use this.
 */
export function groupAttributes<T extends { sortOrder: number; label: string; groupName: string | null }>(defs: T[]): [string, T[]][] {
  const sorted = [...defs].sort((a, b) => a.sortOrder - b.sortOrder || a.label.localeCompare(b.label));
  const map = new Map<string, T[]>();
  for (const d of sorted) {
    const g = d.groupName || DEFAULT_SECTION;
    map.set(g, [...(map.get(g) ?? []), d]);
  }
  return [...map.entries()];
}
