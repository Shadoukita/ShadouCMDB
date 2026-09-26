# Contributing

## Repository layout

| Path | What |
| --- | --- |
| `backend/` | Node + Fastify + Drizzle API (TypeScript). The only database client. |
| `frontend/` | React + Vite + TanStack Query web UI. Talks to the API only. |
| `sql/` | Database artifacts: migrations, bootstrap scripts, ER diagram. |
| `docs/` | Architecture and data-model documentation. |
| `.github/` | CI workflow and pull request template. |

## Workflow

1. Branch from an up-to-date `main`. Name the branch after the tracking issue:
   `shaa-<number>-<short-slug>`, e.g. `shaa-3-backend-api`.
2. Commit in small, reviewable steps. Write commit subjects in the imperative
   ("Add CI search endpoint") and reference the issue in the body (`Refs SHAA-3`).
3. Push the branch and open a pull request against `main` using the template.
4. CI (typecheck and build) must be green before merging. Squash-merge, then delete the branch.

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
npm run typecheck
npm run build --workspaces --if-present
```
