# Hardening guide

A CMDB is a map of your network: every server, its owner, its location and how it connects to
everything else. Treat a ShadouCMDB installation, its database and its backups as confidential.
This guide is the secure configuration we recommend for production. [deployment.md](../deployment.md)
covers installation; this page covers what to change and why.

## Checklist

- [ ] HTTPS only, terminated by a reverse proxy with TLS 1.2 or 1.3 ([TLS](#tls-reverse-proxy))
- [ ] `shadoucmdb` reachable only from the proxy; the database only from `shadoucmdb` ([network](#network-segmentation))
- [ ] `DATABASE_SSL=verify-full` ([database TLS](#database-tls))
- [ ] A dedicated, non-superuser database role; no other application shares it ([roles](#database-roles))
- [ ] The env file readable only by the service account ([secrets](#secrets-and-configuration))
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
```

- **Bind `shadoucmdb` to the proxy's network only.** On the same host: `API_HOST=127.0.0.1`. On
  separate hosts: bind to the internal interface and firewall port 3000 so only the proxy reaches
  it. The default `API_HOST=0.0.0.0` listens everywhere.
- **The database accepts connections only from the `shadoucmdb` hosts** (host firewall or
  security group, plus `pg_hba.conf` below). Don't expose PostgreSQL to user networks or the
  internet.
- **Don't publish `/docs` and `/openapi.json` to untrusted networks.** They are public today and
  can't be switched off yet (planned: SHAA-77 workstream 3); they reveal the API surface, not data.
  If users reach the server from the internet, block both paths at the proxy.
- **No outbound internet is needed.** The server only connects to PostgreSQL ([telemetry](telemetry.md)),
  so deny its outbound traffic apart from the database and your log collector.
- **Administration from a management network.** Run `migrate`, `create-admin` and database
  maintenance from a jump host in the management zone, not from user workstations.

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

`shadoucmdb` needs one PostgreSQL role that owns its database. That role must not be able to do
anything beyond it.

- **Not a superuser**, and no `CREATEDB`, `CREATEROLE`, `REPLICATION` or `BYPASSRLS`.
  [`sql/bootstrap/00_create_role_and_database.sql`](../../sql/bootstrap/00_create_role_and_database.sql)
  creates it that way.
- **One database, one role.** Don't share the role or the database with other applications.
- **Keep other roles out of the database:**

  ```sql
  REVOKE ALL ON DATABASE shadoucmdb FROM PUBLIC;
  \connect shadoucmdb
  ALTER SCHEMA public OWNER TO shadoucmdb_app;     -- PostgreSQL 14 only; 15+ gives it to the database owner already
  REVOKE ALL ON SCHEMA public FROM PUBLIC;
  ALTER ROLE shadoucmdb_app CONNECTION LIMIT 40;   -- above DATABASE_POOL_MAX x instances, plus room for migrate
  ```

- **Restrict where it can log in from** in `pg_hba.conf`: TLS only, SCRAM, from the application
  hosts only.

  ```
  # TYPE     DATABASE    USER            ADDRESS          METHOD
  hostssl    shadoucmdb  shadoucmdb_app  10.0.20.0/28     scram-sha-256
  hostnossl  all         all             0.0.0.0/0        reject
  ```

- **Separate roles for people.** DBAs use their own named roles for maintenance, never the
  application's password. A long, random password for the application role, stored only in the
  env file or your secret manager; rotate it when someone with access leaves.
- **Reporting and BI** get a read-only role that can't see credentials:

  ```sql
  CREATE ROLE shadoucmdb_report LOGIN PASSWORD '…';
  GRANT CONNECT ON DATABASE shadoucmdb TO shadoucmdb_report;
  GRANT USAGE ON SCHEMA public TO shadoucmdb_report;
  GRANT SELECT ON ALL TABLES IN SCHEMA public TO shadoucmdb_report;
  REVOKE SELECT ON sessions, users FROM shadoucmdb_report;       -- session tokens, password hashes
  GRANT SELECT (id, username, display_name, email, is_active, created_at, updated_at)
    ON users TO shadoucmdb_report;                                 -- adjust to the columns you need
  ```

  Such a role bypasses the permission profiles of the application: it sees every class. Grant it
  only to people who may see the whole CMDB.

Planned: a separate migration role so that the running server cannot change the schema of the
fixed tables, and database-level protection of `audit_log` against `TRUNCATE` (SHAA-77
workstream 3). Until then, the application role owns every table, including the audit log, so
protect its password accordingly.

## Secrets and configuration

- The env file holds the database password. Make it readable only by the service account:
  `0640 root:shadoucmdb` on Linux, the ACL shown in [deployment.md](../deployment.md#windows-server-windows-service)
  on Windows. Under Docker or Kubernetes, inject it from a secret store (Docker secrets,
  Kubernetes Secrets with encryption at rest, Vault) rather than baking it into an image.
- Never put the password in `DATABASE_URL` on a command line; it shows up in `ps` and shell
  history. Use the env file.
- **Secrets at rest.** The server has to read some secrets back, so the database stores them
  unencrypted: the authenticator (TOTP) secret of every user with two-factor sign-in, the OIDC
  client secret and the directory bind password. They are never returned by the API or written
  to the audit log, but anyone who can read the database or a backup of it can compute every
  user's current sign-in code and use the provider credentials. The second factor then only
  protects against someone who has a password but not the data. Until these secrets are
  encrypted with a key kept outside the database
  ([#189](https://github.com/Shadoukita/ShadouCMDB/issues/189)):
  - Restrict read access to the database to the application and schema-owner roles
    ([roles](#database-roles)), and keep ad hoc reporting accounts off the `cmdb` schema.
  - Treat every backup as a copy of these secrets ([backup](#backup-and-restore)).
  - If the database or a backup leaks, reset two-factor sign-in for every user who has it
    (`DELETE /api/v1/admin/users/{id}/mfa`, or **Reset two-factor** under the user's account actions) so they enrol
    a new authenticator, and rotate the OIDC client secret and the bind password at the
    provider. Password resets alone are not enough.
- Leave `CORS_ORIGINS` empty unless you serve the UI from another origin.
- Keep `LOG_LEVEL` at `info` in production. `trace` logs every SQL statement.

## First run

Until the first administrator exists, anyone who can reach the server can create it through the
first-run setup in the UI. Either create it with `shadoucmdb create-admin` before the server is
reachable, or make sure only you can reach it until you have completed the setup.

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

  The check also holds for sessions already open: when a profile starts to require MFA, a group
  mapping changes, or a provider is switched from trust to verify, an OIDC session whose sign-in
  did not prove MFA is ended on its next request (`session.revoke`, `reason: mfa_not_enforced`)
  and the user signs in again. Whether a sign-in proved MFA is judged once, against the provider's
  settings at that moment: adding or changing `requiredAcr` later does not end sessions that proved
  MFA through `amr`. To apply a stricter setting to everyone at once, disable and re-enable the
  provider after the change, which ends the sessions of its accounts. Each `login.success` row of an OIDC sign-in carries `providerMfa`
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
  the directory (636, or 389 with StartTLS). Allow exactly those in the egress firewall; OIDC calls
  honour `HTTPS_PROXY`/`NO_PROXY`.
- **Secrets at rest.** The OIDC client secret and the directory bind password are stored in the
  database (the server has to present them) and in backups; they are never returned by the API
  or written to the audit log. See [secrets at rest](#secrets-and-configuration) for what that
  means and what to do if a backup is lost.
- **Leavers.** Disable the person in the provider. Their next sign-in is refused; a session they
  already have lasts until it idles out (`SESSION_IDLE_TIMEOUT_MINUTES`) or ends. To end it at
  once, disable the account in ShadouCMDB too, or disable the provider (ends all its sessions).

## Backup and restore

The database is the whole state of ShadouCMDB: data, users, settings and audit log. The server
holds no state of its own besides its env file.

- **Back up with PostgreSQL's tools.** Logical: `pg_dump --format=custom --file=shadoucmdb-$(date +%F).dump`
  with a role that can read every table (the application role works). For a point-in-time
  recovery, use physical backups with WAL archiving (pgBackRest, Barman or your provider's
  snapshots).
- **Encrypt backups** at rest and in transit (e.g. `age` or `gpg` before the file leaves the host,
  or an encrypted backup repository). A backup, whether a `pg_dump` or a `shadoucmdb backup`
  file, contains the password hashes, the whole CMDB and the
  [secrets stored unencrypted](#secrets-and-configuration): the users' authenticator secrets and
  the identity provider credentials. Whoever has an unencrypted backup and cracks one password
  hash can sign in as that user despite two-factor sign-in. Keep the decryption key away from the
  backups and from the database host.
- **Restrict the file on the host.** `shadoucmdb backup` creates its file with mode `0600` on
  Linux. On Windows the file inherits the folder's ACL: write backups only to a folder limited to
  Administrators and the account that runs the backup (see
  [backup-and-reset.md](../backup-and-reset.md#taking-backups)).
- **Keep them apart:** store backups outside the database host and outside the reach of the
  application role, with at least one copy offline or immutable (object lock), so ransomware on
  the server cannot delete them.
- **Back up the env file** (or the secret-store entry) separately and just as carefully.
- **Test a restore** at least every quarter into a separate database: `pg_restore --no-owner --role=shadoucmdb_app -d shadoucmdb_restore shadoucmdb-….dump`,
  then run `shadoucmdb verify` and `shadoucmdb migrate` against it, and sign in.
- **Retention:** keep backups only as long as you need them; they are copies of personal data
  (user accounts, audit log IPs) too.

The built-in `shadoucmdb backup` and `restore` commands and the decommission wipe are described
in [backup-and-reset.md](../backup-and-reset.md).

## Logging and monitoring

- Collect the JSON logs centrally (journald, Windows Event Log via the log file, your SIEM).
- The audit log (`GET /api/v1/audit-log`, permission `audit.view`) records every change and every
  sign-in success, failure and lockout with the client IP. Review it, and alert on bursts of
  `login.failure` or `login.locked`.
- Watch `/readyz` from your monitoring; it returns 503 if the database is unreachable or
  migrations are pending.

## Updates

ShadouCMDB doesn't update itself or check for updates. Subscribe to security releases (see
[support period](support-period.md#how-updates-reach-you)), verify downloads against `SHA256SUMS`,
and apply security releases promptly: replace the binary or image, run `migrate`, restart.
