#!/usr/bin/env node
// Proof that the ZAP API scans in .github/workflows/dast.yml used the path-parameter examples of
// tools/dast/path-examples.mjs (GH#318).
//
// path-examples.mjs checks that every path parameter has an example; it cannot check that ZAP sent
// it. Had a ZAP upgrade stopped reading `example`, every `/{id}` request would again be refused 400
// while the path is parsed (GH#290) and the scans would still pass. This script reads the server's
// `request completed` log lines (backend/src/http/request_id.rs) and fails unless each probe below
// was requested with its example id and answered with a status only its handler returns (the
// probe's `statuses` allow-list): the handler ran.
//
// Not 400 is not enough (GH#345): 401 and 403 come from the authentication and CSRF checks before the
// handler (a scan that lost its session cookie or X-CSRF-Token), 405 from the router, 408, 413 and 415
// from middleware and body extractors, 429 from rate limiting, and 5xx from a crash or an unavailable
// database. None of them shows that the handler ran with the example id.
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
 *
 * `statuses` is the allow-list of statuses that count: the responses of the operation in
 * backend/openapi.json that its handler returns once the request is authenticated and authorized
 * and the id parsed. 404 is the handler looking the id up: the path is matched exactly, so the router
 * never answers it. Everything else (400, 401, 403, 405, 408, 413, 415, 429, 5xx) does not count.
 */
export const PROBES = [
  { method: "GET", path: "/api/v1/configuration-items/{id}", resource: "configuration-items", statuses: [200, 404] },
  { method: "DELETE", path: "/api/v1/admin/users/{id}", resource: "admin/users", statuses: [204, 404, 409] },
  {
    method: "POST",
    path: "/api/v1/admin/identity-providers/{id}/test",
    resource: "admin/identity-providers",
    statuses: [200, 404],
  },
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
 * of those were 400, and per probe the requests made, the ones answered with a status of its
 * `statuses` allow-list, and the other statuses seen.
 */
export function reachedHandlers(requests, examples) {
  const ids = Object.values(examples).filter((v) => typeof v === "string" && UUID.test(v));
  const withId = requests.filter((r) => ids.some((id) => r.path.includes(id)));
  const probes = PROBES.map((probe) => {
    const id = examples[probe.resource];
    const path = probe.path.replace("{id}", id);
    const sent = requests.filter((r) => r.method === probe.method && r.path === path);
    const reached = sent.filter((r) => probe.statuses.includes(r.status)).length;
    const refused = [...new Set(sent.map((r) => r.status).filter((s) => !probe.statuses.includes(s)))].sort((a, b) => a - b);
    return { ...probe, id, sent: sent.length, reached, refused, ok: id !== undefined && reached > 0 };
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
    "| Probe | Requests | Reached | Counted statuses | Other statuses | Result |",
    "| --- | ---: | ---: | --- | --- | --- |",
    ...result.probes.map(
      (p) =>
        `| \`${p.method} ${p.path}\` | ${p.sent} | ${p.reached} | ${p.statuses.join(", ")} | ${p.refused.join(", ") || "none"} | ${p.ok ? "reached" : "**not reached**"} |`,
    ),
  ];
  if (!result.ok) {
    lines.push(
      "",
      "ZAP did not send the path-parameter examples of openapi-dast.json, or the server refused them before the handler (GH#290, GH#318). " +
        "401 or 403 on the admin probes means the session scan lost its cookie or X-CSRF-Token (GH#345).",
    );
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
