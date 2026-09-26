// The "walk" trail: when an operator clicks from one CI to a related CI, the path
// they took is carried in history state and shown in the breadcrumb
// (Inventory › CRM › crm-app-01 › fra1-esx-01). It survives reload and
// back/forward, but is not part of the shareable URL.
import { computed, type ComputedRef } from "vue";
import { useRoute } from "vue-router";

// A type alias (not an interface) so it satisfies vue-router's HistoryState index signature.
export type TrailStep = { id: string; name: string };

const MAX_TRAIL = 6;

export function useTrail(): ComputedRef<TrailStep[]> {
  const route = useRoute();
  return computed(() => {
    void route.fullPath; // history.state is not reactive; re-read it on every navigation
    const trail = (window.history.state as { trail?: unknown } | null)?.trail;
    return Array.isArray(trail) ? (trail as TrailStep[]) : [];
  });
}

/** Trail to hand to the next CI when navigating away from `current`. */
export function extendTrail(trail: TrailStep[], current: TrailStep | undefined, nextId: string): TrailStep[] {
  const base = current ? [...trail.filter((s) => s.id !== current.id), current] : trail;
  const cut = base.findIndex((s) => s.id === nextId);
  const result = cut >= 0 ? base.slice(0, cut) : base;
  return result.slice(-MAX_TRAIL);
}
