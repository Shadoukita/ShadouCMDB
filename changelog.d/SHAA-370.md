### Security: API tokens can no longer add, change or delete an identity provider

A token scoped to the Administrator profile could add an OIDC provider or LDAP directory, point an
existing one at another server or remap its groups to profiles, and the sign-ins that set up kept
working after the token was revoked ([GH#137]). These routes now need a signed-in
session and answer an API token with `403 FORBIDDEN`, like account and token administration:

- `POST /api/v1/admin/identity-providers`
- `PATCH /api/v1/admin/identity-providers/{id}` (settings, secrets, `isEnabled` and `groupMappings`)
- `DELETE /api/v1/admin/identity-providers/{id}`

Listing and reading providers and `POST /api/v1/admin/identity-providers/{id}/test` (it changes
nothing) still accept tokens. Each refused request writes a `token.use` audit row with outcome
`session_only`.

**Upgrade:** a script that configures identity providers with an API token gets `403` after the
upgrade. Make those changes as a signed-in administrator in the web UI. The OpenAPI document lists
these operations without the token scheme.

[GH#137]: https://github.com/Shadoukita/ShadouCMDB/issues/137
