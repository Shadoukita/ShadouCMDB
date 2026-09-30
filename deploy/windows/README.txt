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

On any machine with psql (it need not be this server), in PowerShell, from the
folder you extracted this archive to (or copy sql\bootstrap\ there). The
passwords are prompted for, so they stay out of the PowerShell history:

  $owner = (Get-Credential shadoucmdb_owner).GetNetworkCredential().Password
  $app   = (Get-Credential shadoucmdb_app).GetNetworkCredential().Password
  $maint = (Get-Credential shadoucmdb_maintenance).GetNetworkCredential().Password
  psql "postgres://admin@db.example.internal:5432/postgres" `
       -v "owner_password=$owner" `
       -v "app_password=$app" `
       -v "maintenance_password=$maint" `
       -f sql\bootstrap\00_create_role_and_database.sql
  Remove-Variable owner, app, maint

Use long random passwords, e.g. 48 hex characters. They go into connection URLs,
where characters such as @ : / # % ? must be percent-encoded (@ -> %40); the
commands below encode the owner and maintenance passwords for you, but the
shadoucmdb_app password in DATABASE_URL you encode yourself (or use PGPASSWORD).

This creates the database shadoucmdb and three roles, none of them superuser:

  shadoucmdb_owner        `shadoucmdb migrate`, `restore`, `factory-reset` and
                          `decommission` (MIGRATION_DATABASE_URL). Keep it out of
                          the service's environment.
  shadoucmdb_app          The service and every other command, including `seed`,
                          `verify`, `create-admin` and `backup` (DATABASE_URL or
                          PGHOST/PGUSER/PGPASSWORD/...).
  shadoucmdb_maintenance  `shadoucmdb prune-audit` only (MAINTENANCE_DATABASE_URL).

Upgrading an install that still has only shadoucmdb_app: see "Upgrade" below.


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

  # The key that encrypts the authenticator secrets: read-only for the service,
  # outside the Modify grant above. Add ENCRYPTION_KEY_FILE='C:\ProgramData\ShadouCMDB\encryption.key'
  # to the env file, in single quotes (unquoted, the backslashes are read as escapes and the
  # service does not start), and store a copy apart from the database backups (password vault):
  & "$bin\shadoucmdb.exe" generate-encryption-key --out "$data\encryption.key"
  icacls "$data\encryption.key" /inheritance:r /grant:r 'Administrators:F' 'SYSTEM:F' 'NT AUTHORITY\LocalService:R'

  # Migrate as the owner. The password is prompted for, so it stays out of the
  # env file and the PowerShell history; the variable is set for this session
  # only and wins over the env file:
  $pw = [uri]::EscapeDataString((Get-Credential shadoucmdb_owner).GetNetworkCredential().Password)
  $env:MIGRATION_DATABASE_URL = "postgres://shadoucmdb_owner:$pw@db.example.internal:5432/shadoucmdb"
  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" migrate
  Remove-Item Env:MIGRATION_DATABASE_URL; Remove-Variable pw
  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" seed
  # Optional starter data model (or install it later under Administration > Templates):
  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" seed --template it_infrastructure
  # First administrator (or skip this and use first-run setup in the web UI, with the setup token from
  # "$data\setup-token" or the log file):
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

If the database still has only shadoucmdb_app (installed before the three-role
setup), shadoucmdb_owner does not exist yet. Split the roles once first, in the
order given in the header of sql\bootstrap\10_split_roles.sql:
  1. stop the service and copy the new binary (first two commands below),
     then run migrate as before, without MIGRATION_DATABASE_URL:
       & 'C:\Program Files\ShadouCMDB\shadoucmdb.exe' --env-file 'C:\ProgramData\ShadouCMDB\shadoucmdb.env' migrate
  2. run 10_split_roles.sql as a PostgreSQL admin;
  3. start the service. From then on, migrate as shown below.

Upgrading from a release without ENCRYPTION_KEY_FILE: the service no longer
starts without it. Create the key once, before Start-Service below, as under
the install steps (generate-encryption-key, icacls, the env file line, a copy
in your password vault). At its first start the service encrypts the existing
authenticator secrets; nobody has to set up two-factor sign-in again. Going
back to the previous release afterwards needs a restore of a backup taken
before the upgrade.

  Stop-Service ShadouCMDB
  Copy-Item .\shadoucmdb.exe 'C:\Program Files\ShadouCMDB' -Force
  $pw = [uri]::EscapeDataString((Get-Credential shadoucmdb_owner).GetNetworkCredential().Password)
  $env:MIGRATION_DATABASE_URL = "postgres://shadoucmdb_owner:$pw@db.example.internal:5432/shadoucmdb"
  & 'C:\Program Files\ShadouCMDB\shadoucmdb.exe' --env-file 'C:\ProgramData\ShadouCMDB\shadoucmdb.env' migrate
  Remove-Item Env:MIGRATION_DATABASE_URL; Remove-Variable pw
  Start-Service ShadouCMDB


Audit log retention
-------------------

Nothing is deleted automatically. Report, then delete, authentication events
(sign-ins, sessions, two-factor and API token use; the default scope "auth")
older than 180 days as shadoucmdb_maintenance. Change history is kept unless you
add `--scope changes`:

  $pw = [uri]::EscapeDataString((Get-Credential shadoucmdb_maintenance).GetNetworkCredential().Password)
  $env:MAINTENANCE_DATABASE_URL = "postgres://shadoucmdb_maintenance:$pw@db.example.internal:5432/shadoucmdb"
  # Report what would go (the default is a dry run), then delete it:
  & 'C:\Program Files\ShadouCMDB\shadoucmdb.exe' --env-file 'C:\ProgramData\ShadouCMDB\shadoucmdb.env' prune-audit --older-than 180d
  & 'C:\Program Files\ShadouCMDB\shadoucmdb.exe' --env-file 'C:\ProgramData\ShadouCMDB\shadoucmdb.env' prune-audit --older-than 180d --execute
  Remove-Item Env:MAINTENANCE_DATABASE_URL; Remove-Variable pw


Uninstall
---------

  & 'C:\Program Files\ShadouCMDB\shadoucmdb.exe' service uninstall

This stops and removes the service. Your settings, logs and the database are
left untouched.


Verify the download
-------------------

Compare the hash with the SHA256SUMS file attached to the GitHub Release:

  Get-FileHash .\shadoucmdb-<version>-windows-x64.zip -Algorithm SHA256
