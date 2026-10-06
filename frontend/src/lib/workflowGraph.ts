// Geometry of the workflow diagram (WorkflowGraph.vue): arrows between state boxes. Pure, so it is unit-tested.
import type { Position } from "./workflowDraft";

export interface Edge {
  key: string;
  name: string;
  from: string;
  to: string;
  /** SVG path: a quadratic curve from the border of `from` to the border of `to`. */
  path: string;
  /** Where the label goes: the middle of the curve. */
  label: Position;
}

/** How far apart transitions between the same two states run: more than a label's height, so their labels never overlap. */
const SPREAD = 40;

/** The longest transition name drawn whole on an arrow; it fits the gap the arrangement leaves between columns. */
export const EDGE_LABEL_MAX = 22;

/** A name as drawn in the diagram: shortened with an ellipsis past `max` characters (the full name is in its tooltip). */
export function shorten(name: string, max: number): string {
  return name.length > max ? `${name.slice(0, max - 1)}…` : name;
}

/** Where the line from a box's centre towards `toward` leaves the box. */
export function clipToBox(center: Position, size: { w: number; h: number }, toward: Position): Position {
  const dx = toward.x - center.x;
  const dy = toward.y - center.y;
  if (dx === 0 && dy === 0) return center;
  const s = Math.min(dx ? size.w / 2 / Math.abs(dx) : Infinity, dy ? size.h / 2 / Math.abs(dy) : Infinity);
  return { x: center.x + dx * s, y: center.y + dy * s };
}

const r = (n: number) => Math.round(n * 10) / 10;

/**
 * One curve per transition. Transitions between the same two states (either way) fan out side by
 * side, so A→B and B→A, or two A→B transitions, never draw on top of each other.
 */
export function edgeGeometry(
  transitions: { key: string; name: string; from: string; to: string }[],
  pos: (state: string) => Position,
  size: { w: number; h: number },
): Edge[] {
  const pairs = new Map<string, string[]>();
  const pairOf = (a: string, b: string) => (a < b ? `${a}\u0000${b}` : `${b}\u0000${a}`);
  for (const t of transitions) {
    const k = pairOf(t.from, t.to);
    pairs.set(k, [...(pairs.get(k) ?? []), t.key]);
  }
  const center = (s: string) => ({ x: pos(s).x + size.w / 2, y: pos(s).y + size.h / 2 });
  return transitions.map((t) => {
    const group = pairs.get(pairOf(t.from, t.to))!;
    const i = group.indexOf(t.key);
    // The normal is taken in one direction for the pair (lower key to higher), so opposite arrows separate too.
    const [lo, hi] = t.from < t.to ? [t.from, t.to] : [t.to, t.from];
    const a = center(lo);
    const b = center(hi);
    const len = Math.hypot(b.x - a.x, b.y - a.y) || 1;
    const n = { x: -(b.y - a.y) / len, y: (b.x - a.x) / len };
    const offset = group.length === 1 ? 0 : (i - (group.length - 1) / 2) * SPREAD;
    const from = center(t.from);
    const to = center(t.to);
    const mid = { x: (from.x + to.x) / 2 + n.x * offset, y: (from.y + to.y) / 2 + n.y * offset };
    // The control point lies twice as far out as the curve's middle.
    const ctrl = { x: 2 * mid.x - (from.x + to.x) / 2, y: 2 * mid.y - (from.y + to.y) / 2 };
    const start = clipToBox(from, size, offset ? ctrl : to);
    const end = clipToBox(to, size, offset ? ctrl : from);
    const label = { x: r(0.25 * start.x + 0.5 * ctrl.x + 0.25 * end.x), y: r(0.25 * start.y + 0.5 * ctrl.y + 0.25 * end.y) };
    const path = `M ${r(start.x)} ${r(start.y)} Q ${r(ctrl.x)} ${r(ctrl.y)} ${r(end.x)} ${r(end.y)}`;
    return { key: t.key, name: t.name, from: t.from, to: t.to, path, label };
  });
}
