/**
 * Keyboard row navigation for an explorer table (design document §2.7, Explorer › Keyboard).
 * Bound to the table body's keydown: ↑/↓ move focus to the previous or next row's link to
 * its CI, so Enter opens it natively; `e` edits the focused row's CI and `c` opens the Columns
 * popover. The tab order is unchanged: Tab still walks every link and button. Nothing fires while
 * focus is in a text field or inside a row's open menu, or with a modifier key held. A row's selection
 * checkbox is not a text field: from it ↑/↓ move to the next row's checkbox, so Space ticks a run of
 * rows, and `e` edits its row.
 */
export interface RowKeyboardActions {
  /** `e`: edit the CI of the row (its `tr` carries `data-id`). The caller decides whether that CI may be edited. */
  edit?: (id: string) => void;
  /** `c`: open the Columns popover. */
  columns?: () => void;
}

/** A checkbox: Space toggles it and it ignores letters and arrows, so row keys still apply. */
function isCheckbox(el: Element): boolean {
  return el.tagName === "INPUT" && (el as HTMLInputElement).type === "checkbox";
}

/** A text field, select or editable region: a typed letter belongs to it. */
export function isTextEntry(el: Element | null): boolean {
  if (!el || isCheckbox(el)) return false;
  return ["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName) || !!(el as HTMLElement).isContentEditable;
}

/**
 * The element a row's keyboard focus lands on: an element marked `data-row-focus` (a row with no page
 * of its own, such as an API token, marks its actions menu button), else the link to the row's own CI
 * (`data-id`), else its first link, else its first button. Other links in the row (a related CI in an
 * attribute column, a token's owner) are only fallbacks, so Enter acts on what the row is about.
 */
export function rowFocusTarget(row: Element | null): HTMLElement | null {
  if (!row) return null;
  const marked = row.querySelector<HTMLElement>("[data-row-focus]:not(:disabled)");
  if (marked) return marked;
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
    const box = isCheckbox(target) ? next?.querySelector<HTMLElement>("input[type=checkbox]:not(:disabled)") : null;
    const to = box ?? rowFocusTarget(next);
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
