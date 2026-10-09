// Unit tests for workflow actions in the designer (SHAA-2736, design SHAA-2725 §3, §11.1): attribute
// actions to and from the draft body and their checks, notification actions to and from the PUT body
// (only the settings a kind and trigger take), the client-side checks, and where problems land.
// Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import {
  actionFromApi,
  actionToApi,
  checkActions,
  checkSetAttributes,
  generalProblems,
  modesFor,
  newAction,
  participantsFor,
  previewableActions,
  problemsOfAction,
  recipientIdentity,
  setAttributeFromApi,
  setAttributeToApi,
  setTargetRefusal,
  sourcesFor,
  transitionChoices,
  type DraftAction,
  type WorkflowAction,
} from "../src/lib/workflowActions";
import { checkDraft, draftFromVersion, toDraftBody } from "../src/lib/workflowDraft";

describe("attribute actions", () => {
  test("a literal and each valueFrom round-trip through the draft body", () => {
    const list = [
      { attribute: "lifecycle_status", value: "retired" },
      { attribute: "retired_on", valueFrom: "today" as const },
      { attribute: "retired_by", valueFrom: "actor" as const },
      { attribute: "monitoring_ref", valueFrom: "clear" as const },
    ];
    const draft = list.map(setAttributeFromApi);
    assert.deepEqual(
      draft.map((s) => s.mode),
      ["literal", "today", "actor", "clear"],
    );
    assert.deepEqual(draft.map(setAttributeToApi), list);
  });

  test("the draft body carries setAttributes only when a transition has some", () => {
    const v = {
      initialState: "in_use",
      states: [
        { key: "in_use", name: "In use", category: "active" as const },
        { key: "retired", name: "Retired", category: "done" as const, terminal: true },
      ],
      transitions: [
        { key: "retire", name: "Retire", from: "in_use", to: "retired", setAttributes: [{ attribute: "retired_on", valueFrom: "now" as const }] },
        { key: "noop", name: "Noop", from: "retired", to: "in_use" },
      ],
      layout: {},
    };
    const body = toDraftBody(draftFromVersion(v));
    assert.deepEqual(body.transitions[0].setAttributes, [{ attribute: "retired_on", valueFrom: "now" }]);
    assert.equal("setAttributes" in body.transitions[1], false);
  });

  test("modes follow the field's type, and a required field cannot be cleared", () => {
    assert.deepEqual(modesFor({ dataType: "date", isRequired: false }), ["literal", "today", "now", "clear"]);
    assert.deepEqual(modesFor({ dataType: "datetime", isRequired: true }), ["literal", "now"]);
    assert.deepEqual(modesFor({ dataType: "reference", isRequired: false }), ["actor", "clear"]);
    assert.deepEqual(modesFor({ dataType: "lookup", isRequired: false }), ["literal", "clear"]);
  });

  test("the picker refuses the targets the publish lint refuses", () => {
    const ctx = { stateFieldKey: "status", transitionFields: ["reason"], personClassIds: new Set(["person"]) };
    const f = { isActive: true, isIdentifying: false, systemRole: null, dataType: "text" as const, referenceClassId: null };
    assert.equal(setTargetRefusal({ ...f, key: "note" }, ctx), undefined);
    assert.ok(setTargetRefusal({ ...f, key: "status" }, ctx));
    assert.ok(setTargetRefusal({ ...f, key: "reason" }, ctx));
    assert.ok(setTargetRefusal({ ...f, key: "hostname", isIdentifying: true }, ctx));
    assert.ok(setTargetRefusal({ ...f, key: "old", isActive: false }, ctx));
    assert.ok(setTargetRefusal({ ...f, key: "site", dataType: "reference", referenceClassId: "location" }, ctx));
    assert.equal(setTargetRefusal({ ...f, key: "owner", dataType: "reference", referenceClassId: "person" }, ctx), undefined);
  });

  test("empty literals and duplicates are caught before saving, on the transition", () => {
    assert.deepEqual(
      checkSetAttributes([
        { attribute: "a", mode: "literal", value: "" },
        { attribute: "a", mode: "clear", value: "" },
        { attribute: "", mode: "literal", value: "x" },
      ]).map((p) => `${p.code}:${p.path}`),
      ["required:setAttributes[0].value", "duplicate:setAttributes[1].attribute", "required:setAttributes[2].attribute"],
    );
    const d = draftFromVersion({ initialState: "a", states: [{ key: "a", name: "A", category: "open" }, { key: "b", name: "B", category: "done" }], transitions: [{ key: "go", name: "Go", from: "a", to: "b" }], layout: {} });
    d.transitions[0].setAttributes.push({ attribute: "x", mode: "literal", value: "" });
    assert.deepEqual(
      checkDraft(d).map((p) => `${p.target.kind}:${p.path}`),
      ["transition:transitions[0].setAttributes[0].value"],
    );
  });
});

const STORED: WorkflowAction = {
  id: "00000000-0000-0000-0000-000000000001",
  key: "notify_cab",
  name: "Tell CAB",
  kind: "email",
  trigger: "approval_closed",
  transition: "submit",
  enabled: true,
  recipients: [
    { source: "group", group: { id: "g1", name: "CAB" } },
    { source: "ci_owner" },
    { source: "participant", participant: "requester" },
    { source: "address", address: "cab@corp.example" },
  ],
  endpoint: null,
  settings: { content: "minimal", subject: { en: "{{ci.label}} closed" }, statuses: ["approved", "rejected"], excludeActor: false },
};

describe("notification actions", () => {
  test("an e-mail action round-trips, recipients by id", () => {
    const a = actionFromApi(STORED);
    assert.deepEqual(actionToApi(a), {
      key: "notify_cab",
      name: "Tell CAB",
      kind: "email",
      trigger: "approval_closed",
      transition: "submit",
      enabled: true,
      recipients: [
        { source: "group", group: "g1" },
        { source: "ci_owner" },
        { source: "participant", participant: "requester" },
        { source: "address", address: "cab@corp.example" },
      ],
      settings: { excludeActor: false, statuses: ["approved", "rejected"], content: "minimal", subject: { en: "{{ci.label}} closed" } },
    });
  });

  test("switching kind sends only what the new kind takes", () => {
    const a = actionFromApi(STORED);
    a.kind = "inbox";
    const inbox = actionToApi(a);
    assert.equal(inbox.recipients?.some((r) => r.source === "address"), false);
    assert.deepEqual(Object.keys(inbox.settings ?? {}).sort(), ["excludeActor", "statuses"]);
    a.kind = "webhook";
    a.endpoint = "itsm-prod";
    a.includeAttributes = ["hostname"];
    const hook = actionToApi(a);
    assert.equal(hook.recipients, undefined);
    assert.equal(hook.endpoint, "itsm-prod");
    assert.deepEqual(hook.settings, { includeAttributes: ["hostname"], statuses: ["approved", "rejected"] });
  });

  test("instance triggers send no transition; every status means no filter", () => {
    const a = actionFromApi(STORED);
    a.trigger = "instance_cancelled";
    assert.equal(actionToApi(a).transition, null);
    a.trigger = "approval_closed";
    a.statuses = ["approved", "rejected", "withdrawn", "cancelled"];
    assert.equal(actionToApi(a).settings?.statuses, undefined);
  });

  test("sources and participants follow the kind and trigger", () => {
    assert.equal(sourcesFor("inbox").includes("address"), false);
    assert.equal(sourcesFor("email").includes("address"), true);
    assert.deepEqual(sourcesFor("webhook"), []);
    assert.deepEqual(participantsFor("transition"), ["actor", "starter"]);
    assert.deepEqual(participantsFor("approval_step"), ["actor", "starter", "requester", "approvers"]);
  });

  test("new actions get a free key and start on the given transition", () => {
    const a = newAction([{ key: "notify" }], "approve");
    assert.equal(a.key, "notify_2");
    assert.equal(a.trigger, "transition");
    assert.equal(a.transition, "approve");
    assert.equal(newAction([], null).trigger, "instance_cancelled");
  });

  test("the client checks name the API's paths", () => {
    const base = actionFromApi(STORED);
    const bad: DraftAction[] = [
      { ...base, key: "Bad", name: " " },
      { ...base, key: "dup", kind: "inbox", trigger: "transition", recipients: [{ source: "participant", ref: null, attribute: null, serviceOwnerRole: null, participant: "approvers", address: null }] },
      { ...base, key: "dup", kind: "webhook", endpoint: "", recipients: [] },
      { ...base, key: "texts", subject: { en: "{{ci.name}}", de: "" }, intro: { en: "", de: "{{ approval.step }}" } },
    ];
    assert.deepEqual(
      checkActions(bad).map((p) => `${p.code}:${p.path}`),
      [
        "invalid:actions[0].key",
        "required:actions[0].name",
        "not_applicable:actions[1].recipients[0].participant",
        "duplicate:actions[2].key",
        "required:actions[2].endpoint",
        "unknown_placeholder:actions[3].settings.subject.en",
      ],
    );
  });

  test("more than ten actions on one trigger and transition are refused", () => {
    const list = Array.from({ length: 11 }, (_, i) => ({ ...actionFromApi(STORED), key: `a${i}` }));
    assert.deepEqual(
      checkActions(list).map((p) => `${p.code}:${p.path}`),
      ["too_many_actions:actions[10]"],
    );
  });

  test("problems land on their action, the rest on the list", () => {
    const problems = [{ path: "actions[1].recipients[0]" }, { path: "actions[10].key" }, { path: "actions[1]" }, { path: "version" }];
    assert.deepEqual(problemsOfAction(problems, 1), [problems[0], problems[2]]);
    assert.deepEqual(problemsOfAction(problems, 1, "recipients"), [problems[0]]);
    assert.deepEqual(generalProblems(problems), [problems[3]]);
  });

  test("a recipient is the same whatever its display name", () => {
    const r = { source: "group" as const, ref: { id: "g1", name: "CAB" }, attribute: null, serviceOwnerRole: null, participant: null, address: null };
    assert.equal(recipientIdentity(r), recipientIdentity({ ...r, ref: { id: "g1", name: "cab" } }));
    const addr = { ...r, source: "address" as const, ref: null, address: "CAB@corp.example" };
    assert.equal(recipientIdentity(addr), recipientIdentity({ ...addr, address: "cab@corp.example " }));
  });

  test("transitions an action can name: draft first, then keys only stored actions name", () => {
    assert.deepEqual(
      transitionChoices([[{ key: "submit", name: "Submit (draft)" }], [{ key: "submit", name: "Submit" }, { key: "close", name: "Close" }]], [{ transition: "gone" }, { transition: null }]),
      [
        { key: "submit", name: "Submit (draft)", orphan: false },
        { key: "close", name: "Close", orphan: false },
        { key: "gone", name: "gone", orphan: true },
      ],
    );
  });
});

describe("previewableActions (GH#852)", () => {
  test("leaves out webhooks: they reach an endpoint, never people", () => {
    const list = [
      { key: "tell", name: "Tell", kind: "inbox" as const },
      { key: "hook", name: "Hook", kind: "webhook" as const },
      { key: "mail", name: "Mail", kind: "email" as const },
    ];
    assert.deepEqual(previewableActions(list), [
      { key: "tell", name: "Tell" },
      { key: "mail", name: "Mail" },
    ]);
    assert.deepEqual(previewableActions([list[1]]), []);
  });
});
