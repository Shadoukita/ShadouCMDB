// Unit tests for when the data model counts as empty (SHAA-961). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { test } from "node:test";
import { dataModelEmpty } from "../src/lib/dataModel";

test("a fresh install with only the built-in business service class is empty", () => {
  assert.equal(dataModelEmpty([]), true);
  assert.equal(dataModelEmpty([{ systemRole: "business_service" }]), true);
});

test("one class of its own makes the data model non-empty", () => {
  assert.equal(dataModelEmpty([{ systemRole: "business_service" }, { systemRole: null }]), false);
  assert.equal(dataModelEmpty([{ systemRole: null }]), false);
});
