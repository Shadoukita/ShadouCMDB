/**
 * Resizing by dragging an edge on a CSS grid (the form designer and the in-page
 * layout editor): a field on its section's grid, a section on its tab's grid of
 * 12 columns. The size snaps to whole grid columns while the pointer moves;
 * `onEnd` runs once the pointer is released (one undo step per drag).
 */

interface GridMetrics {
  left: number;
  tracks: number;
  track: number;
  gap: number;
}

function measure(grid: HTMLElement, columns: number): GridMetrics {
  const style = getComputedStyle(grid);
  const tracks = style.gridTemplateColumns.split(" ").filter(Boolean).length || columns;
  const gap = parseFloat(style.columnGap) || 0;
  const rect = grid.getBoundingClientRect();
  const padLeft = parseFloat(style.paddingLeft) || 0;
  const padRight = parseFloat(style.paddingRight) || 0;
  const inner = rect.width - padLeft - padRight;
  return { left: rect.left + padLeft, tracks, gap, track: (inner - gap * (tracks - 1)) / tracks };
}

/** Follows the pointer from `e` (on the handle) until it is released. */
function follow(e: PointerEvent, onMove: (ev: PointerEvent) => void, onEnd?: () => void) {
  e.preventDefault();
  e.stopPropagation();
  const handle = e.currentTarget as HTMLElement;
  handle.setPointerCapture(e.pointerId);
  const onUp = () => {
    handle.removeEventListener("pointermove", onMove);
    handle.removeEventListener("pointerup", onUp);
    handle.removeEventListener("pointercancel", onUp);
    onEnd?.();
  };
  handle.addEventListener("pointermove", onMove);
  handle.addEventListener("pointerup", onUp);
  handle.addEventListener("pointercancel", onUp);
}

/**
 * Resizes a field by its right edge. The new width is the number of grid
 * columns the pointer reaches from the field's left edge; `onWidth` gets every
 * change while the pointer moves.
 */
export function startGridResize(e: PointerEvent, cell: HTMLElement | undefined, columns: number, width: () => number, onWidth: (w: number) => void, onEnd?: () => void) {
  const grid = cell?.parentElement;
  if (!cell || !grid) return;
  const m = measure(grid, columns);
  const left = cell.getBoundingClientRect().left;
  follow(
    e,
    (ev) => {
      const w = Math.round((ev.clientX - left + m.gap) / (m.track + m.gap));
      const next = Math.max(1, Math.min(w, m.tracks, columns));
      if (next !== width()) onWidth(next);
    },
    onEnd,
  );
}

/**
 * Drags an edge of a section on its tab's grid: `onLine` gets the grid line
 * nearest to the pointer (0 at the left edge of the grid, `columns` at its right
 * edge) whenever it changes.
 */
export function startLineDrag(e: PointerEvent, grid: HTMLElement | null | undefined, columns: number, onLine: (line: number) => void, onEnd?: () => void) {
  if (!grid) return;
  const m = measure(grid, columns);
  let last = -1;
  follow(
    e,
    (ev) => {
      const line = Math.max(0, Math.min(Math.round((ev.clientX - m.left + m.gap / 2) / (m.track + m.gap)), m.tracks));
      if (line === last) return;
      last = line;
      onLine(line);
    },
    onEnd,
  );
}
