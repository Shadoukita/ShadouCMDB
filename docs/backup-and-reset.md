# Backup, restore, factory reset and decommission

The `shadoucmdb` binary has four commands for this. They need no pg_dump, no
PostgreSQL client tools and no superuser, and they work the same way on Linux,
Windows and in the distroless Docker image. They use the same database settings
as the server (`DATABASE_URL` or `PG*`, see [`.env.example`](../.env.example)):

- `backup` connects as the API role (`DATABASE_URL`), which can read every table.
- `restore`, `factory-reset` and `decommission` rebuild or drop the schema. Like
  `migrate`, they connect with `MIGRATION_DATABASE_URL` (the schema owner,
  `shadoucmdb_owner`) when it is set. On a single-role install it is unset
  and they use `DATABASE_URL`.

| Command | What it does | Changes the database |
| --- | --- | --- |
| `shadoucmdb backup [--out FILE]` | Writes every table to one file from a single consistent snapshot, then reads the file back and checks it | No |
| `shadoucmdb restore FILE [--replace] [--dry-run] [--yes]` | Checks the file, then restores it in one transaction | Yes (all or nothing) |
| `shadoucmdb factory-reset [--yes]` | Deletes everything and rebuilds an empty schema. No users exist, so first-run setup is forced | Yes (all or nothing) |
| `shadoucmdb decommission [--yes]` | Deletes every ShadouCMDB table, row and setting and rebuilds nothing | Yes (all or nothing) |

None of these are available over HTTP. A stolen session or API credential cannot
download, replace or wipe the database. Only someone who can run the binary with
the database credentials can.

## What a backup contains

- **Everything in the database:**
  - Every system table in the `cmdb` schema: CIs, relationships, the data model
    (areas, types, fields, relationship types, lookup lists), locations, owners,
    statuses, environments, users with their password hashes, permission
    profiles, UI settings including the logo and favicon, the schema change
    history and the full audit log.
  - The schema of every area (for example `infrastruktur`) with the table of each
    type, which holds the field values of the CIs.
  - The migration level.
- **Not the structure of the type tables.** Their columns, checks, foreign keys,
  indexes and reporting views follow from the data model. `restore` rebuilds
  them from the data model in the backup, with the same engine that builds them
  when an administrator edits a type.
- **Not the sessions.** Restoring them would sign people back in with tokens from
  the past. After a restore, everyone signs in again.
- **Not the server's own keys** (`server_keys`). The restored server generates a
  new key for OIDC sign-ins; a sign-in in progress during the restore ends with
  "expired", and the user starts it again.
- **Not what lives outside the database:** the env file (database password,
  settings), log files and the binary. Keep a copy of the env file in your
  secrets store. It is configuration, not data, and it must not sit next to the
  backups.

### Consistency checks

- All tables are read in one `REPEATABLE READ` snapshot. The backup is consistent
  even while the server keeps writing, so you don't need to stop it.
- The file is gzip-compressed JSON Lines. It holds a header (migrations and
  their checksums, tables, columns, row counts, sequences), one section per
  table, and an end marker with the total row count and the SHA-256 of the
  whole content. `zcat FILE | head -1` shows the header.
- `backup` writes to `FILE.partial`, reads it back, checks the SHA-256 and every
  row count, and only then renames it to `FILE`. A backup that stopped halfway
  never looks like a finished one. An existing file is never overwritten.
- `restore` checks the whole file (SHA-256, structure, row counts) **before** it
  connects. It then checks that this binary knows every migration in the backup
  with identical SQL, and that the schema at that level has exactly the backup's
  system tables and columns. After the system tables are loaded, it rebuilds the
  area schemas and type tables from the data model and checks that they match
  the backup's tables and columns as well. Foreign keys are re-created after the load, so every
  reference is checked again, and each table's row count is compared with the
  header. The file is hashed a second time while it is loaded. If anything
  fails, the transaction rolls back and the database is left as it was.

### Protecting backup files

A backup holds password hashes (argon2id) and personal data: names, email
addresses, and the IP addresses and user agents in the audit log. It also holds
secrets the server stores unencrypted: every user's authenticator (TOTP)
secret, the OIDC client secret and the directory bind password (see
[secrets at rest](security/hardening.md#secrets-and-configuration)). Treat it
like the database itself.

- On Linux and macOS, `backup` creates the file readable by its owner only (mode `0600`).
- Encrypt backups before they leave the host (for example `age -r <recipient>` or
  `gpg --encrypt`), and store them on a different system from the database.
- The SHA-256 protects against damage, not against tampering. Anyone who can
  change a backup can also recompute its checksum, and a changed backup could
  add an administrator. Only restore files from your own protected backup store.
- Define a retention period and delete expired backups. Data that someone asked
  to have erased stays in older backups until those backups expire.

## Taking backups

```sh
shadoucmdb backup                                      # ./shadoucmdb-backup-20260926T120000Z.jsonl.gz
shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env backup --out /var/backups/shadoucmdb/cmdb-$(date -u +%F).jsonl.gz
```

```
Backing up database "shadoucmdb" on 10.0.4.12:5432
Wrote 197 rows from 31 tables at migration 9 (19407 bytes)
Verified: SHA-256 and row counts match
Backup: cmdb-2026-09-26.jsonl.gz
```

**Linux, daily with systemd:** a oneshot service and a timer next to
`shadoucmdb.service`:

```ini
# /etc/systemd/system/shadoucmdb-backup.service
[Service]
Type=oneshot
User=shadoucmdb
UMask=0077
ExecStart=/bin/sh -c 'exec /usr/local/bin/shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env backup --out /var/backups/shadoucmdb/cmdb-$(date -u +%%Y%%m%%dT%%H%%M%%SZ).jsonl.gz'

# /etc/systemd/system/shadoucmdb-backup.timer
[Timer]
OnCalendar=daily
Persistent=true
[Install]
WantedBy=timers.target
```

Enable it with `systemctl enable --now shadoucmdb-backup.timer`, then add your
own step that encrypts the files, ships them off the host and deletes expired ones.

**Windows:** schedule the same command with Task Scheduler, running as the service
account:
`shadoucmdb.exe --env-file C:\ProgramData\ShadouCMDB\shadoucmdb.env backup --out D:\Backups\cmdb.jsonl.gz`.
The file inherits the folder's ACL, so restrict `D:\Backups` to administrators
and the service account.

**Docker:** mount a directory that the image's non-root user (uid 65532) can write to:

```sh
install -d -m 0700 -o 65532 ./backups
docker run --rm --env-file .env -v "$PWD/backups:/backups" shadoucmdb backup --out /backups/cmdb.jsonl.gz
```

**Database-level backups as well.** Managed PostgreSQL snapshots and
point-in-time recovery, or `pg_dump -Fc` if you run PostgreSQL yourself, still
make sense as a second line of defence. `shadoucmdb backup` is the portable one:
it restores into any PostgreSQL 14 or newer, on any host, with any later
ShadouCMDB release.

## Restoring

1. **Stop the server** (`systemctl stop shadoucmdb`, stop the Windows Service or
   the container). `restore`, `factory-reset` and `decommission` refuse to run
   while another ShadouCMDB connection is open to the database. That check
   cannot see a server that has not opened a connection yet, so stop the server
   anyway.
2. **Optional: rehearse it.** `--dry-run` does the complete restore, including
   every check, and then rolls it back:

   ```sh
   shadoucmdb restore cmdb-2026-09-26.jsonl.gz --replace --dry-run
   ```

3. **Restore.** Into a new, empty database, created as in the README (on a
   three-role install, as `sql/bootstrap/00_create_role_and_database.sql` does):

   ```sh
   shadoucmdb restore cmdb-2026-09-26.jsonl.gz
   ```

   Or over an existing installation. All of its current data is deleted first.
   You have to type the database name to confirm, or pass `--yes` in scripts:

   ```sh
   shadoucmdb restore cmdb-2026-09-26.jsonl.gz --replace
   ```

   ```
   Checking cmdb-2026-09-26.jsonl.gz ...
   Backup of database "shadoucmdb" taken 2026-09-26 00:14:14 UTC by ShadouCMDB 0.1.0: 197 rows in 31 tables, migration 9
   File is intact (SHA-256 and row counts match) and fits this release
   Restored 197 rows into database "shadoucmdb" on 10.0.4.12:5432
   1 user(s) restored; sessions are not part of a backup, so everyone signs in again
   ```

4. **Start the server.** `GET /readyz` reports the migration state. Users sign in
   with the passwords they had when the backup was taken.

The area schemas and type tables are rebuilt as they were, owned by the same
role: the API role on a three-role install, so administrators can keep editing
types. Restoring is not a schema change, so `cmdb.schema_changes` holds exactly
the backup's history. One case differs on purpose. A required field that some
CIs have no value for stays nullable, and `restore` lists it as a warning,
exactly as `migrate` does.

In Docker, add `-it` so the confirmation can be typed, or pass `--yes`:
`docker run --rm -it --env-file .env -v "$PWD/backups:/backups" shadoucmdb restore /backups/cmdb.jsonl.gz --replace`.

### Versions

- **A backup from an older release** restores into a newer one. The schema is
  built up to the backup's migration level, the rows are loaded, and the newer
  migrations then run on top, exactly as an upgrade would. For example, a backup
  from before migration 0009 gets its type tables built and its attribute values
  moved into them.
- **A backup from a newer release** is refused ("migration NNNN which this
  binary does not know"). Restore it with that release or a later one.
- A backup whose migration SQL differs from this binary's (a modified build) is
  refused as well.

### Test your backups

A backup nobody has restored is a hope, not a backup. Now and then, restore the
latest one with `--dry-run` into a scratch database, or for real into a staging
database. Record the date and the result.

## Factory reset

This returns an installation to the state of a fresh install. It deletes every
CI, relationship, the data model with every area schema and type table, every
user, the settings, the logo, the schema change history and the audit log. It
then rebuilds the empty schema through the migrations. No user
exists afterwards, so the web UI shows first-run setup again. `create-admin`
works as well.

```sh
shadoucmdb backup --out before-reset.jsonl.gz     # if there is anything worth keeping
shadoucmdb factory-reset                          # stop the server first; asks for the database name
```

If you have only lost access, you don't need a reset: `shadoucmdb create-admin`
adds a new administrator and keeps everything else (see
[deployment.md](deployment.md#the-first-administrator)).

## Decommissioning

`decommission` deletes every ShadouCMDB object from the database:
- the schema of every area, with its type tables and reporting views;
- the `cmdb` schema, with all system tables and their rows, functions and sequences;
- the migration history;
- the old Node/Drizzle bookkeeping schema. Nothing is rebuilt. It then checks that nothing
is left. The `pg_trgm` extension stays, because it may be shared and holds no data.

```sh
shadoucmdb decommission          # stop the server first; asks for the database name
```

The command removes the data. Removing the installation takes these further
steps:

1. **Database and role**, as a PostgreSQL administrator:

   ```sql
   DROP DATABASE shadoucmdb;
   DROP ROLE shadoucmdb_app;
   -- three-role install: the other two as well
   DROP ROLE shadoucmdb_owner;
   DROP ROLE shadoucmdb_maintenance;
   ```

   On a managed service, delete the database there, and the whole instance if it
   served only ShadouCMDB. The provider's own snapshots and PITR history are
   separate: delete them too, or let them expire.
2. **Service:** `systemctl disable --now shadoucmdb` and remove the unit files,
   `shadoucmdb service uninstall` on Windows, or remove the container and its
   compose files.
3. **Settings and secrets:** delete the env file, which holds the database
   password, and rotate that password if the role is being kept. Revoke the TLS
   certificate of the reverse proxy if it is no longer used.
4. **Logs and binary:** delete the log files, the binary and the Docker image.
5. **Backups:** destroy them, or keep them until the end of your retention
   period and then destroy them. They contain the same personal data as the
   database.

Deleting database rows marks storage as free. It does not overwrite the disk.
For media that held personal data, rely on encrypted storage or on your
provider's media sanitisation, not on `DELETE`.

## Troubleshooting

| Message | Meaning |
| --- | --- |
| `N other ShadouCMDB connection(s) are open` | The server or another `shadoucmdb` command is still connected. Stop it and retry. |
| `already contains ShadouCMDB tables` | `restore` without `--replace` only writes into an empty database. |
| `no terminal to confirm on` | A script or container without `-it`: pass `--yes`. |
| `failed the consistency check` | The file is damaged, truncated or was changed. Use another backup. |
| `which this binary does not know` | The backup is from a newer release. Restore it with that release. |
| `could not run: DROP ...: must be owner` | Someone created objects in the schema as another role. Drop them as that role, or as the database owner, and retry. On a three-role install, set `MIGRATION_DATABASE_URL`. |
| `columns of AREA.TYPE differ between the backup (...) and the data model in the backup (...)` | Someone added or removed a column of a type table by hand, outside ShadouCMDB. Restore with a backup taken before that, or ask for help: the extra column's data is in the file. |

## Limits

- The backup covers the `cmdb` schema, the area schemas and what migrations
  before 0008 kept in `public`. Objects that someone added to these schemas by
  hand (a DBA's view in an area, for example) are not backed up, and are dropped
  by `--replace`, `factory-reset` and `decommission`. Keep the database dedicated
  to ShadouCMDB, and put your own reporting objects in a schema of their own.
- A `jsonb` value that is the JSON literal `null` is restored as SQL `NULL`.
  ShadouCMDB does not store such values.
- Restore loads the rows in one transaction. On very large databases, make sure
  the database has room for the extra WAL and that nothing else needs its tables
  locked while the restore runs.
