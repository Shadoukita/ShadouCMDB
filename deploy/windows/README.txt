ShadouCMDB for Windows Server x64
=================================

This archive contains:

  shadoucmdb.exe            The server: REST API, web UI, migrations and the
                            Windows Service integration, in one file. It needs
                            no runtime, no Visual C++ Redistributable, no OpenSSL.
  shadoucmdb.env.example    Every setting, with a comment per variable.
  README.txt                This file.

PostgreSQL is external: ShadouCMDB never installs or bundles a database. You need
a reachable PostgreSQL 14 or newer and a database plus a login role for the app.
Full documentation: https://github.com/Shadoukita/ShadouCMDB/blob/main/docs/deployment.md


Install as a Windows Service
----------------------------

Run in an elevated PowerShell, from the folder you extracted this archive to:

  $bin  = 'C:\Program Files\ShadouCMDB'
  $data = 'C:\ProgramData\ShadouCMDB'
  New-Item -ItemType Directory -Force $bin, $data | Out-Null
  Copy-Item .\shadoucmdb.exe $bin
  Copy-Item .\shadoucmdb.env.example "$data\shadoucmdb.env"
  notepad "$data\shadoucmdb.env"      # set DATABASE_URL, or PGHOST/PGUSER/PGPASSWORD/...

  # The service runs as the low-privilege LocalService account: let it read the
  # settings and write its log, and keep the env file away from other users.
  icacls $data /inheritance:r /grant:r 'Administrators:(OI)(CI)F' 'SYSTEM:(OI)(CI)F' 'NT AUTHORITY\LocalService:(OI)(CI)M'

  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" migrate
  & "$bin\shadoucmdb.exe" --env-file "$data\shadoucmdb.env" seed
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
  & 'C:\Program Files\ShadouCMDB\shadoucmdb.exe' --env-file 'C:\ProgramData\ShadouCMDB\shadoucmdb.env' migrate
  Start-Service ShadouCMDB


Uninstall
---------

  & 'C:\Program Files\ShadouCMDB\shadoucmdb.exe' service uninstall

This stops and removes the service. Your settings, logs and the database are
left untouched.


Verify the download
-------------------

Compare the hash with the SHA256SUMS file attached to the GitHub Release:

  Get-FileHash .\shadoucmdb-<version>-windows-x64.zip -Algorithm SHA256
