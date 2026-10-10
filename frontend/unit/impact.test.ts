// Unit tests for the impact analysis tab's URL state, paths, groups and tree (SHAA-886). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { afterEach, describe, test } from "node:test";
import { setLocaleForTests } from "../src/i18n/index";
import { visibleRows } from "../src/lib/graphTree";
import {
  DEFAULT_STATE,
  groupItems,
  impactParams,
  impactQuery,
  impactTree,
  parseImpactQuery,
  pathTo,
  STATE_LABELS,
  viaFor,
  type ImpactAnalysis,
  type ImpactItem,
  type ImpactVia,
} from "../src/lib/impact";

const TYPE = { id: "t", key: "runs_on", name: "Runs on", forwardLabel: "runs on", reverseLabel: "hosts" };
const via = (parentId: string, source: string, target: string): ImpactVia => ({
  parentId,
  relationshipId: `${source}-${target}`,
  relationshipType: TYPE,
  edgeSourceId: source,
  edgeTargetId: target,
});
function item(id: string, hops: number, v: ImpactVia, extra: Partial<ImpactItem> = {}): ImpactItem {
  return {
    id,
    ident: id.toUpperCase(),
    name: id,
    classId: "c-app",
    className: "Application",
    criticality: null,
    active: true,
    status: null,
    directions: ["downstream"],
    hops,
    via: v,
    upstreamVia: null,
    downstreamVia: null,
    reachedByCount: 1,
    ...extra,
  };
}
const ROOT = { id: "srv", ident: "SRV", name: "srv", classId: "c-srv", className: "Server", criticality: null, active: true };
const analysis = (items: ImpactItem[], direction: "downstream" | "upstream" | "both" = "downstream") =>
  ({ root: ROOT, items, parameters: { direction, depth: 3, relationshipTypeIds: [], includeInactive: true, maxNodes: 500 } }) as Pick<
    ImpactAnalysis,
    "root" | "items" | "parameters"
  >;

describe("URL state", () => {
  test("a business service's tab defaults to Upstream: a plain link means Upstream, a named direction wins", () => {
    const upstream = { ...DEFAULT_STATE, direction: "upstream" as const };
    assert.deepEqual(parseImpactQuery({}, 10, upstream).state, upstream);
    assert.deepEqual(impactQuery(upstream, upstream), {});
    assert.deepEqual(impactQuery(DEFAULT_STATE, upstream), { direction: "downstream" });
    assert.equal(parseImpactQuery({ direction: "downstream" }, 10, upstream).state.direction, "downstream");
  });
  test("a plain link is the defaults, and the defaults write no query", () => {
    assert.deepEqual(parseImpactQuery({}), { state: DEFAULT_STATE, invalid: [] });
    assert.deepEqual(impactQuery(DEFAULT_STATE), {});
  });

  test("round trip: what the controls set comes back from the URL", () => {
    const s = { ...DEFAULT_STATE, direction: "both" as const, depth: 5, types: ["0b7c4a3e-1d2f-4c5b-9a8e-7f6d5c4b3a21"], includeInactive: false, view: "tree" as const, group: "criticality" as const, sort: "-hops" };
    assert.deepEqual(parseImpactQuery(impactQuery(s), 10), { state: s, invalid: [] });
  });

  test("unusable parameters fall back to their defaults and are named", () => {
    const { state, invalid } = parseImpactQuery({ direction: "sideways", depth: "0", types: "not-a-uuid", inactive: "yes", view: "graph", group: "owner", sort: "id" }, 10);
    assert.deepEqual(state, DEFAULT_STATE);
    assert.deepEqual(invalid, ["direction", "depth", "types", "includeInactive", "view", "group", "sort"]);
    // Every name is a state key, so the reset notice can label it.
    for (const k of invalid) assert.ok(STATE_LABELS[k as keyof typeof STATE_LABELS], k);
  });

  test("no relationship type chosen is distinct from all of them", () => {
    const none = { ...DEFAULT_STATE, types: [] };
    assert.deepEqual(impactQuery(none), { types: "none" });
    assert.deepEqual(parseImpactQuery(impactQuery(none)), { state: none, invalid: [] });
    assert.equal(DEFAULT_STATE.types, null);
    assert.equal("relationshipTypeId" in impactParams(DEFAULT_STATE), false);
  });

  test("a depth beyond the server's limit is refused, never lowered silently", () => {
    assert.deepEqual(parseImpactQuery({ depth: "12" }, 10).invalid, ["depth"]);
    // Before the limit is known the depth is kept; the tab judges it once the settings load.
    assert.equal(parseImpactQuery({ depth: "12" }).state.depth, 12);
  });
});

describe("paths and via", () => {
  // srv ← app-a runs on srv; app-b runs on app-a.
  const a = item("app-a", 1, via("srv", "app-a", "srv"));
  const b = item("app-b", 2, via("app-a", "app-b", "app-a"));
  const byId = new Map([a, b].map((i) => [i.id, i]));

  test("the path runs from the root to the CI, each edge read from the step before it", () => {
    assert.deepEqual(pathTo(b, { id: "srv", name: "srv" }, byId), [
      { id: "srv", name: "srv" },
      { id: "app-a", name: "app-a", label: "hosts" },
      { id: "app-b", name: "app-b", label: "hosts" },
    ]);
  });

  test("a broken chain stops instead of looping", () => {
    const loop = item("x", 2, via("y", "x", "y"));
    const y = item("y", 1, via("x", "y", "x"));
    const steps = pathTo(loop, { id: "srv", name: "srv" }, new Map([loop, y].map((i) => [i.id, i])));
    assert.deepEqual(steps.map((s) => s.id), ["y", "x"]);
  });

  test("in both mode, each walk has its own last hop", () => {
    const down = via("srv", "app", "srv");
    const up = via("db", "db", "app");
    const both = item("app", 1, down, { directions: ["downstream", "upstream"], upstreamVia: up });
    assert.equal(viaFor(both, "downstream"), down);
    assert.equal(viaFor(both, "upstream"), up);
    const one = item("only", 1, down);
    assert.equal(viaFor(one, "upstream"), null);
  });
});

describe("list groups", () => {
  const crit = (key: string, label: string, rank: number) => ({ key, label, rank });
  const items = [
    item("b", 2, via("a", "b", "a"), { criticality: crit("high", "High", 2) }),
    item("a", 1, via("srv", "a", "srv"), { criticality: crit("critical", "Critical", 1) }),
    item("c", 1, via("srv", "c", "srv"), { className: "Database", classId: "c-db" }),
  ];
  const viaName = () => "";

  test("by criticality: most critical first, Not set last", () => {
    assert.deepEqual(
      groupItems(items, "criticality", "hops", viaName).map((g) => [g.label, g.items.map((i) => i.id)]),
      [["Critical", ["a"]], ["High", ["b"]], ["Not set", ["c"]]],
    );
  });

  test("by class, alphabetically; by hops, nearest first; rows sorted within", () => {
    assert.deepEqual(groupItems(items, "class", "name", viaName).map((g) => [g.label, g.items.map((i) => i.id)]), [["Application", ["a", "b"]], ["Database", ["c"]]]);
    assert.deepEqual(groupItems(items, "hops", "-name", viaName).map((g) => [g.label, g.items.map((i) => i.id)]), [["1 hop", ["c", "a"]], ["2 hops", ["b"]]]);
  });

  test("sorting by criticality keeps unset values last in both directions", () => {
    assert.deepEqual(groupItems(items, "none", "criticality", viaName)[0].items.map((i) => i.id), ["a", "b", "c"]);
    assert.deepEqual(groupItems(items, "none", "-criticality", viaName)[0].items.map((i) => i.id), ["c", "b", "a"]);
  });

  describe("in German (SHAA-3035)", () => {
    afterEach(() => setLocaleForTests(null));
    test("every group heading the list builds comes from the catalog", () => {
      setLocaleForTests("de");
      const labels = (group: "none" | "criticality" | "hops") => groupItems(items, group, "name", viaName).map((g) => g.label);
      assert.deepEqual(labels("none"), ["Alle betroffenen CIs"]);
      assert.deepEqual(labels("criticality"), ["Critical", "High", "Nicht gesetzt"]);
      assert.deepEqual(labels("hops"), ["1 Schritt", "2 Schritte"]);
    });
  });
});

describe("tree", () => {
  test("each CI once, under the CI its shortest path comes from, depth first", () => {
    const a = item("a", 1, via("srv", "a", "srv"));
    const c = item("c", 1, via("srv", "c", "srv"), { reachedByCount: 2 });
    const b = item("b", 2, via("a", "b", "a"));
    const [tree] = impactTree(analysis([a, c, b]));
    assert.deepEqual(tree.rows.map((r) => [r.level, r.item.id, r.label, r.hasChildren]), [
      [1, "a", "hosts", true],
      [2, "b", "hosts", false],
      [1, "c", "hosts", false],
    ]);
  });

  test("both mode: two subtrees, a CI reached both ways in each", () => {
    const shared = item("x", 1, via("srv", "x", "srv"), { directions: ["downstream", "upstream"], upstreamVia: via("srv", "srv", "x") });
    const upOnly = item("y", 1, via("srv", "srv", "y"), { directions: ["upstream"] });
    const trees = impactTree(analysis([shared, upOnly], "both"));
    assert.deepEqual(trees.map((t) => [t.title, t.rows.map((r) => `${r.item.id}:${r.label}`)]), [
      ["Affected by this CI", ["x:hosts"]],
      ["This CI depends on", ["x:runs on", "y:runs on"]],
    ]);
  });

  test("collapsing a row hides everything below it", () => {
    const rows = [
      { key: "a", level: 1 },
      { key: "b", level: 2 },
      { key: "c", level: 3 },
      { key: "d", level: 1 },
    ];
    assert.deepEqual(visibleRows(rows, new Set(["a"])).map((r) => r.key), ["a", "d"]);
    assert.deepEqual(visibleRows(rows, new Set(["b"])).map((r) => r.key), ["a", "b", "d"]);
  });
});
