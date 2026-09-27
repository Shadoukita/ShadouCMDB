ShadouCMDB for Windows Server x64
=================================

This archive contains:

  shadoucmdb.exe            The server: REST API, web UI, migrations and the
                            Windows Service integration, in one file. It needs
                            no runtime, no Visual C++ Redistributable, no OpenSSL.
  shadoucmdb.env.example    Every setting, with a comment per variable.
  sql\bootstrap\            One-time database setup for a PostgreSQL admin:
                            00_create_role_and_database.sql for a new install,
                            10_split_roles.sql to upgrade a single-role install.
  README.txt                This file.

PostgreSQL is external: ShadouCMDB never installs or bundles a database. You need
a reachable PostgreSQL 14 or newer.
Full documentation: https://github.com/Shadoukita/ShadouCMDB/blob/main/docs/deployment.md


Create the database (once, as a PostgreSQL admin)
-------------------------------------------------

On any machine with psql (it need not be this server):

  psql "postgres://admin@db.example.internal:5432/postgres" `
       -v owner_password='<strong password 1>' `
       -v app_password='<strong password 2>' `
       -v maintenance_password='<strong password 3>' `
       -f sql\bootstrap\00_create_role_and_database.sql

This creates the database shadoucmdb and three roles, none of them superuser:

  shadoucmdb_owner        Runs `shadoucmdb migrate` (MIGRATION_DATABASE_URL).
                          Keep it out of the service's environment.
  shadoucmdb_app          The service and every other command (DATABASE_URL or
                          PGHOST/PGUSER/PGPASSWORD/...).
  shadoucmdb_maintenance  `shadoucmdb prune-audit` only (MAINTENANCE_DATABASE_URL).

Upgrading an install that still has only shadoucmdb_app: see the header of
sql\bootstrap\10_split_roles.sql, or "Database roles" in docs/deployment.md.


Install as a Windows Service
----------------------------

Run in an elevated PowerShell, from the folder you extracted this archive to:

  $bin  = 'C:\Program Files\ShadouCMDB'
  $data = 'C:\ProgramData\ShadouCMDB'
  New-Item -ItemType Directory -Force $bin, $data | Out-Null
  Copy-Item .\shadoucmdb.exe $bin
  Copy-Item .\shadoucmdb.env.example "$data\shadoucmdb.env"
  notepad "$data\shadoucmdb.env"      # DATABASE_URL, or PGHOST/PGUSER/PGPASSWORD/..., as shadoucmdb_app

  # The service runs as the low-privilege LocalService account: let it read the
  # settings and write its log, and keep the env file away from other users.
  icacls $data /inheritance:r /grant:r 'Administrators:(OI)(CI)F' 'SYSTEM:(OI)(CI)F' 'NT AUTHORITY\LocalService:(OI)(CI)M'

  # Migrate as the owner. The variable is set for this session only and wins
  # over the env file, so the owner's password never lands in it:
  $env:MIGRATION_DATABASE_URL = 'postgres://shadoucmdb_owner:<password 1>@db.example.internal:5432/shadoucmdb'
  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" migrate
  Remove-Item Env:MIGRATION_DATABASE_URL
  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" seed
  # Optional starter data model (or install it later under Administration > Templates):
  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" seed --template it_infrastructure
  # First administrator (or skip this and use first-run setup in the web UI):
  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" create-admin --username admin
  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" --log-file "$data\logs\shadoucmdb.log" service install
  Start-Service ShadouCMDB
  Invoke-RestMethod http://127.0.0.1:3000/readyz

The service starts automatically at boot and restarts 10 s after a failure. Logs
(one JSON object per line) go to C:\ProgramData\ShadouCMDB\logs\shadoucmdb.log.

To let other machines connect:

  New-NetFirewallRule -DisplayName ShadouCMDB -Direction Inbound -Protocol TCP -LocalPort 3000 -Action Allow

Then open http://<server>:3000/ in a browser.


Upgrade
-------

  Stop-Service ShadouCMDB
  Copy-Item .\shadoucmdb.exe 'C:\Program Files\ShadouCMDB' -Force
  $env:MIGRATION_DATABASE_URL = 'postgres://shadoucmdb_owner:<password 1>@db.example.internal:5432/shadoucmdb'
  & 'C:\Program Files\ShadouCMDB\shadoucmdb.exe' --env-file 'C:\ProgramData\ShadouCMDB\shadoucmdb.env' migrate
  Remove-Item Env:MIGRATION_DATABASE_URL
  Start-Service ShadouCMDB


Audit log retention
-------------------

Nothing is deleted automatically. Report, then delete, audit entries older than
180 days as shadoucmdb_maintenance:

  $env:MAINTENANCE_DATABASE_URL = 'postgres://shadoucmdb_maintenance:<password 3>@db.example.internal:5432/shadoucmdb'
  & 'C:\Program Files\ShadouCMDB\shadoucmdb.exe' --env-file 'C:\ProgramData\ShadouCMDB\shadoucmdb.env' prune-audit --older-than 180d
  & 'C:\Program Files\ShadouCMDB\shadoucmdb.exe' --env-file 'C:\ProgramData\ShadouCMDB\shadoucmdb.env' prune-audit --older-than 180d --execute
  Remove-Item Env:MAINTENANCE_DATABASE_URL


Uninstall
---------

  & 'C:\Program Files\ShadouCMDB\shadoucmdb.exe' service uninstall

This stops and removes the service. Your settings, logs and the database are
left untouched.


Verify the download
-------------------

Compare the hash with the SHA256SUMS file attached to the GitHub Release:

  Get-FileHash .\shadoucmdb-<version>-windows-x64.zip -Algorithm SHA256
