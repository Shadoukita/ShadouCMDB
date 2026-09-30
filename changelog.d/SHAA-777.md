### Fixed: The list of outbound connections now includes identity providers and audit export

[Telemetry and offline operation](docs/security/telemetry.md) said the server connects only to
PostgreSQL. It now lists every outbound connection: PostgreSQL, the OIDC providers and LDAP/AD
directories you configure, and the SIEM or log collector named in `AUDIT_EXPORT`, with when each
is used, the setting that enables it, and its protocol and TLS. Nothing about the server's
behaviour has changed, and it still contacts no vendor or third-party endpoint. If your egress
firewall rules were based on the old page, compare them with the new list.
