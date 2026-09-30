// Unit tests for the admin dialogs' "send only changed fields" rule (GH#289). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { changedFields } from "../src/lib/changes";

describe("changedFields", () => {
  test("keeps only what differs, so a stored refused name does not travel with a description edit", () => {
    const before = { name: "Rack\nA", description: null, isActive: true };
    assert.deepEqual(changedFields({ name: "Rack\nA", description: "Row 2", isActive: true }, before), { description: "Row 2" });
  });
  test("clearing a value sends null", () => {
    assert.deepEqual(changedFields({ description: null }, { description: "old" }), { description: null });
  });
  test("compares arrays and objects by value", () => {
    const before = { enumValues: ["a", "b"], validation: { maxLength: 10 } };
    assert.deepEqual(changedFields({ enumValues: ["a", "b"], validation: { maxLength: 10 } }, before), {});
    assert.deepEqual(changedFields({ enumValues: ["b", "a"], validation: { maxLength: 10 } }, before), { enumValues: ["b", "a"] });
  });
  test("a field the original lacked counts as changed", () => {
    assert.deepEqual(changedFields({ dataType: "integer" }, {}), { dataType: "integer" });
  });
});
