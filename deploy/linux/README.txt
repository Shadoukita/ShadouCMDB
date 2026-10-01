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


Create the database (once, as a PostgreSQL admin)
-------------------------------------------------

On any machine with psql (it need not be this server), from the directory you
extracted this archive to (or copy sql/bootstrap/ there):

  psql "postgres://admin@db.example.internal:5432/postgres" \
       -f sql/bootstrap/00_create_role_and_database.sql
  psql "postgres://admin@db.example.internal:5432/postgres" \
       -c '\password shadoucmdb_owner' \
       -c '\password shadoucmdb_app' \
       -c '\password shadoucmdb_maintenance'

The script creates the roles without a password; they cannot log in until the
second command has set one. \password prompts for each password twice and
sends the server only a SCRAM-SHA-256 verifier computed by psql, so the
password appears neither in the process list nor in the server log. (This
needs password_encryption = scram-sha-256 on the server, the default since
PostgreSQL 14.) Do not pass passwords as psql variables (-v): the script
refuses the owner_password, app_password and maintenance_password variables of
earlier releases.

Generate each password with `openssl rand -hex 24`. They go into connection
URLs, where characters such as @ : / # % ? must be percent-encoded (@ -> %40);
hex passwords need none.

Unattended setup (configuration management, CI): when psql has no terminal,
\password reads the password and its confirmation as two lines from standard
input. printf is a shell builtin, so the password stays out of the process
list:

  printf '%s\n%s\n' "$OWNER_PW" "$OWNER_PW" | setsid -w psql "$ADMIN_URL" -X \
       -v ON_ERROR_STOP=1 -c '\password shadoucmdb_owner'

setsid detaches psql from the terminal, if there is one, so that it reads
standard input instead of prompting.

This creates the database shadoucmdb and three roles, none of them superuser:

  shadoucmdb_owner        Runs `shadoucmdb migrate` (MIGRATION_DATABASE_URL).
                          Keep it out of the running server's environment.
  shadoucmdb_app          The server and every other command (DATABASE_URL or
                          PGHOST/PGUSER/PGPASSWORD/...).
  shadoucmdb_maintenance  `shadoucmdb prune-audit` only (MAINTENANCE_DATABASE_URL).

Upgrading an install that still has only shadoucmdb_app: see "Upgrade" below.


Install as a systemd service
----------------------------

  sudo install -m 0755 shadoucmdb /usr/local/bin/shadoucmdb
  sudo useradd --system --no-create-home --shell /usr/sbin/nologin shadoucmdb
  sudo install -d -m 0750 -o root -g shadoucmdb /etc/shadoucmdb
  sudo install -m 0640 -o root -g shadoucmdb shadoucmdb.env.example /etc/shadoucmdb/shadoucmdb.env
  sudoedit /etc/shadoucmdb/shadoucmdb.env     # DATABASE_URL, or PGHOST/PGUSER/PGPASSWORD/..., as shadoucmdb_app
  # The key that encrypts the authenticator secrets: readable by the service's
  # group only. Set ENCRYPTION_KEY_FILE=/etc/shadoucmdb/encryption.key in the
  # env file, and store a copy apart from the database backups (password vault):
  sudo shadoucmdb generate-encryption-key --out /etc/shadoucmdb/encryption.key
  sudo chown root:shadoucmdb /etc/shadoucmdb/encryption.key && sudo chmod 0640 /etc/shadoucmdb/encryption.key
  # Migrate as the owner. The password is prompted for, so it stays out of the
  # env file, shell history, the sudo log and `ps`; the variable wins over the
  # env file:
  read -rsp 'shadoucmdb_owner password: ' PW; echo
  export MIGRATION_DATABASE_URL="postgres://shadoucmdb_owner:$PW@db.example.internal:5432/shadoucmdb"
  sudo --preserve-env=MIGRATION_DATABASE_URL -u shadoucmdb \
    shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env migrate
  unset PW MIGRATION_DATABASE_URL
  sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env seed
  # Optional starter data model (or install it later under Administration > Templates):
  sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env seed --template it_infrastructure
  # First administrator (or skip this and use first-run setup in the web UI, with the setup token from
  # `sudo cat /var/lib/shadoucmdb/setup-token`; it is in the journal only if that file cannot be written):
  sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env create-admin --username admin
  sudo install -m 0644 shadoucmdb.service /etc/systemd/system/
  sudo systemctl daemon-reload && sudo systemctl enable --now shadoucmdb
  curl -s http://127.0.0.1:3000/readyz
  journalctl -u shadoucmdb -f


Upgrade
-------

If the database still has only shadoucmdb_app (installed before the three-role
setup), shadoucmdb_owner does not exist yet. Split the roles once first, in the
order given in the header of sql/bootstrap/10_split_roles.sql:
  1. install the new binary (first command below), then run migrate as
     before, without MIGRATION_DATABASE_URL:
       sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env migrate
  2. stop the server, run 10_split_roles.sql as a PostgreSQL admin, then set
     the passwords of the roles it created with \password, as for a new install:
       psql "postgres://admin@db.example.internal:5432/shadoucmdb" -f sql/bootstrap/10_split_roles.sql
       psql "postgres://admin@db.example.internal:5432/shadoucmdb" \
            -c '\password shadoucmdb_owner' -c '\password shadoucmdb_maintenance'
  3. start the server. From then on, migrate as shown below.

Upgrading from a release without ENCRYPTION_KEY_FILE: the server no longer
starts without it. Create the key once, before the restart below, as under
"Install as a systemd service" (generate-encryption-key, chown, chmod, the
env file line, a copy in your password vault). At its first start the server
encrypts the existing authenticator secrets; nobody has to set up two-factor
sign-in again. Going back to the previous release afterwards needs a restore of
a backup taken before the upgrade.

  sudo install -m 0755 shadoucmdb /usr/local/bin/shadoucmdb
  read -rsp 'shadoucmdb_owner password: ' PW; echo
  export MIGRATION_DATABASE_URL="postgres://shadoucmdb_owner:$PW@db.example.internal:5432/shadoucmdb"
  sudo --preserve-env=MIGRATION_DATABASE_URL -u shadoucmdb \
    shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env migrate
  unset PW MIGRATION_DATABASE_URL
  sudo systemctl restart shadoucmdb


Audit log retention
-------------------

Nothing is deleted automatically. Report, then delete, audit entries older than
180 days as shadoucmdb_maintenance:

  read -rsp 'shadoucmdb_maintenance password: ' PW; echo
  export MAINTENANCE_DATABASE_URL="postgres://shadoucmdb_maintenance:$PW@db.example.internal:5432/shadoucmdb"
  # Report what would go (the default is a dry run):
  sudo --preserve-env=MAINTENANCE_DATABASE_URL -u shadoucmdb \
    shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env prune-audit --older-than 180d
  # Delete it:
  sudo --preserve-env=MAINTENANCE_DATABASE_URL -u shadoucmdb \
    shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env prune-audit --older-than 180d --execute
  unset PW MAINTENANCE_DATABASE_URL


Verify the download
-------------------

  sha256sum --ignore-missing -c SHA256SUMS
