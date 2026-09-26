# Contributing

## Repository layout

| Path | What |
| --- | --- |
| `backend/` | The only database client: the Rust server `shadoucmdb` (`Cargo.toml`, `src/`, sqlx offline query data in `.sqlx/`, generated `openapi.json`). |
| `frontend/` | Vue 3 + Vite + TanStack Query web UI. Talks to the API only. |
| `sql/` | Database artifacts: migrations, bootstrap scripts, ER diagram. |
| `docs/` | Architecture, API, deployment and data-model documentation. |
| `tools/` | Smoke test (`smoke/smoke.ts`, runs against any API URL) and the OpenAPI diff script. |
| `.github/` | CI and release workflows, pull request template. |
| `deploy/` | systemd unit, release Dockerfile, READMEs shipped inside the release archives. |

## Workflow

1. Branch from an up-to-date `main`. Name the branch after the tracking issue:
   `shaa-<number>-<short-slug>`, e.g. `shaa-3-backend-api`.
2. Commit in small, reviewable steps. Write commit subjects in the imperative
   ("Add CI search endpoint") and reference the issue in the body (`Refs SHAA-3`).
3. Push the branch and open a pull request against `main` using the template.
4. CI must be green before merging: **CI** (frontend typecheck, API types, build), **Rust** (fmt, clippy,
   tests, `openapi --check`, PostgreSQL integration and smoke suite, Windows, Docker) and, for PRs that
   touch the pipeline, **Release** as a dry run. Squash-merge, then delete the branch.

Never push directly to `main`, force-push a shared branch, or rewrite merged history.

## Rules

- **No secrets in git.** Database credentials and tokens live in `.env` (ignored) or a secret
  store. `.env.example` documents every variable with placeholder values only.
- **Schema changes go through `sql/migrations/`.** See [`sql/README.md`](sql/README.md).
- **PostgreSQL is external.** No code may assume `localhost` or a co-located database.
- Keep `README.md`, `docs/` and `sql/diagrams/` accurate in the same PR as the change that affects them.

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

1. **Bump the version** in `backend/Cargo.toml` (`version = "1.2.0"`, or `"1.2.0-rc.1"` for a
   pre-release) and refresh the lockfile with `cargo update -p shadoucmdb --offline`. Open a PR
   (`shaa-<n>-release-1.2.0`) and squash-merge it once CI is green.
2. **Tag the merge commit on `main`** and push the tag:

   ```sh
   git switch main && git pull --ff-only
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
     Linux binaries, tests both images, pushes them, then pulls and runs each platform again;
   - publishes the GitHub Release with the three archives and `SHA256SUMS`.

   The Release is only created if every job succeeded, so a failed run leaves no partial release.
   Fix the problem in a PR, then delete and re-push the tag
   (`git push origin :v1.2.0 && git tag -d v1.2.0`, then step 2), or cut the next version.
4. **Check the result:** the release page lists
   `shadoucmdb-<version>-{linux-x64.tar.gz,linux-arm64.tar.gz,windows-x64.zip}` and `SHA256SUMS`, and
   `docker pull ghcr.io/shadoukita/shadoucmdb:<version>` works.

Image tags: `1.2.0` gets `1.2.0`, `1.2`, `1` and `latest` (`0.x` versions get no bare major tag). A
pre-release such as `1.2.0-rc.1` gets only `1.2.0-rc.1`, is marked as a pre-release on GitHub and
never moves `latest`.

To try the pipeline without publishing, run *Actions → Release → Run workflow* on a branch: it runs
every build and test and skips the push and the GitHub Release. PRs that change the pipeline,
`deploy/` or the Cargo manifest do the same automatically.
