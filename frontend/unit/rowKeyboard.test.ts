// Keyboard rows in the explorer tables (design §2.7): ↑/↓ move to the row's own CI link, `e` edits,
// `c` opens Columns, and nothing fires in a text field, an open row menu or with a modifier held.
// Node has no DOM, so rows and links are small stand-ins with the members the module uses.
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { onRowKeydown, rowFocusTarget } from "../src/lib/rowKeyboard";

interface Fake {
  tagName: string;
  focused?: boolean;
}
function link(href: string) {
  const el = { tagName: "A", href, focused: false, focus: () => (el.focused = true), closest: (s: string) => (s === "tbody > tr" ? row : null) };
  let row: unknown = null;
  return Object.assign(el, { attach: (r: unknown) => (row = r) });
}
function table(rows: { id: string; hrefs: string[] }[]) {
  const trs = rows.map((r) => {
    const links = r.hrefs.map(link);
    const tr = {
      tagName: "TR",
      dataset: { id: r.id },
      links,
      nextElementSibling: null as unknown,
      previousElementSibling: null as unknown,
      querySelectorAll: () => links,
      querySelector: () => null,
    };
    links.forEach((l) => l.attach(tr));
    return tr;
  });
  trs.forEach((tr, i) => {
    tr.nextElementSibling = trs[i + 1] ?? null;
    tr.previousElementSibling = trs[i - 1] ?? null;
  });
  return trs;
}
function key(k: string, target: unknown, mods: Partial<KeyboardEvent> = {}) {
  const e = { key: k, target, ctrlKey: false, metaKey: false, altKey: false, defaultPrevented: false, ...mods, prevented: false } as KeyboardEvent & {
    prevented: boolean;
  };
  (e as { preventDefault: () => void }).preventDefault = () => (e.prevented = true);
  return e;
}
const base = "http://cmdb.example";

describe("rowKeyboard", () => {
  test("focus lands on the link to the row's own CI, not a related CI in another column", () => {
    const [tr] = table([{ id: "b", hrefs: [`${base}/cis/x`, `${base}/cis/b`] }]);
    assert.equal(rowFocusTarget(tr as unknown as Element), tr.links[1]);
  });
  test("a row with no page of its own focuses its marked actions button, not the owner link", () => {
    // An API token row: the first link is the owner's user page; ⋯ carries data-row-focus.
    const [a, b] = table([
      { id: "a", hrefs: [`${base}/admin/users/u`] },
      { id: "b", hrefs: [`${base}/admin/users/u`] },
    ]);
    const menu = { tagName: "BUTTON", focused: false, focus: () => (menu.focused = true) };
    b.querySelector = ((s: string) => (s === "[data-row-focus]:not(:disabled)" ? menu : null)) as never;
    assert.equal(rowFocusTarget(b as unknown as Element), menu);
    onRowKeydown(key("ArrowDown", a.links[0]));
    assert.ok(menu.focused);
    assert.equal(b.links[0].focused, false);
  });
  test("↓ and ↑ move to the next and previous row and keep the page from scrolling", () => {
    const [a, b] = table([
      { id: "a", hrefs: [`${base}/cis/a`] },
      { id: "b", hrefs: [`${base}/cis/b`] },
    ]);
    const down = key("ArrowDown", a.links[0]);
    onRowKeydown(down);
    assert.ok(b.links[0].focused);
    assert.ok(down.prevented);
    onRowKeydown(key("ArrowUp", b.links[0]));
    assert.ok(a.links[0].focused);
  });
  test("↓ on the last row does nothing", () => {
    const [a] = table([{ id: "a", hrefs: [`${base}/cis/a`] }]);
    const e = key("ArrowDown", a.links[0]);
    onRowKeydown(e);
    assert.equal(e.prevented, false);
  });
  test("e edits the focused row's CI and c opens Columns", () => {
    const [a] = table([{ id: "a", hrefs: [`${base}/cis/a`] }]);
    const edited: string[] = [];
    let columns = 0;
    const actions = { edit: (id: string) => edited.push(id), columns: () => columns++ };
    onRowKeydown(key("e", a.links[0]), actions);
    onRowKeydown(key("c", a.links[0]), actions);
    assert.deepEqual(edited, ["a"]);
    assert.equal(columns, 1);
  });
  test("from a row's selection checkbox ↑/↓ move to the next row's checkbox and e edits its row", () => {
    const [a, b] = table([
      { id: "a", hrefs: [`${base}/cis/a`] },
      { id: "b", hrefs: [`${base}/cis/b`] },
    ]);
    const box = (row: unknown) => {
      const el = { tagName: "INPUT", type: "checkbox", focused: false, focus: () => (el.focused = true), closest: (s: string) => (s === "tbody > tr" ? row : null) };
      return el;
    };
    const boxA = box(a);
    const boxB = box(b);
    a.querySelector = ((s: string) => (s === "input[type=checkbox]:not(:disabled)" ? boxA : null)) as never;
    b.querySelector = ((s: string) => (s === "input[type=checkbox]:not(:disabled)" ? boxB : null)) as never;
    const down = key("ArrowDown", boxA);
    onRowKeydown(down);
    assert.ok(boxB.focused);
    assert.ok(down.prevented);
    onRowKeydown(key("ArrowUp", boxB));
    assert.ok(boxA.focused);
    const edited: string[] = [];
    onRowKeydown(key("e", boxB), { edit: (id) => edited.push(id) });
    assert.deepEqual(edited, ["b"]);
    // Space stays with the checkbox: the row handler leaves it alone.
    const space = key(" ", boxA);
    onRowKeydown(space);
    assert.equal(space.prevented, false);
  });
  test("nothing fires in a text field, inside a row menu or with a modifier key", () => {
    const [a, b] = table([
      { id: "a", hrefs: [`${base}/cis/a`] },
      { id: "b", hrefs: [`${base}/cis/b`] },
    ]);
    const edited: string[] = [];
    const input: Fake & { closest: () => unknown } = { tagName: "INPUT", closest: () => a };
    onRowKeydown(key("e", input), { edit: (id) => edited.push(id) });
    const search = { tagName: "INPUT", type: "search", closest: () => a };
    onRowKeydown(key("ArrowDown", search));
    const select = { tagName: "SELECT", closest: () => a };
    onRowKeydown(key("e", select), { edit: (id) => edited.push(id) });
    const menuItem = { tagName: "BUTTON", closest: (s: string) => (s === "[role=menu]" ? {} : a) };
    onRowKeydown(key("ArrowDown", menuItem));
    onRowKeydown(key("ArrowDown", a.links[0], { ctrlKey: true }));
    onRowKeydown(key("e", a.links[0], { metaKey: true }), { edit: (id) => edited.push(id) });
    assert.deepEqual(edited, []);
    assert.equal(b.links[0].focused, false);
  });
});
