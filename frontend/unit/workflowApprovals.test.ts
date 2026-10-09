// Unit tests for the approvals part of the workflow designer (SHAA-2798, design SHAA-1869 A5): the
// policy to and from the draft body, the due period, the client-side checks, the approvers matrix
// and its PUT body, where the approvers lint lands, and the publish warning. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { afterEach, describe, test } from "node:test";
import { setLocaleForTests } from "../src/i18n/index";
import {
  approverFromApi,
  approverIdentity,
  approverLabel,
  approversBody,
  changedPolicies,
  checkSteps,
  describeDuration,
  durationMinutes,
  isComplete,
  joinDuration,
  newStep,
  otherProblems,
  splitDuration,
  stepFromApi,
  stepProblems,
  stepRows,
  stepToApi,
  type DraftApprover,
} from "../src/lib/workflowApprovals";
import { checkDraft, draftFromVersion, toDraftBody } from "../src/lib/workflowDraft";

afterEach(() => setLocaleForTests(null));

// The design's draft transition body (§10.1).
const POLICY = {
  steps: [
    { key: "tech", name: "Technical review", requiredApprovals: 1, dueAfter: "P2D" },
    { key: "cab", name: "CAB", requiredApprovals: 2, dueAfter: "P5D", onOverdue: "flag" as const, distinctFromEarlier: true, excludeActorsOf: ["implement"], allowApiTokens: false },
  ],
};
const VERSION = {
  initialState: "planned",
  states: [
    { key: "planned", name: "Planned", category: "open" as const },
    { key: "approved", name: "Approved", category: "active" as const },
    { key: "done", name: "Done", category: "done" as const, terminal: true },
  ],
  transitions: [
    { key: "approve", name: "Approve", from: "planned", to: "approved", approval: POLICY },
    { key: "implement", name: "Implement", from: "approved", to: "done" },
  ],
  layout: {},
};

describe("the policy in the draft", () => {
  test("steps take the API's defaults and round-trip into the draft body", () => {
    const d = draftFromVersion(VERSION as never);
    assert.equal(d.transitions[0].approval.length, 2);
    assert.deepEqual(d.transitions[0].approval[0], {
      key: "tech",
      name: "Technical review",
      requiredApprovals: 1,
      dueAfter: "P2D",
      onOverdue: "flag",
      distinctFromEarlier: true,
      excludeActorsOf: [],
      allowApiTokens: false,
    });
    const body = toDraftBody(d);
    assert.deepEqual(body.transitions[0].approval?.steps[1], POLICY.steps[1]);
  });
  test("a transition without steps sends no approval, so its checksum stays as before approvals", () => {
    const body = toDraftBody(draftFromVersion(VERSION as never));
    assert.equal("approval" in body.transitions[1], false);
  });
  test("no due period sends no dueAfter", () => {
    assert.equal("dueAfter" in stepToApi(stepFromApi({ key: "a", name: "A" })), false);
  });
  test("a new step gets the next free key and the defaults", () => {
    const s = newStep([{ key: "step_1" }, { key: "step_3" }]);
    assert.equal(s.key, "step_4");
    assert.equal(s.requiredApprovals, 1);
    assert.equal(s.distinctFromEarlier, true);
  });
});

describe("the due period", () => {
  test("ISO durations of weeks, days, hours and minutes", () => {
    assert.equal(durationMinutes("P2D"), 2880);
    assert.equal(durationMinutes("PT4H"), 240);
    assert.equal(durationMinutes("P1DT12H"), 2160);
    assert.equal(durationMinutes("P1W"), 10080);
    assert.equal(durationMinutes("PT"), null);
    assert.equal(durationMinutes("P1M"), null, "months are not accepted");
    assert.equal(durationMinutes("2 days"), null);
  });
  test("shown in the largest unit that divides it, and joined back", () => {
    assert.deepEqual(splitDuration("P1DT12H"), { amount: 36, unit: "hours" });
    assert.deepEqual(splitDuration("P14D"), { amount: 2, unit: "weeks" });
    assert.deepEqual(splitDuration("PT90M"), { amount: 90, unit: "minutes" });
    assert.deepEqual(splitDuration(null), { amount: null, unit: "days" });
    assert.equal(joinDuration(36, "hours"), "PT36H");
    assert.equal(joinDuration(2, "weeks"), "P2W");
    assert.equal(joinDuration(0, "days"), null);
    assert.equal(joinDuration(null, "days"), null);
  });
  test("described in en and de", () => {
    assert.equal(describeDuration("P2D"), "2 days");
    assert.equal(describeDuration("PT1H"), "1 hour");
    setLocaleForTests("de");
    assert.equal(describeDuration("P2D"), "2 Tage");
    assert.equal(describeDuration(null), "Keine Frist");
  });
});

describe("client-side checks", () => {
  const ok = () => stepFromApi({ key: "tech", name: "Tech", requiredApprovals: 1 });
  test("a valid policy has no problems", () => {
    assert.deepEqual(checkSteps([ok()]), []);
  });
  test("the column checks of design §3.1", () => {
    const codes = (steps: ReturnType<typeof ok>[]) => checkSteps(steps).map((p) => `${p.path}:${p.code}`);
    assert.deepEqual(codes([{ ...ok(), key: "Bad" }]), ["approval.steps[0].key:invalid"]);
    assert.deepEqual(codes([ok(), ok()]), ["approval.steps[1].key:duplicate"]);
    assert.deepEqual(codes([{ ...ok(), name: " " }]), ["approval.steps[0].name:required"]);
    assert.deepEqual(codes([{ ...ok(), requiredApprovals: 21 }]), ["approval.steps[0].requiredApprovals:range"]);
    assert.deepEqual(codes([{ ...ok(), requiredApprovals: 0 }]), ["approval.steps[0].requiredApprovals:range"]);
    assert.deepEqual(codes([{ ...ok(), dueAfter: "PT10M" }]), ["approval.steps[0].dueAfter:range"]);
    assert.deepEqual(codes([{ ...ok(), dueAfter: "P91D" }]), ["approval.steps[0].dueAfter:range"]);
    assert.deepEqual(codes([{ ...ok(), onOverdue: "reject" }]), ["approval.steps[0].onOverdue:needs_due"]);
    assert.deepEqual(codes([{ ...ok(), onOverdue: "reject", dueAfter: "PT15M" }]), []);
    const six = Array.from({ length: 6 }, (_, i) => ({ ...ok(), key: `s${i}` }));
    assert.deepEqual(codes(six), ["approval.steps:too_many"]);
  });
  test("checkDraft places step problems on their transition", () => {
    const d = draftFromVersion(VERSION as never);
    d.transitions[0].approval[1].requiredApprovals = 99;
    const p = checkDraft(d);
    assert.equal(p.length, 1);
    assert.deepEqual(p[0].target, { kind: "transition", key: "approve" });
    assert.equal(p[0].path, "transitions[0].approval.steps[1].requiredApprovals");
  });
});

describe("the approvers matrix", () => {
  const api = [
    { transitionKey: "approve", stepKey: "tech", role: "approver" as const, source: "profile" as const, profile: { id: "p1", name: "Platform engineers" }, group: null, user: null, attribute: null, serviceOwnerRole: null },
    { transitionKey: "approve", stepKey: "cab", role: "approver" as const, source: "group" as const, profile: null, group: { id: "g1", name: "CAB" }, user: null, attribute: null, serviceOwnerRole: null },
    { transitionKey: "approve", stepKey: "cab", role: "escalation" as const, source: "user" as const, profile: null, group: null, user: { id: "u1", name: "head.of.ops" }, attribute: null, serviceOwnerRole: null },
    {
      transitionKey: "approve",
      stepKey: "tech",
      role: "approver" as const,
      source: "ci_attribute" as const,
      profile: null,
      group: null,
      user: null,
      attribute: { id: "a1", key: "owner", classKey: "change_request", label: "Owner" },
      serviceOwnerRole: null,
    },
    { transitionKey: "approve", stepKey: "cab", role: "approver" as const, source: "service_owner" as const, profile: null, group: null, user: null, attribute: null, serviceOwnerRole: "business" as const },
  ];
  test("the five sources (format 9's example) become the PUT body by id", () => {
    const list = api.map(approverFromApi);
    const body = approversBody(list);
    assert.equal(body.length, 5);
    assert.ok(body.some((b) => b.source === "profile" && b.profile === "p1" && b.stepKey === "tech"));
    assert.ok(body.some((b) => b.source === "group" && b.group === "g1"));
    assert.ok(body.some((b) => b.source === "user" && b.user === "u1" && b.role === "escalation"));
    assert.ok(body.some((b) => b.source === "ci_attribute" && b.attribute === "a1"));
    assert.ok(body.some((b) => b.source === "service_owner" && b.serviceOwnerRole === "business"));
    for (const b of body) assert.equal(Object.keys(b).length, 5, "exactly the field the source names");
  });
  test("the body is sorted, so the same set compares equal in any order", () => {
    const list = api.map(approverFromApi);
    assert.deepEqual(approversBody(list), approversBody([...list].reverse()));
  });
  test("incomplete assignments are not sent, and identities tell duplicates apart", () => {
    const half: DraftApprover = { transitionKey: "approve", stepKey: "tech", role: "approver", source: "group", ref: null, attribute: null, serviceOwnerRole: null };
    assert.equal(isComplete(half), false);
    assert.equal(approversBody([half]).length, 0);
    const [a, b] = [approverFromApi(api[1]), approverFromApi(api[1])];
    assert.equal(approverIdentity(a), approverIdentity(b));
    assert.notEqual(approverIdentity(a), approverIdentity({ ...b, role: "escalation" }));
  });
  test("labels in en and de", () => {
    const list = api.map(approverFromApi);
    assert.equal(approverLabel(list[1]), "Group CAB");
    assert.equal(approverLabel(list[3]), "The person in the field Owner");
    setLocaleForTests("de");
    assert.equal(approverLabel(list[4]), "Fachliche Verantwortliche der Geschäftsservices des CI");
  });
  test("rows: the draft's steps first, then the current version's, then pairs only assignments name", () => {
    const draftT = [{ key: "approve", name: "Approve (new)", approval: { steps: [{ key: "tech", name: "Tech", requiredApprovals: 1 }] } }];
    const currentT = [{ key: "approve", name: "Approve", approval: POLICY }, { key: "implement", name: "Implement" }];
    const rows = stepRows([draftT, currentT], [{ transitionKey: "old", stepKey: "gone" }, { transitionKey: "approve", stepKey: "cab" }]);
    assert.deepEqual(
      rows.map((r) => [r.transitionName, r.stepKey, r.requiredApprovals, r.orphan]),
      [
        ["Approve (new)", "tech", 1, false],
        ["Approve", "cab", 2, false],
        ["old", "gone", 1, true],
      ],
    );
  });
  test("the approvers lint lands on its step by path, the rest stays general", () => {
    const problems = [
      { path: "transitions.approve.steps.cab", code: "understaffed", message: "", severity: "warning" as const },
      { path: "transitions.approve.steps.cab_2", code: "no_approvers", message: "", severity: "warning" as const },
      { path: "approvers[3]", code: "unknown_step", message: "", severity: "warning" as const },
    ];
    assert.deepEqual(stepProblems(problems, "approve", "cab").map((p) => p.code), ["understaffed"]);
    const rows = [{ transitionKey: "approve", stepKey: "cab" }];
    assert.deepEqual(otherProblems(problems, rows).map((p) => p.code), ["no_approvers", "unknown_step"]);
  });
});

describe("the publish warning", () => {
  test("names transitions whose policy is new or changed against the current version", () => {
    const d = draftFromVersion(VERSION as never);
    assert.deepEqual(changedPolicies(d.transitions, VERSION.transitions as never), []);
    assert.deepEqual(changedPolicies(d.transitions, [{ key: "approve" }]), ["Approve"]);
    d.transitions[0].approval[0].requiredApprovals = 2;
    assert.deepEqual(changedPolicies(d.transitions, VERSION.transitions as never), ["Approve"]);
    assert.deepEqual(changedPolicies(d.transitions, undefined), ["Approve"]);
  });
});
