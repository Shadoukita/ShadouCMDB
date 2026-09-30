### Fixed: hardening guide nginx sample blocked large configuration imports

The nginx sample in the [hardening guide][SHAA-806-hardening] limited every request body to 1 MiB,
stating that the server rejects larger bodies anyway. Configuration import accepts files up to
16 MiB, so an nginx set up from the sample answered 413 to imports over 1 MiB. The sample now keeps
1 MiB for the API and allows 16 MiB on `POST /api/v1/admin/config/import` only.

**Action on upgrade:** if your nginx configuration came from the earlier sample and you use
configuration import, add the import `location` block from the guide. Nothing changes in the
product.

[SHAA-806-hardening]: docs/security/hardening.md#tls-reverse-proxy
