// Unit tests for the inventory's bulk edit (SHAA-2586, gap G10). Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { describe, test } from "node:test";
import { BULK_EDIT_LIMIT, bulkEditBlocked, bulkOutcome, bulkUpdateBody, CRITICALITY_FIELD, problemField, retryable, type BulkUpdateReport } from "../src/lib/bulkEdit";

const A = "11111111-1111-4111-8111-111111111111";
const B = "22222222-2222-4222-8222-222222222222";
const def = (key: string, dataType: "text" | "integer" | "boolean") =>
  ({ key, dataType, enumValues: null, validation: null, referenceClassId: null, lookupListId: null }) as const;
const DEFS = [def("hostname", "text"), def("cores", "integer"), def("monitored", "boolean")];

describe("bulk update body", () => {
  test("converts each value as the CI form does; an empty value clears", () => {
    const body = bulkUpdateBody(
      [A, B],
      [
        { field: "cores", value: "8" },
        { field: "monitored", value: "true" },
        { field: "hostname", value: "" },
      ],
      DEFS,
      false,
    );
    assert.deepEqual(body, { ids: [A, B], attributes: { cores: 8, monitored: true, hostname: null } });
  });
  test("criticality goes to criticalityValueId; empty clears it", () => {
    assert.deepEqual(bulkUpdateBody([A], [{ field: CRITICALITY_FIELD, value: B }], DEFS, false), { ids: [A], criticalityValueId: B });
    assert.deepEqual(bulkUpdateBody([A], [{ field: CRITICALITY_FIELD, value: "" }], DEFS, true), { ids: [A], criticalityValueId: null, allOrNothing: true });
  });
  test("nothing to send without ids or changes, or with only unknown fields", () => {
    assert.equal(bulkUpdateBody([], [{ field: "cores", value: "1" }], DEFS, false), null);
    assert.equal(bulkUpdateBody([A], [], DEFS, false), null);
    assert.equal(bulkUpdateBody([A], [{ field: "gone", value: "1" }], DEFS, false), null);
  });
});

describe("bulk update outcome", () => {
  const refusal = {
    index: 1,
    id: B,
    ok: false,
    item: null,
    error: {
      code: "VALIDATION_ERROR" as const,
      message: "Validation failed",
      details: [{ in: "body" as const, field: "attributes.cores", message: "must be at most 64", code: "max" }],
    },
  };
  test("counts the written CIs and lists the refused ones with their problems", () => {
    const report: BulkUpdateReport = { succeeded: 1, failed: 1, committed: true, results: [{ index: 0, id: A, ok: true, item: null, error: null }, refusal] };
    assert.deepEqual(bulkOutcome(report), {
      committed: true,
      updated: 1,
      heldBack: 0,
      refused: [{ id: B, code: "VALIDATION_ERROR", message: "Validation failed", problems: [{ field: "attributes.cores", message: "must be at most 64" }] }],
    });
  });
  test("all or nothing: the passing CIs are held back, none is updated", () => {
    const report: BulkUpdateReport = { succeeded: 1, failed: 1, committed: false, results: [{ index: 0, id: A, ok: true, item: null, error: null }, refusal] };
    const out = bulkOutcome(report);
    assert.equal(out.committed, false);
    assert.equal(out.updated, 0);
    assert.equal(out.heldBack, 1);
  });
  test("a detail that repeats the message is left out", () => {
    const forbidden = { ...refusal, error: { code: "FORBIDDEN" as const, message: "No edit right", details: [{ in: "body" as const, field: "", message: "No edit right", code: "forbidden" }] } };
    assert.deepEqual(bulkOutcome({ succeeded: 0, failed: 1, committed: true, results: [forbidden] }).refused[0].problems, []);
  });
  test("problem fields read as labels", () => {
    const labelOf = (k: string) => (k === "cores" ? "CPU cores" : undefined);
    assert.equal(problemField("attributes.cores", labelOf, "Criticality"), "CPU cores");
    assert.equal(problemField("criticalityValueId", labelOf, "Criticality"), "Criticality");
    assert.equal(problemField("attributes.other", labelOf, "Criticality"), "attributes.other");
  });
});

describe("bulk edit guard", () => {
  test("refuses more than 500 CIs before sending, then a user without an edit right", () => {
    assert.equal(BULK_EDIT_LIMIT, 500);
    assert.equal(bulkEditBlocked(500, true), null);
    assert.equal(bulkEditBlocked(501, true), "tooMany");
    assert.equal(bulkEditBlocked(501, false), "tooMany");
    assert.equal(bulkEditBlocked(3, false), "noPermission");
  });
});

test("refused CIs that no longer exist leave the selection", () => {
  assert.equal(retryable("VALIDATION_ERROR"), true);
  assert.equal(retryable("FORBIDDEN"), true);
  assert.equal(retryable("NOT_FOUND"), false);
  assert.equal(retryable("GONE"), false);
});
