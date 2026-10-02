import type { components } from "../api/schema";
import type { LayoutSection, LayoutTab } from "./layoutDesign";
import { sectionPlaces } from "./layoutDesign";
import { GRID_COLUMNS, SECTION_GRID, sectionKind } from "./uiSettings";

/**
 * Layout tabs are free (`placement: "free"`): every section is a window with a
 * `frame` (x and w as fractions of the tab's width, y and h in px, z the
 * stacking order) and windows may overlap. The rules here mirror the API's
 * (backend ui_settings/document.rs `grid_frames`, `UiLayoutTab::normalize`), so
 * the editor shows what a save stores: a tab stored on the earlier 12-column
 * grid becoming free, the frame limits, and the stacking order. Every function
 * that takes a tab changes it in place (it is part of the reactive draft).
 */

export type Frame = components["schemas"]["UiSectionFrame"];

/** The frame limits the API checks. */
export const FRAME_MIN_W = 0.05;
export const FRAME_MIN_H = 48;
export const FRAME_MAX_H = 4000;
export const FRAME_MAX_Y = 100_000;
/** Height estimates of the API's grid → free conversion. */
const HEADER_PX = 48;
const ROW_PX = 48;
export const FRAME_GAP_PX = 16;
const NOTE_PX = 144;
const PANEL_PX = 320;
/** Below this tab width (and in print) a free tab stacks its windows in reading order, as the grid does. */
export const STACK_BELOW_PX = 820;
/** The editor's fine guide grid, in px. */
export const GUIDE_PX = 8;
/** How close an edge has to come to another window's edge to snap to it, in px. */
export const SNAP_PX = 6;

const round4 = (v: number) => Math.round(v * 10_000) / 10_000;
const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

/** A section's least height: its `minH`, never under the title bar. */
export const minHeightOf = (f: Frame) => Math.max(FRAME_MIN_H, f.minH ?? FRAME_MIN_H);

/** A frame within the API's limits: at least 5% wide and inside the tab, 48–4000 px tall and not under its `minH`. */
export function clampFrame(f: Frame): Frame {
  const w = clamp(round4(f.w), FRAME_MIN_W, 1);
  const out: Frame = {
    x: round4(clamp(f.x, 0, 1 - w)),
    y: Math.round(clamp(f.y, 0, FRAME_MAX_Y)),
    w,
    h: Math.round(clamp(f.h, minHeightOf(f), FRAME_MAX_H)),
    z: f.z,
  };
  if (f.minH !== undefined) out.minH = f.minH;
  return out;
}

/** The API's estimate of a section's height when it leaves the grid (title bar plus field rows, a note, a panel). */
export function estimatedHeight(s: LayoutSection): number {
  const kind = sectionKind(s);
  if (kind === "note") return NOTE_PX;
  if (kind !== "fields") return PANEL_PX;
  const columns = s.columns ?? GRID_COLUMNS;
  let rows = 0;
  let col = columns;
  for (const f of s.fields ?? []) {
    const w = clamp(f.width ?? 1, 1, Math.max(columns, 1));
    if (col + w > columns) {
      rows += 1;
      col = 0;
    }
    col += w;
  }
  return clamp(HEADER_PX + Math.max(rows, s.minHeight ?? 1, 1) * ROW_PX, FRAME_MIN_H, FRAME_MAX_H);
}

/**
 * Frames for sections in grid order from `top` px down: x and w from their grid
 * columns (`width`, `newRow`), y and h the API's estimate by grid row. z is 1..n in order.
 */
export function gridFrames(sections: readonly LayoutSection[], top: number): Frame[] {
  const places = sectionPlaces(sections);
  let rowY = top;
  let rowH = 0;
  let row = 0;
  return sections.map((s, i) => {
    const p = places[i];
    if (p.row !== row) {
      rowY += rowH + FRAME_GAP_PX;
      rowH = 0;
      row = p.row;
    }
    const h = estimatedHeight(s);
    rowH = Math.max(rowH, h);
    return {
      x: round4(p.start / SECTION_GRID),
      y: clamp(rowY, 0, FRAME_MAX_Y),
      w: round4(p.width / SECTION_GRID),
      h,
      z: i + 1,
    };
  });
}

/** How far down the tab its windows reach: `max(y + h)`, 0 without any. */
export function tabHeight(tab: LayoutTab): number {
  return Math.max(0, ...(tab.sections ?? []).flatMap((s) => (s.frame ? [s.frame.y + s.frame.h] : [])));
}

/**
 * Makes a tab free as the API stores it: a tab from the earlier grid (or a
 * section without a frame) gets each section as a window where it was on the
 * grid, below the windows it already has and above them in the stack. The grid
 * settings (`width`, `newRow`) stay; the frames decide from then on.
 */
export function makeFree(tab: LayoutTab): void {
  const sections = tab.sections ?? [];
  const framed = sections.filter((s) => s.frame);
  const top = framed.length > 0 ? tabHeight(tab) + FRAME_GAP_PX : 0;
  const topZ = Math.max(0, ...framed.map((s) => s.frame!.z));
  const frames = gridFrames(sections.filter((s) => !s.frame), top);
  let i = 0;
  for (const s of sections) {
    if (s.frame) continue;
    const f = frames[i++];
    s.frame = { ...f, z: f.z + topZ };
  }
  tab.placement = "free";
}

/** A free copy of a tab (makeFree), leaving `tab` as it is: how the page shows a tab stored on the earlier grid. */
export function freeCopy<T extends LayoutTab>(tab: T): T {
  const copy = { ...tab, sections: (tab.sections ?? []).map((s) => ({ ...s })) };
  makeFree(copy);
  return copy;
}

/** The sections in reading order (y, then x), as the API stores a free tab and the page renders it. */
export function readingOrder<T extends { frame?: Frame }>(sections: readonly T[]): T[] {
  return sections
    .map((s, i) => ({ s, i }))
    .sort((a, b) => {
      const fa = a.s.frame;
      const fb = b.s.frame;
      if (fa && fb) return fa.y - fb.y || fa.x - fb.x || a.i - b.i;
      return fa ? -1 : fb ? 1 : a.i - b.i;
    })
    .map((x) => x.s);
}

/** The tab's windows from the bottom of the stack to the top (ties: the section order). */
export function stackOrder(tab: LayoutTab): LayoutSection[] {
  return (tab.sections ?? [])
    .filter((s) => s.frame)
    .map((s, i) => ({ s, i }))
    .sort((a, b) => a.s.frame!.z - b.s.frame!.z || a.i - b.i)
    .map((x) => x.s);
}

export type LayerMove = "front" | "forward" | "backward" | "back";
export const LAYER_MOVES: { move: LayerMove; label: string; keys: string }[] = [
  { move: "front", label: "Bring to front", keys: "Ctrl+Shift+PageUp" },
  { move: "forward", label: "Bring forward", keys: "Ctrl+PageUp" },
  { move: "backward", label: "Send backward", keys: "Ctrl+PageDown" },
  { move: "back", label: "Send to back", keys: "Ctrl+Shift+PageDown" },
];

/** Compact symbols of the layer moves, for toolbars (the buttons carry the labels as their names). */
export const LAYER_ICONS: Record<LayerMove, string> = { front: "⤒", forward: "↑", backward: "↓", back: "⤓" };

/** A window's place in the stack: 1 is the bottom, n the top. */
export function layerOf(tab: LayoutTab, section: LayoutSection): { index: number; count: number } {
  const stack = stackOrder(tab);
  return { index: stack.indexOf(section) + 1, count: stack.length };
}

/** Moves a window in its tab's stack; z becomes 1..n. Returns whether it moved. */
export function moveLayer(tab: LayoutTab, section: LayoutSection, move: LayerMove): boolean {
  const stack = stackOrder(tab);
  const i = stack.indexOf(section);
  if (i < 0) return false;
  const j = move === "front" ? stack.length - 1 : move === "back" ? 0 : move === "forward" ? Math.min(i + 1, stack.length - 1) : Math.max(i - 1, 0);
  stack.splice(i, 1);
  stack.splice(j, 0, section);
  stack.forEach((s, n) => (s.frame!.z = n + 1));
  return i !== j;
}

/**
 * A window for a section that comes into a free tab (added, or moved from
 * another tab): below the lowest window at the full width, on top of the stack.
 */
export function frameNew(tab: LayoutTab, section: LayoutSection): void {
  const others = (tab.sections ?? []).filter((s) => s !== section && s.frame);
  const bottom = tabHeight({ ...tab, sections: others });
  const top = Math.max(0, ...others.map((s) => s.frame!.z));
  section.frame = { x: 0, y: others.length > 0 ? bottom + FRAME_GAP_PX : 0, w: 1, h: estimatedHeight(section), z: top + 1 };
}

// ---------- Moving and resizing with snapping ----------

/** A frame in px of a tab `width` px wide. */
export interface Box {
  left: number;
  top: number;
  width: number;
  height: number;
}
export const toBox = (f: Frame, width: number): Box => ({ left: f.x * width, top: f.y, width: f.w * width, height: f.h });
export function fromBox(b: Box, width: number, f: Frame): Frame {
  return clampFrame({ ...f, x: b.left / width, y: b.top, w: b.width / width, h: b.height });
}

/** Which edges a drag changes: all four for a move, one or two for a resize handle. */
export interface Edges {
  n?: boolean;
  s?: boolean;
  e?: boolean;
  w?: boolean;
}
export const MOVE: Edges = { n: true, s: true, e: true, w: true };

/** What a snap lined up with: the x or y position of a guide line to show. */
export interface SnapLine {
  axis: "x" | "y";
  at: number;
}

/** The nearest target within SNAP_PX of any of `values`: the shift to apply, and the target. */
function nearest(values: readonly number[], targets: readonly number[]): { shift: number; at: number } | null {
  let best: { shift: number; at: number } | null = null;
  for (const v of values) {
    for (const t of targets) {
      const d = t - v;
      if (Math.abs(d) <= SNAP_PX && (!best || Math.abs(d) < Math.abs(best.shift))) best = { shift: d, at: t };
    }
  }
  return best;
}
const onGuide = (v: number) => Math.round(v / GUIDE_PX) * GUIDE_PX;

/**
 * The box a drag gives: `start` moved by (dx, dy) on the edges it changes,
 * within the tab (`width` px wide) and the minimum size. With `snap`, the moving
 * edges line up with the other windows' edges and the tab's edges when they come
 * within SNAP_PX (the lines it lined up with are returned, to show them), else
 * with the fine guide grid (GUIDE_PX).
 */
export function dragBox(
  start: Box,
  edges: Edges,
  dx: number,
  dy: number,
  opts: { width: number; minH: number; others: readonly Box[]; snap: boolean },
): { box: Box; lines: SnapLine[] } {
  const minW = FRAME_MIN_W * opts.width;
  const move = !!(edges.n && edges.s && edges.e && edges.w);
  let left = start.left + (edges.w ? dx : 0);
  let right = start.left + start.width + (edges.e ? dx : 0);
  let top = start.top + (edges.n ? dy : 0);
  let bottom = start.top + start.height + (edges.s ? dy : 0);
  const lines: SnapLine[] = [];
  if (opts.snap) {
    const xs = [0, opts.width, ...opts.others.flatMap((b) => [b.left, b.left + b.width])];
    const ys = [0, ...opts.others.flatMap((b) => [b.top, b.top + b.height])];
    /** The shift that lines up the first of `values` that comes close to a target, else the first one with the guide grid. */
    const shift = (axis: "x" | "y", values: number[], targets: number[]) => {
      const hit = nearest(values, targets);
      if (hit) lines.push({ axis, at: hit.at });
      return hit ? hit.shift : onGuide(values[0]) - values[0];
    };
    if (move) {
      const sx = shift("x", [left, right], xs);
      const sy = shift("y", [top, bottom], ys);
      left += sx;
      right += sx;
      top += sy;
      bottom += sy;
    } else {
      // A resize snaps each moving edge on its own.
      if (edges.w) left += shift("x", [left], xs);
      if (edges.e) right += shift("x", [right], xs);
      if (edges.n) top += shift("y", [top], ys);
      if (edges.s) bottom += shift("y", [bottom], ys);
    }
  }
  if (move) {
    const w = right - left;
    left = clamp(left, 0, Math.max(0, opts.width - w));
    top = clamp(top, 0, FRAME_MAX_Y);
    return { box: { left, top, width: w, height: bottom - top }, lines };
  }
  // A resize keeps the opposite edges where they are, within the tab and the minimum size.
  if (edges.w) left = clamp(left, 0, right - minW);
  if (edges.e) right = clamp(right, left + minW, opts.width);
  if (edges.n) top = clamp(top, Math.max(0, bottom - FRAME_MAX_H), bottom - opts.minH);
  if (edges.s) bottom = clamp(bottom, top + opts.minH, top + FRAME_MAX_H);
  return { box: { left, top, width: right - left, height: bottom - top }, lines };
}

/** "240, 96 · 480 × 320 px": a window's position and size in px, for the live readout and screen readers. */
export function describeBox(b: Box): string {
  return `${Math.round(b.left)}, ${Math.round(b.top)} · ${Math.round(b.width)} × ${Math.round(b.height)} px`;
}


/**
 * Keeps every section of a layout framed: a section without a frame (added, or
 * moved from another tab) gets one below the windows of its tab (frameNew), and
 * every tab is free.
 */
export function settleFrames(tabs: readonly LayoutTab[] | undefined): void {
  for (const t of tabs ?? []) {
    for (const s of t.sections ?? []) if (!s.frame) frameNew(t, s);
    t.placement = "free";
  }
}
