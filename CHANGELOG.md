# Changelog

Changes operators need to act on. Everything else is in the generated notes of each
[GitHub release](https://github.com/Shadoukita/ShadouCMDB/releases).

## Unreleased

### Changed: database TLS verifies the server certificate by default

`DATABASE_SSL` now defaults to `verify-full` instead of `require` ([#39]). With the old default the
connection was encrypted, but the server certificate and host name were not checked, so anything
on the network path to PostgreSQL could pose as the database.

**Action on upgrade** if `DATABASE_SSL` is not set in your environment:

- The certificate is from a public CA and matches the host name in `PGHOST` / `DATABASE_URL`:
  nothing to do.
- Private CA, or a managed service (Amazon RDS, Azure Database for PostgreSQL): set
  `DATABASE_SSL_CA_FILE` to the CA bundle.
- Connecting by IP address: use a host name that is in the certificate, or a certificate that
  contains the IP.
- No TLS on the database (local development only): set `DATABASE_SSL=disable`.

`DATABASE_SSL=require` still works as an explicit setting, and the server now logs a warning at
startup when it is used with a host that is not loopback. See
[docs/security/hardening.md](docs/security/hardening.md#database-tls).

[#39]: https://github.com/Shadoukita/ShadouCMDB/issues/39
