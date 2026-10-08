/**
 * The inventory's facet panel (design document §2.7, SHAA-1670 rollout 5f): which facets it shows,
 * how ticking a value changes the URL, and the per-browser preference. The counts come from
 * GET /configuration-items/facets; nothing is counted or filtered in the browser.
 */

/** The facet parameters the inventory URL holds (a business service filter has no place in it or in a saved view yet). */
export const FACET_PARAMS = ["classId", "criticalityValueId", "lookupValueId"] as const;
export type FacetParam = (typeof FACET_PARAMS)[number];

export const isFacetParam = (p: string): p is FacetParam => (FACET_PARAMS as readonly string[]).includes(p);

/** Values shown per facet before "Show more"; selected values are always shown. */
export const FACET_PREVIEW = 8;

/** The ids of a comma-separated URL parameter. */
export const idsOf = (raw: string): string[] => raw.split(",").filter(Boolean);

/** The parameter's new value with `id` ticked or unticked; undefined once nothing is left. */
export function toggleId(raw: string, id: string, on: boolean): string | undefined {
  const ids = idsOf(raw).filter((x) => x !== id);
  if (on) ids.push(id);
  return ids.length ? ids.join(",") : undefined;
}

/** The values a group shows: the first FACET_PREVIEW, plus any selected value past them, unless expanded. */
export function visibleValues<V extends { selected: boolean }>(values: readonly V[], expanded: boolean): V[] {
  if (expanded) return [...values];
  return values.filter((v, i) => i < FACET_PREVIEW || v.selected);
}

// ---------- Preference: whether the panel is open, and which groups are collapsed ----------

export interface FacetPref {
  /** Undefined: never chosen, so open on wide screens and closed on narrow ones. */
  open?: boolean;
  collapsed: string[];
}

const PREF_KEY = "shadoucmdb.facets";

export function readFacetPref(storage: Pick<Storage, "getItem"> | undefined = globalThis.localStorage): FacetPref {
  try {
    const raw = storage?.getItem(PREF_KEY);
    if (raw) {
      const v = JSON.parse(raw) as unknown;
      if (v && typeof v === "object") {
        const o = v as { open?: unknown; collapsed?: unknown };
        return {
          open: typeof o.open === "boolean" ? o.open : undefined,
          collapsed: Array.isArray(o.collapsed) ? o.collapsed.filter((k): k is string => typeof k === "string").slice(0, 100) : [],
        };
      }
    }
  } catch {
    // Storage disabled or a bad value: the defaults.
  }
  return { collapsed: [] };
}

export function writeFacetPref(pref: FacetPref, storage: Pick<Storage, "setItem"> | undefined = globalThis.localStorage): void {
  try {
    storage?.setItem(PREF_KEY, JSON.stringify(pref));
  } catch {
    // Storage disabled: the choice lasts for this page load.
  }
}
