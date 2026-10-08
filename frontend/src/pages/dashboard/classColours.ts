import { useQueries } from "@tanstack/vue-query";
import { computed } from "vue";
import { ciCountQuery, useCiClasses } from "../../api/queries";
import { colourByRank } from "../../lib/dashboard";
import { useSessionStore } from "../../stores/session";

/**
 * The colour of each inventory class on the dashboard: by its CI count, largest first, as the "CIs by class"
 * bars rank them, so a class keeps its colour from the bars to the recent activity. The per-class counts are
 * the same cached requests the bars make.
 */
export function useClassColours() {
  const session = useSessionStore();
  const classes = useCiClasses();
  const concrete = computed(() =>
    (classes.data.value ?? []).filter((c) => !c.isAbstract && c.kind === "asset" && session.canOnClass(c.id, "view")),
  );
  const counts = useQueries({ queries: computed(() => concrete.value.map((c) => ciCountQuery({ classId: c.id }))) });
  return computed(() => {
    const ranked = concrete.value
      .map((c, i) => ({ id: c.id, name: c.name, count: counts.value[i]?.data ?? -1 }))
      .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
    return new Map(ranked.map((c, rank) => [c.id, colourByRank(rank)]));
  });
}
