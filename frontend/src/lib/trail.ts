// The "walk" trail: when an operator clicks from one CI to a related CI, the path
// they took is carried in router state and shown in the breadcrumb
// (Inventory › CRM › crm-app-01 › fra1-esx-01). It lives in history state, so it
// survives reload and back/forward, but is not part of the shareable URL.
import { useLocation } from "react-router-dom";

export interface TrailStep {
  id: string;
  name: string;
}

const MAX_TRAIL = 6;

export function useTrail(): TrailStep[] {
  const state = useLocation().state as { trail?: TrailStep[] } | null;
  return Array.isArray(state?.trail) ? state.trail : [];
}

/** Trail to hand to the next CI when navigating away from `current`. */
export function extendTrail(trail: TrailStep[], current: TrailStep | undefined, nextId: string): TrailStep[] {
  const base = current ? [...trail.filter((s) => s.id !== current.id), current] : trail;
  const cut = base.findIndex((s) => s.id === nextId);
  const result = cut >= 0 ? base.slice(0, cut) : base;
  return result.slice(-MAX_TRAIL);
}
