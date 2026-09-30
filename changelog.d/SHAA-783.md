### Fixed: OpenAPI document version, public operations and error responses

`backend/openapi.json` (and `/openapi.json` of a running server, see the [API reference][api]) now
matches the server (SHAA-783). No request or response changed on the wire.

- `info.version` is the release version (`0.1.0-rc.1`) instead of `0.1.0`.
- `info.description` lists all 12 operations that need no session, names the list operations that
  are not paginated, and gives the body size limit and request timeout.
- Every operation publishes `408 REQUEST_TIMEOUT`, and every operation with a request body publishes
  `413 PAYLOAD_TOO_LARGE`.
- The throttle descriptions of setup, sign-in, password change and MFA say that the 5th failure
  already locks (the first 4 cost nothing).
- The response schema of `listIdentityProviders` is named `IdentityProviderList` instead of `Vec`.
  It is still a plain JSON array.

**Upgrade:** clients generated from the document that refer to the schema `Vec` must use
`IdentityProviderList` after regenerating.

[api]: docs/api.md
