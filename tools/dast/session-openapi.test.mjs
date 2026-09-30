// node --test tools/dast/*.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { SESSION_ENDING, sessionOnlySpec } from "./session-openapi.mjs";

const OPENAPI = new URL("../../backend/openapi.json", import.meta.url);

const op = (security) => (security === undefined ? {} : { security });
const SESSION = [{ sessionCookie: [] }];
const EITHER = [{ sessionCookie: [] }, { apiToken: [] }];

test("keeps session-only operations, drops token and public ones", () => {
  const { spec, operations } = sessionOnlySpec({
    openapi: "3.1.0",
    security: EITHER,
    paths: {
      "/a": { get: op(SESSION), post: op([{ csrfHeader: [], sessionCookie: [] }]), patch: op(EITHER) },
      "/b/{id}": { parameters: [{ name: "id", in: "path" }], delete: op(SESSION), get: op() },
      "/public": { post: op([]) },
      "/api/v1/auth/logout": { post: op(SESSION) },
    },
  });
  assert.deepEqual(operations, ["GET /a", "POST /a", "DELETE /b/{id}"]);
  assert.deepEqual(Object.keys(spec.paths), ["/a", "/b/{id}"]);
  assert.deepEqual(Object.keys(spec.paths["/a"]), ["get", "post"]);
  // Path-level parameters stay; the inherited top-level security (token allowed) is dropped.
  assert.deepEqual(spec.paths["/b/{id}"], { parameters: [{ name: "id", in: "path" }], delete: op(SESSION) });
});

// The operations the token scan cannot reach (SHAA-715). A route that becomes session-only is
// picked up without a change here; one that is dropped from this list fails the test.
test("backend/openapi.json: every session-only operation is scanned, logout is not", () => {
  const openapi = JSON.parse(readFileSync(OPENAPI, "utf8"));
  const { operations } = sessionOnlySpec(openapi);
  for (const expected of [
    "GET /api/v1/auth/me",
    "PUT /api/v1/auth/password",
    "POST /api/v1/auth/mfa/totp",
    "POST /api/v1/admin/users",
    "PUT /api/v1/admin/users/{id}/password",
    "PATCH /api/v1/admin/profiles/{id}",
    "POST /api/v1/admin/api-tokens",
    "POST /api/v1/admin/identity-providers",
    "POST /api/v1/admin/identity-providers/{id}/test",
    "POST /api/v1/admin/config/import",
  ]) {
    assert.ok(operations.includes(expected), `${expected} is not in the session scan`);
  }
  for (const ending of SESSION_ENDING) assert.ok(!operations.includes(ending), ending);
  for (const id of operations) {
    const [method, path] = id.split(" ");
    const security = openapi.paths[path][method.toLowerCase()].security;
    assert.ok(!security.some((r) => "apiToken" in r), `${id} accepts a token; the token scan covers it`);
  }
});
