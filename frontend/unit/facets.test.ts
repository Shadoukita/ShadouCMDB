// The inventory's facet panel (SHAA-1670 rollout 5f): ticking values in the URL, the shown values
// and the stored preference. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { FACET_PREVIEW, isFacetParam, readFacetPref, toggleId, visibleValues, writeFacetPref } from "../src/lib/facets";

describe("toggleId", () => {
  test("adds and removes an id in a comma-separated parameter", () => {
    assert.equal(toggleId("", "a", true), "a");
    assert.equal(toggleId("a", "b", true), "a,b");
    assert.equal(toggleId("a,b", "a", false), "b");
    assert.equal(toggleId("a", "a", false), undefined);
  });
  test("never duplicates an id that is already ticked", () => {
    assert.equal(toggleId("a,b", "a", true), "b,a");
  });
});

describe("isFacetParam", () => {
  test("knows the parameters the inventory URL holds", () => {
    assert.ok(isFacetParam("classId"));
    assert.ok(isFacetParam("lookupValueId"));
    assert.ok(isFacetParam("criticalityValueId"));
    assert.ok(!isFacetParam("businessServiceId"));
  });
});

describe("visibleValues", () => {
  const values = Array.from({ length: 12 }, (_, i) => ({ id: `v${i}`, selected: i === 10 }));
  test("shows the preview and any selected value past it", () => {
    const shown = visibleValues(values, false);
    assert.equal(shown.length, FACET_PREVIEW + 1);
    assert.equal(shown.at(-1)?.id, "v10");
  });
  test("shows every value when expanded", () => {
    assert.equal(visibleValues(values, true).length, 12);
  });
});

describe("facet preference", () => {
  test("round-trips through storage", () => {
    const store = new Map<string, string>();
    const storage = { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => void store.set(k, v) };
    assert.deepEqual(readFacetPref(storage), { collapsed: [] });
    writeFacetPref({ open: false, collapsed: ["class"] }, storage);
    assert.deepEqual(readFacetPref(storage), { open: false, collapsed: ["class"] });
  });
  test("ignores a malformed value", () => {
    assert.deepEqual(readFacetPref({ getItem: () => "{not json" }), { collapsed: [] });
    assert.deepEqual(readFacetPref({ getItem: () => JSON.stringify({ open: "yes", collapsed: [1, "x"] }) }), { open: undefined, collapsed: ["x"] });
  });
});
