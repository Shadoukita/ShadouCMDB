## What and why

<!-- One or two sentences. Link the issue: Refs SHAA-<n> -->

## How it was verified

<!-- Commands run and their result. For DB changes: migrate on a fresh and an existing database. -->

## Checklist

- [ ] CI is green (typecheck, build)
- [ ] Schema changes include a migration in `sql/migrations/` and an updated `sql/diagrams/erd.md`
- [ ] No secrets, `.env` files or credentials committed
- [ ] Docs updated where behaviour or setup changed
- [ ] Operator-facing change? Then a fragment `changelog.d/SHAA-<n>.md` ([how](CONTRIBUTING.md#changelog)); `CHANGELOG.md` itself is not edited
- [ ] Security-relevant? Then: `security` label, human review by the security owner, AI-generated parts named above
