### Security: API tokens can no longer change permission profiles or import a configuration

A token scoped to the Administrator profile could turn off `requireMfa` on the built-in Administrator
profile, widen a profile that other accounts or identity-provider groups hold, or do the same through a
configuration import with a `permissionProfiles` section. These changes stayed in place after the token
was revoked ([GH#154]). The following routes now need a signed-in session. They answer an API token with
`403 FORBIDDEN`, as account, token and identity provider administration already do:

- `POST /api/v1/admin/profiles`
- `PATCH /api/v1/admin/profiles/{id}` (including `requireMfa` on the built-in Administrator profile)
- `DELETE /api/v1/admin/profiles/{id}`
- `POST /api/v1/admin/profiles/{id}/clone`
- `POST /api/v1/admin/config/import` (both `dry_run` and `apply`, whatever sections the file has)

Listing and reading profiles and `GET /api/v1/admin/config/export` still accept tokens. Each refused
request writes a `token.use` audit row with outcome `session_only`.

**Upgrade:** a script that manages permission profiles or imports configuration files with an API token
gets `403` after the upgrade. Make those changes as a signed-in administrator, in the web UI or with a
session (cookie plus `X-CSRF-Token`). Exporting with a token still works. The OpenAPI document lists these
operations without the token scheme. To find profile changes made with tokens before
the upgrade, filter the audit log by `entityType=permission_profiles` and look for `actorType` `api_client`.

[GH#154]: https://github.com/Shadoukita/ShadouCMDB/issues/154
