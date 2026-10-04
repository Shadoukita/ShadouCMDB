/**
 * Keyboard row navigation for an explorer table (design document §2.7, Explorer › Keyboard).
 * Bound to the table body's keydown: ↑/↓ move focus to the previous or next row's link to
 * its CI, so Enter opens it natively; `e` edits the focused row's CI and `c` opens the Columns
 * popover. The tab order is unchanged: Tab still walks every link and button. Nothing fires while
 * focus is in a text field or inside a row's open menu, or with a modifier key held.
 */
export interface RowKeyboardActions {
  /** `e`: edit the CI of the row (its `tr` carries `data-id`). The caller decides whether that CI may be edited. */
  edit?: (id: string) => void;
  /** `c`: open the Columns popover. */
  columns?: () => void;
}

/** A text field, select or editable region: a typed letter belongs to it. */
export function isTextEntry(el: Element | null): boolean {
  if (!el) return false;
  return ["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName) || !!(el as HTMLElement).isContentEditable;
}

/**
 * The element a row's keyboard focus lands on: the link to the row's own CI (`data-id`), else its
 * first link, else its first button. Other links in the row (a related CI in an attribute column)
 * are only fallbacks, so Enter opens the CI the row is about.
 */
export function rowFocusTarget(row: Element | null): HTMLElement | null {
  if (!row) return null;
  const links = [...row.querySelectorAll<HTMLAnchorElement>("a[href]")];
  const id = (row as HTMLElement).dataset.id;
  const own = id ? links.find((a) => new URL(a.href, "http://host.invalid").pathname.endsWith(`/cis/${id}`)) : undefined;
  return own ?? links[0] ?? row.querySelector<HTMLElement>("button:not(:disabled)");
}

export function onRowKeydown(e: KeyboardEvent, actions: RowKeyboardActions = {}): void {
  if (e.ctrlKey || e.metaKey || e.altKey || e.defaultPrevented) return;
  const target = e.target as Element | null;
  if (!target || isTextEntry(target) || target.closest("[role=menu]")) return;
  const row = target.closest("tbody > tr");
  if (!row) return;

  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    let next = e.key === "ArrowDown" ? row.nextElementSibling : row.previousElementSibling;
    while (next && !rowFocusTarget(next)) next = e.key === "ArrowDown" ? next.nextElementSibling : next.previousElementSibling;
    const to = rowFocusTarget(next);
    if (!to) return;
    e.preventDefault(); // no page scroll; focusing the row scrolls it into view
    to.focus();
  } else if (e.key === "e" && actions.edit) {
    const id = (row as HTMLElement).dataset.id;
    if (!id) return;
    e.preventDefault();
    actions.edit(id);
  } else if (e.key === "c" && actions.columns) {
    e.preventDefault();
    actions.columns();
  }
}
