// node --test tools/dast/*.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { CREATED, LISTED, OPTIONAL, neededResources, resourceOf, withExamples } from "./path-examples.mjs";
import { sessionOnlySpec } from "./session-openapi.mjs";

const OPENAPI = new URL("../../backend/openapi.json", import.meta.url);

test("resourceOf: the path before the parameter, with aliases", () => {
  assert.equal(resourceOf("/api/v1/configuration-items/{id}/graph", "id"), "configuration-items");
  assert.equal(resourceOf("/api/v1/admin/users/{id}/password", "id"), "admin/users");
  assert.equal(resourceOf("/api/v1/auth/oidc/{id}/start", "id"), "admin/identity-providers");
  assert.equal(resourceOf("/api/v1/ui-settings/versions/{version}", "version"), "ui-settings/versions");
});

test("withExamples: sets path-level and operation-level parameters, enums, and reports the rest", () => {
  const spec = {
    paths: {
      "/api/v1/a/{id}": {
        parameters: [{ name: "id", in: "path", schema: { type: "string" } }],
        get: { parameters: [{ name: "q", in: "query" }] },
      },
      "/api/v1/b/{id}": { delete: { parameters: [{ name: "id", in: "path" }] } },
      "/api/v1/c/{kind}": { get: { parameters: [{ name: "kind", in: "path", schema: { enum: ["logo", "favicon"] } }] } },
      "/api/v1/d/{id}": { get: { parameters: [{ name: "id", in: "path" }] } },
    },
  };
  const { spec: out, missing } = withExamples(spec, { a: "A", b: "B" });
  assert.equal(out.paths["/api/v1/a/{id}"].parameters[0].example, "A");
  assert.equal(out.paths["/api/v1/a/{id}"].get.parameters[0].example, undefined);
  assert.equal(out.paths["/api/v1/b/{id}"].delete.parameters[0].example, "B");
  assert.equal(out.paths["/api/v1/c/{kind}"].get.parameters[0].example, "logo");
  assert.deepEqual(missing, ["/api/v1/d/{id} {id}"]);
  assert.equal(spec.paths["/api/v1/a/{id}"].parameters[0].example, undefined, "the input is not changed");
});

// A new `/{id}` route under a resource the script does not know fails here, not as a silent 400 in
// the scan (GH#290).
test("backend/openapi.json: every path parameter of both scanned specs gets an example", () => {
  const openapi = JSON.parse(readFileSync(OPENAPI, "utf8"));
  const known = new Set([...LISTED, ...CREATED, ...OPTIONAL]);
  for (const resource of neededResources(openapi)) assert.ok(known.has(resource), `no example source for ${resource}`);

  const examples = Object.fromEntries([...known].map((resource) => [resource, `example-${resource}`]));
  const { spec, missing } = withExamples(openapi, examples);
  assert.deepEqual(missing, []);
  const session = sessionOnlySpec(spec).spec;
  for (const scanned of [spec, session]) {
    for (const [path, item] of Object.entries(scanned.paths)) {
      for (const operation of [item, ...Object.values(item)]) {
        for (const parameter of operation?.parameters ?? []) {
          if (parameter.in === "path") assert.notEqual(parameter.example, undefined, `${path} {${parameter.name}}`);
        }
      }
    }
  }
  const test = session.paths["/api/v1/admin/identity-providers/{id}/test"].post.parameters[0];
  assert.equal(test.example, "example-admin/identity-providers");
});
