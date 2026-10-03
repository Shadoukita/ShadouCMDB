// node --test tools/sbom/*.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { evaluate, validateAllowlist } from "./audit.mjs";

const NOW = Date.parse("2026-10-03T12:00:00Z");
const CH52 = {
  source: 1113000,
  name: "http-cache-semantics",
  title: "http-cache-semantics max-stale handling can disclose cross-user cached responses",
  url: "https://github.com/advisories/GHSA-ch52-4w7c-c8xp",
  severity: "high",
  range: "<=4.2.0",
};
const ENTRY = {
  id: "GHSA-ch52-4w7c-c8xp",
  package: "http-cache-semantics",
  reason: "build-time only",
  issue: "https://github.com/Shadoukita/ShadouCMDB/issues/538",
  added: "2026-10-03",
  expires: "2026-12-31",
};

function report(...extra) {
  return {
    auditReportVersion: 2,
    vulnerabilities: {
      "http-cache-semantics": { name: "http-cache-semantics", severity: "high", via: [CH52] },
      "make-fetch-happen": { name: "make-fetch-happen", severity: "high", via: ["http-cache-semantics"] },
      ...Object.fromEntries(extra.map((v) => [v.name, { name: v.name, severity: v.severity, via: [v] }])),
    },
  };
}

test("the allowlisted advisory passes with a warning", () => {
  const { failures, warnings } = evaluate(report(), { advisories: [ENTRY] }, NOW);
  assert.deepEqual(failures, []);
  assert.equal(warnings.length, 1);
  assert.match(warnings[0], /allowlisted until 2026-12-31: GHSA-ch52-4w7c-c8xp/);
});

test("without the allowlist entry the same report fails", () => {
  const { failures } = evaluate(report(), { advisories: [] }, NOW);
  assert.equal(failures.length, 1);
  assert.match(failures[0], /GHSA-ch52-4w7c-c8xp \(http-cache-semantics, high\)/);
});

test("any other high or critical advisory still fails; moderate does not", () => {
  const other = { name: "left-pad", title: "x", url: "https://github.com/advisories/GHSA-2222-3333-4444", severity: "critical" };
  const moderate = { name: "right-pad", title: "y", url: "https://github.com/advisories/GHSA-5555-6666-7777", severity: "moderate" };
  const { failures } = evaluate(report(other, moderate), { advisories: [ENTRY] }, NOW);
  assert.equal(failures.length, 1);
  assert.match(failures[0], /GHSA-2222-3333-4444 \(left-pad, critical\)/);
});

test("the entry only covers its own package", () => {
  const elsewhere = { ...CH52, name: "other-cache" };
  const { failures } = evaluate(report(elsewhere), { advisories: [ENTRY] }, NOW);
  assert.equal(failures.length, 1);
  assert.match(failures[0], /other-cache/);
});

test("an expired entry fails again", () => {
  const { failures } = evaluate(report(), { advisories: [ENTRY] }, Date.parse("2027-01-01T00:00:00Z"));
  assert.equal(failures.length, 1);
  assert.match(failures[0], /expired on 2026-12-31/);
});

test("the expiry date itself still passes and warns", () => {
  const { failures, warnings } = evaluate(report(), { advisories: [ENTRY] }, Date.parse("2026-12-31T23:00:00Z"));
  assert.deepEqual(failures, []);
  assert.ok(warnings.some((w) => /expires on 2026-12-31 \(0 days\)/.test(w)));
});

test("a stale entry warns so it gets removed", () => {
  const { failures, warnings } = evaluate({ vulnerabilities: {} }, { advisories: [ENTRY] }, NOW);
  assert.deepEqual(failures, []);
  assert.ok(warnings.some((w) => /no longer reported/.test(w)));
});

test("unreadable audit output fails closed", () => {
  assert.throws(() => evaluate({ error: { code: "ENOLOCK" } }, { advisories: [] }, NOW), /did not return a report/);
  assert.throws(() => evaluate({}, { advisories: [] }, NOW), /did not return a report/);
});

test("entries must be traceable and time-boxed to 90 days", () => {
  assert.throws(() => validateAllowlist({ advisories: [{ ...ENTRY, expires: "2027-01-02" }] }), /at most 90 days/);
  assert.throws(() => validateAllowlist({ advisories: [{ ...ENTRY, expires: undefined }] }), /expires/);
  assert.throws(() => validateAllowlist({ advisories: [{ ...ENTRY, issue: "" }] }), /issue is required/);
  assert.throws(() => validateAllowlist({ advisories: [{ ...ENTRY, reason: " " }] }), /reason is required/);
  assert.throws(() => validateAllowlist({ advisories: [{ ...ENTRY, id: "CVE-2026-1" }] }), /GHSA/);
});

test("the committed allowlist is valid", () => {
  validateAllowlist(JSON.parse(readFileSync(new URL("audit-allowlist.json", import.meta.url), "utf8")));
});
