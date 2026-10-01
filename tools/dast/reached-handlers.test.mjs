// node --test tools/dast/*.test.mjs
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { completedRequests, PROBES, reachedHandlers, summary } from "./reached-handlers.mjs";

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

// LOG with every probe request answered `status` instead (GH#345): a scan that lost its session
// cookie (401) or X-CSRF-Token (403), or a server that failed (5xx).
const PROBE_PATHS = PROBES.map((p) => p.path.replace("{id}", EXAMPLES[p.resource]));
const probesAnswered = (status) =>
  LOG.split("\n")
    .map((line) => (PROBE_PATHS.some((path) => line.includes(`"path":"${path}"`)) ? line.replace(/"status":\d+/, `"status":${status}`) : line))
    .join("\n");

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

for (const [name, status] of [
  ["401-only log (session cookie lost)", 401],
  ["403-only log (X-CSRF-Token lost)", 403],
  ["405-only log", 405],
  ["429-only log", 429],
  ["5xx log", 500],
  ["503 log (database unavailable)", 503],
]) {
  test(`reachedHandlers: ${name} fails (GH#345)`, () => {
    const result = reachedHandlers(completedRequests(probesAnswered(status)), EXAMPLES);
    assert.equal(result.ok, false, summary(result));
    assert.ok(result.probes.every((p) => !p.ok && p.sent > 0 && p.reached === 0));
    assert.ok(result.probes.every((p) => p.refused.includes(status)));
    assert.match(summary(result), new RegExp(`\\| ${status} \\| \\*\\*not reached\\*\\* \\|`));
  });
}

test("reachedHandlers: mixed log passes only for the probes with a status of their allow-list", () => {
  const [ci, user, idp] = PROBE_PATHS;
  const requests = [
    { method: "GET", path: ci, status: 401 },
    { method: "GET", path: ci, status: 404 },
    { method: "DELETE", path: user, status: 403 },
    { method: "DELETE", path: user, status: 429 },
    { method: "DELETE", path: user, status: 500 },
    { method: "POST", path: idp, status: 405 },
    { method: "POST", path: idp, status: 415 },
    { method: "POST", path: idp, status: 400 },
  ];
  const result = reachedHandlers(requests, EXAMPLES);
  assert.equal(result.ok, false);
  assert.deepEqual(
    result.probes.map(({ sent, reached, refused, ok }) => ({ sent, reached, refused, ok })),
    [
      { sent: 2, reached: 1, refused: [401], ok: true },
      { sent: 3, reached: 0, refused: [403, 429, 500], ok: false },
      { sent: 3, reached: 0, refused: [400, 405, 415], ok: false },
    ],
  );

  requests.push({ method: "DELETE", path: user, status: 409 }, { method: "POST", path: idp, status: 200 });
  assert.equal(reachedHandlers(requests, EXAMPLES).ok, true);
});

test("reachedHandlers: the allow-lists hold no status from before or around the handler", () => {
  for (const probe of PROBES) {
    for (const status of [400, 401, 403, 405, 408, 413, 415, 429, 500, 502, 503, 504]) {
      assert.ok(!probe.statuses.includes(status), `${probe.method} ${probe.path} counts ${status}`);
    }
  }
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

// The step pipes into tee for the step summary. GitHub's default shell has no pipefail, so the
// step took tee's exit status and could never fail. Run its command the way `shell: bash` does.
test("workflow step: fails on an all-400 log", () => {
  const workflow = readFileSync(new URL("../../.github/workflows/dast.yml", import.meta.url), "utf8");
  const step = workflow.match(/- name: Path parameters reached their handlers\n((?:        .*\n)+)/)[1];
  assert.match(step, /^        shell: bash$/m, "without pipefail the step takes tee's exit status");
  const command = step.match(/^        run: (.*)$/m)[1];

  const dir = mkdtempSync(join(tmpdir(), "reached-"));
  try {
    mkdirSync(join(dir, "zap"));
    cpSync(fileURLToPath(new URL(".", import.meta.url)), join(dir, "tools/dast"), { recursive: true });
    writeFileSync(join(dir, "zap/path-examples.json"), JSON.stringify(EXAMPLES));
    const run = (log) => {
      writeFileSync(join(dir, "serve.log"), log);
      return spawnSync("bash", ["--noprofile", "--norc", "-eo", "pipefail", "-c", command], {
        cwd: dir,
        env: { ...process.env, GITHUB_STEP_SUMMARY: join(dir, "summary.md") },
        encoding: "utf8",
      });
    };
    assert.equal(run(ALL_400).status, 1);
    assert.equal(run(probesAnswered(403)).status, 1);
    assert.equal(run(LOG).status, 0);
    assert.match(readFileSync(join(dir, "summary.md"), "utf8"), /answered 400/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
