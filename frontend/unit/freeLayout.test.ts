import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { UiClassLayout } from "../src/api/uiSettings";
import { makeFree, settleFrames } from "../src/lib/freeLayout";
import { addTab, materialize, moveSectionToTab } from "../src/lib/layoutDesign";
import { CORE_FIELDS, normalizeDocument, resolveLayout } from "../src/lib/uiSettings";

/** A layout saved on the 12-column grid before every tab was free (SHAA-1471): no placement, no frames. */
function gridLayout(placement?: "grid"): UiClassLayout {
  return {
    classKey: "server",
    tabs: [
      {
        key: "main",
        label: "Main",
        ...(placement ? { placement } : {}),
        sections: [
          { key: "a", label: "A", columns: 3, width: 6, collapsed: false, fields: [{ field: "ident", width: 1 }] },
          { key: "b", label: "B", columns: 3, width: 6, minHeight: 3, collapsed: false, fields: [{ field: "criticality", width: 1 }] },
          { key: "c", label: "C", columns: 2, width: 12, collapsed: false, fields: [{ field: "validFrom", width: 1 }, { field: "validUntil", width: 1 }] },
          { key: "d", label: "D", width: 4, newRow: true, kind: "note", text: "Read me" },
        ],
      },
    ],
    hiddenFields: [],
    readOnlyFields: [],
  };
}

/** The frames as the API makes them of the same tab (backend document.rs grid_frames). */
const API_FRAMES = [
  { key: "a", frame: { x: 0, y: 0, w: 0.5, h: 96, z: 1 } },
  { key: "b", frame: { x: 0.5, y: 0, w: 0.5, h: 192, z: 2 } },
  { key: "c", frame: { x: 0, y: 208, w: 1, h: 96, z: 3 } },
  { key: "d", frame: { x: 0, y: 320, w: 0.3333, h: 144, z: 4 } },
];

describe("free placement only: layouts saved on the grid", () => {
  for (const placement of [undefined, "grid" as const]) {
    test(`a stored grid tab (${placement ?? "no placement"}) loads in the editor as windows where its sections were`, () => {
      const doc = normalizeDocument({ layouts: [gridLayout(placement)] });
      const tab = doc.layouts[0].tabs![0];
      assert.equal(tab.placement, "free");
      assert.deepEqual(
        tab.sections!.map((s) => ({ key: s.key, frame: s.frame })),
        API_FRAMES,
      );
      // Every field is still there.
      assert.deepEqual(
        tab.sections!.flatMap((s) => (s.fields ?? []).map((f) => f.field)),
        ["ident", "criticality", "validFrom", "validUntil"],
      );
    });

    test(`a stored grid tab (${placement ?? "no placement"}) shows on the page as windows with every field`, () => {
      const layout = gridLayout(placement);
      const before = JSON.stringify(layout);
      const [tab] = resolveLayout(layout, [], CORE_FIELDS);
      const placed = tab.sections.filter((s) => !s.auto);
      assert.deepEqual(
        placed.map((s) => ({ key: s.key, frame: s.frame })),
        API_FRAMES,
      );
      assert.deepEqual(
        placed.flatMap((s) => s.fields.map((f) => f.field)),
        ["ident", "criticality", "validFrom", "validUntil"],
      );
      assert.equal(placed.find((s) => s.key === "d")!.text, "Read me");
      // The layout itself is left as it is.
      assert.equal(JSON.stringify(layout), before);
    });
  }

  test("windows a tab already has stay; sections without a frame go below them, on top of the stack", () => {
    const tab = gridLayout().tabs![0];
    tab.placement = "free";
    tab.sections![0].frame = { x: 0.25, y: 40, w: 0.5, h: 300, z: 5 };
    makeFree(tab);
    assert.deepEqual(tab.sections![0].frame, { x: 0.25, y: 40, w: 0.5, h: 300, z: 5 });
    // b, c, d from their grid places, from 356 px (below 40 + 300, plus the gap) down.
    assert.deepEqual(
      tab.sections!.slice(1).map((s) => s.frame),
      [
        { x: 0, y: 356, w: 0.5, h: 192, z: 6 },
        { x: 0, y: 564, w: 1, h: 96, z: 7 },
        { x: 0, y: 676, w: 0.3333, h: 144, z: 8 },
      ],
    );
  });

  test("new tabs, the built-in layout made explicit and moved sections are windows", () => {
    const l = materialize("server", []);
    assert.ok(l.tabs!.every((t) => t.placement === "free" && t.sections!.every((s) => s.frame)));
    const t = addTab(l, "Hardware");
    settleFrames(l.tabs);
    assert.equal(t.placement, "free");
    assert.deepEqual(t.sections![0].frame, { x: 0, y: 0, w: 1, h: 96, z: 1 });
    // A section moved to another tab gets a window below the ones there.
    const general = l.tabs![0].sections![0];
    moveSectionToTab(l, general, t);
    settleFrames(l.tabs);
    assert.equal(general.frame!.y, 96 + 16);
    assert.equal(general.frame!.z, 2);
  });
});
