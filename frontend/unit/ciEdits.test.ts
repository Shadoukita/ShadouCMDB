// Unit tests for what a CI form changed (SHAA-1644). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { ciEdits, coreToApi, type CiFormState } from "../src/lib/ciEdits";

const defs = [
  { key: "name", dataType: "text" },
  { key: "cpu_cores", dataType: "integer" },
  { key: "notes", dataType: "text", validation: { multiline: true } },
] as CiFormState["defs"];
const core = { ident: "CI-1", validFrom: "2026-10-01T08:00", validUntil: "" };
const values = { name: "srv-01", cpu_cores: "16", notes: "" };
const state = (over: Partial<CiFormState> = {}): CiFormState => ({
  defs,
  core: { ...core },
  initialCore: { ...core },
  criticalityId: "",
  initialCriticality: "",
  values: { ...values },
  initialValues: { ...values },
  ...over,
});

describe("ciEdits", () => {
  test("nothing changed: nothing to save", () => {
    assert.equal(ciEdits(state()), null);
  });
  test("a value typed back to what it was is no change", () => {
    assert.equal(ciEdits(state({ values: { ...values, cpu_cores: "16" } })), null);
  });
  test("only the changed attributes are sent, as API values", () => {
    assert.deepEqual(ciEdits(state({ values: { ...values, cpu_cores: "32" } })), { attributes: { cpu_cores: 32 } });
  });
  test("an emptied attribute is cleared", () => {
    assert.deepEqual(ciEdits(state({ values: { ...values, name: "" } })), { attributes: { name: null } });
  });
  test("an attribute the definitions arrived for after the CI counts from its seeded value", () => {
    assert.equal(ciEdits(state({ values: { ...values, extra: "x" }, initialValues: { ...values, extra: "x" } })), null);
  });
  test("core fields: a set valid until, a cleared one, an emptied ident kept", () => {
    assert.deepEqual(Object.keys(ciEdits(state({ core: { ...core, validUntil: "2027-01-01T00:00" } })) ?? {}), ["validUntil"]);
    assert.deepEqual(
      ciEdits(state({ core: { ...core }, initialCore: { ...core, validUntil: "2027-01-01T00:00" } })),
      { validUntil: null },
    );
    assert.equal(ciEdits(state({ core: { ...core, ident: "  " } })), null);
    assert.deepEqual(ciEdits(state({ core: { ...core, ident: " CI-2 " } })), { ident: "CI-2" });
  });
  test("criticality set and cleared", () => {
    assert.deepEqual(ciEdits(state({ criticalityId: "c1" })), { criticalityValueId: "c1" });
    assert.deepEqual(ciEdits(state({ initialCriticality: "c1" })), { criticalityValueId: null });
  });
});

describe("coreToApi", () => {
  test("empty values are null; the ident is trimmed", () => {
    assert.deepEqual(coreToApi({ ident: " ", validFrom: "", validUntil: "" }), { ident: null, validFrom: null, validUntil: null });
    assert.equal(coreToApi({ ident: " CI-9 ", validFrom: "", validUntil: "" }).ident, "CI-9");
  });
});
