/**
 * The section every form and detail page starts with: the core CI fields (ident,
 * validity) and the attributes without a groupName.
 */
export const GENERAL_SECTION = "General";

/**
 * Groups attribute definitions by groupName (the form section), keeping the API's
 * sortOrder inside and across groups: the General section (no groupName) comes
 * first, the others in the order of their first attribute. The CI form, the detail
 * page and the attribute editor all use this.
 */
export function groupAttributes<T extends { sortOrder: number; label: string; groupName: string | null }>(defs: T[]): [string, T[]][] {
  const sorted = [...defs].sort((a, b) => a.sortOrder - b.sortOrder || a.label.localeCompare(b.label));
  const map = new Map<string, T[]>([[GENERAL_SECTION, []]]);
  for (const d of sorted) {
    const g = d.groupName || GENERAL_SECTION;
    map.set(g, [...(map.get(g) ?? []), d]);
  }
  return [...map.entries()].filter(([, items]) => items.length > 0);
}
