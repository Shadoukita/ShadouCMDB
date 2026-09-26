ShadouCMDB for Linux (x64 / ARM64)
==================================

This archive contains:

  shadoucmdb              The server: REST API, web UI and migrations in one
                          statically linked (musl) executable. It runs on any
                          Linux distribution of that architecture, with no
                          glibc, OpenSSL or other runtime dependency.
  shadoucmdb.service      Hardened systemd unit.
  shadoucmdb.env.example  Every setting, with a comment per variable.
  README.txt              This file.

PostgreSQL is external: ShadouCMDB never installs or bundles a database. You need
a reachable PostgreSQL 14 or newer and a database plus a login role for the app.
Full documentation: https://github.com/Shadoukita/ShadouCMDB/blob/main/docs/deployment.md


Install as a systemd service
----------------------------

  sudo install -m 0755 shadoucmdb /usr/local/bin/shadoucmdb
  sudo useradd --system --no-create-home --shell /usr/sbin/nologin shadoucmdb
  sudo install -d -m 0750 -o root -g shadoucmdb /etc/shadoucmdb
  sudo install -m 0640 -o root -g shadoucmdb shadoucmdb.env.example /etc/shadoucmdb/shadoucmdb.env
  sudoedit /etc/shadoucmdb/shadoucmdb.env     # set DATABASE_URL, or PGHOST/PGUSER/PGPASSWORD/...
  sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env migrate
  sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env seed
  # First administrator (or skip this and use first-run setup in the web UI):
  sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env create-admin --username admin
  sudo install -m 0644 shadoucmdb.service /etc/systemd/system/
  sudo systemctl daemon-reload && sudo systemctl enable --now shadoucmdb
  curl -s http://127.0.0.1:3000/readyz
  journalctl -u shadoucmdb -f


Upgrade
-------

  sudo install -m 0755 shadoucmdb /usr/local/bin/shadoucmdb
  sudo -u shadoucmdb shadoucmdb --env-file /etc/shadoucmdb/shadoucmdb.env migrate
  sudo systemctl restart shadoucmdb


Verify the download
-------------------

  sha256sum --ignore-missing -c SHA256SUMS
