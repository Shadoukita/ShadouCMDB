// Unit tests for workflow problems in the user's language (SHAA-3003, GH#870). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { afterEach, describe, test } from "node:test";
import { setLocaleForTests } from "../src/i18n/index";
import { en } from "../src/i18n/en";
import { placeProblems } from "../src/lib/workflowDraft";
import { paramsFromBody, problemContext, problemKey, problemText, valueAt, type ProblemLike } from "../src/lib/workflowProblems";

afterEach(() => setLocaleForTests(null));

const p = (path: string, code: string, params: ProblemLike["params"] = {}, message = `API: ${code}`): ProblemLike => ({ path, code, message, params });

describe("problemText", () => {
  test("graph lint problems in English and German, with the state they name", () => {
    const dead = p("states[3]", "dead_end", { state: "limbo" });
    assert.equal(problemText(dead), "State limbo is not terminal and has no outgoing transition.");
    setLocaleForTests("de");
    assert.equal(problemText(dead), "Der Zustand limbo ist kein Endzustand und hat keinen ausgehenden Übergang.");
    assert.equal(problemText(p("states", "no_states")), "Ein Workflow braucht mindestens einen Zustand.");
    assert.equal(
      problemText(p("transitions[0]", "ungranted_transition", { transition: "submit" })),
      "Kein Profil hat das Recht für den Übergang submit: Nur Administratoren könnten ihn ausführen.",
    );
  });

  test("an unknown code falls back to the API's message, never to a blank or a key", () => {
    const future = p("states[0]", "some_future_check", { state: "a" }, "Something a newer backend checks");
    assert.equal(problemKey(future), undefined);
    assert.equal(problemText(future), "Something a newer backend checks");
    setLocaleForTests("de");
    assert.equal(problemText(future), "Something a newer backend checks");
  });

  test("a known code without the data its message needs falls back to the API's message", () => {
    const bare = p("states[2]", "unreachable_state", {}, "State x cannot be reached from the initial state");
    assert.equal(problemText(bare), "State x cannot be reached from the initial state");
  });

  test("the state or transition a problem was placed on fills in for missing params", () => {
    setLocaleForTests("de");
    const placed = { ...p("states[2]", "dead_end"), target: { kind: "state" as const, key: "review" } };
    assert.equal(problemText(placed), "Der Zustand review ist kein Endzustand und hat keinen ausgehenden Übergang.");
  });

  test("the path narrows the wording: the state field, an approval step, an attribute action", () => {
    setLocaleForTests("de");
    assert.equal(
      problemText(p("stateAttributeId", "inactive_attribute", { attribute: "lifecycle" })),
      "Das Zustandsfeld lifecycle ist archiviert.",
    );
    assert.equal(problemText(p("transitions[0].fields[0].attribute", "inactive_attribute", { attribute: "notes" })), "Das Feld notes ist archiviert.");
    assert.equal(
      problemText(p("transitions[0].approval.steps[1]", "inactive_attribute", { attribute: "owner" })),
      "Das Feld owner ist archiviert; seine Werte bestimmen weiterhin Genehmigende.",
    );
    assert.equal(
      problemText(p("transitions[1].setAttributes[8].value", "unknown_value", { attribute: "ops_status", value: "scrapped" })),
      "scrapped ist kein Schlüssel eines Werts der Liste von ops_status.",
    );
    assert.equal(
      problemText(p("transitions[0].conditions.all[0].value", "unknown_value", { attribute: "environment", value: "qa" })),
      "qa ist kein Wert von environment.",
    );
  });

  test("the params pick the variant: another workflow by key, a hidden one, a field that is gone", () => {
    const path = "transitions[0].fields[0].attribute";
    assert.equal(
      problemText(p(path, "state_field", { attribute: "status" })),
      "status is this workflow's state field: its states set it, not transition fields.",
    );
    assert.equal(
      problemText(p(path, "state_field", { attribute: "status", workflow: "change" })),
      "status is the state field of the active workflow change: a transition cannot set it.",
    );
    assert.equal(
      problemText(p(path, "state_field", { attribute: "status", hiddenWorkflow: "true" })),
      "status is the state field of another active workflow on these CIs: a transition cannot set it.",
    );
    assert.equal(problemText(p(path, "unknown_attribute", { attribute: "x", class: "server" })), "x is not a field of type server.");
    assert.equal(problemText(p(path, "unknown_attribute", { attribute: "x" })), "The type has no field x.");
    assert.equal(problemText(p(path, "unknown_attribute")), "The field no longer exists.");
    assert.equal(problemText(p("stateAttributeId", "unknown_attribute")), "The state field no longer exists.");
    const step = "transitions[0].approval.steps[0]";
    assert.equal(
      problemText(p(step, "attribute_type", { attribute: "owner", refersTo: "server" })),
      "Field owner refers to server, not to the Person type.",
    );
    assert.equal(
      problemText(p(step, "attribute_type", { attribute: "owner", dataType: "text" })),
      "Field owner is a Text field, not a reference to the Person type.",
    );
    assert.equal(problemText(p(step, "attribute_type", { attribute: "owner" })), "Field owner refers to another type, not to the Person type.");
  });

  test("data types, comparisons, value sources and approver sources are worded in the locale", () => {
    setLocaleForTests("de");
    assert.equal(
      problemText(p("transitions[1].setAttributes[5].valueFrom", "value_from_type", { attribute: "retired_on", dataType: "date", valueFrom: "actor", expected: "person_reference" })),
      "retired_on hat den Typ Datum: „Die ausführende Person“ braucht ein Feld, das auf eine Person verweist.",
    );
    assert.equal(
      problemText(p("transitions[1].conditions.op", "op_type", { attribute: "owner_team", dataType: "text", op: "gt" })),
      "Der Vergleich „ist größer als“ passt nicht zu einem Feld vom Typ Text.",
    );
    assert.equal(
      problemText(p("transitions.approve.steps.tech", "approvers_cannot_view", { step: "tech", class: "server", approverKind: "group", approver: "CAB" })),
      "Kein aktiver Benutzer von Gruppe CAB darf den Typ server sehen: Anfragen der Stufe tech würden sie nie erreichen.",
    );
  });

  test("counts are plurals", () => {
    const params = { step: "cab", transition: "approve", class: "server", available: 1 };
    assert.match(problemText(p("transitions[0].approval.steps[1]", "understaffed", { ...params, required: 1 })), /needs 1 approval, but/);
    setLocaleForTests("de");
    assert.match(problemText(p("transitions[0].approval.steps[1]", "understaffed", { ...params, required: 3 })), /braucht 3 Genehmigungen, es sind/);
  });

  test("a check the designer made itself is already worded and shown as is", () => {
    setLocaleForTests("de");
    assert.equal(problemText({ ...p("states[0].key", "duplicate"), message: "Schon vergeben", localized: true }), "Schon vergeben");
  });

  test("every message names only params the API sends or the designer derives", () => {
    const known = new Set([
      "state", "transition", "step", "attribute", "class", "dataType", "value", "op", "valueFrom", "expected", "workflow",
      "hiddenWorkflow", "approverKind", "approver", "refersTo", "excluded", "limit", "options", "pattern", "required",
      "available", "given", "name",
    ]);
    for (const key of Object.keys(en).filter((k) => k.startsWith("wfProblem."))) {
      for (const m of en[key as keyof typeof en].matchAll(/\{(\w+)/g)) assert.ok(known.has(m[1]), `${key}: {${m[1]}}`);
    }
  });
});

describe("contexts and params from the body", () => {
  test("what a path is about", () => {
    assert.equal(problemContext("stateAttributeId"), "stateField");
    assert.equal(problemContext("states[0].stateValue"), "stateValue");
    assert.equal(problemContext("transitions[1].setAttributes[2].value"), "action");
    assert.equal(problemContext("transitions[0].approval.steps[1].excludeActorsOf[0]"), "approval");
    assert.equal(problemContext("transitions.approve.steps.cab"), "approval");
    assert.equal(problemContext("transitions[0].conditions.all[1].value"), "condition");
    assert.equal(problemContext("transitions[0].fields[1].attribute"), "transitionField");
    assert.equal(problemContext("approvers[3]"), "assignment");
    assert.equal(problemContext("approvers"), "approvers");
    assert.equal(problemContext("states[2]"), undefined);
  });

  const body = {
    initialState: "ghost",
    states: [{ key: "a", stateValue: "planned" }, { key: "a" }],
    transitions: [
      { key: "go", from: "a", to: "b", fields: [{ attribute: "notes" }], conditions: { all: [{ field: "env", op: "in", value: ["prod", "qa"] }] } },
    ],
  };

  test("valueAt walks keys and indices", () => {
    assert.equal(valueAt(body, "transitions[0].fields[0].attribute"), "notes");
    assert.equal(valueAt(body, "transitions[0].conditions.all[0].value[1]"), "qa");
    assert.equal(valueAt(body, "transitions[3].key"), undefined);
  });

  test("a save refusal's details get the field, value, key and condition they are about", () => {
    assert.deepEqual(paramsFromBody("transitions[0].fields[0].attribute", body), { attribute: "notes" });
    assert.deepEqual(paramsFromBody("states[0].stateValue", body), { value: "planned" });
    assert.deepEqual(paramsFromBody("states[1].key", body), { given: "a" });
    assert.deepEqual(paramsFromBody("initialState", body), { given: "ghost" });
    assert.deepEqual(paramsFromBody("transitions[0].conditions.all[0].value[1]", body), { attribute: "env", op: "in", value: "qa" });
    assert.deepEqual(paramsFromBody("transitions[0].conditions.all[0].op", body), { attribute: "env", op: "in" });
  });

  test("placed save refusals are worded in German from the body they refused", () => {
    setLocaleForTests("de");
    const saved = body as never;
    const placed = placeProblems(
      [
        { path: "states[1].key", code: "duplicate", message: "\"a\" is used more than once", severity: "error" },
        { path: "initialState", code: "unknown_state", message: "No state \"ghost\" in this graph", severity: "error" },
        { path: "states[0].stateValue", code: "unknown_value", message: "No value \"planned\" in the state field's list", severity: "error" },
      ],
      saved,
    );
    assert.deepEqual(placed.map(problemText), [
      "„a“ wird mehrfach verwendet.",
      "Dieser Graph hat keinen Zustand „ghost“.",
      "Die Liste des Zustandsfelds hat keinen Wert planned.",
    ]);
  });
});
