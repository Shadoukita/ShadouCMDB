// The notifications bell (SHAA-2649): what each kind says and where it leads. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { Notification } from "../src/api/notifications";
import { en } from "../src/i18n/en";
import { badgeText, describeNotification, notificationTarget } from "../src/lib/notifications";

const KINDS = ["approval_requested", "approval_closed", "workflow_transition", "import_finished", "workflow_action"] as const;
const ID = "0b9c5e8e-7a51-4c3e-9d6c-3f2a1b0c9d8e";
const INSTANCE = "5f1c2d3e-4b5a-4687-9a0b-1c2d3e4f5a6b";
const note = (kind: Notification["kind"], entityType: Notification["entityType"], data: Record<string, unknown>) =>
  ({ kind, entityType, entityId: ID, data }) as unknown as Notification;

describe("notification labels", () => {
  test("every kind has a label in the catalog", () => {
    for (const k of KINDS) assert.ok(`notifications.kind.${k}` in en, k);
  });

  test("approvals name the transition and the CI as they were at the event", () => {
    const n = note("approval_requested", "workflow_approval_requests", {
      ciLabel: "web-01",
      transitionName: "Retire",
      definitionName: "Decommission",
      requestedByName: "alice",
    });
    const d = describeNotification(n);
    assert.equal(d.title, "Retire on web-01 awaits your approval");
    assert.equal(d.detail, "Decommission · requested by alice");
  });

  test("a closed approval says how it ended and why", () => {
    const d = describeNotification(
      note("approval_closed", "workflow_approval_requests", { ciIdent: "CI-7", transitionKey: "retire", status: "rejected", closeReason: "overdue" }),
    );
    assert.equal(d.title, "retire on CI-7 was rejected");
    assert.equal(d.detail, "overdue");
  });

  test("workflow events: a transition, a cancel and a forced state read differently", () => {
    const base = { ciLabel: "db-02", fromStateName: "Active", toStateName: "Retired", actorName: "bob" };
    assert.equal(describeNotification(note("workflow_transition", "workflow_instances", { ...base, event: "transition" })).title, "db-02 moved from Active to Retired");
    assert.equal(describeNotification(note("workflow_transition", "workflow_instances", { ...base, event: "cancel" })).title, "The workflow on db-02 was cancelled");
    assert.equal(describeNotification(note("workflow_transition", "workflow_instances", { ...base, event: "force" })).title, "db-02 was set to Retired by force");
  });

  test("a configured workflow notification reads like the event it reports, with the action's name", () => {
    const base = { ciLabel: "db-02", fromStateName: "Active", toStateName: "Retired", actorName: "bob", actionName: "Tell Ops" };
    const moved = describeNotification(note("workflow_action", "workflow_instances", { ...base, event: "transition", definitionName: "Lifecycle", transitionName: "Retire" }));
    assert.equal(moved.title, "db-02 moved from Active to Retired");
    assert.equal(moved.detail, "Lifecycle · Retire · Tell Ops · by bob");
    const approval = describeNotification(
      note("workflow_action", "workflow_approval_requests", { ...base, event: "approval_request", transitionName: "Retire" }),
    );
    assert.equal(approval.title, "Approval of Retire on db-02: requested");
  });

  test("imports say whether they failed", () => {
    assert.equal(describeNotification(note("import_finished", "import_jobs", { fileName: "servers.xlsx", status: "failed" })).title, "Import of servers.xlsx failed");
    assert.equal(
      describeNotification(note("import_finished", "import_jobs", { fileName: "servers.xlsx", status: "completed_with_errors" })).title,
      "Import of servers.xlsx completed with errors",
    );
  });

  test("missing display values fall back to neutral words, never a blank", () => {
    const d = describeNotification(note("workflow_transition", "workflow_instances", { ciLabel: null, event: "transition" }));
    assert.equal(d.title, "a CI moved from unknown state to unknown state");
    assert.equal(describeNotification(note("import_finished", "import_jobs", {})).title, "Import of a file finished");
  });
});

describe("notification targets", () => {
  test("an import opens its job, a workflow event its instance", () => {
    assert.equal(notificationTarget(note("import_finished", "import_jobs", {})), `/imports/${ID}`);
    assert.equal(notificationTarget(note("workflow_transition", "workflow_instances", {})), `/workflows/${ID}`);
  });

  test("an approval opens the instance it gates; without one there is nothing to open", () => {
    assert.equal(notificationTarget(note("approval_requested", "workflow_approval_requests", { instanceId: INSTANCE })), `/workflows/${INSTANCE}`);
    assert.equal(notificationTarget(note("approval_closed", "workflow_approval_requests", { instanceId: null })), null);
  });
});

test("the badge caps at 99+", () => {
  assert.equal(badgeText(7), "7");
  assert.equal(badgeText(99), "99");
  assert.equal(badgeText(100), "99+");
});
