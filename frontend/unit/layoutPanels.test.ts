import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { UiClassLayout } from "../src/api/uiSettings";
import { estimatedHeight } from "../src/lib/freeLayout";
import {
  addPanel,
  addSeparator,
  canRemoveTab,
  materialize,
  moveFieldBy,
  moveSeparatorBy,
  placeSeparator,
  removalSummary,
  removeSection,
  removeSeparator,
  setColumns,
  setSeparatorLabel,
} from "../src/lib/layoutDesign";
import { builtInLayout, DETAIL_CORE, DETAIL_RECORD, isSeparator, normalizeLayout, placedPanels, resolveLayout } from "../src/lib/uiSettings";

/** A layout with tabs: "Main" with two field sections, "More" with one (SHAA-1645). */
function layout(): UiClassLayout {
  return {
    classKey: "server",
    tabs: [
      {
        key: "main",
        label: "Main",
        placement: "free",
        sections: [
          { key: "a", label: "A", columns: 3, width: 12, collapsed: false, fields: [{ field: "ident", width: 1 }, { field: "validFrom", width: 1 }] },
          { key: "b", label: "B", columns: 2, width: 12, collapsed: false, fields: [{ field: "validUntil", width: 1 }] },
        ],
      },
      { key: "more", label: "More", placement: "free", sections: [{ key: "c", label: "C", columns: 3, width: 12, collapsed: false, fields: [{ field: "criticality", width: 1 }] }] },
    ],
    hiddenFields: [],
    readOnlyFields: [],
  };
}
const sectionsOf = (l: UiClassLayout) => resolveLayout(l, [], DETAIL_CORE, DETAIL_RECORD).flatMap((t) => t.sections);
const fieldsOf = (l: UiClassLayout, key: string) => l.tabs!.flatMap((t) => t.sections!).find((s) => s.key === key)!.fields!;

describe("record and relations panels", () => {
  test("without tabs the built-in arrangement ends with the record details", () => {
    const last = sectionsOf(builtInLayout("server")).at(-1)!;
    assert.equal(last.kind, "record");
    assert.deepEqual(last.fields.map((f) => f.field), DETAIL_RECORD);
  });

  test("a layout with tabs shows the record details only where it places them", () => {
    const l = layout();
    assert.ok(!sectionsOf(l).some((s) => s.kind === "record"));
    addPanel(l, l.tabs![1], "record");
    const tabs = resolveLayout(l, [], DETAIL_CORE, DETAIL_RECORD);
    const record = tabs[1].sections.find((s) => s.kind === "record")!;
    assert.deepEqual(record.fields.map((f) => f.field), DETAIL_RECORD);
    assert.ok(!tabs[0].sections.some((s) => s.kind === "record"));
  });

  test("the record details leave out what a field section places, wherever that is", () => {
    const l = layout();
    addPanel(l, l.tabs![0], "record", 0);
    fieldsOf(l, "c").push({ field: "createdAt", width: 1 });
    const record = sectionsOf(l).find((s) => s.kind === "record")!;
    assert.ok(!record.fields.some((f) => f.field === "createdAt"));
    assert.ok(record.fields.some((f) => f.field === "updatedAt"));
  });

  test("the record panel is placed once, and removing it says it is gone from the page", () => {
    const l = layout();
    assert.ok(addPanel(l, l.tabs![0], "record"));
    assert.equal(addPanel(l, l.tabs![1], "record"), undefined);
    const record = l.tabs![0].sections!.find((s) => s.kind === "record")!;
    assert.match(removalSummary(l, { section: record }), /no longer shown on the detail page/);
    assert.ok(removeSection(l, record));
    assert.ok(!placedPanels(l).has("record"));
  });

  test("the built-in layout made explicit has the record details and the relationships at the end of the first tab", () => {
    const l = materialize("server", []);
    const kinds = l.tabs![0].sections!.map((s) => s.kind ?? "fields");
    assert.deepEqual(kinds.slice(-2), ["record", "relations"]);
    assert.ok(l.tabs![0].sections!.every((s) => s.frame));
    // The record window is the API's estimate for it.
    assert.equal(l.tabs![0].sections!.at(-2)!.frame!.h, 144);
  });
});

describe("separators in field sections", () => {
  test("are shown in order between the fields, and kept by normalizeLayout", () => {
    const l = layout();
    fieldsOf(l, "a").splice(1, 0, { separator: true, label: "Lifecycle", width: 3 });
    const a = sectionsOf(normalizeLayout(l)).find((s) => s.key === "a")!;
    assert.deepEqual(a.fields.map((f) => f.field), ["ident", "validFrom"]);
    assert.deepEqual(
      a.items.map((i) => (isSeparator(i) ? `-${i.label}-` : i.field)),
      ["ident", "-Lifecycle-", "validFrom"],
    );
    assert.deepEqual(normalizeLayout(l).tabs![0].sections![0].fields![1], { separator: true, label: "Lifecycle", width: 3 });
  });

  test("a section holding only separators is not shown", () => {
    const l = layout();
    l.tabs![1].sections![0].fields = [{ separator: true, width: 3 }];
    assert.ok(!sectionsOf(l).some((s) => s.key === "c"));
  });

  test("are added, labelled, moved within and across sections, and removed", () => {
    const l = layout();
    const at = addSeparator(l, "a", "  Dates  ", 1)!;
    assert.deepEqual(at, { section: "a", index: 1 });
    assert.deepEqual(fieldsOf(l, "a")[1], { separator: true, label: "Dates", width: 3 });

    setSeparatorLabel(l, at, "   ");
    assert.deepEqual(fieldsOf(l, "a")[1], { separator: true, width: 3 });
    setSeparatorLabel(l, at, "Validity");

    assert.deepEqual(moveSeparatorBy(l, at, -1), { section: "a", index: 0 });
    assert.deepEqual(fieldsOf(l, "a").map((f) => f.field ?? "|"), ["|", "ident", "validFrom"]);
    assert.equal(moveSeparatorBy(l, { section: "a", index: 0 }, -1), undefined);

    // Past the last field it goes to the start of the next field section, at that section's width.
    let to = { section: "a", index: 0 };
    to = moveSeparatorBy(l, to, 1)!;
    to = moveSeparatorBy(l, to, 1)!;
    assert.deepEqual(to, { section: "a", index: 2 });
    to = moveSeparatorBy(l, to, 1)!;
    assert.deepEqual(to, { section: "b", index: 0 });
    assert.deepEqual(fieldsOf(l, "b")[0], { separator: true, label: "Validity", width: 2 });

    // Dropped elsewhere (the drag), then a field moves past it.
    to = placeSeparator(l, to, "c", 1)!;
    assert.deepEqual(to, { section: "c", index: 1 });
    moveFieldBy(l, "criticality", 1);
    assert.deepEqual(fieldsOf(l, "c").map((f) => f.field ?? "|"), ["|", "criticality"]);

    assert.equal(removeSeparator(l, { section: "c", index: 1 }), false, "not a separator");
    assert.ok(removeSeparator(l, { section: "c", index: 0 }));
    assert.deepEqual(fieldsOf(l, "c"), [{ field: "criticality", width: 1 }]);
  });

  test("keep the full width of their section", () => {
    const l = layout();
    addSeparator(l, "a");
    setColumns(l.tabs![0].sections![0], 6);
    assert.equal(fieldsOf(l, "a").at(-1)!.width, 6);
    // Removing the section moves it into the one before or after it, as wide as that one.
    removeSection(l, l.tabs![0].sections![0]);
    assert.deepEqual(fieldsOf(l, "b").at(-1), { separator: true, width: 2 });
    // A separator takes a row of its own in the height estimate: validUntil and ident, validFrom, the line.
    assert.equal(estimatedHeight(l.tabs![0].sections![0]), 48 + 3 * 48);
  });

  test("do not keep a tab whose fields have nowhere to go from being removed", () => {
    const l = layout();
    l.tabs![1].sections![0].fields = [{ separator: true, width: 3 }];
    l.tabs![0].sections = [];
    assert.ok(canRemoveTab(l, l.tabs![1]));
  });
});
