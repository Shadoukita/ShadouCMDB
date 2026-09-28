#!/usr/bin/env node
// Changelog fragments (CONTRIBUTING.md#changelog).
//
// Each operator-facing change is one file in changelog.d/ (`SHAA-<n>.md`, `GH-<n>.md`, optionally
// with a `-<slug>` suffix) whose first line is the entry's heading exactly as it appears in
// CHANGELOG.md, e.g. `### Fixed: ...`. Pull requests add fragments and never edit CHANGELOG.md, so
// they no longer conflict with each other there. At release time this script moves every fragment
// into a new version section of CHANGELOG.md, grouped by section, and deletes the fragments.
//
//   node tools/changelog/collect.mjs --check
//       Validates every fragment and that *Unreleased* in CHANGELOG.md is only the pointer to
//       changelog.d/. Run by CI on every pull request and push.
//   node tools/changelog/collect.mjs --version 1.2.0 [--date 2026-10-01] [--dry-run]
//       Writes `## 1.2.0 (<date>)` below *Unreleased* and deletes the collected fragments. With
//       --dry-run, prints the section and the fragments it would delete and changes nothing.
//   node tools/changelog/collect.mjs --verify-release 1.2.0
//       Fails unless CHANGELOG.md has a `## 1.2.0` section and no fragment is left. Run by the
//       release workflow for a tag; pre-releases (1.2.0-rc.1) only need the fragments to be valid.
//
// No dependencies: runs on the Node.js preinstalled on the GitHub runners.

import { readFileSync, readdirSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const CHANGELOG = join(ROOT, "CHANGELOG.md");
const FRAGMENTS = join(ROOT, "changelog.d");

// Sections in the order a release lists them: what operators must act on comes first.
const SECTIONS = [
  { name: "Security", test: (s) => s === "Security" },
  { name: "Changed (breaking)", test: (s) => /^Changed \(breaking[^)]*\)$/.test(s) },
  { name: "Removed", test: (s) => s === "Removed" },
  { name: "Changed", test: (s) => s === "Changed" },
  { name: "Added", test: (s) => s === "Added" },
  { name: "Fixed", test: (s) => s === "Fixed" },
];

export const UNRELEASED_POINTER = [
  "Entries for the next release are kept as one file per change in [`changelog.d/`](changelog.d/) and",
  "collected into a version section here when the release is cut. See",
  "[CONTRIBUTING.md](CONTRIBUTING.md#changelog).",
].join("\n");

const NAME = /^(SHAA|GH)-([1-9][0-9]*)(-[a-z0-9]+(?:-[a-z0-9]+)*)?\.md$/;
const HEADING = /^### (Security|Changed \(breaking[^)]*\)|Removed|Changed|Added|Fixed): \S/;
const VERSION = /^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$/;
const IGNORED = new Set(["README.md", ".gitkeep"]);

function fail(errors) {
  for (const e of errors) console.error(`error: ${e}`);
  process.exit(1);
}

function readText(path) {
  return readFileSync(path, "utf8").replace(/\r\n/g, "\n");
}

/** Every fragment in changelog.d/, parsed and in release order; errors instead of throwing. */
export function readFragments(dir = FRAGMENTS) {
  const fragments = [];
  const errors = [];
  let files;
  try {
    files = readdirSync(dir);
  } catch (e) {
    if (e.code === "ENOENT") return { fragments, errors };
    throw e;
  }
  for (const file of files.sort()) {
    if (IGNORED.has(file)) continue;
    const where = `changelog.d/${file}`;
    const name = NAME.exec(file);
    if (!name) {
      errors.push(`${where}: name it after the issue, e.g. SHAA-123.md, GH-45.md or SHAA-123-api.md`);
      continue;
    }
    const text = readText(join(dir, file)).trim();
    const lines = text.split("\n");
    const heading = HEADING.exec(lines[0]);
    if (!heading) {
      errors.push(
        `${where}: the first line must be the entry heading, "### <Section>: <title>" with Section one of ` +
          "Security, Changed (breaking API change), Removed, Changed, Added, Fixed",
      );
      continue;
    }
    let fenced = false;
    for (const [i, line] of lines.entries()) {
      if (/^\s*(```|~~~)/.test(line)) fenced = !fenced;
      if (i > 0 && !fenced && /^#{1,3} /.test(line)) {
        errors.push(`${where}:${i + 1}: one entry per fragment; use #### or bold text below the heading`);
      }
    }
    const section = SECTIONS.findIndex((s) => s.test(heading[1]));
    fragments.push({ file, path: join(dir, file), text, section, prefix: name[1], number: Number(name[2]), suffix: name[3] ?? "" });
  }
  // Within a section: newest issue first (higher number), SHAA before GH, then by suffix.
  fragments.sort(
    (a, b) =>
      a.section - b.section ||
      (a.prefix === b.prefix ? 0 : a.prefix === "SHAA" ? -1 : 1) ||
      b.number - a.number ||
      a.suffix.localeCompare(b.suffix),
  );
  return { fragments, errors };
}

/** Splits CHANGELOG.md around *Unreleased*: text up to its heading, its body, and the rest. */
export function splitChangelog(text) {
  const lines = text.split("\n");
  const start = lines.indexOf("## Unreleased");
  if (start < 0) return null;
  let end = lines.findIndex((l, i) => i > start && l.startsWith("## "));
  if (end < 0) end = lines.length;
  return {
    head: lines.slice(0, start + 1).join("\n"),
    unreleased: lines.slice(start + 1, end).join("\n").trim(),
    rest: lines.slice(end).join("\n").trim(),
  };
}

function versionHeadings(text) {
  return text
    .split("\n")
    .filter((l) => l.startsWith("## ") && l !== "## Unreleased")
    .map((l) => l.slice(3).split(" ")[0]);
}

function checkChangelog(text, errors) {
  const parts = splitChangelog(text);
  if (!parts) {
    errors.push('CHANGELOG.md: the "## Unreleased" heading is missing');
  } else if (parts.unreleased !== UNRELEASED_POINTER) {
    errors.push(
      "CHANGELOG.md: *Unreleased* must only point to changelog.d/. Put the entry in " +
        "changelog.d/SHAA-<n>.md instead (CONTRIBUTING.md#changelog)",
    );
  }
  return parts;
}

export function renderSection(version, date, fragments) {
  const body = fragments.length
    ? fragments.map((f) => f.text).join("\n\n")
    : "No changes that need operator action. See the GitHub release notes.";
  return `## ${version} (${date})\n\n${body}\n`;
}

function parseArgs(argv) {
  const args = { dryRun: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const value = () => {
      if (i + 1 >= argv.length) fail([`${a} needs a value`]);
      return argv[++i];
    };
    if (a === "--check") args.mode = "check";
    else if (a === "--version") (args.mode = "collect"), (args.version = value());
    else if (a === "--verify-release") (args.mode = "verify"), (args.version = value());
    else if (a === "--date") args.date = value();
    else if (a === "--dry-run") args.dryRun = true;
    else fail([`unknown argument ${a}; see the usage at the top of tools/changelog/collect.mjs`]);
  }
  if (!args.mode) fail(["pass --check, --version <x.y.z> or --verify-release <x.y.z>"]);
  if (args.version !== undefined && !VERSION.test(args.version)) {
    fail([`${args.version} is not a version like 1.2.0 or 1.2.0-rc.1 (no leading v)`]);
  }
  if (args.date !== undefined && !/^\d{4}-\d{2}-\d{2}$/.test(args.date)) {
    fail([`--date ${args.date} is not YYYY-MM-DD`]);
  }
  return args;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const changelog = readText(CHANGELOG);
  const { fragments, errors } = readFragments();
  const parts = checkChangelog(changelog, errors);

  if (args.mode === "check") {
    if (errors.length) fail(errors);
    console.log(`changelog: ${fragments.length} pending fragment(s) in changelog.d/, *Unreleased* is the pointer`);
    return;
  }

  if (args.mode === "verify") {
    if (args.version.includes("-")) {
      if (errors.length) fail(errors);
      console.log(`changelog: pre-release ${args.version}, pending fragments stay in changelog.d/ for the final version`);
      return;
    }
    if (!versionHeadings(changelog).includes(args.version)) {
      errors.push(
        `CHANGELOG.md has no "## ${args.version}" section: run ` +
          `node tools/changelog/collect.mjs --version ${args.version} in the release PR`,
      );
    }
    if (fragments.length) {
      errors.push(
        `${fragments.length} fragment(s) left in changelog.d/ (${fragments.map((f) => f.file).join(", ")}): ` +
          "collect them before tagging a release",
      );
    }
    if (errors.length) fail(errors);
    console.log(`changelog: ${args.version} is collected`);
    return;
  }

  // collect
  if (versionHeadings(changelog).includes(args.version)) {
    errors.push(`CHANGELOG.md already has a "## ${args.version}" section`);
  }
  if (errors.length) fail(errors);
  const date = args.date ?? new Date().toISOString().slice(0, 10);
  const section = renderSection(args.version, date, fragments);
  if (args.dryRun) {
    process.stdout.write(section);
    console.error(`dry run: would delete ${fragments.map((f) => `changelog.d/${f.file}`).join(" ") || "no fragments"}`);
    return;
  }
  const out = [parts.head, "", UNRELEASED_POINTER, "", section.trimEnd(), parts.rest && `\n${parts.rest}`]
    .join("\n")
    .trimEnd();
  writeFileSync(CHANGELOG, `${out}\n`);
  for (const f of fragments) unlinkSync(f.path);
  console.log(`changelog: wrote "## ${args.version} (${date})" with ${fragments.length} entr${fragments.length === 1 ? "y" : "ies"}`);
  for (const f of fragments) console.log(`  deleted changelog.d/${f.file}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
