### Fixed: security documents corrected against the code

The [hardening guide][SHAA-782-hardening], [risk assessment][SHAA-782-risk],
[incident response plan][SHAA-782-ir], [telemetry page][SHAA-782-telemetry] and SDL described
parts of the product as they were before 0.1.0-rc.1. Corrected: the database role model (three
roles, system tables in schema `cmdb`, the `cmdb_reporting` role for read-only reporting on the
area views; the old advice to make `shadoucmdb_app` own schema `public` is removed); the
`DATABASE_SSL` default (`verify-full`); `/docs` being off by default; the outbound connections the
server makes (OIDC providers, LDAP/AD directories, syslog export), with their controls and two new
risks (T21, T22); disabling or deleting a user revoking the API tokens they created for others;
and controls listed as planned that are in place (audit hash chain and export, SBOM, CodeQL,
cosign signatures, pinned actions).

**Action on upgrade:** none in the product. If your reporting role was set up from the earlier
hardening guide (`GRANT SELECT ON ALL TABLES IN SCHEMA public`), replace it with `cmdb_reporting`.
Take `pg_dump` backups as `shadoucmdb_owner`, not the API role.

[SHAA-782-hardening]: docs/security/hardening.md#database-roles
[SHAA-782-risk]: docs/security/risk-assessment.md
[SHAA-782-ir]: docs/security/incident-response.md
[SHAA-782-telemetry]: docs/security/telemetry.md
