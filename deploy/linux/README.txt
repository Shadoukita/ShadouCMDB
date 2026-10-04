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
list. Guard the variable and check the result:

  : "${OWNER_PW:?OWNER_PW is unset or empty}"
  printf '%s\n%s\n' "$OWNER_PW" "$OWNER_PW" | setsid -w psql "$ADMIN_URL" -X \
       -v ON_ERROR_STOP=1 -c '\password shadoucmdb_owner'
  test "$(psql "$ADMIN_URL" -XAtc "SELECT rolpassword LIKE 'SCRAM-SHA-256\$%' \
       FROM pg_authid WHERE rolname = 'shadoucmdb_owner'")" = t

setsid detaches psql from the terminal, if there is one, so that it reads
standard input instead of prompting.

Why the guard and the check: when standard input is empty or runs out, psql
reads empty strings, the server answers "NOTICE: empty string is not a valid
password, clearing password", and psql still exits 0, even with
ON_ERROR_STOP. The role is left without a password and cannot log in, but the
step reports success and the installation fails later, when the server cannot
connect. A confirmation that differs from the password does exit non-zero;
only empty input does not. An unset or empty variable is the usual cause, so
the first line stops the script before psql runs. The last command checks the
outcome. It reads pg_authid, which needs a superuser administrator. Without
one, log in as the role instead; this works when pg_hba.conf requires a
password for the connection (PGPASSWORD is an environment variable of that
one command, not part of the command line):

  PGPASSWORD="$OWNER_PW" psql "postgres://shadoucmdb_owner@<db-host>:5432/shadoucmdb" \
       -X -c 'SELECT 1'

With several roles, give psql one password and one confirmation line for each
\password command, in the same order (six lines for three roles), or run one
psql per role. Too few lines leave the later roles without a password. Check
every role. Run the guard for every password variable.

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
  sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env create-admin --username admin --email admin@example.com
  sudo install -m 0644 shadoucmdb.service /etc/systemd/system/
  sudo systemctl daemon-reload && sudo systemctl enable --now shadoucmdb
  curl -s http://127.0.0.1:3000/readyz
  journalctl -u shadoucmdb -f

The unit sets LimitNOFILE=65536. The server raises its soft open-files limit
to the hard limit at start-up and logs the connection limit in effect
("connection limit", with max_connections and open_files). When you write
your own unit or init script, set a hard limit of at least 4 open files per
HTTP_MAX_CONCURRENT_REQUESTS plus a quarter on top (2,731 at the default of
512), or fewer connections are accepted.

The server speaks plain HTTP only. Sign-in passwords, session cookies and API
tokens cross the network unencrypted unless a TLS front end terminates HTTPS
for it. For anything beyond a quick evaluation:

  1. Put a TLS reverse proxy in front (nginx, Apache httpd, HAProxy or your
     load balancer), forwarding to http://127.0.0.1:3000/ and sending
     X-Forwarded-Proto so session cookies are marked Secure.
  2. When the proxy runs on this machine, set API_HOST=127.0.0.1 in
     shadoucmdb.env so port 3000 is not reachable from the network. When it
     runs elsewhere, let only the proxy's address reach port 3000 in the host
     firewall.
  3. Add the proxy's address to TRUSTED_PROXIES and set PUBLIC_URL to the
     https:// address users open.


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

Upgrading from a release where e-mail addresses were optional: from then on
API tokens of an account without an e-mail are refused (403 EMAIL_REQUIRED)
until it has one. Before the upgrade, give every service account that scripts
use (backup, monitoring, import) a unique e-mail under Administration > Users,
or right after migrate the same way. migrate prints the accounts without an
e-mail that own working tokens; their tokens work again once they have one.

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
