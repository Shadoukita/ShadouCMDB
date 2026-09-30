#!/usr/bin/env node
// The session-only part of backend/openapi.json, for the second ZAP API scan in
// .github/workflows/dast.yml (SHAA-715).
//
// The first API scan authenticates with an administrator's API token. Operations that accept only
// a session (no `apiToken` in their `security`: account, MFA, users, profiles, API tokens, identity
// providers, configuration import) refuse a token with 403 before they read the body, so that scan
// never reaches their handlers and still passes. This script writes the spec those operations need
// for a second scan that signs in with a session cookie and sends the X-CSRF-Token header.
//
//   node tools/dast/session-openapi.mjs <openapi.json> <out.json>
//
// Left out: operations that end the scan's own session (SESSION_ENDING); the scan would sign itself
// out and every later request would be answered 401. Fails when no operation is left, so a change to
// the security schemes cannot quietly empty the scan.
//
// No dependencies: runs on the Node.js preinstalled on the GitHub runners.

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const METHODS = ["get", "put", "post", "delete", "options", "head", "patch", "trace"];

/** `METHOD path` of the operations the session scan must not call. */
export const SESSION_ENDING = new Set(["POST /api/v1/auth/logout"]);

/** An operation is session-only when it needs authentication and no alternative accepts a token. */
export function isSessionOnly(operation, spec) {
  const security = operation.security ?? spec.security ?? [];
  if (security.length === 0) return false; // public
  return !security.some((requirement) => Object.hasOwn(requirement, "apiToken"));
}

/** The spec with only the session-only operations, and their `METHOD path` list. */
export function sessionOnlySpec(spec) {
  const paths = {};
  const operations = [];
  for (const [path, item] of Object.entries(spec.paths ?? {})) {
    const kept = {};
    for (const [key, value] of Object.entries(item)) {
      if (!METHODS.includes(key)) continue;
      const id = `${key.toUpperCase()} ${path}`;
      if (SESSION_ENDING.has(id) || !isSessionOnly(value, spec)) continue;
      kept[key] = value;
      operations.push(id);
    }
    if (Object.keys(kept).length) {
      // Path-level fields (shared parameters) stay with the operations that use them.
      const shared = Object.fromEntries(Object.entries(item).filter(([key]) => !METHODS.includes(key)));
      paths[path] = { ...shared, ...kept };
    }
  }
  return { spec: { ...spec, paths }, operations };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [input, output] = process.argv.slice(2);
  if (!input || !output) {
    console.error("usage: node tools/dast/session-openapi.mjs <openapi.json> <out.json>");
    process.exit(2);
  }
  const { spec, operations } = sessionOnlySpec(JSON.parse(readFileSync(input, "utf8")));
  if (operations.length === 0) {
    console.error(`${input}: no session-only operation found; the session scan would test nothing`);
    process.exit(1);
  }
  writeFileSync(output, JSON.stringify(spec, null, 2));
  console.log(`${operations.length} session-only operations:\n  ${operations.join("\n  ")}`);
}
