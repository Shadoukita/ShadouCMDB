// npm audit for tools/sbom with a time-boxed allowlist (GH#538).
//
//   node audit.mjs            (run in tools/sbom)
//
// Runs `npm audit --json` and fails on every high or critical advisory that is not
// in audit-allowlist.json, on every allowlist entry past its expiry, and on any
// audit output it cannot read. Allowlisted advisories are reported as warnings.
// Only this directory uses an allowlist; the root and frontend audit stays strict.
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const FAILING = new Set(["high", "critical"]);
const MAX_DAYS = 90;
const WARN_DAYS = 14;
const DAY_MS = 24 * 60 * 60 * 1000;
const GHSA = /^GHSA(-[23456789cfghjmpqrvwx]{4}){3}$/;
const DATE = /^\d{4}-\d{2}-\d{2}$/;

function parseDate(value, what) {
  if (typeof value !== "string" || !DATE.test(value) || Number.isNaN(Date.parse(`${value}T00:00:00Z`))) {
    throw new Error(`${what}: expected a YYYY-MM-DD date, got ${JSON.stringify(value)}`);
  }
  return Date.parse(`${value}T00:00:00Z`);
}

// Rejects entries that would make the allowlist open-ended or untraceable.
export function validateAllowlist(allowlist) {
  if (!allowlist || !Array.isArray(allowlist.advisories)) {
    throw new Error("allowlist: expected an object with an `advisories` array");
  }
  for (const entry of allowlist.advisories) {
    const where = `allowlist entry ${JSON.stringify(entry?.id)}`;
    if (typeof entry?.id !== "string" || !GHSA.test(entry.id)) throw new Error(`${where}: id must be a GHSA identifier`);
    for (const field of ["package", "reason", "issue"]) {
      if (typeof entry[field] !== "string" || entry[field].trim() === "") throw new Error(`${where}: ${field} is required`);
    }
    if (!entry.issue.startsWith("https://github.com/Shadoukita/ShadouCMDB/issues/")) {
      throw new Error(`${where}: issue must link a ShadouCMDB GitHub issue`);
    }
    const added = parseDate(entry.added, `${where}: added`);
    const expires = parseDate(entry.expires, `${where}: expires`);
    if (expires <= added || expires - added > MAX_DAYS * DAY_MS) {
      throw new Error(`${where}: expires must be after added and at most ${MAX_DAYS} days later`);
    }
  }
}

// The advisories themselves; `via` strings only point at other affected packages.
function advisories(report) {
  if (!report || typeof report !== "object" || report.error || typeof report.vulnerabilities !== "object") {
    throw new Error(`npm audit did not return a report: ${JSON.stringify(report?.error ?? report).slice(0, 500)}`);
  }
  const found = new Map();
  for (const [name, vulnerability] of Object.entries(report.vulnerabilities)) {
    for (const via of vulnerability.via ?? []) {
      if (typeof via !== "object") continue;
      const id = String(via.url ?? "").split("/").pop();
      const key = `${id || via.source}:${via.name ?? name}`;
      found.set(key, { id, package: via.name ?? name, severity: via.severity, title: via.title, url: via.url });
    }
  }
  return [...found.values()];
}

// Returns { failures, warnings } for an `npm audit --json` report.
export function evaluate(report, allowlist, now = Date.now()) {
  validateAllowlist(allowlist);
  const failures = [];
  const warnings = [];
  const today = Date.parse(new Date(now).toISOString().slice(0, 10) + "T00:00:00Z");
  const used = new Set();

  for (const entry of allowlist.advisories) {
    const left = Math.round((parseDate(entry.expires, "expires") - today) / DAY_MS);
    if (left < 0) {
      failures.push(`${entry.id} (${entry.package}): allowlist entry expired on ${entry.expires}. Fix it or re-review it in ${entry.issue}.`);
    } else if (left <= WARN_DAYS) {
      warnings.push(`${entry.id} (${entry.package}): allowlist entry expires on ${entry.expires} (${left} days). ${entry.issue}`);
    }
  }

  for (const advisory of advisories(report)) {
    if (!FAILING.has(advisory.severity)) continue;
    const label = `${advisory.id || "advisory without a GHSA id"} (${advisory.package}, ${advisory.severity}): ${advisory.title ?? ""} ${advisory.url ?? ""}`.trim();
    const entry = allowlist.advisories.find((e) => e.id === advisory.id && e.package === advisory.package);
    if (!entry) {
      failures.push(label);
      continue;
    }
    used.add(entry);
    if (parseDate(entry.expires, "expires") >= today) {
      warnings.push(`allowlisted until ${entry.expires}: ${label} - ${entry.issue}`);
    }
  }

  for (const entry of allowlist.advisories) {
    if (!used.has(entry)) {
      warnings.push(`${entry.id} (${entry.package}) is no longer reported; remove it from audit-allowlist.json.`);
    }
  }
  return { failures, warnings };
}

function main() {
  const dir = fileURLToPath(new URL(".", import.meta.url));
  const allowlist = JSON.parse(readFileSync(new URL("audit-allowlist.json", import.meta.url), "utf8"));
  // npm audit exits non-zero whenever it finds anything, so its status is not used;
  // the report is. Output that is not a report fails below.
  const audit = spawnSync("npm", ["audit", "--json"], { cwd: dir, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  if (audit.error) throw audit.error;
  let report;
  try {
    report = JSON.parse(audit.stdout);
  } catch {
    throw new Error(`npm audit did not print JSON (exit ${audit.status}): ${audit.stderr.slice(0, 2000)}`);
  }
  const { failures, warnings } = evaluate(report, allowlist);
  for (const w of warnings) console.log(`::warning title=npm audit (tools/sbom)::${w}`);
  for (const f of failures) console.log(`::error title=npm audit (tools/sbom)::${f}`);
  if (failures.length > 0) {
    console.error(`${failures.length} high/critical finding(s) not covered by tools/sbom/audit-allowlist.json.`);
    process.exit(1);
  }
  console.log(`No high or critical advisories outside the allowlist (${warnings.length} warning(s)).`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    main();
  } catch (error) {
    console.error(`::error title=npm audit (tools/sbom)::${error.message}`);
    process.exit(1);
  }
}
