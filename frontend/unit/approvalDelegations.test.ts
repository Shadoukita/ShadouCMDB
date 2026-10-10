// Unit tests for approval delegations (SHAA-2916, design SHAA-1869 A6b): local date-time values, the checks
// made before sending, the request bodies and the scope label. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { afterEach, describe, test } from "node:test";
import { setLocaleForTests } from "../src/i18n/index";
import {
  canRevoke,
  delegationBody,
  delegationProblems,
  delegationScope,
  fromLocalInput,
  newDelegationDraft,
  toLocalInput,
  type DelegationDraft,
} from "../src/lib/approvalDelegations";

afterEach(() => setLocaleForTests(null));

const NOW = new Date(2026, 9, 10, 9, 30);
const draft = (patch: Partial<DelegationDraft> = {}): DelegationDraft => ({ ...newDelegationDraft(NOW), delegateId: "d", ...patch });

describe("local date-time", () => {
  test("round trip to the minute", () => {
    assert.equal(toLocalInput(NOW), "2026-10-10T09:30");
    assert.equal(fromLocalInput("2026-10-10T09:30"), NOW.toISOString());
    assert.equal(fromLocalInput(""), null);
    assert.equal(fromLocalInput("not a date"), null);
  });
  test("a new draft runs a week from now", () => {
    const d = newDelegationDraft(NOW);
    assert.equal(d.startsAt, "2026-10-10T09:30");
    assert.equal(d.endsAt, "2026-10-17T09:30");
  });
});

describe("delegationProblems", () => {
  const now = NOW.getTime();
  test("a complete draft has none", () => {
    assert.deepEqual(delegationProblems(draft(), false, "me", now), {});
  });
  test("delegate missing, or yourself", () => {
    assert.deepEqual(Object.keys(delegationProblems(draft({ delegateId: "" }), false, "me", now)), ["delegateUserId"]);
    assert.match(delegationProblems(draft({ delegateId: "me" }), false, "me", now).delegateUserId, /themselves/);
  });
  test("admin: principal required, not the principal, not the admin", () => {
    assert.ok(delegationProblems(draft(), true, "admin", now).principalUserId);
    assert.match(delegationProblems(draft({ principalId: "d" }), true, "admin", now).delegateUserId, /themselves/);
    assert.match(delegationProblems(draft({ principalId: "p", delegateId: "admin" }), true, "admin", now).delegateUserId, /cannot make yourself/);
  });
  test("the window: an end in the future, after the start, within 90 days", () => {
    assert.match(delegationProblems(draft({ endsAt: "2026-10-09T09:30" }), false, "me", now).endsAt, /future/);
    assert.match(delegationProblems(draft({ startsAt: "2026-10-20T09:30" }), false, "me", now).endsAt, /after the start/);
    assert.match(delegationProblems(draft({ endsAt: "2027-02-01T09:30" }), false, "me", now).endsAt, /90 days/);
    assert.match(delegationProblems(draft({ startsAt: "" }), false, "me", now).startsAt, /date and time/);
  });
  test("a workflow key must be a key; German too", () => {
    assert.ok(delegationProblems(draft({ definitionKey: "Change Request" }), false, "me", now).definitionKey);
    assert.deepEqual(delegationProblems(draft({ definitionKey: "change_request" }), false, "me", now), {});
    setLocaleForTests("de");
    assert.equal(delegationProblems(draft({ delegateId: "" }), false, "me", now).delegateUserId, "Wählen Sie die Vertretung.");
  });
});

describe("bodies and labels", () => {
  test("own and admin body", () => {
    const own = delegationBody(draft({ reason: "  " }), false);
    assert.deepEqual(own, { delegateUserId: "d", startsAt: NOW.toISOString(), endsAt: new Date(2026, 9, 17, 9, 30).toISOString(), definitionKey: undefined, reason: null });
    const admin = delegationBody(draft({ principalId: "p", definitionKey: "change", reason: "Leave" }), true);
    assert.equal(admin.principalUserId, "p");
    assert.equal(admin.definitionKey, "change");
    assert.equal(admin.reason, "Leave");
  });
  test("scope and revocability", () => {
    assert.equal(delegationScope({ scoped: false, definitionKey: null, definitionName: null }), "All workflows");
    assert.equal(delegationScope({ scoped: true, definitionKey: "change", definitionName: "Change" }), "Change");
    assert.match(delegationScope({ scoped: true, definitionKey: null, definitionName: null }), /may not view/);
    assert.deepEqual((["scheduled", "active", "ended", "revoked"] as const).map(canRevoke), [true, true, false, false]);
  });
});
