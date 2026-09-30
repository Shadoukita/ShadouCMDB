// node --test tools/dast/*.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { completedRequests, reachedHandlers, summary } from "./reached-handlers.mjs";

// serve.log of a scan in the shape of backend/src/logging.rs: `request completed` lines with the
// request span, other events, and a line that is not JSON.
const LOG = readFileSync(new URL("./fixtures/serve.log", import.meta.url), "utf8");
const EXAMPLES = {
  "configuration-items": "0b6f6a52-6e0a-4d5e-9d57-3f0d3c1e9a01",
  "admin/users": "5c2a9e11-8b4f-4f4e-a8a1-7d6b2c0e4b02",
  "admin/identity-providers": "9e7d3b20-1c5a-4a8e-b3f2-6a4d8c1f5e03",
  "ui-settings/versions": 1,
  "admin/templates": "it_infrastructure",
};
const ALL_400 = LOG.replace(/"status":\d+/g, '"status":400');

test("completedRequests: reads method, path and status, skips other lines", () => {
  const requests = completedRequests(LOG);
  assert.ok(requests.length > 3);
  assert.deepEqual(requests[0], { method: "GET", path: "/readyz", status: 200 });
  assert.ok(requests.every((r) => typeof r.status === "number" && r.path.startsWith("/")));
});

test("reachedHandlers: every probe answered 400 fails (GH#290 state)", () => {
  const result = reachedHandlers(completedRequests(ALL_400), EXAMPLES);
  assert.equal(result.ok, false);
  assert.ok(result.probes.every((p) => !p.ok && p.sent > 0 && p.reached === 0));
  assert.equal(result.badRequest, result.withId);
  assert.match(summary(result), /\*\*not reached\*\*/);
});

test("reachedHandlers: one request per probe that is not 400 passes", () => {
  const result = reachedHandlers(completedRequests(LOG), EXAMPLES);
  assert.equal(result.ok, true, summary(result));
  assert.deepEqual(result.probes.map((p) => p.reached), [1, 1, 1]);
  assert.ok(result.badRequest > 0 && result.badRequest < result.withId);
  assert.match(summary(result), new RegExp(`Requests with an example id in the path: ${result.withId}, answered 400: ${result.badRequest}\\.`));
});

test("reachedHandlers: the literal parameter name or a mutated id does not count", () => {
  const requests = [
    { method: "GET", path: "/api/v1/configuration-items/id", status: 404 },
    { method: "GET", path: `/api/v1/configuration-items/${EXAMPLES["configuration-items"]}'`, status: 404 },
    { method: "GET", path: `/api/v1/configuration-items/${EXAMPLES["configuration-items"]}/graph`, status: 200 },
    { method: "PATCH", path: `/api/v1/admin/users/${EXAMPLES["admin/users"]}`, status: 200 },
  ];
  const result = reachedHandlers(requests, EXAMPLES);
  assert.equal(result.ok, false);
  assert.deepEqual(result.probes.map((p) => p.sent), [0, 0, 0]);
  assert.equal(result.withId, 3, "the version number and template key are not ids");
});

test("reachedHandlers: a probe without an example fails", () => {
  const { "admin/users": _, ...rest } = EXAMPLES;
  const result = reachedHandlers(completedRequests(LOG), rest);
  assert.equal(result.ok, false);
  assert.equal(result.probes.find((p) => p.resource === "admin/users").ok, false);
});
