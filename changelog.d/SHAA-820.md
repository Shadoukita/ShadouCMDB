### Security: docker-compose keeps the schema owner's connection out of the API container

In `docker-compose.yml` every service read `.env`, so a `MIGRATION_DATABASE_URL` (the schema owner)
or `MAINTENANCE_DATABASE_URL` put there also reached the long-running `api` container, which only
needs the `shadoucmdb_app` role. Now:

- `api` and `seed` set both variables to empty, so they never reach `serve` or `seed`, wherever
  they are defined.
- `migrate` also reads an optional `.env.migrate` for the owner and maintenance connections. It is
  also the service for `restore`, `factory-reset`, `decommission` and `prune-audit`
  (`docker compose run --rm migrate prune-audit --older-than 180d`).
- Passing the owner connection per run, `docker compose run --rm -e MIGRATION_DATABASE_URL migrate`,
  still works.

**Upgrade:** if your `.env` holds `MIGRATION_DATABASE_URL` or `MAINTENANCE_DATABASE_URL`, move them
to `.env.migrate` (`chmod 600`), or pass them per run with `-e`. Left in `.env`, they no longer reach
`api` or `seed`, and `migrate` keeps working. The optional `env_file` entry
needs Docker Compose 2.24 or later; on an older version, upgrade Compose. Installs without Compose
are unaffected.
