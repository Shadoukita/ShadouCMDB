import { computed } from "vue";
import { useRoute, useRouter, type LocationQueryRaw } from "vue-router";

/**
 * URL-backed list state (filters, sort, page) for admin lists, so a view
 * survives reload and can be shared — the same convention as the inventory.
 */
export function useListQuery(defaults: { sort: string; limit?: number }) {
  const route = useRoute();
  const router = useRouter();
  const defaultLimit = defaults.limit ?? 50;
  const get = (k: string) => {
    const v = route.query[k];
    return typeof v === "string" ? v : "";
  };
  const limit = computed(() => clampInt(get("limit"), defaultLimit, 1, 200));
  const offset = computed(() => clampInt(get("offset"), 0, 0, 1_000_000));
  const sort = computed(() => get("sort") || defaults.sort);

  function update(patch: Record<string, string | undefined>, resetPage = true) {
    const next: LocationQueryRaw = { ...route.query };
    for (const [k, v] of Object.entries(patch)) {
      if (v) next[k] = v;
      else delete next[k];
    }
    if (resetPage) delete next.offset;
    const to = { path: route.path, query: next };
    if ("q" in patch) router.replace(to);
    else router.push(to);
  }

  function onPage(p: { limit: number; offset: number }) {
    update({ limit: p.limit === defaultLimit ? undefined : String(p.limit), offset: p.offset ? String(p.offset) : undefined }, false);
  }

  const toggleSort = (field: string) => update({ sort: sort.value === field ? `-${field}` : field });
  const ariaSort = (field: string): "ascending" | "descending" | "none" =>
    sort.value === field ? "ascending" : sort.value === `-${field}` ? "descending" : "none";

  return { get, limit, offset, sort, update, onPage, toggleSort, ariaSort };
}

function clampInt(raw: string, fallback: number, min: number, max: number): number {
  const n = raw ? Number.parseInt(raw, 10) : NaN;
  return Number.isFinite(n) ? Math.min(max, Math.max(min, n)) : fallback;
}
