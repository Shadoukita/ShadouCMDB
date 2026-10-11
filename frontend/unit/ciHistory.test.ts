// Unit tests for the CI point-in-time view and compare (SHAA-3131). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { AuditEntry, Ci } from "../src/api/queries";
import {
  diffSnapshots,
  fieldValue,
  HISTORY_WALK_LIMIT,
  isoToUtcInput,
  purgeCutoff,
  snapshotOf,
  stateAt,
  utcInputToIso,
  workflowEventsBetween,
  type HistoryWalk,
} from "../src/lib/ciHistory";

const CI_ID = "00000000-0000-0000-0000-0000000000c1";
const CLASS = "00000000-0000-0000-0000-00000000c1a5";
const SECRET_HOST = "00000000-0000-0000-0000-00000000beef";

function ci(attributes: Ci["attributes"], extra: Partial<Ci> = {}): Ci {
  return {
    id: CI_ID,
    ident: "CI-7K3M9Q2X",
    label: String(attributes.name ?? "CI-7K3M9Q2X"),
    classId: CLASS,
    class: { id: CLASS, key: "server", name: "Server" } as Ci["class"],
    validFrom: "2026-01-01T00:00:00Z",
    validUntil: null,
    active: true,
    version: 1,
    createdAt: "2026-01-01T09:00:00Z",
    updatedAt: "2026-01-01T09:00:00Z",
    deletedAt: null,
    criticality: null,
    attributes,
    attributeReferences: {},
    ...extra,
  };
}

let seq = 0;
function entry(action: AuditEntry["action"], occurredAt: string, oldValue: unknown, newValue: unknown, extra: Partial<AuditEntry> = {}): AuditEntry {
  return {
    id: ++seq,
    occurredAt,
    actorType: "user",
    actorId: null,
    actorName: "admin",
    action,
    entityType: "configuration_items",
    entityId: CI_ID,
    oldValue,
    newValue,
    requestId: null,
    redacted: false,
    ...extra,
  };
}

// The history: created on 1 Jan, renamed on 1 Feb, given 16 GB RAM on 1 Mar, moved to 32 GB on 1 Apr.
const v1 = ci({ name: "web-01" });
const v2 = ci({ name: "web-02" });
const v3 = ci({ name: "web-02", ram_gb: 16 });
const v4 = ci({ name: "web-02", ram_gb: 32 });
const created = entry("create", "2026-01-01T09:00:00Z", null, v1);
const renamed = entry("update", "2026-02-01T09:00:00Z", v1, v2);
const ram16 = entry("update", "2026-03-01T09:00:00Z", v2, v3);
const ram32 = entry("update", "2026-04-01T09:00:00Z", v3, v4);
const transition = entry("workflow.transition", "2026-03-15T09:00:00Z", { stateKey: "draft" }, { stateKey: "approved", transitionKey: "approve" });
const all = [ram32, transition, ram16, renamed, created];
const walk = (since: string, entries = all): HistoryWalk => ({ complete: true, since, entries: entries.filter((e) => e.occurredAt >= since) });
const value = (state: ReturnType<typeof stateAt>, key: Parameters<typeof fieldValue>[1]) => {
  assert.equal(state.kind, "snapshot");
  return state.kind === "snapshot" ? fieldValue(state.snapshot, key) : undefined;
};

describe("stateAt", () => {
  test("before the CI was created it did not exist yet", () => {
    const s = stateAt(v4, walk("2025-12-31T00:00:00Z"), "2025-12-31T00:00:00Z", null);
    assert.deepEqual(s, { kind: "notYet", createdAt: v4.createdAt });
  });
  test("walks back through every update after the moment", () => {
    assert.deepEqual(value(stateAt(v4, walk("2026-01-15T00:00:00Z"), "2026-01-15T00:00:00Z", null), "attributes.name"), { kind: "value", value: "web-01" });
    assert.deepEqual(value(stateAt(v4, walk("2026-02-15T00:00:00Z"), "2026-02-15T00:00:00Z", null), "attributes.name"), { kind: "value", value: "web-02" });
    assert.deepEqual(value(stateAt(v4, walk("2026-02-15T00:00:00Z"), "2026-02-15T00:00:00Z", null), "attributes.ram_gb"), { kind: "unset" });
    assert.deepEqual(value(stateAt(v4, walk("2026-03-20T00:00:00Z"), "2026-03-20T00:00:00Z", null), "attributes.ram_gb"), { kind: "value", value: 16 });
  });
  test("a change made at exactly the moment counts as made", () => {
    assert.deepEqual(value(stateAt(v4, walk("2026-03-01T09:00:00Z"), "2026-03-01T09:00:00Z", null), "attributes.ram_gb"), { kind: "value", value: 16 });
  });
  test("after the last change it is the current CI", () => {
    const s = stateAt(v4, walk("2026-05-01T00:00:00Z"), "2026-05-01T00:00:00Z", null);
    assert.deepEqual(s, { kind: "snapshot", snapshot: snapshotOf(v4) });
  });
  test("a walk from an earlier moment serves a later one too", () => {
    const w = walk("2026-01-15T00:00:00Z");
    assert.deepEqual(value(stateAt(v4, w, "2026-03-20T00:00:00Z", null), "attributes.ram_gb"), { kind: "value", value: 16 });
  });
  test("before a retention purge's cutoff the history is gone, even when no entry after it is left", () => {
    // The purge removed everything before 1 Mar: the rename and the creation are gone.
    const kept = [ram32, transition, ram16];
    assert.deepEqual(stateAt(v4, walk("2026-02-15T00:00:00Z", kept), "2026-02-15T00:00:00Z", "2026-03-01T00:00:00Z"), {
      kind: "purged",
      before: "2026-03-01T00:00:00Z",
    });
    // A CI unchanged since the purge: its current values are no evidence for a moment before the cutoff.
    assert.equal(stateAt(v4, walk("2026-02-15T00:00:00Z", []), "2026-02-15T00:00:00Z", "2026-03-01T00:00:00Z").kind, "purged");
    // After the cutoff every entry is kept.
    assert.deepEqual(value(stateAt(v4, walk("2026-03-20T00:00:00Z", kept), "2026-03-20T00:00:00Z", "2026-03-01T00:00:00Z"), "attributes.ram_gb"), {
      kind: "value",
      value: 16,
    });
  });
  test("a CI created after the cutoff is not reported as purged before it existed", () => {
    const fresh = ci({ name: "new" }, { createdAt: "2026-06-01T00:00:00Z" });
    assert.equal(stateAt(fresh, walk("2026-02-01T00:00:00Z", []), "2026-02-01T00:00:00Z", "2026-03-01T00:00:00Z").kind, "notYet");
  });
  test("more entries than the bound: stop and say so", () => {
    const s = stateAt(v4, { complete: false, since: "2026-01-02T00:00:00Z", total: HISTORY_WALK_LIMIT + 1 }, "2026-01-02T00:00:00Z", null);
    assert.deepEqual(s, { kind: "tooMany", total: HISTORY_WALK_LIMIT + 1 });
  });
  test("a withheld entry gives no state rather than a guess", () => {
    const withheld = entry("update", "2026-02-01T09:00:00Z", null, null, { redacted: true });
    const s = stateAt(v4, walk("2026-01-15T00:00:00Z", [ram32, ram16, withheld, created]), "2026-01-15T00:00:00Z", null);
    assert.deepEqual(value(s, "attributes.name"), { kind: "hidden" });
    assert.deepEqual(value(s, "ident"), { kind: "hidden" });
  });
  test("a moment while the CI was deleted says so", () => {
    const gone = ci({ name: "web-02" }, { deletedAt: "2026-02-10T00:00:00Z" });
    const deleted = entry("delete", "2026-02-10T00:00:00Z", v2, gone);
    const restored = entry("restore", "2026-02-20T00:00:00Z", gone, v2);
    const s = stateAt(v2, walk("2026-02-15T00:00:00Z", [restored, deleted]), "2026-02-15T00:00:00Z", null);
    assert.equal(s.kind === "snapshot" && s.snapshot.deleted, true);
  });
  test("workflow steps carry no CI representation and are not taken for one", () => {
    assert.deepEqual(value(stateAt(v4, walk("2026-03-10T00:00:00Z"), "2026-03-10T00:00:00Z", null), "attributes.ram_gb"), { kind: "value", value: 16 });
  });
});

describe("restricted field visibility", () => {
  // The reader may not view the class of the host the CI runs on: the audit log keeps only its id in
  // `attributes` and marks the reference hidden, as the item endpoints do.
  const hidden = { id: SECRET_HOST, name: null, deleted: false, hidden: true };
  const before = ci({ name: "web-02", runs_on: SECRET_HOST }, { attributeReferences: { runs_on: hidden } });
  const after = ci({ name: "web-02", runs_on: "00000000-0000-0000-0000-00000000f00d" }, { attributeReferences: { runs_on: { ...hidden, id: "00000000-0000-0000-0000-00000000f00d" } } });
  const moved = entry("update", "2026-05-01T00:00:00Z", before, after);

  test("the point-in-time view shows the field as hidden, never its id", () => {
    const s = stateAt(after, walk("2026-04-15T00:00:00Z", [moved]), "2026-04-15T00:00:00Z", null);
    assert.deepEqual(value(s, "attributes.runs_on"), { kind: "hidden" });
    // Not in the reconstructed state at all, so no view of it can show it.
    assert.ok(!JSON.stringify(s).includes(SECRET_HOST));
    assert.ok(!JSON.stringify(snapshotOf(after)).includes("00000000-0000-0000-0000-00000000f00d"));
  });
  test("a workflow action's partial entry does not reveal a reference the newer state hides", () => {
    const action = entry("update", "2026-05-02T00:00:00Z", { classId: CLASS, attributes: { runs_on: SECRET_HOST } }, {
      classId: CLASS,
      attributes: { runs_on: "00000000-0000-0000-0000-00000000f00d" },
      source: { kind: "workflow_action" },
    });
    const s = stateAt(after, walk("2026-05-01T12:00:00Z", [action]), "2026-05-01T12:00:00Z", null);
    assert.deepEqual(value(s, "attributes.runs_on"), { kind: "hidden" });
    assert.ok(!JSON.stringify(s).includes(SECRET_HOST));
  });
  test("the comparison does not tell whether a hidden reference changed", () => {
    const changes = diffSnapshots(snapshotOf(before), snapshotOf(after), ["name", "runs_on"]);
    assert.deepEqual(changes, []);
  });
  test("a reference that becomes hidden shows the visible side only", () => {
    const visible = ci({ runs_on: "00000000-0000-0000-0000-0000000000aa" }, { attributeReferences: { runs_on: { id: "00000000-0000-0000-0000-0000000000aa", name: "host-a", deleted: false, hidden: false } } });
    const changes = diffSnapshots(snapshotOf(visible), snapshotOf(before), ["runs_on"]);
    const runsOn = changes.find((c) => c.key === "attributes.runs_on");
    assert.deepEqual(runsOn, { key: "attributes.runs_on", from: { kind: "value", value: "00000000-0000-0000-0000-0000000000aa" }, to: { kind: "hidden" } });
    assert.ok(!JSON.stringify(changes).includes(SECRET_HOST));
  });
});

describe("entries that do not record the whole CI", () => {
  test("a workflow attribute action puts back only the attributes it set", () => {
    const action = entry("update", "2026-04-15T00:00:00Z", { classId: CLASS, attributes: { ram_gb: 32 } }, {
      classId: CLASS,
      attributes: { ram_gb: 64 },
      source: { kind: "workflow_action", definitionKey: "resize", versionNo: 1, transitionKey: "grow" },
    });
    const v5 = ci({ name: "web-02", ram_gb: 64 });
    const s = stateAt(v5, walk("2026-04-10T00:00:00Z", [action, ram32]), "2026-04-10T00:00:00Z", null);
    assert.deepEqual(value(s, "attributes.ram_gb"), { kind: "value", value: 32 });
    assert.deepEqual(value(s, "attributes.name"), { kind: "value", value: "web-02" });
    // Before the action, after the update that recorded the whole CI: 16 GB.
    const earlier = stateAt(v5, walk("2026-03-20T00:00:00Z", [action, ram32, transition, ram16]), "2026-03-20T00:00:00Z", null);
    assert.deepEqual(value(earlier, "attributes.ram_gb"), { kind: "value", value: 16 });
  });
  test("a business service's member change touches no field", () => {
    const members = entry("update", "2026-04-15T00:00:00Z", { members: { added: [], removed: [] } }, { members: { added: [SECRET_HOST], removed: [] } });
    assert.deepEqual(value(stateAt(v4, walk("2026-04-10T00:00:00Z", [members]), "2026-04-10T00:00:00Z", null), "attributes.ram_gb"), { kind: "value", value: 32 });
  });
  test("the 0036 criticality move leaves the criticality before it not recorded", () => {
    const moved = entry("update", "2026-04-15T00:00:00Z", { classId: CLASS, criticality: "high" }, { classId: CLASS, criticality: { id: "c1", key: "high", name: "High" }, from: "criticality" });
    const s = stateAt(v4, walk("2026-04-10T00:00:00Z", [moved]), "2026-04-10T00:00:00Z", null);
    assert.deepEqual(value(s, "criticality"), { kind: "notRecorded" });
    assert.deepEqual(value(s, "attributes.name"), { kind: "value", value: "web-02" });
  });
  test("an entry in an older shape leaves every earlier value not recorded", () => {
    const legacy = entry("update", "2026-04-15T00:00:00Z", { statusId: "s1", attributes: { name: "web-02" }, label: "web-02" }, { statusId: "s2" });
    const s = stateAt(v4, walk("2026-04-10T00:00:00Z", [legacy]), "2026-04-10T00:00:00Z", null);
    assert.deepEqual(value(s, "attributes.name"), { kind: "notRecorded" });
    assert.deepEqual(value(s, "class"), { kind: "notRecorded" });
  });
  test("entries of other kinds on the CI's id (a layout override) are not taken for CI changes", () => {
    const layout = entry("update", "2026-04-15T00:00:00Z", { tabs: [] }, { tabs: [1] }, { entityType: "ci_layout_overrides" });
    assert.deepEqual(value(stateAt(v4, walk("2026-04-10T00:00:00Z", [layout]), "2026-04-10T00:00:00Z", null), "attributes.ram_gb"), { kind: "value", value: 32 });
  });
});

describe("diffSnapshots", () => {
  test("the net difference between two moments, not every step between them", () => {
    // web-01 → web-02 → (16 GB) → 32 GB: from 15 Jan to 1 May the name changed and 32 GB was set.
    const changes = diffSnapshots(snapshotOf(v1), snapshotOf(v4), ["name", "ram_gb"]);
    assert.deepEqual(changes, [
      { key: "attributes.name", from: { kind: "value", value: "web-01" }, to: { kind: "value", value: "web-02" } },
      { key: "attributes.ram_gb", from: { kind: "unset" }, to: { kind: "value", value: 32 } },
    ]);
  });
  test("built-in fields are compared by name; a change back and forth nets out", () => {
    const crit = { id: "c1", key: "high", name: "High", rank: 1 } as Ci["criticality"];
    assert.deepEqual(diffSnapshots(snapshotOf(v1), snapshotOf(ci({ name: "web-01" }, { criticality: crit })), []), [
      { key: "criticality", from: { kind: "unset" }, to: { kind: "value", value: "c1", name: "High" } },
    ]);
    assert.deepEqual(diffSnapshots(snapshotOf(v1), snapshotOf(ci({ name: "web-01" })), ["name"]), []);
    // A value renamed since is the same value: compared by id, not by the name recorded with it.
    assert.deepEqual(diffSnapshots(snapshotOf(ci({}, { criticality: crit })), snapshotOf(ci({}, { criticality: { ...crit!, name: "Hoch" } })), []), []);
  });
  test("attributes follow the definitions' order, retired or unknown keys last", () => {
    const changes = diffSnapshots(snapshotOf(ci({})), snapshotOf(ci({ zeta: 1, alpha: 2, retired_field: "x" })), ["zeta", "alpha"]);
    assert.deepEqual(
      changes.map((c) => c.key),
      ["attributes.zeta", "attributes.alpha", "attributes.retired_field"],
    );
  });
});

describe("workflowEventsBetween", () => {
  test("the workflow steps in the range, oldest first", () => {
    assert.deepEqual(workflowEventsBetween(walk("2026-03-01T00:00:00Z"), "2026-03-01T00:00:00Z", "2026-04-01T00:00:00Z"), [transition]);
    assert.deepEqual(workflowEventsBetween(walk("2026-03-16T00:00:00Z"), "2026-03-16T00:00:00Z", "2026-05-01T00:00:00Z"), []);
  });
});

describe("purgeCutoff", () => {
  test("the latest cutoff of the purges", () => {
    const purge = (cutoff: string, scope = "changes") => entry("audit.purge", "2026-06-01T00:00:00Z", null, { scope, cutoff }, { entityType: "audit_log" });
    assert.equal(purgeCutoff([purge("2026-01-01T00:00:00Z"), purge("2026-03-01T00:00:00Z")]), "2026-03-01T00:00:00Z");
    // A purge of sign-in events removes no CI history.
    assert.equal(purgeCutoff([purge("2026-01-01T00:00:00Z"), purge("2026-05-01T00:00:00Z", "auth")]), "2026-01-01T00:00:00Z");
    assert.equal(purgeCutoff([]), null);
  });
});

describe("UTC picker values", () => {
  test("read as UTC whatever the browser's time zone", () => {
    assert.equal(utcInputToIso("2026-03-01T00:00"), "2026-03-01T00:00:00.000Z");
    assert.equal(utcInputToIso("2026-03-01"), null);
    assert.equal(isoToUtcInput("2026-03-01T13:45:30.123Z"), "2026-03-01T13:45");
  });
});
