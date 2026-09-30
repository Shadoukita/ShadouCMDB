# Hardening guide

A CMDB is a map of your network: every server, its owner, its location and how it connects to
everything else. Treat a ShadouCMDB installation, its database and its backups as confidential.
This guide is the secure configuration we recommend for production. [deployment.md](../deployment.md)
covers installation; this page covers what to change and why.

## Checklist

- [ ] HTTPS only, terminated by a reverse proxy with TLS 1.2 or 1.3 ([TLS](#tls-reverse-proxy))
- [ ] `shadoucmdb` reachable only from the proxy; the database only from `shadoucmdb` ([network](#network-segmentation))
- [ ] `DATABASE_SSL=verify-full`, the default ([database TLS](#database-tls))
- [ ] Outbound traffic limited to the destinations you configured ([outbound connections](#outbound-connections))
- [ ] The three dedicated, non-superuser database roles; the owner's password not in the server's environment ([roles](#database-roles))
- [ ] The env file readable only by the service account ([secrets](#secrets-and-configuration))
- [ ] The encryption key readable only by the service account, with a copy held apart from the database backups ([encryption key](#encryption-key))
- [ ] First administrator created by you, before the server is reachable by others ([first run](#first-run))
- [ ] With single sign-on: `PUBLIC_URL` on https, a read-only directory account, and a local break-glass administrator with MFA ([enterprise sign-in](#enterprise-sign-in))
- [ ] Encrypted, tested backups ([backup](#backup-and-restore))
- [ ] Logs collected centrally, sign-in failures alerted on ([logging](#logging-and-monitoring))
- [ ] Subscribed to security releases ([updates](#updates))

## TLS reverse proxy

`shadoucmdb` speaks plain HTTP. Always put a TLS-terminating reverse proxy in front of it; it
carries passwords and session cookies. Requirements for the proxy:

- **TLS 1.2 and 1.3 only.** Disable SSL 3, TLS 1.0 and 1.1. With TLS 1.2 use only AEAD cipher
  suites with forward secrecy (ECDHE with AES-GCM or ChaCha20-Poly1305), as in Mozilla's
  "intermediate" profile. If every client supports it, allow TLS 1.3 only ("modern").
- **Redirect HTTP to HTTPS**, and don't serve the application on port 80.
- **Pass the scheme:** `X-Forwarded-Proto: https`. Without it the server doesn't mark the session
  cookies `Secure` and doesn't send HSTS. If the proxy can't, set `COOKIE_SECURE=always`.
- **Replace `X-Forwarded-For`** with the client address; never append to what the client sent.
  The audit log records that address.
- **List the proxy in `TRUSTED_PROXIES`** on the ShadouCMDB server (its address as the server sees
  it, or a CIDR range for a pool: `TRUSTED_PROXIES=10.0.0.5`). The sign-in throttle reads the
  forwarding headers only from a listed peer, right to left, and skips listed hops; without it,
  every client counts as the proxy's network, so one client's wrong passwords lock a username for
  all of them. Never list a range your users' clients are in, and never `0.0.0.0/0` (refused at
  start-up): a listed address can make the server believe any client address. The server reads
  `X-Forwarded-For` before `Forwarded`: a proxy that writes only `Forwarded` must also add or
  overwrite `X-Forwarded-For`, or remove it from client requests, or the client picks its own
  throttle network.
- **Keep `SIGN_IN_FAILURE_FLOOR_MS`** (default 1000) above the slowest sign-in your LDAP/AD
  directory takes, so refused sign-ins all take the same time.
- **Don't add `Content-Security-Policy`, `Strict-Transport-Security`, `X-Content-Type-Options`,
  `Referrer-Policy`, `X-Frame-Options`, `Permissions-Policy` or `Cross-Origin-Opener-Policy`**: the
  server sends them (see [deployment.md](../deployment.md#https-and-session-cookies)). If an earlier
  version of this guide had you add `Permissions-Policy` at the proxy, remove it.
- **Don't cache `/api/`** (see [deployment.md](../deployment.md#https-and-session-cookies)).
- Use a certificate from a CA your clients trust (ACME or your internal PKI). Monitor its expiry.

nginx:

```nginx
server {
    listen 80;
    server_name cmdb.example.com;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    http2 on;
    server_name cmdb.example.com;

    ssl_certificate     /etc/ssl/cmdb.example.com/fullchain.pem;
    ssl_certificate_key /etc/ssl/cmdb.example.com/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_ciphers ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305;
    ssl_prefer_server_ciphers off;
    ssl_session_tickets off;

    client_max_body_size 1m;   # the server rejects larger bodies anyway

    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto https;
        proxy_set_header X-Forwarded-For $remote_addr;   # replaces, not $proxy_add_x_forwarded_for
        proxy_set_header Forwarded "";
    }
}
```

Caddy (automatic certificates; TLS 1.2+ by default):

```caddyfile
cmdb.example.com {
    reverse_proxy 127.0.0.1:3000 {
        header_up X-Forwarded-For {remote_host}
    }
}
```

Check the result from outside: `curl -sI https://cmdb.example.com/ | grep -i strict-transport`
should show the HSTS header, and `testssl.sh cmdb.example.com` (or `nmap --script ssl-enum-ciphers -p 443`)
should list only TLS 1.2 and 1.3.

## Network segmentation

Three zones, each reachable only from the one in front of it:

```
users ──HTTPS──▶ reverse proxy ──HTTP──▶ shadoucmdb ──TLS 5432──▶ PostgreSQL
                                             │
                                             └──▶ only what you configure: OIDC providers,
                                                  LDAP/AD directories, syslog/SIEM collector
```

- **Bind `shadoucmdb` to the proxy's network only.** On the same host: `API_HOST=127.0.0.1`. On
  separate hosts: bind to the internal interface and firewall port 3000 so only the proxy reaches
  it. The default `API_HOST=0.0.0.0` listens everywhere.
- **The database accepts connections only from the `shadoucmdb` hosts** (host firewall or
  security group, plus `pg_hba.conf` below). Don't expose PostgreSQL to user networks or the
  internet.
- **Keep `/docs` and `/openapi.json` off** (`API_DOCS=off`, the default: both answer 404). If
  people need the API reference, use `API_DOCS=authenticated`, which serves it to signed-in users
  only; `public` is for development. They reveal the API surface, not data; the contract is also
  in the repository as `backend/openapi.json`.
- **Deny outbound traffic by default.** The server needs no internet access and sends nothing to
  us ([telemetry](telemetry.md)). Allow only the destinations listed under
  [outbound connections](#outbound-connections).
- **Administration from a management network.** Run `migrate`, `create-admin` and database
  maintenance from a jump host in the management zone, not from user workstations.

### Outbound connections

Besides PostgreSQL, the server opens a connection only to what an administrator or operator has
configured. Nothing below is on in a fresh install.

| Destination | Protocol | Enabled by | Controls |
| --- | --- | --- | --- |
| PostgreSQL | TLS (5432) | `DATABASE_URL` / `PG*`, always | `DATABASE_SSL=verify-full` by default ([database TLS](#database-tls)) |
| OIDC providers: discovery document, key set (JWKS), token endpoint | HTTPS (443) | an OIDC provider added under **Administration › Sign-in** | https only, certificates always verified (the provider's `caCertificate` or the built-in roots), redirects never followed, 10 s timeout, responses size-limited; `OIDC_ALLOWED_HOSTS` limits the hosts; honours `HTTPS_PROXY`/`NO_PROXY` |
| LDAP/AD directories | LDAPS (636) or LDAP with StartTLS (389) | a directory added under **Administration › Sign-in** | TLS required and certificates always verified; timeouts; read-only bind account recommended |
| Syslog / SIEM collector | UDP or TCP syslog, **no TLS** | `AUDIT_EXPORT=udp://…` or `tcp://…` | off by default; `stdout` and `file:` export open no connection |

Bulk import adds no destination: an uploaded file is read from the database, and links, embedded
objects and images in a workbook are never fetched or opened.

- **Set `OIDC_ALLOWED_HOSTS`** to the OIDC provider hosts whenever you use OIDC. Unset, the server
  contacts whatever host a provider's issuer or discovery document names. Adding a provider and
  running its connection test needs the Administrator profile and a signed-in session (not an API
  token), but an administrator could still point it at an internal address; the allowlist and an
  egress firewall stop that.
- **Allow exactly those hosts and ports** in the egress firewall, per destination. LDAP has no
  host allowlist setting; the firewall is the control.
- **Audit export over the network is plain text.** The rows carry user names, client IPs and the
  old and new values of every change. Send them to a relay on the same host or a trusted segment
  (rsyslog, Vector, Fluent Bit) that forwards over TLS, and prefer `tcp://`: UDP drops datagrams
  without notice ([deployment.md](../deployment.md#hardening-settings)).
- The browser side is separate: the web UI loads nothing from third parties, and only the
  optional `CSP_REPORT_URI` makes browsers send reports elsewhere ([telemetry](telemetry.md)).

## Database TLS

Keep the default, `DATABASE_SSL=verify-full`. `require` encrypts the connection but doesn't check
the server's certificate, so anyone who can intercept traffic between the server and the database
can impersonate the database and read the credentials and data. The server logs a warning at
startup when `require` is used with a host that is not loopback.

```sh
DATABASE_SSL=verify-full
DATABASE_SSL_CA_FILE=/etc/shadoucmdb/db-ca.pem   # only for a private or managed-service CA
```

- The host name in `PGHOST` or `DATABASE_URL` must match the certificate (SAN). Connect by name,
  not IP address, unless the certificate contains the IP.
- Without `DATABASE_SSL_CA_FILE` the Mozilla root set built into the binary is used. For a private
  CA, or managed services such as Amazon RDS or Azure Database for PostgreSQL, download the
  provider's CA bundle and point `DATABASE_SSL_CA_FILE` at it.
- On the PostgreSQL side: `ssl = on`, `ssl_min_protocol_version = 'TLSv1.2'`,
  `password_encryption = 'scram-sha-256'`.
- `DATABASE_SSL=disable` is only for a database on the same host over a Unix socket or loopback.

## Database roles

`shadoucmdb` uses three PostgreSQL roles, none of them a superuser.
[`sql/bootstrap/00_create_role_and_database.sql`](../../sql/bootstrap/00_create_role_and_database.sql)
creates them; [deployment.md](../deployment.md#database-roles) has the full table, and
[`10_split_roles.sql`](../../sql/bootstrap/10_split_roles.sql) splits an install created with the
older single-role script ([upgrading](../deployment.md#upgrading-a-single-role-install)).

| Role | Used by | Owns / may |
| --- | --- | --- |
| `shadoucmdb_owner` | `migrate`, `restore`, `factory-reset`, `decommission` (`MIGRATION_DATABASE_URL`) | Owns the database, the `cmdb` system schema and every system table, including `audit_log`. The only role that may create objects in `public`. Member of `shadoucmdb_app`. |
| `shadoucmdb_app` | `serve` and the other commands (`DATABASE_URL`) | Reads and writes the system tables in `cmdb`, but only `SELECT` and `INSERT` on `audit_log`, `schema_changes` and `server_keys`, and no access to the audit hash-chain head. Owns the area schemas, their type tables and reporting views, which it changes at run time, so it holds `CREATE` on the database. |
| `shadoucmdb_maintenance` | `prune-audit` (`MAINTENANCE_DATABASE_URL`) | Executes `cmdb.prune_audit_log()`, nothing else. |

The running server therefore cannot change the system schema, disable the audit log's
append-only trigger, or update, delete or truncate audit rows (migrations 0007 and 0018 also
reject `UPDATE`, `DELETE` and `TRUNCATE` on `audit_log` with triggers). Keep it that way:

- **No superuser**, and no `CREATEDB`, `CREATEROLE`, `REPLICATION` or `BYPASSRLS` for any of the
  three. The bootstrap script creates them that way.
- **Keep the owner's password out of the server's environment.** Pass `MIGRATION_DATABASE_URL`
  only to the command that needs it ([example](../deployment.md#database-roles)). The owner can
  change anything, including the audit log's trigger; with its password in the env file, the
  separation above is lost.
- **One database, these roles.** Don't share the roles or the database with other applications.
  The bootstrap script revokes all rights on the database from `PUBLIC` and `CREATE` on schema
  `public` from everyone but the owner (PostgreSQL 15 and later withhold it already). `migrate`
  warns if it finds `CREATE` on `public` still open.
- **Limit connections** of the API role to what the pool needs:

  ```sql
  ALTER ROLE shadoucmdb_app CONNECTION LIMIT 40;   -- above DATABASE_POOL_MAX x instances
  ```

- **Restrict where each role can log in from** in `pg_hba.conf`: TLS only, SCRAM, the API and
  maintenance roles from the application hosts, the owner from where you run `migrate`.

  ```
  # TYPE     DATABASE    USER                    ADDRESS          METHOD
  hostssl    shadoucmdb  shadoucmdb_app          10.0.20.0/28     scram-sha-256
  hostssl    shadoucmdb  shadoucmdb_maintenance  10.0.20.0/28     scram-sha-256
  hostssl    shadoucmdb  shadoucmdb_owner        10.0.30.10/32    scram-sha-256   # management host
  hostnossl  all         all                     0.0.0.0/0        reject
  ```

- **Separate roles for people.** DBAs use their own named roles for maintenance, never the
  application's passwords. Long, random passwords for the three roles, stored only in the env
  file (API and maintenance role) or your secret manager (owner); rotate them when someone with
  access leaves.
- **Reporting and BI** use the built-in reporting role. If a role named `cmdb_reporting` exists,
  the server grants it `USAGE` on every area schema and `SELECT` on every reporting view
  (`<area>.v_<type>`), and nothing else: no access to the `cmdb` system tables (users, sessions,
  API tokens, audit log) or to the raw type tables. It keeps the grants up to date as areas and
  types are added ([data model](../data-model.md)).

  ```sql
  CREATE ROLE cmdb_reporting NOLOGIN;
  CREATE ROLE report_reader LOGIN PASSWORD '…' IN ROLE cmdb_reporting;
  -- then run `shadoucmdb migrate` or POST /api/v1/schema-changes/reconcile once
  ```

  Such a role bypasses the permission profiles of the application: it sees every class. Grant it
  only to people who may see the whole CMDB. Don't grant reporting accounts rights on schema
  `cmdb` or `public`.

## Secrets and configuration

- The env file holds the database password. Make it readable only by the service account:
  `0640 root:shadoucmdb` on Linux, the ACL shown in [deployment.md](../deployment.md#windows-server-windows-service)
  on Windows. Under Docker or Kubernetes, inject it from a secret store (Docker secrets,
  Kubernetes Secrets with encryption at rest, Vault) rather than baking it into an image.
- Never put the password in `DATABASE_URL` on a command line; it shows up in `ps` and shell
  history. Use the env file.
- **Secrets at rest.** The server has to read some secrets back. None of them is returned by the
  API or written to the audit log.
  - The **authenticator (TOTP) secret** of every user with two-factor sign-in is encrypted
    (AES-256-GCM) with the [encryption key](#encryption-key), which is kept outside the database.
    Each secret is bound to its user: copied onto another account, it does not decrypt. Someone
    with only the database or only a backup cannot compute sign-in codes; someone with the
    database **and** the key file can.
  - The **OIDC client secret** and the **directory bind password** of every identity provider
    are encrypted the same way, with their own subkey. Each is bound to its provider and to its
    field: copied onto another provider, or from the bind password to the client secret, it does
    not decrypt. The server decrypts them only to present them to the provider.
  - Backups and database copies taken **before** the version that introduced this encryption
    still hold these secrets unencrypted, and so do PostgreSQL's WAL and dead row versions until
    they are vacuumed and recycled. If such backups or copies have left your custody, rotate the
    OIDC client secret and the LDAP bind password at the identity provider.
  - Register ShadouCMDB at the OIDC provider as a dedicated client with no application
    (client-credentials) permissions of its own, so a leaked client secret gives nothing beyond
    the sign-in client (for example, an Entra ID app registration without Graph application
    permissions). Give the LDAP bind account read access only.
- Leave `CORS_ORIGINS` empty unless you serve the UI from another origin.
- Keep `LOG_LEVEL` at `info` in production. `trace` logs every SQL statement.

## Encryption key

`ENCRYPTION_KEY_FILE` names a file holding the key that encrypts the users' authenticator secrets
and the identity providers' secrets (OIDC client secrets, LDAP bind passwords): the base64 of 32
random bytes. `serve` does not start without it. The key never enters the database,
the logs or a backup, and the commands that only move data (`migrate`, `backup`, `restore`,
`factory-reset`, `decommission`, `create-admin`) do not need it. `migrate` and `verify` print which
key the database's secrets are encrypted with and warn when the configured key does not match.

**Create it** once per installation, offline:

```sh
shadoucmdb generate-encryption-key --out /etc/shadoucmdb/encryption.key
```

The command never overwrites a file, creates it with mode `0600` on Linux and prints the key id, a
fingerprint that identifies the key without revealing it (e.g. `8be4d177`). `openssl rand -base64 32`
produces an equivalent key. The key file may end with a newline or CRLF or start with a UTF-8 byte
order mark; anything but the base64 of exactly 32 bytes stops the server with
`ENCRYPTION_KEY_FILE: expected the base64 of 32 bytes`.

**Restrict it** to the service account:

- **Linux:** `chown root:shadoucmdb` and `chmod 0640` (or `0600` owned by the service account). The
  server refuses to start when the file is readable by everyone or writable by its group or
  others.
- **Windows:** keep it out of the inherited Modify grant on `C:\ProgramData\ShadouCMDB`:
  `icacls encryption.key /inheritance:r /grant:r "Administrators:F" "SYSTEM:F" "NT AUTHORITY\LocalService:R"`.
  Windows has no programmatic check.
- **Docker, Kubernetes, secret stores:** mount it as a file (Docker secret, Kubernetes Secret volume,
  CSI secrets-store driver, Vault Agent template) and point `ENCRYPTION_KEY_FILE` at it. With
  systemd (250 or later), `LoadCredentialEncrypted=encryption.key:…` keeps it TPM-sealed at rest; add
  `Environment=ENCRYPTION_KEY_FILE=%d/encryption.key` in a drop-in for the unit (it takes precedence
  over the env file).

**Keep a copy apart from the database backups**: in a password vault or escrow with different
custody, and in your disaster-recovery runbook. Losing the key loses no CMDB data, only the
two-factor enrolments and the stored identity provider secrets: the server refuses to start until
the key is configured again, or until `shadoucmdb mfa reset-undecryptable` has turned off
two-factor sign-in for the users concerned (they set it up again) and
`shadoucmdb identity-providers reset-undecryptable` has disabled the providers concerned (an
administrator enters their secret again). Include the key in the quarterly restore test.

**Rotate** it when someone who had access to it leaves, or on your key-rotation schedule:

1. `shadoucmdb generate-encryption-key --out /etc/shadoucmdb/encryption-2027.key`
2. In the env file: `ENCRYPTION_KEY_PREVIOUS_FILE=<the old path>`, `ENCRYPTION_KEY_FILE=<the new path>`.
3. Restart. With several instances, stop all of them first: an instance still on the old key cannot
   read the secrets the others have re-encrypted. At start-up the server re-encrypts every secret
   under the new key, authenticator secrets and identity provider secrets alike (logged per table,
   e.g. `Encrypted N authenticator secrets (key …): … from previous key …`).
4. Check with `shadoucmdb verify` that every secret is under the new key, then remove
   `ENCRYPTION_KEY_PREVIOUS_FILE` and restart. If `verify` still lists secrets under the previous
   key, those do not decrypt (altered or copied from another row), and the start-up log names the
   users or providers concerned ("… does not decrypt"). The key is not lost, so do not run
   `reset-undecryptable`: reset two-factor sign-in for those users, or enter the provider's secret
   again under Administration > Sign-in, then check with `verify` again before you remove the
   previous key.
5. **Keep the old key as long as you keep backups taken before the rotation**: their secrets are
   encrypted with it. Destroy it only after those backups have expired.

Rotation protects later backups and limits what a leaked key file alone is worth. It does not help
once the database **and** the key have leaked: then every authenticator secret and every identity
provider secret is known.

**If the database or a backup leaks together with the key** (e.g. the application server was
compromised): treat it as an incident. Reset two-factor sign-in for every user who has it
(`DELETE /api/v1/admin/users/{id}/mfa`, or **Reset two-factor** under the user's account actions) so
they enrol a new authenticator, rotate the key as above, and rotate the OIDC client secret and the
bind password at the provider. Password resets alone are not enough. A leak of the database or a
backup **without** the key exposes no authenticator secret and no provider credential, but still
the password hashes ([secrets at rest](#secrets-and-configuration)).

**Lost key.** When the database holds secrets under a key that is not configured, `serve` exits
with a message naming both key ids and the commands that apply. Configure the right key (from
escrow), or give up those secrets:

```sh
shadoucmdb mfa reset-undecryptable --dry-run   # lists the users concerned
shadoucmdb mfa reset-undecryptable             # asks for the database name, then turns it off
shadoucmdb identity-providers reset-undecryptable --dry-run   # lists the providers concerned
shadoucmdb identity-providers reset-undecryptable             # asks for the database name, then disables them
```

Run them with the service's environment (`--env-file`, or the same `ENCRYPTION_KEY_FILE`): they
keep every secret under the configured key and its previous key. Without `ENCRYPTION_KEY_FILE`
both commands refuse and change nothing, so that a shell missing the service's variables does not
reset everyone. Only when the key is lost and no key is configured any more, pass `--no-key`:
every encrypted secret then counts as undecryptable (try it with `--dry-run` first). `--no-key`
together with a configured key is refused too.

```sh
shadoucmdb mfa reset-undecryptable --no-key --dry-run   # no key at all: lists every enrolled user
```

`mfa reset-undecryptable` deletes the users' authenticator and recovery codes in one transaction
and writes one `mfa.disable` audit event per user with `reason: key_lost`.
`identity-providers reset-undecryptable` disables each provider concerned and clears its secret
(for a directory also its bind DN, which needs its password), ends the sessions of its accounts,
and writes one `update` audit row per provider with `reason: key_lost`; the old bind DN stays in
that row. Sign-in through those providers stops until an administrator enters the secret again
under **Administration › Sign-in** and enables the provider; sign in with a local administrator
account to do so. Users and providers under the configured key are not touched.

A secret that does not decrypt although its key is configured (altered in the database, or copied
from another user's row) fails closed: the user's authenticator codes are refused, the server logs
an error with the user id and writes an `mfa.failure` event with `reason: secret_undecryptable`.
Recovery codes still work, and an administrator can reset the user's two-factor sign-in. The same
holds for an identity provider's secret: sign-in through that provider answers
`IDENTITY_PROVIDER_UNAVAILABLE` (the OIDC button ends at `ssoError=unavailable`), the connection
test says the stored secret cannot be decrypted, and the server logs an error with the provider id
and name. Enter the secret again to re-encrypt it.

**Downgrading** after the upgrade that introduced encryption needs a restore of a backup taken
before it: the previous release cannot read encrypted secrets and rejects every authenticator code.
For the same reason, do not run the previous release next to this one during the upgrade: once
the new release has encrypted the secrets at start-up, sign-in through OIDC and LDAP fails on the
previous release's instances and saving an identity provider secret there fails. Stop every
instance of the previous release before you start the new one.

Residual risk: the key is in the server's memory while it runs, so a memory dump of the running
process exposes it, and the previous key widens the exposure while a rotation is in progress.

## First run

Until the first administrator exists, first-run setup in the UI creates it. Setup needs the one-time
setup token the server writes to its log and to its setup token file (or the `SETUP_TOKEN` you set),
so someone who can only reach the server over the network cannot claim it
([details](../deployment.md#the-setup-token)). Still, create the first administrator with
`shadoucmdb create-admin` before the server is reachable, or keep it reachable only by you until
setup is done: whoever can read the log (a log collector, a shared container host) can read the token too.

Then: give each person their own account, grant the smallest permission profile that fits
their job, and keep the number of administrators small. Deactivate accounts of people who leave
(it ends their sessions immediately and their API tokens stop working). If an account may be
compromised, reset its password: that ends its sessions and revokes every API token it owns and
every token it created for another account, so a token created with the stolen password does not
keep working. Then review what the account changed in the audit log, including identity provider
changes (see the [incident response plan](incident-response.md#customer-notification)).

## Enterprise sign-in

OIDC providers and LDAP/AD directories are configured in the web UI (API:
[Enterprise sign-in](../api.md#enterprise-sign-in)). Recommended set-up:

- **Keep a local break-glass administrator.** Local accounts keep working next to every provider.
  Give one local administrator a long password and two-factor authentication, keep them in your
  emergency procedure, and use them only when the provider is down or misconfigured. Mark the
  built-in Administrator profile `requireMfa` so that account cannot sign in on a password alone.
- **`requireMfa` for local and directory accounts.** `requireMfa` covers local **and LDAP/AD
  directory** accounts: a directory user holding such a profile sets up an authenticator app here
  at their next sign-in, confirming with their directory password, and from then on signs in with
  the directory password plus a code. Tell directory administrators before you upgrade or mark a
  profile `requireMfa`.
- **`requireMfa` for OIDC accounts: let ShadouCMDB verify it.** OIDC accounts have no password
  here, so the provider runs the second factor (conditional access, Okta policies, Keycloak OTP).
  Each OIDC provider has an MFA setting (`mfaAssurance`):
  - **Verify** (`verify`, the default for new providers, recommended): a user holding a
    `requireMfa` profile is signed in only when the signed ID token proves a second factor. With
    **required ACR values** (`requiredAcr`) set, the token's `acr` must be exactly one of them
    (they are also sent as `acr_values`, so the provider can step up). Otherwise its `amr` must
    contain `mfa`, or values from two different factor categories of RFC 8176 (knowledge `pwd`
    `pin` `kba`; possession `hwk` `swk` `otp` `sc` `sms` `tel` `pop`; inherence `fpt` `face`
    `iris` `retina` `vbm`). A token without that proof is refused (`ssoError=mfa_not_enforced`, a
    `login.failure` row with `reason: mfa_not_enforced`, the received `acr`/`amr` in the server
    log). Users without a `requireMfa` profile sign in as before.
  - **Trust the provider** (`trustProvider`): ShadouCMDB does not check the token. Choose it only
    when the provider enforces MFA for this client and cannot put the proof in the token. The
    provider list shows these providers as "MFA not verified", and their sign-ins are recorded
    with `providerMfa: trusted`.

  The check also holds for sessions already open and for API tokens created without a second factor: when a profile starts to require MFA, a group
  mapping changes, or a provider is switched from trust to verify, an OIDC session whose sign-in
  did not prove MFA is ended on its next request (`session.revoke`, `reason: mfa_not_enforced`)
  and the user signs in again. Whether a sign-in proved MFA is judged once, against the provider's
  settings at that moment: adding or changing `requiredAcr` later does not end sessions that proved
  MFA through `amr`. To apply a stricter setting to everyone at once, disable and re-enable the
  provider after the change, which ends the sessions of its accounts. An API token is accepted for
  a `requireMfa` owner only if it was created from a session that proved a second factor (an
  authenticator or recovery code, OIDC under Verify, or TOTP enrolment confirmed in that session);
  others are refused with `token.use` outcome `mfa_required` while the requirement applies (list them
  under **Administration › API tokens** with the **Refused for two-factor only** filter, or with
  `GET /api/v1/admin/api-tokens?refusedForMfa=true`). Put automation accounts in profiles
  without `requireMfa`, scope their tokens to the least privileged profile and keep expiries short;
  where an automation account must hold a `requireMfa` profile, an administrator signed in with a
  second factor creates its token. Each `login.success` row of an OIDC sign-in carries `providerMfa`
  (`verified`, `trusted` or `none`). The provider's connection test warns when the discovery
  document suggests the check cannot pass. Per provider:
  - **Microsoft Entra ID:** `amr` contains `mfa` when Conditional Access required MFA for the
    sign-in. Require MFA for the ShadouCMDB application in a Conditional Access policy and use
    Verify without ACR values.
  - **Okta, Auth0, Ping:** `amr` lists the methods used (for example `pwd` and `otp`, or `mfa`).
    Require MFA in the application's sign-on policy and use Verify without ACR values.
  - **Keycloak:** configure step-up authentication (an ACR to level-of-authentication mapping on
    the realm or client, with the OTP or WebAuthn step on the higher level) and set `requiredAcr`
    to the ACR of that level. Keycloak does not send `amr` by default.
  - **Google Workspace:** the ID token carries neither `amr` nor a usable `acr`. Enforce 2-Step
    Verification in the Google Admin console and use Trust the provider.

  **Upgrading:** providers that existed before this setting are set to Trust the provider, so the
  upgrade locks nobody out. Review each one and switch it to Verify where the provider sends the
  proof; test with a non-administrator account first, and keep the local break-glass
  administrator at hand.
- **`PUBLIC_URL=https://...`** It is the only source of the OIDC redirect URI (never the request's
  Host header). Register exactly `{PUBLIC_URL}/api/v1/auth/oidc/callback` at the provider.
- **Map groups, not everyone.** A sign-in whose groups map to no profile is refused. Map a
  dedicated group per profile ("CMDB-Admins" → Administrator) rather than a company-wide group,
  and review the mappings like any other admin rights: whoever controls a mapped group in the
  provider controls who holds that profile here. Only holders of the Administrator profile can
  change providers and mappings, and only from a signed-in session: API tokens, even with the
  Administrator profile, can read providers but get `403` on every change and on the connection
  test (it makes the server connect out with the stored secret), so a leaked token cannot add a provider or remap a group that keeps signing people in
  after the token is revoked.
- **Directory service account read-only.** It only searches for users; it needs no write rights.
  Prefer `ldaps://`; `ldap://` is accepted only with StartTLS, and certificates are always
  verified. For a private CA paste its certificate into the directory's `caCertificate`, or
  install it in the operating system's trust store (read once per server process, at the first provider connection).
- **Outbound traffic.** The server calls the OIDC provider (discovery, keys, token endpoint) and
  the directory (636, or 389 with StartTLS). Allow exactly those in the egress firewall and set
  `OIDC_ALLOWED_HOSTS` ([outbound connections](#outbound-connections)); OIDC calls honour
  `HTTPS_PROXY`/`NO_PROXY`.
- **Secrets at rest.** The OIDC client secret and the directory bind password are stored in the
  database encrypted with the [encryption key](#encryption-key) (the server has to present them);
  they are never returned by the API or written to the audit log. See
  [secrets at rest](#secrets-and-configuration) for what that means, and what to do about
  backups taken before that encryption.
- **Leavers.** Disable the person in the provider. Their next sign-in is refused; a session they
  already have lasts until it idles out (`SESSION_IDLE_TIMEOUT_MINUTES`) or ends. To end it at
  once, disable the account in ShadouCMDB too, or disable the provider (ends all its sessions and
  refuses its accounts' API tokens while it stays disabled).

  **API tokens outlive the provider account.** A token is never checked against the provider: it
  keeps working until it expires (up to 366 days) or the account is disabled or deleted in
  ShadouCMDB, even when the person is disabled or deleted in the provider. So for anyone who has
  or had API tokens, or created tokens for service accounts, disabling the account in ShadouCMDB
  (**Administration › Users**, or `PATCH /api/v1/admin/users/{id}` with `"isActive": false`) is a
  required step, not an option. To find them, list the working tokens by owner and by creator
  (**Administration › API tokens**, or `GET /api/v1/admin/api-tokens?userId=<id>` and
  `?createdBy=<id>`).
  Disabling or deleting an account in ShadouCMDB also revokes its API tokens and every token it
  created for another owner, such as a service account. Before an administrator leaves, mint new
  tokens for the service accounts they looked after, so the integrations keep running.
- **Group removal takes effect at the next sign-in.** The profiles of a provider account are set
  from its groups only when it signs in. Removing someone from a mapped group, or changing a
  mapping, does not change the profiles they already hold: their sessions and API tokens keep
  those rights until they sign in again, which may be never for an account that only uses
  tokens. When the change must apply at once, also remove the profile from the account in
  ShadouCMDB (**Administration › Users**; the next sign-in sets the profiles from the groups
  again) or revoke its tokens. A person moving to another job is a leaver for the rights they
  lose.

## Backup and restore

The database is the whole state of ShadouCMDB: data, users, settings and audit log. The server
holds no state of its own besides its env file.

- **Back up with PostgreSQL's tools.** Logical: `pg_dump --format=custom --file=shadoucmdb-$(date +%F).dump`
  as `shadoucmdb_owner`, which can read every table (the API role cannot read the audit hash-chain
  head, so `pg_dump` fails with it), or use `shadoucmdb backup`. For a point-in-time
  recovery, use physical backups with WAL archiving (pgBackRest, Barman or your provider's
  snapshots).
- **Encrypt backups** at rest and in transit (e.g. `age` or `gpg` before the file leaves the host,
  or an encrypted backup repository). A backup, whether a `pg_dump` or a `shadoucmdb backup`
  file, contains the password hashes and the whole CMDB. The users' authenticator secrets and the
  [identity provider credentials](#secrets-and-configuration) in it are encrypted with the
  [encryption key](#encryption-key), which is never part of a backup. Keep the backup decryption key away from the backups and from
  the database host.
- **Back up the encryption key separately.** A restored database needs the key it was encrypted
  with (a backup taken before a rotation needs the previous key); without it, the users with
  two-factor sign-in have to set it up again, and an administrator has to enter the identity
  providers' secrets again. Store the key where the backups are not, so that one
  leak does not expose both.
- **Restrict the file on the host.** `shadoucmdb backup` creates its file with mode `0600` on
  Linux. On Windows the file inherits the folder's ACL: write backups only to a folder limited to
  Administrators and the account that runs the backup (see
  [backup-and-reset.md](../backup-and-reset.md#taking-backups)).
- **Keep them apart:** store backups outside the database host and outside the reach of the
  application role, with at least one copy offline or immutable (object lock), so ransomware on
  the server cannot delete them.
- **Back up the env file** (or the secret-store entry) separately and just as carefully.
- **Test a restore** at least every quarter into a separate database with the same three roles:
  `shadoucmdb restore` (it connects as the owner, `MIGRATION_DATABASE_URL`; see
  [backup-and-reset.md](../backup-and-reset.md)), or `pg_restore` as `shadoucmdb_owner` for a
  `pg_dump` file. Then run `shadoucmdb migrate` and `shadoucmdb verify` against it, and sign in.
- **Retention:** keep backups only as long as you need them; they are copies of personal data
  (user accounts, audit log IPs) too.

The built-in `shadoucmdb backup` and `restore` commands and the decommission wipe are described
in [backup-and-reset.md](../backup-and-reset.md).

## Logging and monitoring

- Collect the JSON logs centrally (journald, Windows Event Log via the log file, your SIEM).
- The audit log (`GET /api/v1/audit-log`, permission `audit.view`) records every change and every
  sign-in success, failure and lockout with the client IP. Review it, and alert on bursts of
  `login.failure` or `login.locked`.
- Copy it to a system the ShadouCMDB administrators don't control with `AUDIT_EXPORT`, and run
  `shadoucmdb audit-verify` on a schedule: the rows form a hash chain, and comparing the printed
  chain head with the exported row of the same `chainSeq` shows rows altered or removed in the
  database ([deployment.md](../deployment.md#hardening-settings)).
- Watch `/readyz` from your monitoring; it returns 503 if the database is unreachable or
  migrations are pending.

## Updates

ShadouCMDB doesn't update itself or check for updates. Subscribe to security releases (see
[support period](support-period.md#how-updates-reach-you)), verify downloads against `SHA256SUMS`,
and apply security releases promptly: replace the binary or image, run `migrate`, restart.
