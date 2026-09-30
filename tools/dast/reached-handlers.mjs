#!/usr/bin/env node
// Proof that the ZAP API scans in .github/workflows/dast.yml used the path-parameter examples of
// tools/dast/path-examples.mjs (GH#318).
//
// path-examples.mjs checks that every path parameter has an example; it cannot check that ZAP sent
// it. Had a ZAP upgrade stopped reading `example`, every `/{id}` request would again be refused 400
// while the path is parsed (GH#290) and the scans would still pass. This script reads the server's
// `request completed` log lines (backend/src/http/request_id.rs) and fails unless each probe below
// was requested with its example id and answered with a status other than 400: the handler ran.
//
//   node tools/dast/reached-handlers.mjs <serve.log> <path-examples.json>
//
// Prints a Markdown summary (append it to $GITHUB_STEP_SUMMARY): the requests whose path holds an
// example id, how many of them were answered 400, and the result of every probe.
//
// No dependencies: runs on the Node.js preinstalled on the GitHub runners.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

/**
 * One operation per scan and kind of handler: the token scan (a read), the session scan (a delete
 * and a body-reading action). `{id}` is the example of `resource` in path-examples.json.
 */
export const PROBES = [
  { method: "GET", path: "/api/v1/configuration-items/{id}", resource: "configuration-items" },
  { method: "DELETE", path: "/api/v1/admin/users/{id}", resource: "admin/users" },
  { method: "POST", path: "/api/v1/admin/identity-providers/{id}/test", resource: "admin/identity-providers" },
];

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** The `request completed` lines of a server log as `[{ method, path, status }]`; other lines are skipped. */
export function completedRequests(log) {
  const requests = [];
  for (const line of log.split("\n")) {
    if (!line.includes('"request completed"')) continue;
    let entry;
    try {
      entry = JSON.parse(line);
    } catch {
      continue;
    }
    const { fields, span } = entry;
    if (fields?.message !== "request completed" || !span?.path) continue;
    requests.push({ method: span.method, path: span.path, status: Number(fields.status) });
  }
  return requests;
}

/**
 * The verdict for `requests` against the ids in `examples`: the object ids used (only UUIDs, so a
 * version number or template key does not match every path), the requests holding one and how many
 * of those were 400, and per probe the requests made and the ones that were not 400.
 */
export function reachedHandlers(requests, examples) {
  const ids = Object.values(examples).filter((v) => typeof v === "string" && UUID.test(v));
  const withId = requests.filter((r) => ids.some((id) => r.path.includes(id)));
  const probes = PROBES.map((probe) => {
    const id = examples[probe.resource];
    const path = probe.path.replace("{id}", id);
    const sent = requests.filter((r) => r.method === probe.method && r.path === path);
    const reached = sent.filter((r) => r.status !== 400).length;
    return { ...probe, id, sent: sent.length, reached, ok: id !== undefined && reached > 0 };
  });
  return {
    withId: withId.length,
    badRequest: withId.filter((r) => r.status === 400).length,
    probes,
    ok: probes.every((p) => p.ok),
  };
}

/** The verdict as Markdown for the job summary. */
export function summary(result) {
  const lines = [
    "### Path parameters reached their handlers",
    "",
    `Requests with an example id in the path: ${result.withId}, answered 400: ${result.badRequest}.`,
    "",
    "| Probe | Requests | Not 400 | Result |",
    "| --- | ---: | ---: | --- |",
    ...result.probes.map(
      (p) => `| \`${p.method} ${p.path}\` | ${p.sent} | ${p.reached} | ${p.ok ? "reached" : "**not reached**"} |`,
    ),
  ];
  if (!result.ok) {
    lines.push("", "ZAP did not send the path-parameter examples of openapi-dast.json, or the server refused them (GH#290, GH#318).");
  }
  return lines.join("\n") + "\n";
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [log, examples] = process.argv.slice(2);
  if (!log || !examples) {
    console.error("usage: node tools/dast/reached-handlers.mjs <serve.log> <path-examples.json>");
    process.exit(2);
  }
  const result = reachedHandlers(
    completedRequests(readFileSync(log, "utf8")),
    JSON.parse(readFileSync(examples, "utf8")),
  );
  process.stdout.write(summary(result));
  process.exit(result.ok ? 0 : 1);
}
