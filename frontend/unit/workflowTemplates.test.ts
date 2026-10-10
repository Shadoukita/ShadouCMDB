// Unit tests for the workflow templates (SHAA-3040, SHAA-3105): the lifecycle and change graphs, how its states find the state
// field's values, and that the result passes the designer's own checks. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { setLocaleForTests } from "../src/i18n/index";
import { checkDraft, toDraftBody } from "../src/lib/workflowDraft";
import {
  CHANGE_ASSESSMENT_STATE,
  CHANGE_CAB_STEP,
  CHANGE_CAB_TRANSITION,
  changeDraft,
  lifecycleDraft,
  matchStateValue,
  templateDraft,
  templateFromQuery,
  type StateValueOption,
} from "../src/lib/workflowTemplates";

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

  // QA (SHAA-3062): a retired candidate is passed over for the next active one, also when its name matches.
  test("a retired first candidate yields to the next active candidate", () => {
    const values: StateValueOption[] = [
      { key: "active", name: "Active", isActive: false },
      { key: "live", name: "Live", isActive: true },
    ];
    assert.equal(matchStateValue(["active", "in_service", "live"], "Active", values), "live");
  });

  // QA (SHAA-3062): a list without the candidate keys is matched by the state names of the active locale.
  test("in German, the states match a list by their German names", () => {
    setLocaleForTests("de");
    try {
      const body = toDraftBody(
        lifecycleDraft([
          { key: "s1", name: "geplant", isActive: true },
          { key: "s2", name: "Aktiv", isActive: true },
          { key: "s3", name: "Außer Betrieb", isActive: true },
        ]),
      );
      assert.deepEqual(
        body.states.map((s) => [s.key, s.stateValue]),
        [
          ["planned", "s1"],
          ["active", "s2"],
          ["retired", "s3"],
        ],
      );
    } finally {
      setLocaleForTests("en");
    }
  });
});

describe("templates", () => {
  test("the blank workflow has no draft", () => {
    assert.equal(templateDraft("blank", STARTER), null);
    assert.equal(templateDraft("lifecycle", null)?.states.length, 3);
    assert.equal(templateDraft("change", null)?.states.length, 7);
  });

  test("an unknown template in the URL is the blank workflow", () => {
    assert.equal(templateFromQuery("lifecycle"), "lifecycle");
    assert.equal(templateFromQuery("change"), "change");
    assert.equal(templateFromQuery("other"), "blank");
    assert.equal(templateFromQuery(undefined), "blank");
    assert.equal(templateFromQuery(["lifecycle"]), "blank");
  });
});

describe("changeDraft", () => {
  test("Requested (initial) → Assessment → Approved → Implementation → Review → Closed, and Rejected", () => {
    const d = changeDraft(null);
    assert.equal(d.initialState, "requested");
    assert.deepEqual(
      d.states.map((s) => [s.key, s.name, s.category, s.terminal]),
      [
        ["requested", "Requested", "open", false],
        ["assessment", "Assessment", "active", false],
        ["approved", "Approved", "active", false],
        ["implementation", "Implementation", "active", false],
        ["review", "Review", "active", false],
        ["closed", "Closed", "done", true],
        ["rejected", "Rejected", "cancelled", true],
      ],
    );
    assert.deepEqual(
      d.transitions.map((x) => [x.key, x.from, x.to]),
      [
        ["submit", "requested", "assessment"],
        ["approve", "assessment", "approved"],
        ["reject", "assessment", "rejected"],
        ["implement", "approved", "implementation"],
        ["complete", "implementation", "review"],
        ["close", "review", "closed"],
        ["cancel", "requested", "rejected"],
      ],
    );
    assert.equal(CHANGE_ASSESSMENT_STATE, "assessment");
  });

  test("only the CAB approval gates a transition: one approval, four-eyes against the submitter", () => {
    const d = changeDraft(null);
    for (const x of d.transitions) {
      if (x.key !== CHANGE_CAB_TRANSITION) assert.deepEqual(x.approval, [], x.key);
    }
    const cab = d.transitions.find((x) => x.key === CHANGE_CAB_TRANSITION)!;
    assert.deepEqual(cab.approval, [
      {
        key: CHANGE_CAB_STEP,
        name: "CAB approval",
        requiredApprovals: 1,
        dueAfter: null,
        onOverdue: "flag",
        distinctFromEarlier: true,
        excludeActorsOf: ["submit"],
        allowApiTokens: false,
      },
    ]);
    // A rejection says why.
    assert.equal(d.transitions.find((x) => x.key === "reject")!.requiresComment, true);
  });

  test("passes the designer's checks, every state has a position, and the body carries the policy", () => {
    const d = changeDraft(null);
    assert.deepEqual(checkDraft(d), []);
    for (const s of d.states) assert.ok(d.positions[s.key], s.key);
    assert.ok(d.positions.requested.x < d.positions.assessment.x);
    assert.ok(d.positions.review.x < d.positions.closed.x);
    const body = toDraftBody(d);
    const approve = body.transitions.find((x) => x.key === CHANGE_CAB_TRANSITION)!;
    assert.deepEqual(approve.approval, {
      steps: [{ key: "cab", name: "CAB approval", requiredApprovals: 1, onOverdue: "flag", distinctFromEarlier: true, excludeActorsOf: ["submit"], allowApiTokens: false }],
    });
    assert.equal(body.transitions.find((x) => x.key === "submit")!.approval, undefined);
  });

  test("every transition is reachable and every non-final state leads on", () => {
    const d = changeDraft(null);
    const out = new Set(d.transitions.map((x) => x.from));
    const into = new Set(d.transitions.map((x) => x.to));
    for (const s of d.states) {
      if (s.key !== d.initialState) assert.ok(into.has(s.key), `${s.key} reachable`);
      assert.equal(out.has(s.key), !s.terminal, `${s.key} outgoing`);
    }
  });

  test("maps the states to a change status list by key, and leaves unmatched states empty", () => {
    const body = toDraftBody(
      changeDraft([
        { key: "new", name: "New", isActive: true },
        { key: "assessment", name: "Assessment", isActive: true },
        { key: "authorized", name: "Authorized", isActive: true },
        { key: "in_progress", name: "In progress", isActive: true },
        { key: "closed", name: "Closed", isActive: true },
        { key: "rejected", name: "Rejected", isActive: false },
      ]),
    );
    assert.deepEqual(
      body.states.map((s) => [s.key, s.stateValue]),
      [
        ["requested", "new"],
        ["assessment", "assessment"],
        ["approved", "authorized"],
        ["implementation", "in_progress"],
        ["review", null],
        ["closed", "closed"],
        ["rejected", null],
      ],
    );
  });

  test("names in the active locale", () => {
    setLocaleForTests("de");
    try {
      const d = changeDraft(null);
      assert.deepEqual(
        d.states.map((s) => s.name),
        ["Beantragt", "Bewertung", "Genehmigt", "Umsetzung", "Review", "Abgeschlossen", "Abgelehnt"],
      );
      assert.equal(d.transitions[1].approval[0].name, "CAB-Genehmigung");
    } finally {
      setLocaleForTests("en");
    }
  });
});
