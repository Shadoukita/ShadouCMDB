// Unit tests for the workflow designer's model (SHAA-1426): conditions to and from the API's JSON,
// the whole-graph PUT body, renaming and removing states, the client-side checks, where lint problems
// land, the grants matrix and the diagram's edge geometry. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import {
  COLUMN_STEP,
  NODE_H,
  NODE_W,
  autoLayout,
  checkDraft,
  conditionsToJson,
  describeConditions,
  draftFromVersion,
  grantRows,
  grantSets,
  grantsBody,
  opsFor,
  parseConditions,
  placeProblems,
  problemsFor,
  removeState,
  renameState,
  toDraftBody,
  uniqueKey,
  type Draft,
} from "../src/lib/workflowDraft";
import { EDGE_LABEL_MAX, clipToBox, edgeGeometry, shorten } from "../src/lib/workflowGraph";

// The design's example (§6.1), with a condition and a field.
const VERSION = {
  initialState: "planned",
  states: [
    { key: "planned", name: "Planned", category: "open" as const, stateValue: "planned" },
    { key: "approved", name: "Approved", category: "active" as const, stateValue: "approved" },
    { key: "done", name: "Done", category: "done" as const, terminal: true, stateValue: "in_production" },
  ],
  transitions: [
    {
      key: "approve",
      name: "Approve",
      from: "planned",
      to: "approved",
      requiresComment: true,
      fields: [{ attribute: "owner" }],
      conditions: { all: [{ field: "environment", op: "eq", value: "prod" }, { any: [{ field: "risk", op: "lte", value: 3 }, { field: "notes", op: "isSet" }] }] },
    },
    { key: "go_live", name: "Go live", from: "approved", to: "done" },
  ],
  layout: { planned: { x: 0, y: 0 }, approved: { x: 240, y: 0 }, junk: "x" },
};

const draft = (): Draft => draftFromVersion(VERSION as never);

describe("conditions", () => {
  test("a stored group round-trips through the editor tree", () => {
    const tree = parseConditions(VERSION.transitions[0].conditions);
    assert.equal(tree.mode, "all");
    assert.equal(tree.children.length, 2);
    assert.deepEqual(conditionsToJson(tree), VERSION.transitions[0].conditions);
  });
  test("a lone leaf becomes the only child of an all group, and an empty root means no condition", () => {
    const tree = parseConditions({ field: "risk", op: "gt", value: 1 });
    assert.deepEqual(tree, { kind: "group", mode: "all", children: [{ kind: "leaf", field: "risk", op: "gt", value: 1 }] });
    assert.equal(conditionsToJson(parseConditions(undefined)), undefined);
  });
  test("empty inner groups are dropped, and isSet carries no value", () => {
    const json = conditionsToJson({
      kind: "group",
      mode: "any",
      children: [
        { kind: "group", mode: "all", children: [] },
        { kind: "leaf", field: "notes", op: "isSet", value: "stale" },
      ],
    });
    assert.deepEqual(json, { any: [{ field: "notes", op: "isSet" }] });
  });
  test("operators follow the field's type (§3.3)", () => {
    assert.ok(opsFor("integer").includes("gte"));
    assert.ok(!opsFor("integer").includes("contains"));
    assert.ok(opsFor("text").includes("contains"));
    assert.ok(!opsFor("lookup").includes("gt"));
    assert.ok(opsFor("date").includes("lt"));
  });
  test("conditions read as one line", () => {
    const text = describeConditions(parseConditions(VERSION.transitions[0].conditions), (k) => k.toUpperCase());
    assert.equal(text, "ENVIRONMENT is prod and (RISK is at most 3 or NOTES is set)");
  });
});

describe("draft <-> PUT body", () => {
  test("defaults are filled in, and junk in the layout is ignored", () => {
    const d = draft();
    assert.equal(d.states[0].terminal, false);
    assert.equal(d.transitions[1].requiresComment, false);
    assert.deepEqual(d.transitions[0].fields, [{ attribute: "owner", required: true }]);
    assert.deepEqual(d.positions, { planned: { x: 0, y: 0 }, approved: { x: 240, y: 0 } });
  });
  test("the body has the design's layout shape and leaves out empty fields and conditions", () => {
    const body = toDraftBody(draft(), "a".repeat(64));
    assert.equal(body.expectedChecksum, "a".repeat(64));
    assert.deepEqual(body.layout, { planned: { x: 0, y: 0 }, approved: { x: 240, y: 0 } });
    assert.deepEqual(body.transitions[1], { key: "go_live", name: "Go live", from: "approved", to: "done", requiresComment: false });
    assert.deepEqual(body.transitions[0].conditions, VERSION.transitions[0].conditions);
  });
  test("an initial state that no longer exists is sent as null", () => {
    const d = draft();
    d.states = d.states.filter((s) => s.key !== "planned");
    assert.equal(toDraftBody(d).initialState, null);
  });
});

describe("editing", () => {
  test("renaming a state follows it into transitions, the initial state and the layout", () => {
    const d = draft();
    renameState(d, "planned", "requested");
    assert.equal(d.initialState, "requested");
    assert.equal(d.transitions[0].from, "requested");
    assert.deepEqual(d.positions.requested, { x: 0, y: 0 });
    assert.equal(d.positions.planned, undefined);
  });
  test("removing a state removes its transitions", () => {
    const d = draft();
    assert.deepEqual(removeState(d, "approved"), ["approve", "go_live"]);
    assert.equal(d.transitions.length, 0);
    assert.equal(d.states.length, 2);
  });
  test("unique keys", () => {
    assert.equal(uniqueKey("new_state", ["new_state", "new_state_2"]), "new_state_3");
    assert.equal(uniqueKey("x", []), "x");
  });
  test("auto layout places unplaced states by distance from the initial state, keeping placed ones", () => {
    const d = draft();
    autoLayout(d);
    assert.deepEqual(d.positions.planned, { x: 0, y: 0 });
    assert.ok(d.positions.done.x > 0);
    autoLayout(d, true);
    assert.ok(d.positions.planned.x < d.positions.approved.x && d.positions.approved.x < d.positions.done.x);
  });
});

describe("client-side checks", () => {
  test("the design example passes", () => {
    assert.deepEqual(checkDraft(draft()), []);
  });
  test("bad keys, duplicates, loops, missing values and fields are caught on their item", () => {
    const d = draft();
    d.states.push({ key: "planned", name: " ", category: "open", terminal: false, stateValue: null });
    d.transitions.push({
      key: "Bad",
      name: "x",
      from: "done",
      to: "done",
      requiresComment: false,
      fields: [{ attribute: "" }, { attribute: "a" }, { attribute: "a" }].map((f) => ({ ...f, required: true })),
      conditions: { kind: "group", mode: "all", children: [{ kind: "leaf", field: "risk", op: "eq", value: "" }] },
    });
    const codes = checkDraft(d).map((p) => `${p.target.kind}:${"key" in p.target ? p.target.key : ""}:${p.code}:${p.path}`);
    assert.deepEqual(codes, [
      "state:planned:duplicate:states[3].key",
      "state:planned:required:states[3].name",
      "transition:Bad:invalid:transitions[2].key",
      "transition:Bad:self_loop:transitions[2].to",
      "transition:Bad:required:transitions[2].fields[0].attribute",
      "transition:Bad:duplicate:transitions[2].fields[2].attribute",
      "transition:Bad:required:transitions[2].conditions.all[0].value",
    ]);
  });
});

describe("lint problems land on what they are about", () => {
  test("paths are resolved by the saved body's indices to keys", () => {
    const body = toDraftBody(draft());
    const placed = placeProblems(
      [
        { path: "states[1].stateValue", code: "unknown_value", message: "m1", severity: "error" },
        { path: "transitions[0].fields[0].attribute", code: "unknown_attribute", message: "m2", severity: "error" },
        { path: "transitions[1]", code: "ungranted_transition", message: "m3", severity: "warning" },
        { path: "initialState", code: "no_initial_state", message: "m4", severity: "error" },
        { path: "states[9]", code: "x", message: "m5", severity: "error" },
      ],
      body,
    );
    assert.deepEqual(
      placed.map((p) => p.target),
      [
        { kind: "state", key: "approved" },
        { kind: "transition", key: "approve" },
        { kind: "transition", key: "go_live" },
        { kind: "graph" },
        { kind: "graph" },
      ],
    );
    assert.deepEqual(problemsFor(placed, "transition", "go_live").map((p) => p.code), ["ungranted_transition"]);
  });
});

describe("grants matrix", () => {
  const grants = [
    { transitionKey: "approve", profiles: [{ id: "p2", name: "B" }, { id: "p1", name: "A" }] },
    { transitionKey: "old_key", profiles: [{ id: "p1", name: "A" }] },
    { transitionKey: "_cancel", profiles: [] },
    { transitionKey: "_start", profiles: [{ id: "p1", name: "A" }] },
  ];
  test("rows: draft and current transitions by key, then keys only grants name, then starting again and cancelling", () => {
    const rows = grantRows([[{ key: "approve", name: "Approve (new)" }], [{ key: "approve", name: "Approve" }, { key: "reject", name: "Reject" }]], grants);
    assert.deepEqual(
      rows.map((r) => [r.key, r.name, r.orphan]),
      [
        ["approve", "Approve (new)", false],
        ["reject", "Reject", false],
        ["old_key", "old_key", true],
        ["_start", "Start again after an instance ended", false],
        ["_cancel", "Cancel an instance", false],
      ],
    );
  });
  test("the PUT body leaves out rows without a profile", () => {
    const sets = grantSets(grants);
    sets.set("reject", new Set());
    assert.deepEqual(grantsBody(sets), [
      { transitionKey: "approve", profiles: ["p1", "p2"] },
      { transitionKey: "old_key", profiles: ["p1"] },
      { transitionKey: "_start", profiles: ["p1"] },
    ]);
  });
});

describe("diagram geometry", () => {
  const size = { w: 100, h: 40 };
  test("a line leaves a box on its border", () => {
    assert.deepEqual(clipToBox({ x: 50, y: 20 }, size, { x: 250, y: 20 }), { x: 100, y: 20 });
    assert.deepEqual(clipToBox({ x: 50, y: 20 }, size, { x: 50, y: 220 }), { x: 50, y: 40 });
  });
  test("opposite transitions between two states curve apart; a single one is straight", () => {
    const pos = (k: string) => (k === "a" ? { x: 0, y: 0 } : { x: 300, y: 0 });
    const [one] = edgeGeometry([{ key: "t", name: "T", from: "a", to: "b" }], pos, size);
    assert.equal(one.path, "M 100 20 Q 200 20 300 20");
    const [ab, ba] = edgeGeometry(
      [
        { key: "ab", name: "AB", from: "a", to: "b" },
        { key: "ba", name: "BA", from: "b", to: "a" },
      ],
      pos,
      size,
    );
    assert.ok(ab.label.y !== ba.label.y, "labels sit on different sides");
    assert.equal(Math.sign(ab.label.y - 20), -Math.sign(ba.label.y - 20));
    // Two lines of 11px text: the labels sit more than a line height apart (GH#644).
    assert.ok(Math.abs(ab.label.y - ba.label.y) >= 2 * 14);
  });
  test("an arrangement leaves room for a whole label between neighbouring columns", () => {
    const node = { w: NODE_W, h: NODE_H };
    const pos = (k: string) => (k === "a" ? { x: 40, y: 24 } : { x: 40 + COLUMN_STEP, y: 24 });
    const [e] = edgeGeometry([{ key: "t", name: "x".repeat(40), from: "a", to: "b" }], pos, node);
    // About 6.5px per character at 11px: the label's half width stays clear of both boxes.
    const half = (shorten(e.name, EDGE_LABEL_MAX).length * 6.5) / 2;
    assert.ok(e.label.x - half > 40 + NODE_W && e.label.x + half < 40 + COLUMN_STEP);
  });
  test("long names are shortened with an ellipsis", () => {
    assert.equal(shorten("Start maintenance", EDGE_LABEL_MAX), "Start maintenance");
    assert.equal(shorten("a".repeat(30), 10), `${"a".repeat(9)}…`);
  });
});
