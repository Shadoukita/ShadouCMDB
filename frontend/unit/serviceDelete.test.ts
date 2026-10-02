// Unit tests for the Delete business service confirmation's gating (GH#477) and the catalog texts of the
// state and criticality badges in the business service tables (GH#478). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { afterEach, describe, test } from "node:test";
import { setLocaleForTests } from "../src/i18n/index";
import { ciStateParts, criticalityNotSet } from "../src/lib/ciState";
import { canConfirmDelete, directParentCount, parentCheckState } from "../src/lib/serviceDelete";

afterEach(() => setLocaleForTests(null));

describe("Delete business service: the included-in check gates Confirm", () => {
  test("pending: the check has not answered yet, Confirm is disabled", () => {
    const s = parentCheckState({ isError: false, isSuccess: false });
    assert.equal(s, "pending");
    assert.equal(canConfirmDelete(s), false);
  });
  test("failed: an error (503 SERVER_BUSY) keeps Confirm disabled, also over earlier data", () => {
    assert.equal(parentCheckState({ isError: true, isSuccess: false }), "failed");
    // A failed refetch keeps the old data: the count may be stale, so it still does not confirm.
    const s = parentCheckState({ isError: true, isSuccess: true });
    assert.equal(s, "failed");
    assert.equal(canConfirmDelete(s), false);
  });
  test("succeeded: Confirm is enabled and the direct parents are counted", () => {
    const s = parentCheckState({ isError: false, isSuccess: true });
    assert.equal(s, "succeeded");
    assert.equal(canConfirmDelete(s), true);
    assert.equal(directParentCount([{ direct: true }, { direct: false }, { direct: true }]), 2);
    assert.equal(directParentCount(undefined), 0);
  });
});

describe("badge texts follow the locale", () => {
  const future = "2999-01-01T00:00:00Z";
  test("en", () => {
    assert.deepEqual(ciStateParts({ active: true }, true), [{ text: "Active", tone: "ok" }]);
    assert.equal(ciStateParts({ active: false })[0].text, "Inactive");
    assert.equal(ciStateParts({ active: true, deletedAt: future })[0].text, "Deleted");
    assert.deepEqual(ciStateParts({ active: true }), []);
    assert.equal(criticalityNotSet(), "Not set");
  });
  test("de: the member and list tables render German badges", () => {
    setLocaleForTests("de");
    assert.deepEqual(ciStateParts({ active: true }, true), [{ text: "Aktiv", tone: "ok" }]);
    assert.equal(criticalityNotSet(), "Nicht gesetzt");
    const inactive = ciStateParts({ active: false, validFrom: future });
    assert.equal(inactive[0].text, "Inaktiv");
    assert.equal(inactive[0].title, "Außerhalb des Gültigkeitszeitraums");
    assert.match(inactive[1].text, /^ · wird am .*2999.* aktiv$/);
    const ending = ciStateParts({ active: true, validUntil: future }, true);
    assert.equal(ending[0].text, "Aktiv");
    assert.match(ending[1].title ?? "", /^Gültig bis .*2999/);
  });
});
