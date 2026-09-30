# Contributing

## Repository layout

| Path | What |
| --- | --- |
| `backend/` | The only database client: the Rust server `shadoucmdb` (`Cargo.toml`, `src/`, sqlx offline query data in `.sqlx/`, generated `openapi.json`). |
| `frontend/` | Vue 3 + Vite + TanStack Query web UI. Talks to the API only. |
| `sql/` | Database artifacts: migrations, bootstrap scripts, ER diagram. |
| `docs/` | Architecture, API, deployment and data-model documentation. |
| `changelog.d/` | Pending changelog entries, one file per change, collected into `CHANGELOG.md` at release ([Changelog](#changelog)). |
| `tools/` | Smoke test (`smoke/smoke.ts`, runs against any API URL), the in-place upgrade check (`upgrade/upgrade-check.ts`), the LDAPS integration test against a real directory (`ldap-it/`, [README](tools/ldap-it/README.md)), the OpenAPI diff script, the pinned CycloneDX generator for the SBOM (`sbom/`, own lockfile), and the changelog collector (`changelog/collect.mjs`). |
| `.github/` | CI, upgrade, supply-chain, CodeQL and release workflows, Dependabot config, pull request template. |
| `deploy/` | systemd unit, release Dockerfile, READMEs shipped inside the release archives. |

## Workflow

1. Branch from an up-to-date `main`. Name the branch after the tracking issue:
   `shaa-<number>-<short-slug>`, e.g. `shaa-3-backend-api`.
2. Commit in small, reviewable steps. Write commit subjects in the imperative
   ("Add CI search endpoint") and reference the issue in the body (`Refs SHAA-3`).
3. If operators will notice the change, add a changelog fragment `changelog.d/SHAA-<n>.md`
   ([Changelog](#changelog)). Never edit `CHANGELOG.md` in a feature or fix PR.
4. Push the branch and open a pull request against `main` using the template.
5. CI must be green before merging: **CI** (frontend typecheck, API types, build, changelog fragments), **Rust** (fmt, clippy,
   tests, `openapi --check`, PostgreSQL integration and smoke suite, LDAPS directory sign-in, Windows, Docker), **Upgrade**
   (in-place upgrade from each published release, no data lost or changed), **Supply chain**
   (cargo-deny, npm audit, gitleaks, SBOM, dependency review, actions pinned by SHA), **CodeQL** and, for
   PRs that touch the pipeline, **Release** as a dry run. Squash-merge, then delete the branch.
   What each check does and how to fix a failure: [docs/supply-chain.md](docs/supply-chain.md).

Never push directly to `main` or a `release/*` branch, force-push a shared branch, or rewrite merged
history.

## Branches

| Branch | Purpose |
| --- | --- |
| `main` | Always the newest code. All features and fixes land here first. |
| `release/X.Y.x` | Permanent maintenance branch for one minor version, e.g. `release/0.1.x` holds `0.1.0`, `0.1.1`, … for as long as 0.1 is supported. Never deleted or renamed; each shipped version is a tag (`v0.1.1`), not a branch. |
| `shaa-<n>-<slug>` | Short-lived work branches, deleted after merge. |

- **Cutting a release branch.** When a minor version is feature-complete, branch `release/X.Y.x` from
  `main`. From then on `main` is the next minor version and its `backend/Cargo.toml` version is
  bumped to it (e.g. `0.2.0-dev`) once `X.Y.0` is tagged. `release/0.1.x` was cut from `main` at
  `ce37bd2`.
- **What goes on a release branch.** Only bug fixes, security fixes, dependency security updates
  and release/version bumps. No features, no schema changes without a migration, no breaking API
  changes.
- **Backports.** Fix on `main` first, then cherry-pick the squash commit onto a branch from
  `release/X.Y.x` (`shaa-<n>-backport-X.Y`, `git cherry-pick -x <sha>`) and open a PR against
  `release/X.Y.x`. A fix that only applies to an old line (the code is gone on `main`) goes straight to
  the release branch and says so in the PR. The same CI checks run on release-branch PRs and pushes.
- Dependabot only opens PRs against `main`; security updates for a supported line are backported by
  hand.

## Rules

- **No secrets in git.** Database credentials and tokens live in `.env` (ignored) or a secret
  store. `.env.example` documents every variable with placeholder values only.
- **New dependencies and actions:** licences must be on the `backend/deny.toml` allow list, and every
  GitHub Action is pinned by commit SHA with the version in a comment (`uses: owner/repo@<sha> # v1.2.3`).
- **Schema changes go through `sql/migrations/`.** See [`sql/README.md`](sql/README.md).
- **PostgreSQL is external.** No code may assume `localhost` or a co-located database.
- Keep `README.md`, `docs/` and `sql/diagrams/` accurate in the same PR as the change that affects them.

## Changelog

[`CHANGELOG.md`](CHANGELOG.md) lists the changes operators need to know about or act on; everything
else is in the generated notes of each GitHub release. Pull requests do not edit it: its *Unreleased*
section is a fixed pointer, so the file no longer conflicts between open PRs. Instead each change adds
one file to [`changelog.d/`](changelog.d/):

- **Name:** the issue, `changelog.d/SHAA-123.md` (`GH-45.md` for a GitHub issue without a SHAA
  issue). A second entry for the same issue gets a suffix, `SHAA-123-api.md`.
- **First line:** the entry heading exactly as it will appear in the changelog,
  `### <Section>: <title>`. Sections: `Security`, `Changed (breaking API change)`, `Removed`,
  `Changed`, `Added`, `Fixed`.
- **Body:** the text operators read, in the same style as the released entries: what changed and why,
  the issue link, and an **Upgrade:** (or **Action on upgrade**) paragraph whenever an operator has to
  do something. Put the link reference definitions the entry uses (`[SHAA-123]: docs/api.md#...`) at
  its end. One entry per file, no `#`/`##`/`###` headings below the first line.

```md
### Fixed: the inventory sorts by attributes again

`GET /api/v1/configuration-items` takes `sort=attributes.<key>` again ([SHAA-335]). ...

**Upgrade:** nothing to do.

[SHAA-335]: docs/api.md
```

To change an entry that is still pending, edit its fragment; to drop it, delete the file. CI runs
`node tools/changelog/collect.mjs --check`, which fails on a malformed fragment and on any direct
edit of *Unreleased*. A branch opened before the switch to fragments moves its `CHANGELOG.md` hunk into
`changelog.d/SHAA-<n>.md` on its next rebase and takes `main`'s `CHANGELOG.md` as is.

At release, `node tools/changelog/collect.mjs --version X.Y.Z` writes the fragments into a new
`## X.Y.Z (date)` section below *Unreleased*, grouped in the order above (within a section, newest
issue first), and deletes them ([Cutting a release](#cutting-a-release), step 1). `--dry-run` prints
that section without changing anything.

## Local checks

```sh
npm ci
npm run lint --workspaces --if-present
npm run typecheck
npm run build --workspaces --if-present
```

```sh
cd backend
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

A few tests need a real PostgreSQL (first-run setup races, locks). They create and drop their
own `shadoucmdb_test_*` database, so they only run when you point them at a server where your
user may `CREATE DATABASE`; otherwise they print a notice and pass (in CI, where `CI` is set, they fail
instead unless `SHADOUCMDB_SKIP_DB_TESTS=1` opts out, so the coverage cannot silently disappear):

```sh
SHADOUCMDB_TEST_DATABASE_URL=postgres://user:password@host:5432/postgres cargo test --locked
```

## Cutting a release

Releases are built by [`.github/workflows/release.yml`](.github/workflows/release.yml) when a tag
`v<version>` is pushed. Nobody builds release binaries by hand.

Every version is released from its maintenance branch: `X.Y.0` and all `X.Y.Z` patches are tagged on
`release/X.Y.x`, never on `main`. The steps below use `1.2.0`; for a patch, replace the branch and the
version accordingly.

1. **Bump the version** on `release/X.Y.x` in `backend/Cargo.toml` (`version = "1.2.0"`, or `"1.2.0-rc.1"` for a
   pre-release) and refresh the lockfile with `cargo update -p shadoucmdb --offline`. For a stable
   version, **collect the changelog** in the same PR:

   ```sh
   node tools/changelog/collect.mjs --version 1.2.0 --dry-run   # review the section
   node tools/changelog/collect.mjs --version 1.2.0             # write it, delete the fragments
   ```

   Pre-releases leave the fragments in place; they are collected for the final version. Open a PR
   (`shaa-<n>-release-1.2.0`) against `release/1.2.x` and squash-merge it once CI is green. The tag
   build refuses a stable version whose `## 1.2.0` section is missing or whose `changelog.d/` still
   holds fragments.
2. **Tag the merge commit on `release/1.2.x`** and push the tag:

   ```sh
   git switch release/1.2.x && git pull --ff-only
   git tag -a v1.2.0 -m "ShadouCMDB 1.2.0"
   git push origin v1.2.0
   ```

   The tag must equal the Cargo version with a `v` in front. The workflow stops at its first job
   if they differ.
3. **Watch the run** under *Actions → Release*. It
   - builds the web UI once and embeds that same `dist/` in every binary;
   - builds `linux-x64` and `linux-arm64` as static musl executables (cargo-zigbuild) and
     `windows-x64` with MSVC and a static C runtime;
   - runs `migrate`, `seed --demo` and `serve` for each binary against PostgreSQL and checks
     `/healthz`, `/readyz` and the embedded UI: Linux x64 and Windows natively, Linux ARM64 under
     QEMU;
   - builds `ghcr.io/shadoukita/shadoucmdb` for `linux/amd64` and `linux/arm64` from those same
     Linux binaries, tests both images, pushes them, signs them with cosign and attaches the SBOM and
     SLSA provenance, verifies that, then pulls and runs each platform again;
   - writes the CycloneDX SBOM, signs every archive, the SBOM and `SHA256SUMS` with cosign (keyless),
     records SLSA build provenance, and verifies all of it
     ([docs/supply-chain.md](docs/supply-chain.md));
   - publishes the GitHub Release with the three archives, the SBOM, `SHA256SUMS`, the
     `.sigstore.json` signatures and the provenance.

   The Release is only created if every job succeeded, so a failed run leaves no partial release.
   Fix the problem in a PR, then delete and re-push the tag
   (`git push origin :v1.2.0 && git tag -d v1.2.0`, then step 2), or cut the next version.
4. **Check the result:** the release page lists
   `shadoucmdb-<version>-{linux-x64.tar.gz,linux-arm64.tar.gz,windows-x64.zip}`,
   `shadoucmdb-<version>.cdx.json`, `shadoucmdb-<version>.provenance.jsonl`, `SHA256SUMS` and a
   `.sigstore.json` for each of them, and the verification steps in
   [docs/supply-chain.md](docs/supply-chain.md#verifying-a-download) pass for one archive and the image.
5. **Add the tag to the upgrade matrix** (`from:` in
   [`.github/workflows/upgrade.yml`](.github/workflows/upgrade.yml)) and to the table in
   [docs/operator-setup.md](docs/operator-setup.md#upgrade-paths-tested-in-ci), in a PR to `main`,
   so every later change is tested against an upgrade from it. In the same PR, **carry the changelog
   section to `main`**: copy `## 1.2.0 (...)` from the release branch into `main`'s `CHANGELOG.md`
   (below *Unreleased*, newest version first) and `git rm` the fragments the collector deleted that
   are also on `main` (for a patch: those of the backported fixes), so they are not released twice.

Image tags: `1.2.0` gets `1.2.0`, `1.2`, `1` and `latest` (`0.x` versions get no bare major tag). A
patch on an older line (e.g. `1.1.4` after `1.2.0`) gets `1.1.4` and `1.1` only: `latest`, the bare
major tag and the GitHub "Latest release" badge move only for the highest stable version. A
pre-release such as `1.2.0-rc.1` gets only `1.2.0-rc.1`, is marked as a pre-release on GitHub and
never moves `latest`.

To try the pipeline without publishing, run *Actions → Release → Run workflow* on a branch: it runs
every build and test and skips the push and the GitHub Release. PRs that change the pipeline,
`deploy/` or the Cargo manifest do the same automatically; from a branch of this repository they also
sign and attest the `-dev` archives, so the signing steps are tested before a tag is cut.
