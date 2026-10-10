// Unit tests for the run-time approvals (SHAA-2916, design SHAA-1869 A6): status and close-reason words, the
// progress line, and how a refused decision is explained. Run: npm run test:unit -w frontend
import assert from "node:assert/strict";
import { afterEach, describe, test } from "node:test";
import type { ApiError, ApiErrorDetail } from "../src/api/client";
import { setLocaleForTests } from "../src/i18n/index";
import { approvalStatusLabel, closeReasonLabel, decisionProblem, pendingProgress, refusalMessage } from "../src/lib/approvalRuntime";

afterEach(() => setLocaleForTests(null));

describe("labels", () => {
  test("status and close reason", () => {
    assert.equal(approvalStatusLabel("pending"), "Pending");
    assert.equal(closeReasonLabel("approved"), null);
    assert.equal(closeReasonLabel(null), null);
    assert.equal(closeReasonLabel("ci_deleted"), "the CI was deleted");
  });
  test("progress, also in German", () => {
    const p = { stepNo: 1, stepCount: 2, approvals: 1, required: 2 };
    assert.equal(pendingProgress(p), "Step 1 of 2: 1 of 2 approvals");
    setLocaleForTests("de");
    assert.equal(pendingProgress(p), "Schritt 1 von 2: 1 von 2 Genehmigungen");
  });
});

describe("refusalMessage", () => {
  test("four-eyes codes get plain words", () => {
    assert.match(refusalMessage("requester"), /four-eyes/);
    assert.match(refusalMessage("earlier_step"), /earlier step/);
  });
  test("actor_of names the transition", () => {
    assert.match(refusalMessage("actor_of:implement", null, (k) => (k === "implement" ? "Implement" : k)), /"Implement"/);
    assert.match(refusalMessage("actor_of:implement"), /"implement"/);
  });
  test("unknown codes fall back to the API message, then a generic one", () => {
    assert.equal(refusalMessage("brand_new", "API says no"), "API says no");
    assert.equal(refusalMessage(null), "The decision is not allowed.");
  });
});

describe("decisionProblem", () => {
  // Shaped like the client's ApiError (importing the class would load the API client and its config).
  const err = (status: number, code: string, details: { field: string; code?: string; message: string }[] = []): ApiError => {
    const e = Object.assign(new Error("api message"), { name: "ApiError", status, code, details: details as ApiErrorDetail[] });
    return Object.assign(e, {
      fieldErrors: () => Object.fromEntries(details.map((d) => [d.field === "(root)" ? "" : d.field, d.message])),
    }) as unknown as ApiError;
  };
  test("no error, a non-API error", () => {
    assert.equal(decisionProblem(null), null);
    assert.deepEqual(decisionProblem(new Error("x")), { kind: "other" });
  });
  test("stale keeps the API's reasons", () => {
    const p = decisionProblem(err(409, "WORKFLOW_APPROVAL_STALE", [{ field: "fields.owner", message: "owner was edited" }]));
    assert.deepEqual(p, { kind: "stale", details: ["owner was edited"] });
  });
  test("self refusal explains the rule", () => {
    const p = decisionProblem(err(403, "WORKFLOW_APPROVAL_SELF", [{ field: "(root)", code: "requester", message: "x" }]));
    assert.equal(p?.kind, "refused");
    assert.match((p as { message: string }).message, /You made this request/);
  });
  test("moved on: version and conflict codes", () => {
    assert.equal(decisionProblem(err(409, "VERSION_CONFLICT"))?.kind, "moved");
    const p = decisionProblem(err(409, "CONFLICT", [{ field: "(root)", code: "not_pending", message: "x" }]));
    assert.match((p as { message: string }).message, /nothing was recorded/);
    assert.equal((decisionProblem(err(409, "CONFLICT")) as { message: string }).message, "api message");
  });
  test("a comment error goes next to the comment", () => {
    assert.deepEqual(decisionProblem(err(400, "VALIDATION_ERROR", [{ field: "comment", message: "too long" }])), { kind: "comment", message: "too long" });
    assert.deepEqual(decisionProblem(err(400, "VALIDATION_ERROR", [{ field: "other", message: "?" }])), { kind: "other" });
  });
});
