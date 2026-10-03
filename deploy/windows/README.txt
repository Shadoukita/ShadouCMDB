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


Create the database (once, as a PostgreSQL admin)
-------------------------------------------------

On any machine with psql (it need not be this server), in PowerShell, from the
folder you extracted this archive to (or copy sql\bootstrap\ there):

  psql "postgres://admin@db.example.internal:5432/postgres" `
       -f sql\bootstrap\00_create_role_and_database.sql
  psql "postgres://admin@db.example.internal:5432/postgres" `
       -c '\password shadoucmdb_owner' `
       -c '\password shadoucmdb_app' `
       -c '\password shadoucmdb_maintenance'

The script creates the roles without a password; they cannot log in until the
second command has set one. \password prompts for each password twice and
sends the server only a SCRAM-SHA-256 verifier computed by psql, so the
password appears neither in the PowerShell history, the process list nor the
server log. (This needs password_encryption = scram-sha-256 on the server, the
default since PostgreSQL 14.) Do not pass passwords as psql variables (-v): the
script refuses the owner_password, app_password and maintenance_password
variables of earlier releases.

Use long random passwords, e.g. 48 hex characters. They go into connection URLs,
where characters such as @ : / # % ? must be percent-encoded (@ -> %40); the
commands below encode the owner and maintenance passwords for you, but the
shadoucmdb_app password in DATABASE_URL you encode yourself (or use PGPASSWORD).

This creates the database shadoucmdb and three roles, none of them superuser:

  shadoucmdb_owner        Runs `shadoucmdb migrate` (MIGRATION_DATABASE_URL).
                          Keep it out of the service's environment.
  shadoucmdb_app          The service and every other command (DATABASE_URL or
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
  # In the env file, set DATABASE_URL (or PGHOST/PGUSER/PGPASSWORD/...) as shadoucmdb_app,
  # and these two lines, in single quotes (unquoted, the backslashes are read as
  # escapes and the service does not start):
  #   ENCRYPTION_KEY_FILE='C:\ProgramData\ShadouCMDB\encryption.key'
  #   SETUP_TOKEN_FILE='C:\ProgramData\ShadouCMDB\state\setup-token'
  notepad "$data\shadoucmdb.env"

  # Only administrators and SYSTEM may change anything in the data folder; other
  # users get no access to it at all (the service is granted read access below).
  # Any user may create folders under C:\ProgramData, and the owner of a file or
  # folder can always change its permissions again, so Administrators take
  # ownership of everything in the data folder. Links are refused: the recursive
  # commands would follow a junction or symbolic link out of the folder, and a
  # folder the check cannot read stops it.
  function Find-ReparsePoint($dir) {
    Get-ChildItem -LiteralPath $dir -Force -ErrorAction Stop | ForEach-Object {
      if ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) { $_.FullName }
      elseif ($_.PSIsContainer) { Find-ReparsePoint $_.FullName } }
  }
  if ((Get-Item -LiteralPath $data -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "$data is a link" }
  icacls $data /setowner '*S-1-5-32-544' /C
  icacls $data /reset /C
  icacls $data /inheritance:r /grant:r 'Administrators:(OI)(CI)F' 'SYSTEM:(OI)(CI)F'
  if ($links = Find-ReparsePoint $data) { throw "Remove these links from the data folder first: $links" }
  New-Item -ItemType Directory -Force "$data\logs", "$data\state" | Out-Null
  icacls $data /setowner '*S-1-5-32-544' /T /C
  icacls "$data\*" /reset /T /C

  # The key that encrypts the authenticator secrets. Store a copy apart from the
  # database backups (password vault):
  & "$bin\shadoucmdb.exe" generate-encryption-key --out "$data\encryption.key"

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
  # "$data\state\setup-token"; it is in the log file only if that file cannot be written):
  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" create-admin --username admin --email admin@example.com
  # Register the service. It runs as its own virtual account, NT SERVICE\ShadouCMDB,
  # which Windows creates with the service; no other service shares it.
  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" --log-file "$data\logs\shadoucmdb.log" service install

  # The service reads its settings and the key, and writes only to logs\ and state\
  # (the first-run setup token). It cannot change the env file or the key:
  $svc = 'NT SERVICE\ShadouCMDB'
  icacls $data /grant "${svc}:RX"
  icacls "$data\shadoucmdb.env" /grant "${svc}:R"
  icacls "$data\encryption.key" /inheritance:r /grant:r 'Administrators:F' 'SYSTEM:F' "${svc}:R"
  icacls "$data\logs" /grant "${svc}:(OI)(CI)M"
  icacls "$data\state" /grant "${svc}:(OI)(CI)M"
  Start-Service ShadouCMDB
  Invoke-RestMethod http://127.0.0.1:3000/readyz

The service starts automatically at boot and restarts 10 s after a failure. Logs
(one JSON object per line) go to C:\ProgramData\ShadouCMDB\logs\shadoucmdb.log.

The server speaks plain HTTP only. Sign-in passwords, session cookies and API
tokens cross the network unencrypted unless a TLS front end terminates HTTPS
for it. For anything beyond a quick evaluation:

  1. Put a TLS reverse proxy in front (IIS with Application Request Routing,
     nginx, HAProxy or your load balancer), forwarding to
     http://127.0.0.1:3000/ and sending X-Forwarded-Proto so session cookies
     are marked Secure.
  2. When the proxy runs on this machine, set API_HOST=127.0.0.1 in
     shadoucmdb.env and open no firewall port for 3000; open only the proxy's
     443. When it runs elsewhere, allow port 3000 from the proxy's address
     only (-RemoteAddress <proxy address> on the rule below).
  3. Add the proxy's address to TRUSTED_PROXIES and set PUBLIC_URL to the
     https:// address users open.

To let other machines connect directly (evaluation only, unencrypted):

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
  2. run 10_split_roles.sql as a PostgreSQL admin, then set the passwords of
     the roles it created with \password, as for a new install:
       psql "postgres://admin@db.example.internal:5432/shadoucmdb" -f sql\bootstrap\10_split_roles.sql
       psql "postgres://admin@db.example.internal:5432/shadoucmdb" `
            -c '\password shadoucmdb_owner' -c '\password shadoucmdb_maintenance'
  3. start the service. From then on, migrate as shown below.

Upgrading from a release without ENCRYPTION_KEY_FILE: the service no longer
starts without it. Create the key once, before Start-Service below, as under
the install steps (generate-encryption-key, icacls, the env file line, a copy
in your password vault). At its first start the service encrypts the existing
authenticator secrets; nobody has to set up two-factor sign-in again. Going
back to the previous release afterwards needs a restore of a backup taken
before the upgrade.

Upgrading an install whose service runs as NT AUTHORITY\LocalService (installed
before the service had its own account): LocalService is shared by many Windows
services, and the old install steps let it change the env file. Switch the
service to NT SERVICE\ShadouCMDB once, after the migrate command below and
before Start-Service. Add SETUP_TOKEN_FILE='C:\ProgramData\ShadouCMDB\state\setup-token'
to the env file, then:

  $bin  = 'C:\Program Files\ShadouCMDB'
  $data = 'C:\ProgramData\ShadouCMDB'
  $svc  = 'NT SERVICE\ShadouCMDB'
  & "$bin\shadoucmdb.exe" service uninstall
  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" --log-file "$data\logs\shadoucmdb.log" service install

  # LocalService still owns the files it created (the log, the setup token, and
  # anything another LocalService service put there), and an owner can grant
  # itself access again. Lock the folder and take ownership as for a new
  # install; /reset also drops every LocalService permission:
  function Find-ReparsePoint($dir) {
    Get-ChildItem -LiteralPath $dir -Force -ErrorAction Stop | ForEach-Object {
      if ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) { $_.FullName }
      elseif ($_.PSIsContainer) { Find-ReparsePoint $_.FullName } }
  }
  if ((Get-Item -LiteralPath $data -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "$data is a link" }
  icacls $data /setowner '*S-1-5-32-544' /C
  icacls $data /reset /C
  icacls $data /inheritance:r /grant:r 'Administrators:(OI)(CI)F' 'SYSTEM:(OI)(CI)F'
  if ($links = Find-ReparsePoint $data) { throw "Remove these links from the data folder first: $links" }
  Remove-Item "$data\setup-token" -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force "$data\logs", "$data\state" | Out-Null
  icacls $data /setowner '*S-1-5-32-544' /T /C
  icacls "$data\*" /reset /T /C

  # The same grants as for a new install:
  icacls $data /grant "${svc}:RX"
  icacls "$data\shadoucmdb.env" /grant "${svc}:R"
  icacls "$data\encryption.key" /inheritance:r /grant:r 'Administrators:F' 'SYSTEM:F' "${svc}:R"
  icacls "$data\logs" /grant "${svc}:(OI)(CI)M"
  icacls "$data\state" /grant "${svc}:(OI)(CI)M"
  icacls $data /T /C        # check: no NT AUTHORITY\LOCAL SERVICE entries remain
  # Check: no output (every file and folder is owned by Administrators):
  @(Get-Item $data -Force) + @(Get-ChildItem $data -Recurse -Force) | Get-Acl |
    Where-Object { $_.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne 'S-1-5-32-544' } | Select-Object Path, Owner

Use the same --env-file and --log-file paths as the original install. A service
installed with --account (a domain or managed service account) keeps it: skip
the uninstall and install commands and set $svc to that account instead.

The virtual account signs in to other computers on the network as the computer
account (DOMAIN\HOST$), where LocalService connected anonymously. ShadouCMDB
only connects to the PostgreSQL, LDAP and OIDC servers you configure; check
that no file share or other server grants the computer account access the
service should not have.

  Stop-Service ShadouCMDB
  Copy-Item .\shadoucmdb.exe 'C:\Program Files\ShadouCMDB' -Force
  $pw = [uri]::EscapeDataString((Get-Credential shadoucmdb_owner).GetNetworkCredential().Password)
  $env:MIGRATION_DATABASE_URL = "postgres://shadoucmdb_owner:$pw@db.example.internal:5432/shadoucmdb"
  & 'C:\Program Files\ShadouCMDB\shadoucmdb.exe' --env-file 'C:\ProgramData\ShadouCMDB\shadoucmdb.env' migrate
  Remove-Item Env:MIGRATION_DATABASE_URL; Remove-Variable pw
  Start-Service ShadouCMDB


Audit log retention
-------------------

Nothing is deleted automatically. Report, then delete, audit entries older than
180 days as shadoucmdb_maintenance:

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
