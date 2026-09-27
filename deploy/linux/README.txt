ShadouCMDB for Linux (x64 / ARM64)
==================================

This archive contains:

  shadoucmdb              The server: REST API, web UI and migrations in one
                          statically linked (musl) executable. It runs on any
                          Linux distribution of that architecture, with no
                          glibc, OpenSSL or other runtime dependency.
  shadoucmdb.service      Hardened systemd unit.
  shadoucmdb.env.example  Every setting, with a comment per variable.
  sql/bootstrap/          One-time database setup for a PostgreSQL admin:
                          00_create_role_and_database.sql for a new install,
                          10_split_roles.sql to upgrade a single-role install.
  README.txt              This file.

PostgreSQL is external: ShadouCMDB never installs or bundles a database. You need
a reachable PostgreSQL 14 or newer.
Full documentation: https://github.com/Shadoukita/ShadouCMDB/blob/main/docs/deployment.md


Create the database (once, as a PostgreSQL admin)
-------------------------------------------------

  psql "postgres://admin@db.example.internal:5432/postgres" \
       -v owner_password='<strong password 1>' \
       -v app_password='<strong password 2>' \
       -v maintenance_password='<strong password 3>' \
       -f sql/bootstrap/00_create_role_and_database.sql

This creates the database shadoucmdb and three roles, none of them superuser:

  shadoucmdb_owner        Runs `shadoucmdb migrate` (MIGRATION_DATABASE_URL).
                          Keep it out of the running server's environment.
  shadoucmdb_app          The server and every other command (DATABASE_URL or
                          PGHOST/PGUSER/PGPASSWORD/...).
  shadoucmdb_maintenance  `shadoucmdb prune-audit` only (MAINTENANCE_DATABASE_URL).

Upgrading an install that still has only shadoucmdb_app: see the header of
sql/bootstrap/10_split_roles.sql, or "Database roles" in docs/deployment.md.


Install as a systemd service
----------------------------

  sudo install -m 0755 shadoucmdb /usr/local/bin/shadoucmdb
  sudo useradd --system --no-create-home --shell /usr/sbin/nologin shadoucmdb
  sudo install -d -m 0750 -o root -g shadoucmdb /etc/shadoucmdb
  sudo install -m 0640 -o root -g shadoucmdb shadoucmdb.env.example /etc/shadoucmdb/shadoucmdb.env
  sudoedit /etc/shadoucmdb/shadoucmdb.env     # DATABASE_URL, or PGHOST/PGUSER/PGPASSWORD/..., as shadoucmdb_app
  # Migrate as the owner. The variable is passed to this one command only and
  # wins over the env file, so the owner's password never lands in it:
  sudo -u shadoucmdb env MIGRATION_DATABASE_URL='postgres://shadoucmdb_owner:<password 1>@db.example.internal:5432/shadoucmdb' \
    shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env migrate
  sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env seed
  # Optional starter data model (or install it later under Administration > Templates):
  sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env seed --template it_infrastructure
  # First administrator (or skip this and use first-run setup in the web UI):
  sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env create-admin --username admin
  sudo install -m 0644 shadoucmdb.service /etc/systemd/system/
  sudo systemctl daemon-reload && sudo systemctl enable --now shadoucmdb
  curl -s http://127.0.0.1:3000/readyz
  journalctl -u shadoucmdb -f


Upgrade
-------

  sudo install -m 0755 shadoucmdb /usr/local/bin/shadoucmdb
  sudo -u shadoucmdb env MIGRATION_DATABASE_URL='postgres://shadoucmdb_owner:<password 1>@db.example.internal:5432/shadoucmdb' \
    shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env migrate
  sudo systemctl restart shadoucmdb


Audit log retention
-------------------

Nothing is deleted automatically. Report, then delete, audit entries older than
180 days as shadoucmdb_maintenance:

  sudo -u shadoucmdb env MAINTENANCE_DATABASE_URL='postgres://shadoucmdb_maintenance:<password 3>@db.example.internal:5432/shadoucmdb' \
    shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env prune-audit --older-than 180d [--execute]


Verify the download
-------------------

  sha256sum --ignore-missing -c SHA256SUMS
