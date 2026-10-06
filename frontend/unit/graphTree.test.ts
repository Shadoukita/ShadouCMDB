// Unit tests for the relationship map's tree rows (lib/graphTree). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { RelationshipGraph } from "../src/api/queries";
import { graphRows } from "../src/lib/graphTree";

const node = (id: string, depth: number) =>
  ({ id, label: id, classId: `c-${id}`, class: { id: `c-${id}`, key: "k", name: "Class" }, active: true, deletedAt: null, depth }) as unknown as RelationshipGraph["nodes"][number];
const edge = (id: string, sourceCiId: string, targetCiId: string, forwardLabel: string) =>
  ({ id, relationshipTypeId: "t", type: { key: "t", name: "t", forwardLabel, reverseLabel: `rev ${forwardLabel}`, isDirectional: true }, sourceCiId, targetCiId, notes: null }) as RelationshipGraph["edges"][number];

describe("graphRows", () => {
  // root runs on vm; vm and db (both one hop) are related; db runs on host, vm runs on host (two hops).
  const g = {
    rootId: "root",
    depth: 2,
    direction: "both",
    truncated: false,
    nodes: [node("root", 0), node("vm", 1), node("db", 1), node("host", 2)],
    edges: [edge("e1", "root", "vm", "runs on"), edge("e2", "root", "db", "uses"), edge("e3", "db", "vm", "runs on"), edge("e4", "vm", "host", "runs on"), edge("e5", "db", "host", "runs on")],
  } as RelationshipGraph;

  test("each row is one hop further out; edges back to the root or within a hop are not rows", () => {
    const rows = graphRows(g, "root", "both");
    assert.deepEqual(
      rows.map((r) => [r.level, r.edgeLabel, r.node.id, !!r.repeat]),
      [
        [1, "runs on", "vm", false],
        [2, "runs on", "host", false],
        [1, "uses", "db", false],
        [2, "runs on", "host", true],
      ],
    );
    assert.equal(rows[0].hasChildren, true);
    assert.equal(rows[0].node.classId, "c-vm");
  });

  test("outgoing reads each edge from its source only", () => {
    const rows = graphRows({ ...g, nodes: [node("root", 0), node("vm", 1), node("db", 1), node("host", 2)] }, "root", "outgoing");
    assert.deepEqual(
      rows.map((r) => r.node.id),
      ["vm", "host", "db", "host"],
    );
  });
});
