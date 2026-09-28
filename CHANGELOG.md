# Changelog

Changes operators need to act on. Everything else is in the generated notes of each
[GitHub release](https://github.com/Shadoukita/ShadouCMDB/releases).

## Unreleased

### Added: dependent lookup lists (cascading dropdowns)

A lookup list can now depend on another list, e.g. "Model" on "Manufacturer": each model names the
manufacturer it belongs to, and a model field names its manufacturer field. The API then refuses a
model that does not belong to the CI's manufacturer. Retiring a manufacturer retires its models
(refused while CIs still use one of them); deleting it is refused while models belong to it. See
[docs/data-model.md](docs/data-model.md#dependent-lookup-lists).

Migration `0015_lookup_parent_lists` adds nullable columns (`lookup_lists.parent_list_id`,
`lookup_list_values.parent_value_id`, `ci_attribute_definitions.parent_attribute_id`) and their
integrity triggers; existing lists, values, fields and CI values are unchanged. `migrate` applies it
as usual; `verify` now runs 23 checks.

API: new nullable fields `parentListId`, `parentValueId`, `parentAttributeId` on lookup lists,
values and attribute definitions (responses and create/update bodies), list filters
`parentListId` and `parentValueId`, and new `409 IN_USE` cases. Configuration files are now written
as `formatVersion: 3` (with `parent` / `parentAttribute` keys); versions 1 and 2 are still imported
and leave existing parent links as they are. **Action** only for scripts that parse exported files
and check `formatVersion === 2`: accept 3.

### Added: sign-in through OIDC providers and LDAP/AD directories

Users can now sign in through OpenID Connect providers (Entra ID, Okta, Keycloak, ADFS, …) and
LDAP / Active Directory directories (LDAPS or StartTLS, certificates always verified), with the
provider's groups mapped to permission profiles. Local accounts keep working as break-glass
access. Migration `0014_enterprise_sign_in` adds the tables; `migrate` applies it as usual.
Holders of the Administrator profile set providers up under **Administration › Identity
providers** (settings, group mappings, a connection test); the sign-in page shows a "Sign in with
…" button per enabled OIDC provider.

**Action** only if you want OIDC sign-in: set `PUBLIC_URL` to the address users open the web UI
at (e.g. `https://cmdb.example.com`) and register `{PUBLIC_URL}/api/v1/auth/oidc/callback` at the
provider. Allow outbound connections from the server to the provider or the directory. See
[docs/api.md](docs/api.md#enterprise-sign-in) and
[docs/security/hardening.md](docs/security/hardening.md#enterprise-sign-in).

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
