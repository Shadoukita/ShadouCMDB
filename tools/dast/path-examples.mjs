#!/usr/bin/env node
// backend/openapi.json with a real object id as the `example` of every path parameter, for the ZAP
// API scans in .github/workflows/dast.yml (SHAA-735, GH#290).
//
// openapi.json has no path-parameter examples, so ZAP filled `{id}` with the literal "id": every
// request was refused 400 while the path was parsed, and no `/{id}` handler was ever attacked. This
// script signs in to the running server as the scan's administrator, reads one id per resource from
// its list endpoint (demo data from `seed --demo`), creates the objects the scan may damage (a user,
// a profile, an identity provider, an API token, an import job), and writes the spec with those ids
// as examples.
//
//   DAST_USERNAME=... DAST_PASSWORD=... node tools/dast/path-examples.mjs <openapi.json> <out.json> [<examples.json>]
//
// With <examples.json>, also writes the chosen value of every resource there, for
// tools/dast/reached-handlers.mjs to check after the scans that ZAP sent them (GH#318).
//
// DAST_BASE_URL defaults to http://127.0.0.1:3000. Fails when a path parameter is left without an
// example, so a new `/{id}` route cannot quietly fall back to the literal name.
//
// No dependencies: runs on the Node.js preinstalled on the GitHub runners.

import { randomBytes, randomUUID } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const METHODS = ["get", "put", "post", "delete", "options", "head", "patch", "trace"];
const PREFIX = "/api/v1/";

/** Resources whose id is the first row of their list endpoint (demo data). */
export const LISTED = [
  "configuration-items",
  "relationships",
  "areas",
  "ci-classes",
  "attribute-definitions",
  "relationship-types",
  "relationship-rules",
  "lookup-lists",
  "lookup-list-values",
];

/**
 * Objects created for the scan. The scan changes, disables and deletes what it is given, so it gets
 * objects of its own: never its own account (a password change would end its session) or token.
 */
export const CREATED = ["admin/profiles", "admin/users", "admin/identity-providers", "admin/api-tokens", "imports"];

/**
 * Resources that may have no row yet; any well-formed value still reaches the handler (404). The
 * legacy lookup tables are read-only since migration 0016 (create answers 410) and empty on a fresh
 * install, so they cannot be given a row.
 */
export const LEGACY = ["statuses", "environments", "locations", "owners"];
export const OPTIONAL = ["schema-changes", ...LEGACY, "ui-settings/versions", "admin/templates"];

/** Paths whose parameter names an object of another resource. */
export const ALIASES = { "auth/oidc": "admin/identity-providers" };

/** The resource a path parameter names: the path between /api/v1/ and the parameter. */
export function resourceOf(path, name) {
  const at = path.indexOf(`{${name}}`);
  const resource = path.slice(PREFIX.length, at).replace(/\/$/, "");
  return ALIASES[resource] ?? resource;
}

/** Every inline path parameter of the spec with where it is used: `[{ path, parameter }]`. */
export function pathParameters(spec) {
  const found = [];
  for (const [path, item] of Object.entries(spec.paths ?? {})) {
    const lists = [item.parameters, ...METHODS.map((m) => item[m]?.parameters)];
    for (const parameter of lists.flat()) {
      if (parameter?.$ref) throw new Error(`${path}: parameter ${parameter.$ref} is a $ref; inline it or resolve it here`);
      if (parameter?.in === "path") found.push({ path, parameter });
    }
  }
  return found;
}

/** The resources the spec's path parameters need an id of (enum parameters need none). */
export function neededResources(spec) {
  const needed = new Set();
  for (const { path, parameter } of pathParameters(spec)) {
    if (!parameter.schema?.enum) needed.add(resourceOf(path, parameter.name));
  }
  return needed;
}

/**
 * A copy of the spec with `example` set on every path parameter: the first enum value, or the id of
 * the parameter's resource in `examples`. Returns the spec and the `path {name}` left without one.
 */
export function withExamples(spec, examples) {
  const copy = structuredClone(spec);
  const missing = [];
  for (const { path, parameter } of pathParameters(copy)) {
    const value = parameter.schema?.enum?.[0] ?? examples[resourceOf(path, parameter.name)];
    if (value === undefined) missing.push(`${path} {${parameter.name}}`);
    else parameter.example = value;
  }
  return { spec: copy, missing: [...new Set(missing)] };
}

/**
 * Signs in and returns `request(method, path, body, contentType)` resolving to the parsed JSON
 * response. `body` is sent as JSON, or as it is when `contentType` is given (a raw-body upload).
 */
async function signIn(base, username, password) {
  const login = await fetch(`${base}${PREFIX}auth/login`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ username, password }),
  });
  if (!login.ok) throw new Error(`sign-in as ${username}: ${login.status}`);
  const { csrfToken } = await login.json();
  const cookie = login.headers.getSetCookie().map((c) => c.split(";")[0]).join("; ");
  return async (method, path, body, contentType, extraHeaders = {}) => {
    const headers = { Cookie: cookie, ...extraHeaders };
    if (body !== undefined) Object.assign(headers, { "Content-Type": contentType ?? "application/json", "X-CSRF-Token": csrfToken });
    const payload = body === undefined || contentType ? body : JSON.stringify(body);
    const response = await fetch(`${base}${PREFIX}${path}`, { method, headers, body: payload });
    if (!response.ok) throw new Error(`${method} ${path}: ${response.status} ${await response.text()}`);
    return response.json();
  };
}

/** The example value of every resource, read from and created on the running server. */
async function collect(request) {
  const examples = {};
  for (const resource of LISTED) {
    const id = (await request("GET", `${resource}?limit=1`)).data[0]?.id;
    if (id) examples[resource] = id;
  }
  for (const resource of ["schema-changes", ...LEGACY]) {
    examples[resource] = (await request("GET", `${resource}?limit=1`)).data[0]?.id ?? randomUUID();
  }
  const version = (await request("GET", "ui-settings/versions?limit=1")).data[0]?.version;
  examples["ui-settings/versions"] = version ?? 1;
  examples["admin/templates"] = (await request("GET", "admin/templates")).data[0]?.key ?? "it_infrastructure";

  const name = "DAST scan target";
  examples["admin/profiles"] = (await request("POST", "admin/profiles", { name })).id;
  examples["admin/users"] = (
    await request("POST", "admin/users", {
      username: "dast-target",
      displayName: name,
      password: randomBytes(24).toString("base64url"),
    })
  ).id;
  // A loopback issuer on the discard port: the connection test fails at once, and SSRF payloads the
  // scan sends in the body are what the server is asked to fetch.
  examples["admin/identity-providers"] = (
    await request("POST", "admin/identity-providers", {
      kind: "oidc",
      name,
      oidc: { issuerUrl: "http://127.0.0.1:9", clientId: "dast" },
    })
  ).id;
  const builtin = (await request("GET", "admin/profiles?limit=200")).data.find((p) => p.isBuiltin);
  const expiresAt = new Date(Date.now() + 86_400_000).toISOString().replace(/\.\d+Z$/, "Z");
  examples["admin/api-tokens"] = (
    await request("POST", "admin/api-tokens", { name, profileId: builtin.id, expiresAt })
  ).token.id;
  // Bulk import is off on a fresh install. The upload is session-only and takes the file as the
  // body; the job is analysed in the background and the scan may cancel it.
  await request("PUT", "imports/settings", { enabled: true });
  examples["imports"] = (
    await request("POST", "imports", "hostname,description\r\ndast-01,DAST scan target\r\n", "text/csv", {
      "X-File-Name": "dast-scan-target.csv",
    })
  ).id;
  return examples;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [input, output, chosen] = process.argv.slice(2);
  const { DAST_USERNAME: username, DAST_PASSWORD: password } = process.env;
  if (!input || !output || !username || !password) {
    console.error("usage: DAST_USERNAME=... DAST_PASSWORD=... node tools/dast/path-examples.mjs <openapi.json> <out.json> [<examples.json>]");
    process.exit(2);
  }
  const base = (process.env.DAST_BASE_URL ?? "http://127.0.0.1:3000").replace(/\/$/, "");
  const examples = await collect(await signIn(base, username, password));
  const { spec, missing } = withExamples(JSON.parse(readFileSync(input, "utf8")), examples);
  if (missing.length) {
    console.error(`path parameters without an example; ZAP would send their names:\n  ${missing.join("\n  ")}`);
    process.exit(1);
  }
  writeFileSync(output, JSON.stringify(spec, null, 2));
  if (chosen) writeFileSync(chosen, JSON.stringify(examples, null, 2));
  console.log(`path-parameter examples:\n${Object.entries(examples).map(([k, v]) => `  ${k}: ${v}`).join("\n")}`);
}
