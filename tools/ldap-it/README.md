# LDAPS integration test

`ldap-it.ts` signs in to a real ShadouCMDB server on PostgreSQL through a real OpenLDAP directory,
over LDAPS with a CA created for the run. The **Rust** workflow runs it in the Linux job
(`.github/workflows/rust.yml`, step *LDAPS directory*). The unit tests never reach a directory; this
is the test that does.

| Area | What it checks |
| --- | --- |
| Sign-in | A directory user signs in; the account is created and linked; the mapped group gives exactly its profile, the unmapped group nothing. A user with only an unmapped group is refused and gets no account. A wrong password and an unknown name get the same 401, and both count toward the login lock (429). Plain `ldap://` without StartTLS is refused. |
| TLS | Another CA, no CA, a host name the certificate does not carry, and a closed port: the connection test answers the one generic text (GH#125), sign-in answers `503 IDENTITY_PROVIDER_UNAVAILABLE`, and neither shows TLS or socket details. A wrong service password shows the directory's own answer. |
| requireMfa (GH#120) | A directory account without an authenticator must enrol; enrolment takes the directory password (a wrong one: `400 currentPassword`); with TOTP on, sign-in answers `401 MFA_REQUIRED` and a code completes it. An unreachable URL and a stopped directory: `503` on the MFA routes and at sign-in. A disabled directory: its sessions end, and a session that was not ended is refused on every request, the MFA routes included (`401`, GH#250). |
| Bind password | Saved through the admin API, it is stored encrypted under `ENCRYPTION_KEY_FILE` (no plaintext column, no plaintext in the ciphertext) and never returned; every directory sign-in above binds with it after the server decrypts it. Moving the directory to another address without entering it again is refused (`422 SECRET_REQUIRED`, GH#238). |
| Renamed entry | When the account's name now finds another entry (other `entryUUID`), the directory password no longer re-authenticates it, and signing in as the new entry is refused. |

## Files

- `ldap.sh`: creates the CA and the certificates, starts and stops slapd, loads and changes entries.
  In CI it runs `osixia/openldap` (pinned by digest) with `slapd.conf.in` instead of the image's own
  setup; `LDAP_IT_MODE=local` runs a slapd installed on the machine with the same configuration.
  Dependabot does not see the digest in a script: update it by hand.
- `slapd.conf.in`: LDAPS only, `memberOf` overlay (as Active Directory has it), a read-only service
  account.
- `seed.ldif`: `alice` (groups `cmdb-operators`, mapped, and `cmdb-guests`, not mapped), `bob`
  (`cmdb-guests` only), and the service account. The passwords are throwaway values for this test.
- `ldap-it.ts`: the test (Node.js 22.18+, no dependencies).

## Running it locally

You need Docker (or a local slapd, see below), `openssl`, `psql`, Node.js 22.18+, the release binary
and an empty PostgreSQL database. From the repository root:

```sh
export PGHOST=127.0.0.1 PGPORT=5432 PGUSER=... PGPASSWORD=... PGDATABASE=shadoucmdb_ldap DATABASE_SSL=disable
psql -d postgres -c 'CREATE DATABASE shadoucmdb_ldap'
backend/target/release/shadoucmdb migrate

tools/ldap-it/ldap.sh start          # makes the certificates on first use; LDAPS on 127.0.0.1:6360
tools/ldap-it/ldap.sh seed

export SETUP_TOKEN=$(openssl rand -hex 24)   # first-run setup token; the test completes setup with it
backend/target/release/shadoucmdb generate-encryption-key --out ldap-it.key   # once
export ENCRYPTION_KEY_FILE=$PWD/ldap-it.key
API_HOST=127.0.0.1 API_PORT=3003 backend/target/release/shadoucmdb serve > serve-ldap.log 2>&1 &
API_URL=http://127.0.0.1:3003 LDAP_IT_SERVE_LOG=serve-ldap.log node tools/ldap-it/ldap-it.ts

tools/ldap-it/ldap.sh remove         # the test changes the directory: remove and seed it before the next run
```

The test completes first-run setup, so the database must have no users; drop and recreate it (and
`migrate`) between runs. `LDAP_IT_SERVE_LOG` is optional: with it, the test also checks that the
server log has the TLS error that the answers leave out.

Without Docker, install slapd and the LDAP tools (Debian/Ubuntu: `apt install slapd ldap-utils`, then
stop the packaged service), and set `LDAP_IT_MODE=local`. `LDAP_IT_SLAPD`, `LDAP_IT_LDAPMODIFY`,
`LDAP_IT_SCHEMA` and `LDAP_IT_MODULES` point to other locations of the binaries, the schema directory and
the module directory. Other settings: `LDAP_IT_PORT` (default 6360), `LDAP_IT_DIR` (certificates,
configuration and local data; default `$RUNNER_TEMP/ldap-it` or `$TMPDIR/ldap-it`), and `LDAP_IT_PSQL`
(default `psql`).
