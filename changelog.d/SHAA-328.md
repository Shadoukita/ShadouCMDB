### Security: API tokens can no longer create, change, delete or reset the password of an account

A token scoped to a profile with `users.manage` could create an account, assign it profiles, re-enable
it or set a user's password, and that account or password kept working after the token was revoked
([SHAA-328], GitHub #119). These routes now need a signed-in session and answer an API token with
`403 FORBIDDEN`, like token administration and the MFA reset already did:

- `POST /api/v1/admin/users`
- `PATCH /api/v1/admin/users/{id}`
- `DELETE /api/v1/admin/users/{id}`
- `PUT /api/v1/admin/users/{id}/password`

Listing and reading accounts (`GET /api/v1/admin/users`, `GET /api/v1/admin/users/{id}`) still accept
tokens. Each refused request writes a `token.use` audit row with outcome `session_only`.

**Upgrade:** a script that provisions or disables accounts with an API token gets `403` after the
upgrade. Run it as a signed-in administrator, or provision accounts through an identity provider
(LDAP or OIDC group mappings) instead. The OpenAPI document lists these operations without the token
scheme.

[SHAA-328]: docs/api.md#api-tokens
