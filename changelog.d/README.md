# Changelog fragments

One file per operator-facing change, collected into [`CHANGELOG.md`](../CHANGELOG.md) when a release
is cut. Pull requests add a file here and never edit `CHANGELOG.md`. The rules are in
[CONTRIBUTING.md](../CONTRIBUTING.md#changelog); in short:

- **Name** it after the issue: `SHAA-123.md` (or `GH-45.md`). A second entry for the same issue gets a
  suffix: `SHAA-123-api.md`.
- **First line** is the entry's heading exactly as it will appear in the changelog:
  `### <Section>: <title>`, with the section one of `Security`, `Changed (breaking API change)`,
  `Removed`, `Changed`, `Added`, `Fixed`.
- **Below it**, the entry text as operators will read it, including an **Upgrade:** / **Action on
  upgrade** paragraph where they have something to do, and any link reference definitions it uses.
  One entry per file; no `#`, `##` or `###` headings below the first line.

Check your fragment with `node tools/changelog/collect.mjs --check` (CI runs it too), and preview the
next release's section with `node tools/changelog/collect.mjs --version 0.0.0 --dry-run`.
