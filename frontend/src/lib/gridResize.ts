/**
 * Resizing a field on a section's grid by dragging its right edge (the form
 * designer and the in-page layout editor). The new width is the number of grid
 * columns the pointer reaches from the field's left edge; `onWidth` gets every
 * change while the pointer moves.
 */
export function startGridResize(e: PointerEvent, cell: HTMLElement | undefined, columns: number, width: () => number, onWidth: (w: number) => void) {
  const grid = cell?.parentElement;
  if (!cell || !grid) return;
  e.preventDefault();
  e.stopPropagation();
  const handle = e.currentTarget as HTMLElement;
  handle.setPointerCapture(e.pointerId);
  const style = getComputedStyle(grid);
  const tracks = style.gridTemplateColumns.split(" ").filter(Boolean).length || columns;
  const gap = parseFloat(style.columnGap) || 0;
  const track = (grid.clientWidth - gap * (tracks - 1)) / tracks;
  const left = cell.getBoundingClientRect().left;
  const onMove = (ev: PointerEvent) => {
    const w = Math.round((ev.clientX - left + gap) / (track + gap));
    const next = Math.max(1, Math.min(w, tracks, columns));
    if (next !== width()) onWidth(next);
  };
  const onUp = () => {
    handle.removeEventListener("pointermove", onMove);
    handle.removeEventListener("pointerup", onUp);
    handle.removeEventListener("pointercancel", onUp);
  };
  handle.addEventListener("pointermove", onMove);
  handle.addEventListener("pointerup", onUp);
  handle.addEventListener("pointercancel", onUp);
}
