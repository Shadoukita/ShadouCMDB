import assert from "node:assert/strict";
import { describe, test } from "node:test";
import {
  BUILT_IN_BASELINE,
  clearedQuery,
  effectiveColumns,
  hasUrlState,
  isUsableColumn,
  parseColumns,
  patchQuery,
  resolveBaseline,
  toggleColumn,
} from "../src/lib/inventoryQuery";
import { DEFAULT_COLUMNS } from "../src/lib/uiSettings";

const all = () => true;

describe("precedence (URL, then the list view, then the built-in defaults)", () => {
  test("a URL with any state parameter is shown as it is", () => {
    assert.equal(hasUrlState({}, "inventory"), false);
    assert.equal(hasUrlState({ offset: "50" }, "inventory"), false);
    assert.equal(hasUrlState({ columns: "label" }, "inventory"), true);
    assert.equal(hasUrlState({ sort: "-label" }, "search"), false, "search has no sort");
    assert.equal(hasUrlState({ classId: "c1" }, "search"), true);
  });

  test("the list view is the baseline; without one the built-in defaults apply", () => {
    assert.deepEqual(resolveBaseline(undefined), BUILT_IN_BASELINE);
    const b = resolveBaseline({
      classKey: "server",
      columns: ["ident", "attributes.os"],
      defaultSort: { field: "attributes.os", direction: "desc" },
      pageSize: 25,
      defaultFilters: { q: null, lookups: {} },
    });
    assert.deepEqual(b, { source: "listView", columns: ["label", "ident", "attributes.os"], sort: "-attributes.os", pageSize: 25 });
  });

  test("a list view without columns shows the default columns", () => {
    const b = resolveBaseline({ classKey: "server", columns: [], defaultSort: null, pageSize: null, defaultFilters: { q: null, lookups: {} } });
    assert.deepEqual(b.columns, DEFAULT_COLUMNS);
    assert.equal(b.sort, undefined);
  });
});

describe("columns", () => {
  test("the URL's columns win over the baseline's; label is always first", () => {
    const baseline = resolveBaseline(undefined);
    assert.deepEqual(effectiveColumns(parseColumns("ident,attributes.os"), baseline, all), ["label", "ident", "attributes.os"]);
    assert.deepEqual(effectiveColumns([], baseline, all), DEFAULT_COLUMNS);
  });

  test("malformed and repeated names in the URL are dropped", () => {
    assert.deepEqual(parseColumns("label,,ident,ident,bogus,attributes.OS,attributes.os,class"), ["label", "ident", "attributes.os", "class"]);
    assert.deepEqual(parseColumns(""), []);
    assert.equal(parseColumns(Array.from({ length: 80 }, (_, i) => `attributes.a${i}`).join(",")).length, 50);
  });

  test("attribute columns need exactly one class that has the attribute", () => {
    assert.equal(isUsableColumn("ident", false, null), true);
    assert.equal(isUsableColumn("attributes.os", false, null), false);
    assert.equal(isUsableColumn("attributes.os", true, null), true, "kept while the attributes load");
    assert.equal(isUsableColumn("attributes.os", true, new Set(["os"])), true);
    assert.equal(isUsableColumn("attributes.gone", true, new Set(["os"])), false);
    // A URL whose only columns are unusable falls back to the baseline, never to an empty table.
    const usable = (f: string) => isUsableColumn(f, false, null);
    assert.deepEqual(effectiveColumns(["attributes.os"], BUILT_IN_BASELINE, usable), DEFAULT_COLUMNS);
  });

  test("adding a column to the default columns keeps them (GH#168)", () => {
    const shown = effectiveColumns([], BUILT_IN_BASELINE, all);
    assert.deepEqual(toggleColumn(shown, "attributes.hostname"), [...DEFAULT_COLUMNS, "attributes.hostname"]);
    // The same from a list view's columns.
    const listView = resolveBaseline({ classKey: "server", columns: ["ident", "attributes.os"], defaultSort: null, pageSize: null, defaultFilters: { q: null, lookups: {} } });
    assert.deepEqual(toggleColumn(effectiveColumns([], listView, all), "createdAt"), ["label", "ident", "attributes.os", "createdAt"]);
  });

  test("unticking removes a column; label cannot be removed", () => {
    assert.deepEqual(toggleColumn(DEFAULT_COLUMNS, "class"), ["label", "ident", "active", "updatedAt"]);
    assert.deepEqual(toggleColumn(DEFAULT_COLUMNS, "label"), DEFAULT_COLUMNS);
  });
});

describe("writing the URL", () => {
  test("a patch sets and removes parameters and goes back to the first page", () => {
    assert.deepEqual(patchQuery({ q: "a", offset: "50" }, { q: undefined, deleted: "include" }), { deleted: "include" });
    assert.deepEqual(patchQuery({ offset: "50" }, { limit: "100" }, false), { offset: "50", limit: "100" });
  });

  test("another class drops the attribute sort and the attribute columns, keeping the rest", () => {
    const q = { classId: "c1", sort: "-attributes.os", columns: "label,attributes.os,updatedAt" };
    assert.deepEqual(patchQuery(q, { classId: "c2" }), { classId: "c2", columns: "label,updatedAt" });
    assert.deepEqual(patchQuery({ classId: "c1", sort: "ident", columns: "attributes.os" }, { classId: undefined }), { sort: "ident" });
    // Other changes leave them alone.
    assert.deepEqual(patchQuery(q, { deleted: "only" }), { ...q, deleted: "only" });
  });

  test("clearing the filters keeps the sort, page size and built-in columns", () => {
    const q = { q: "web", classId: "c1", active: "all", sort: "-updatedAt", limit: "100", columns: "label,attributes.os,ident", offset: "100" };
    assert.deepEqual(clearedQuery(q), { sort: "-updatedAt", limit: "100", columns: "label,ident" });
    assert.deepEqual(clearedQuery({ ...q, sort: "attributes.os" }), { limit: "100", columns: "label,ident" });
    assert.deepEqual(clearedQuery({ q: "web", classId: "c1" }, ["q"]), { q: "web" });
  });

  test("clearing the filters keeps the saved view named, unless view=<id> would be left alone", () => {
    assert.deepEqual(clearedQuery({ view: "v1", classId: "c1", sort: "label", limit: "50" }), { view: "v1", sort: "label", limit: "50" });
    assert.deepEqual(clearedQuery({ view: "v1", q: "web", classId: "c1", limit: "50" }, ["q"], "search"), { view: "v1", q: "web", limit: "50" });
    assert.deepEqual(clearedQuery({ view: "v1", classId: "c1" }), {}, "view=<id> alone would apply the view again");
  });
});
