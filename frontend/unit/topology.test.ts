// Unit tests for the topology panel's model and layout (lib/topology). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { RelationshipGraph } from "../src/api/queries";
import type { ImpactAnalysis } from "../src/lib/impact";
import { BOX_HEIGHT, layoutTopology, topologyFromGraph, topologyFromImpact, type Topology } from "../src/lib/topology";

const node = (id: string, depth: number, label = id) =>
  ({
    id,
    ident: `CI-${id}`,
    label,
    classId: `class-${id}`,
    class: { id: `class-${id}`, key: "server", name: "Server" },
    validFrom: "2026-01-01T00:00:00Z",
    validUntil: null,
    active: true,
    version: 1,
    createdAt: "2026-01-01T00:00:00Z",
    updatedAt: "2026-01-01T00:00:00Z",
    deletedAt: null,
    criticality: null,
    depth,
  }) as RelationshipGraph["nodes"][number];
const edge = (id: string, source: string, target: string, forwardLabel: string, typeId = "t-dep", isDirectional = true) =>
  ({
    id,
    relationshipTypeId: typeId,
    type: { key: typeId, name: typeId, forwardLabel, reverseLabel: `rev ${forwardLabel}`, isDirectional },
    sourceCiId: source,
    targetCiId: target,
    notes: null,
  }) as RelationshipGraph["edges"][number];
const graph = (nodes: RelationshipGraph["nodes"], edges: RelationshipGraph["edges"]): RelationshipGraph => ({
  rootId: "root",
  depth: 2,
  direction: "both",
  nodes,
  edges,
  truncated: false,
});

describe("topologyFromGraph", () => {
  // app runs on root; root runs on host; host is located in rack (does not propagate); peer is connected to root (symmetric).
  const g = graph(
    [node("root", 0), node("app", 1), node("host", 1), node("rack", 2), node("peer", 1)],
    [
      edge("e1", "app", "root", "runs on"),
      edge("e2", "root", "host", "runs on"),
      edge("e3", "host", "rack", "is located in", "t-loc"),
      edge("e4", "root", "peer", "connected to", "t-link", false),
      edge("e5", "root", "host", "depends on"),
    ],
  );
  const topo = topologyFromGraph(g, (id) => (id === "t-loc" ? false : id === "t-dep" ? true : undefined));
  const by = (id: string) => topo.nodes.find((n) => n.id === id)!;

  test("what the CI points to goes right, what points at it goes left, further hops follow their parent", () => {
    assert.equal(by("root").side, 0);
    assert.equal(by("host").side, 1);
    assert.equal(by("app").side, -1);
    assert.equal(by("rack").side, 1);
    assert.equal(by("rack").hops, 2);
    assert.equal(by("rack").parentId, "host");
  });
  test("a symmetric neighbour of the root goes to the emptier side", () => {
    // app (left) and host (right) came first: balanced, so right; one more would go left.
    assert.equal(by("peer").side, 1);
  });
  test("one edge per pair, labels joined; dashed only where the type does not propagate", () => {
    const toHost = topo.edges.filter((e) => e.to === "host");
    assert.equal(toHost.length, 1);
    assert.equal(toHost[0].label, "runs on, depends on");
    assert.equal(topo.edges.find((e) => e.to === "rack")!.dashed, true);
    assert.equal(topo.edges.find((e) => e.to === "host")!.dashed, false);
    // Unknown type (not in the list): solid.
    assert.equal(topo.edges.find((e) => e.to === "peer")!.dashed, false);
  });
  test("the arrow end: outward when the parent is the source; symmetric types have none", () => {
    assert.equal(topo.edges.find((e) => e.to === "host")!.outward, true);
    assert.equal(topo.edges.find((e) => e.to === "app")!.outward, false);
    assert.equal(topo.edges.find((e) => e.to === "peer")!.directional, false);
  });
  test("a root the response does not contain gives an empty topology", () => {
    assert.deepEqual(topologyFromGraph({ ...g, rootId: "missing" }).nodes, []);
  });
});

describe("topologyFromImpact", () => {
  const rt = (forwardLabel: string) => ({ id: "t", key: "runs_on", name: "Runs on", forwardLabel, reverseLabel: "hosts" });
  const item = (id: string, hops: number, parentId: string, edgeSourceId: string) =>
    ({
      id,
      ident: `CI-${id}`,
      name: id,
      classId: `class-${id}`,
      className: "Application",
      criticality: null,
      active: true,
      status: null,
      directions: ["downstream"],
      hops,
      via: { parentId, relationshipId: `r-${id}`, relationshipType: rt("runs on"), edgeSourceId, edgeTargetId: edgeSourceId === id ? parentId : id },
      upstreamVia: null,
      downstreamVia: null,
      reachedByCount: 1,
    }) as unknown as ImpactAnalysis["items"][number];
  const a = {
    root: { id: "db", name: "db", className: "Database" },
    parameters: { direction: "downstream" },
    items: [item("svc", 2, "app", "svc"), item("app", 1, "db", "app")],
  } as unknown as ImpactAnalysis;

  test("each CI hangs off its parent, in hop order, on the side its first edge gives", () => {
    const t = topologyFromImpact(a, "class-db");
    assert.deepEqual(
      t.nodes.map((n) => [n.id, n.hops, n.side, n.parentId]),
      [
        ["db", 0, 0, null],
        ["app", 1, -1, "db"],
        ["svc", 2, -1, "app"],
      ],
    );
    assert.equal(t.nodes[0].classId, "class-db");
    assert.deepEqual(
      t.edges.map((e) => [e.from, e.to, e.outward, e.directional, e.dashed]),
      [
        ["db", "app", false, true, false],
        ["app", "svc", false, true, false],
      ],
    );
  });
});

describe("layoutTopology", () => {
  const star = (left: number, right: number): Topology => ({
    rootId: "root",
    nodes: [
      { id: "root", label: "root", classId: "", className: "", active: true, hops: 0, parentId: null, side: 0 },
      ...Array.from({ length: left }, (_, i) => ({ id: `l${i}`, label: `l${i}`, classId: "", className: "", active: true, hops: 1, parentId: "root", side: -1 as const })),
      ...Array.from({ length: right }, (_, i) => ({ id: `r${i}`, label: `r${i}`, classId: "", className: "", active: true, hops: 1, parentId: "root", side: 1 as const })),
    ],
    edges: [
      ...Array.from({ length: left }, (_, i) => ({ from: "root", to: `l${i}`, label: "x", outward: false, directional: true, dashed: false })),
      ...Array.from({ length: right }, (_, i) => ({ from: "root", to: `r${i}`, label: "x", outward: true, directional: true, dashed: false })),
    ],
  });

  test("the root is centred, its neighbours in a column on each side, edges between facing sides", () => {
    const l = layoutTopology(star(1, 2), 900);
    const root = l.nodes.find((n) => n.id === "root")!;
    assert.ok(Math.abs(root.x + l.boxWidth / 2 - 450) <= 1);
    const left = l.nodes.find((n) => n.id === "l0")!;
    const right = l.nodes.filter((n) => n.side === 1);
    assert.ok(left.x + l.boxWidth < root.x);
    assert.ok(right.every((n) => n.x > root.x + l.boxWidth));
    assert.equal(new Set(right.map((n) => n.x)).size, 1);
    // No two boxes of a column overlap.
    assert.ok(Math.abs(right[0].y - right[1].y) >= BOX_HEIGHT);
    const e = l.edges.find((x) => x.to === "r0")!;
    assert.equal(e.x1, root.x + l.boxWidth);
    assert.equal(e.x2, right.find((n) => n.id === "r0")!.x);
  });

  test("a column holds at most maxRows boxes; the rest are counted in a +n box", () => {
    const l = layoutTopology(star(0, 12), 900, 8);
    assert.equal(l.nodes.filter((n) => n.side === 1).length, 7);
    assert.deepEqual(
      l.more.map((m) => [m.column, m.count]),
      [[1, 5]],
    );
    assert.equal(l.hidden, 5);
    // The edges of hidden CIs are not drawn.
    assert.equal(l.edges.length, 7);
  });

  test("labels sit on each child's own straight run, not where the edges converge (GH#671)", () => {
    const l = layoutTopology(star(7, 7), 1000);
    for (const side of [-1, 1]) {
      const edges = l.edges.filter((e) => l.nodes.find((n) => n.id === e.to)!.side === side);
      assert.equal(edges.length, 7);
      for (const e of edges) {
        const child = l.nodes.find((n) => n.id === e.to)!;
        // At the child's height, between the end of the curve and the child, clear of the arrowhead.
        assert.equal(e.ly, child.y + BOX_HEIGHT / 2);
        assert.ok(side === 1 ? e.xm < e.lx && e.lx < e.x2 : e.x2 < e.lx && e.lx < e.xm);
        // And no wider than the run, so it hides none of the curves beside it.
        assert.ok(Math.abs(e.lx - e.xm) >= e.labelRoom);
        // Every curve of the column ends before any run begins: no edge crosses another's run.
        for (const f of edges) assert.ok(side === 1 ? f.xm <= e.xm : f.xm >= e.xm);
      }
      // One row apart: a 16px label never overlaps the next one.
      const ys = edges.map((e) => e.ly).sort((a, b) => a - b);
      for (let i = 1; i < ys.length; i++) assert.ok(ys[i] - ys[i - 1] >= BOX_HEIGHT);
    }
  });

  test("the canvas is as tall as its tallest column", () => {
    assert.ok(layoutTopology(star(1, 1), 900).height < layoutTopology(star(1, 6), 900).height);
  });
});
