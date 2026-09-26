import type { EffectiveAttribute } from "../api/queries";

/** Groups attribute definitions by groupName, keeping the API's sortOrder inside and across groups. */
export function groupAttributes(defs: EffectiveAttribute[]): [string, EffectiveAttribute[]][] {
  const sorted = [...defs].sort((a, b) => a.sortOrder - b.sortOrder || a.label.localeCompare(b.label));
  const map = new Map<string, EffectiveAttribute[]>();
  for (const d of sorted) {
    const g = d.groupName || "Other";
    map.set(g, [...(map.get(g) ?? []), d]);
  }
  return [...map.entries()];
}
