import assert from "node:assert/strict";
import { describe, test } from "node:test";
import type { ApiError } from "../src/api/client";
import type { ImportJob } from "../src/api/imports";
import { checkFile, columnLetter, defaultSheet, errorPlace, pollInterval, shownStep, stepOf, uploadErrorMessage } from "../src/lib/imports";

const LIMITS = { maxFileBytes: 50 * 1024 * 1024, maxRows: 100_000, maxColumns: 200, maxCellChars: 10_000 };
type StepJob = Parameters<typeof stepOf>[0];
const job = (status: StepJob["status"], extra: Partial<StepJob> = {}): StepJob => ({ status, phase: null, mapping: null, summary: null, ...extra });
const mapping = { classKey: "server", mode: "create_or_update", columns: [] } as StepJob["mapping"];
const summary = { create: 1, update: 0, unchanged: 0, errorRows: 0, warnings: 0, relationshipsToAdd: 0, issuesTotal: 0 };

describe("the wizard step comes from the job (a reload resumes at the right place)", () => {
  test("reading the file is step 1, a read file without a mapping too", () => {
    assert.equal(stepOf(job("uploading")), 1);
    assert.equal(stepOf(job("analysing", { phase: "analyse" })), 1);
    assert.equal(stepOf(job("queued", { phase: "analyse" })), 1);
    assert.equal(stepOf(job("ready")), 1);
  });
  test("a mapped file is step 2, the dry run step 3, the commit step 4", () => {
    assert.equal(stepOf(job("ready", { mapping })), 2);
    assert.equal(stepOf(job("queued", { phase: "validate" })), 3);
    assert.equal(stepOf(job("validating", { phase: "validate" })), 3);
    assert.equal(stepOf(job("validated")), 3);
    assert.equal(stepOf(job("queued", { phase: "commit" })), 4);
    assert.equal(stepOf(job("committing", { phase: "commit" })), 4);
    assert.equal(stepOf(job("completed_with_errors")), 4);
  });
  test("failed and cancelled jobs stay at the step of their phase", () => {
    assert.equal(stepOf(job("failed", { phase: "analyse" })), 1);
    assert.equal(stepOf(job("failed", { phase: "validate" })), 3);
    assert.equal(stepOf(job("cancelled", { phase: "commit" })), 4);
  });
  test("an expired job shows the last step it reached", () => {
    assert.equal(stepOf(job("expired")), 1);
    assert.equal(stepOf(job("expired", { mapping })), 2);
    assert.equal(stepOf(job("expired", { mapping, summary })), 3);
    assert.equal(stepOf(job("expired", { mapping, summary: { ...summary, committed: { created: 1, updated: 0, unchanged: 0, skipped: 0, failed: 0, relationshipsAdded: 0 } } })), 4);
  });
  test("?step= reopens earlier steps, and step 2 once the file is read, but never a later one", () => {
    assert.equal(shownStep(job("ready"), "2"), 2);
    assert.equal(shownStep(job("analysing"), "2"), 1);
    assert.equal(shownStep(job("validated"), "1"), 1);
    assert.equal(shownStep(job("validated"), "4"), 3);
    assert.equal(shownStep(job("validated"), "x"), 3);
    assert.equal(shownStep(job("validated"), undefined), 3);
  });
});

describe("polling", () => {
  const running = (startedAt: string) => ({ status: "validating", progress: { startedAt, updatedAt: startedAt } }) as unknown as ImportJob;
  const t0 = Date.parse("2026-09-30T10:00:00Z");
  test("every 1 s for 10 s, then 2 s, then 5 s after a minute", () => {
    const j = running("2026-09-30T10:00:00Z");
    assert.equal(pollInterval(j, 0, t0 + 5_000), 1_000);
    assert.equal(pollInterval(j, 0, t0 + 30_000), 2_000);
    assert.equal(pollInterval(j, 0, t0 + 120_000), 5_000);
  });
  test("backs off after failed polls, up to 30 s", () => {
    const j = running("2026-09-30T10:00:00Z");
    assert.equal(pollInterval(j, 1, t0), 2_000);
    assert.equal(pollInterval(j, 3, t0), 8_000);
    assert.equal(pollInterval(j, 10, t0), 30_000);
  });
  test("stops in idle and final states", () => {
    for (const status of ["ready", "validated", "completed", "completed_with_errors", "failed", "cancelled", "expired"]) {
      assert.equal(pollInterval({ status, progress: { updatedAt: "2026-09-30T10:00:00Z" } } as unknown as ImportJob, 0), false, status);
    }
    assert.equal(pollInterval(undefined, 0), false);
  });
});

describe("the browser-side file check", () => {
  test("accepts CSV and XLSX, whatever the case of the extension", () => {
    assert.deepEqual(checkFile({ name: "servers.csv", size: 10 }, LIMITS), { format: "csv" });
    assert.deepEqual(checkFile({ name: "Servers 2026.XLSX", size: 10 }, LIMITS), { format: "xlsx" });
  });
  test("refuses old and macro formats with the way out", () => {
    const r = checkFile({ name: "old.xls", size: 10 }, LIMITS);
    assert.ok("error" in r && r.error.includes("Save it as .xlsx"));
    assert.ok("error" in checkFile({ name: "macros.xlsm", size: 10 }, LIMITS));
    assert.ok("error" in checkFile({ name: "notes.txt", size: 10 }, LIMITS));
    assert.ok("error" in checkFile({ name: "noext", size: 10 }, LIMITS));
  });
  test("refuses empty files and files over the server's limit", () => {
    assert.ok("error" in checkFile({ name: "a.csv", size: 0 }, LIMITS));
    const r = checkFile({ name: "big.csv", size: LIMITS.maxFileBytes + 1 }, LIMITS);
    assert.ok("error" in r && r.error.includes("The limit is 50 MB"));
    assert.deepEqual(checkFile({ name: "edge.csv", size: LIMITS.maxFileBytes }, LIMITS), { format: "csv" });
  });
});

describe("wording", () => {
  const err = (code: string, detail?: string, message = "API message") =>
    ({ code, message, status: 429, details: detail ? [{ field: "", message, code: detail }] : [] }) as unknown as ApiError;
  test("refusal codes in details[0].code get operator wording", () => {
    assert.match(uploadErrorMessage(err("RATE_LIMITED", "import_busy"), LIMITS), /Another import of yours is still running/);
    assert.match(uploadErrorMessage(err("RATE_LIMITED", "import_storage_full"), LIMITS), /no room/);
    assert.match(uploadErrorMessage(err("UNSUPPORTED_MEDIA_TYPE", "workbook_encrypted_or_xls"), LIMITS), /password protected/);
    assert.match(uploadErrorMessage(err("PAYLOAD_TOO_LARGE"), LIMITS), /50 MB/);
  });
  test("unknown codes fall back to the API's message", () => {
    assert.equal(uploadErrorMessage(err("VALIDATION_ERROR", "something_new"), LIMITS), "API message");
  });
  test("columns are named as spreadsheets name them", () => {
    assert.equal(columnLetter(0), "A");
    assert.equal(columnLetter(25), "Z");
    assert.equal(columnLetter(26), "AA");
    assert.equal(columnLetter(701), "ZZ");
    assert.equal(errorPlace({ row: 1204, column: 2 }), "Row 1,204, column C");
    assert.equal(errorPlace({ column: 0 }), "Column A");
    assert.equal(errorPlace({}), "");
  });
  test("the default sheet is the one read, else the first visible one", () => {
    assert.equal(defaultSheet({ sheets: ["Hidden", "Servers"], hiddenSheets: ["Hidden"] }), "Servers");
    assert.equal(defaultSheet({ sheets: ["A", "B"], hiddenSheets: [], sheet: "B" }), "B");
  });
});
