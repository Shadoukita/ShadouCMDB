// Unit tests for the workflow templates (SHAA-3040): the lifecycle graph, how its states find the state
// field's values, and that the result passes the designer's own checks. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { setLocaleForTests } from "../src/i18n/index";
import { checkDraft, toDraftBody } from "../src/lib/workflowDraft";
import { lifecycleDraft, matchStateValue, templateDraft, templateFromQuery, type StateValueOption } from "../src/lib/workflowTemplates";

setLocaleForTests("en");

/** The IT infrastructure starter's "status" list. */
const STARTER: StateValueOption[] = [
  { key: "planned", name: "Planned", isActive: true },
  { key: "in_service", name: "In service", isActive: true },
  { key: "maintenance", name: "Maintenance", isActive: true },
  { key: "retired", name: "Retired", isActive: true },
  { key: "disposed", name: "Disposed", isActive: true },
];

describe("lifecycleDraft", () => {
  test("Planned (initial) → Active → Retired (final), and Planned → Retired", () => {
    const d = lifecycleDraft(null);
    assert.equal(d.initialState, "planned");
    assert.deepEqual(
      d.states.map((s) => [s.key, s.name, s.category, s.terminal]),
      [
        ["planned", "Planned", "open", false],
        ["active", "Active", "active", false],
        ["retired", "Retired", "done", true],
      ],
    );
    assert.deepEqual(
      d.transitions.map((x) => [x.key, x.from, x.to]),
      [
        ["activate", "planned", "active"],
        ["retire", "active", "retired"],
        ["cancel_plan", "planned", "retired"],
      ],
    );
  });

  test("passes the designer's checks and every state has a position", () => {
    const d = lifecycleDraft(STARTER);
    assert.deepEqual(checkDraft(d), []);
    for (const s of d.states) assert.ok(d.positions[s.key], s.key);
    // Planned → Active → Retired reads left to right.
    assert.ok(d.positions.planned.x < d.positions.active.x);
  });

  test("without a state field no state has a value", () => {
    const body = toDraftBody(lifecycleDraft(null));
    assert.deepEqual(
      body.states.map((s) => s.stateValue),
      [null, null, null],
    );
  });

  test("maps the states to the starter's status values", () => {
    const body = toDraftBody(lifecycleDraft(STARTER));
    assert.deepEqual(
      body.states.map((s) => [s.key, s.stateValue]),
      [
        ["planned", "planned"],
        ["active", "in_service"],
        ["retired", "retired"],
      ],
    );
    assert.equal(body.initialState, "planned");
    // Plain transitions: no conditions, fields, approval or actions.
    for (const x of body.transitions) assert.deepEqual(Object.keys(x).sort(), ["from", "key", "name", "requiresComment", "to"]);
  });

  test("names in the active locale", () => {
    setLocaleForTests("de");
    try {
      const d = lifecycleDraft(null);
      assert.deepEqual(
        d.states.map((s) => s.name),
        ["Geplant", "Aktiv", "Außer Betrieb"],
      );
      assert.equal(d.transitions[0].name, "In Betrieb nehmen");
    } finally {
      setLocaleForTests("en");
    }
  });
});

describe("matchStateValue", () => {
  test("first candidate key that is an active value", () => {
    assert.equal(matchStateValue(["active", "in_service"], "Active", STARTER), "in_service");
    assert.equal(
      matchStateValue(["active", "in_service"], "Active", [...STARTER, { key: "active", name: "Live", isActive: true }]),
      "active",
    );
  });

  test("falls back to a value with the state's name, ignoring case", () => {
    assert.equal(matchStateValue(["x"], "Active", [{ key: "prod_running", name: " active ", isActive: true }]), "prod_running");
  });

  test("never a retired value, and null when nothing matches", () => {
    assert.equal(matchStateValue(["retired"], "Retired", [{ key: "retired", name: "Retired", isActive: false }]), null);
    assert.equal(matchStateValue(["planned"], "Planned", []), null);
  });
});

describe("templates", () => {
  test("the blank workflow has no draft", () => {
    assert.equal(templateDraft("blank", STARTER), null);
    assert.equal(templateDraft("lifecycle", null)?.states.length, 3);
  });

  test("an unknown template in the URL is the blank workflow", () => {
    assert.equal(templateFromQuery("lifecycle"), "lifecycle");
    assert.equal(templateFromQuery("other"), "blank");
    assert.equal(templateFromQuery(undefined), "blank");
    assert.equal(templateFromQuery(["lifecycle"]), "blank");
  });
});
