# ShadouCMDB API

The backend is the only database client. Everything the UI needs goes through this API.

- **Contract:** [`backend/openapi.json`](../backend/openapi.json) (OpenAPI 3.1). It is generated from the code
  (utoipa): the same route table builds the router and the document, and requests are validated against the
  same JSON Schemas the document publishes. The running server serves it at `GET /openapi.json`, with a
  browsable UI (Swagger UI, bundled in the binary) at `GET /docs`, only when `API_DOCS` allows it: `off`
  (default, 404), `authenticated` (any signed-in user; 401 otherwise) or `public`.
- **Base path:** `/api/v1`. The health probes `/healthz` and `/readyz` sit at the root.
- **Sign-in required:** every operation except `/healthz`, `/readyz`, `GET /api/v1/version`,
  `GET/POST /api/v1/setup` and `POST /api/v1/auth/login` needs a session (see
  [Authentication and permissions](#authentication-and-permissions)). The web UI's static files stay public.
- **Version:** `GET /healthz` returns `{"status":"ok","version":"…"}`; `GET /api/v1/version` returns the
  version, the API major version (`v1`) and the number of migrations the build ships.
- **Timeouts:** a request not answered within `HTTP_REQUEST_TIMEOUT_SECS` (default 120) gets
  `408 REQUEST_TIMEOUT` and its transaction is rolled back.
- **Request bodies:** at most 1 MiB (16 MiB on `POST /api/v1/admin/config/import`, 64 KiB on the public
  routes), else `413 PAYLOAD_TOO_LARGE`. The body is read only after authentication and permission checks.
- **Busy server:** past `HTTP_MAX_CONCURRENT_REQUESTS` requests in progress the server answers
  `503 SERVER_BUSY` with a `Retry-After` header. Setup and sign-in have their own, smaller pool and
  must send their body within `HTTP_HEADER_READ_TIMEOUT_SECS`, else `408 REQUEST_TIMEOUT`.
- **Regenerate the contract** after changing a route: `shadoucmdb openapi --out backend/openapi.json`
  (or `cargo run -- openapi --out openapi.json` in `backend/`). `shadoucmdb openapi --check backend/openapi.json`
  fails if the committed file is stale; CI runs it. Then refresh the UI types with `npm run api:types -w frontend`.

## Conventions

| Topic | Rule |
| --- | --- |
| Bodies | JSON only (`Content-Type: application/json`); any other type gets `415`. Unknown fields are rejected with `400`. |
| Create / update / delete | `POST` returns `201` and the created resource. `PATCH` is partial and returns the full resource. `DELETE` returns `204`. |
| Pagination | `limit` (1–200, default 50) and `offset`. Responses are `{ "data": [...], "page": { "limit", "offset", "total" } }`. There is no unpaginated collection. |
| Sorting | `sort=name` sorts ascending, `sort=-name` descending. Each endpoint lists its allowed fields in the spec. |
| Search | `q` does a case-insensitive search. On CIs it covers the label, the ident and attribute values (text and enum by substring, IP and CIDR by prefix, and IP containment when `q` is an IP or CIDR). |
| Id filters | Accept comma-separated lists (`lookupValueId=a,b`) or repeated keys. |
| Optimistic locking | CIs carry `version`. Send it in `PATCH`; if it is stale you get `409 VERSION_CONFLICT`. |
| Audit | Every write adds an `audit_log` row in the same transaction, with the signed-in user as the actor (`actorType: user`, `actorId`, `actorName` = username). `X-Request-Id` is echoed back and stored. |
| Sessions | The `shadoucmdb_session` cookie (see below). Writes (`POST`, `PUT`, `PATCH`, `DELETE`) also need `X-CSRF-Token`. |
| API tokens | Scripts and services send `Authorization: Bearer scmdb_…` instead; no cookie, no CSRF token (see [API tokens](#api-tokens)). |
| CORS | Off by default. Set `CORS_ORIGINS` when the UI is served from another origin; those origins may send credentials (the session cookie). |

## Error envelope

Every non-2xx response has this shape:

```json
{
  "error": {
    "code": "VALIDATION_ERROR",
    "message": "Request validation failed",
    "details": [
      { "in": "body", "field": "attributes.cpu_cores", "message": "Too small: expected number to be >=1", "code": "too_small" }
    ],
    "requestId": "2d4bd91c-2d77-473e-9d54-b234179ab959"
  }
}
```

| HTTP | `code` | When |
| --- | --- | --- |
| 400 | `VALIDATION_ERROR` | The body, query or path failed validation, including database rule violations such as an illegal relationship class, a class cycle or an abstract class. `details[]` gives each field. |
| 401 | `UNAUTHENTICATED` | No session, an expired or idle session, a disabled user, an unknown, expired or revoked API token, or (on login) a wrong username, password or authenticator code. |
| 401 | `MFA_REQUIRED` | Login only: the password was right and the user has two-factor authentication; send the code to `POST /auth/login/mfa`. |
| 403 | `FORBIDDEN` | Signed in, but a global permission or a class permission is missing; or an API token on a route that needs a session. |
| 403 | `CSRF_TOKEN_INVALID` | A write without the session's `X-CSRF-Token` header. |
| 403 | `MFA_ENROLMENT_REQUIRED` | A profile the user holds requires two-factor authentication and they have not set it up: only sign-out, `/auth/me`, the password change and the `/auth/mfa` set-up routes answer. |
| 404 | `NOT_FOUND` | The id does not exist, or the route does not exist. |
| 409 | `CONFLICT` | A duplicate (unique key or live edge), or a write to a soft-deleted CI or relationship. |
| 409 | `IN_USE` | A hard delete of a row that is still referenced. `details[]` names each kind of reference and its count (`field` is the kind, e.g. `configurationItems`; `code` is `in_use`). Retire the row with `PATCH {"isActive": false}` instead. |
| 409 | `VERSION_CONFLICT` | A stale `version` on a CI `PATCH`. |
| 409 | `LAST_ADMINISTRATOR` | The change would leave no active user holding the Administrator profile. |
| 410 | `GONE` | The operation was removed; `message` names its replacement. Today: `POST`, `PATCH` and `DELETE` on the read-only `/statuses`, `/environments`, `/locations` and `/owners` (use `/lookup-list-values`). |
| 422 | `INVALID_NAME` | A technical name (area, type or field `key`) is malformed, reserved (SQL keyword, `pg_` or `shadoucmdb_` prefix, system schema, registry column) or already taken. `details[]` names the field and the reason. |
| 422 | `SCHEMA_CHANGE_REFUSED` | A data-loss guard stopped a schema change: a type change some stored values would not survive, `isRequired` while assets lack a value, removing stored enum values, or a purge that is not allowed yet (still active, wrong `confirm`, dependants). Nothing was changed. |
| 409 | `CONFLICT` (password) | `PUT /auth/password` or `PUT /admin/users/{id}/password` for an account that signs in through an identity provider. |
| 429 | `RATE_LIMITED` | Too many failed sign-ins (for this username, or on the whole server), or too many wrong current passwords on `PUT /auth/password`; wait for `Retry-After` seconds. |
| 413 / 415 | `PAYLOAD_TOO_LARGE` / `UNSUPPORTED_MEDIA_TYPE` | The body is over 1 MiB (16 MiB for a configuration import), or is not JSON. |
| 503 | `DATABASE_UNAVAILABLE` | PostgreSQL is unreachable. |
| 503 | `IDENTITY_PROVIDER_UNAVAILABLE` | Login only: the LDAP/AD directory that would check this username could not be reached (or its certificate is not trusted). Local accounts still sign in. |
| 503 | `SCHEMA_NOT_MIGRATED` | The database has migrations pending (the message says how many are applied). Run `shadoucmdb migrate`; the server picks the change up without a restart. |
| 500 | `INTERNAL_ERROR` | A bug. The message is generic and the log carries `requestId`. |

## Endpoints

| Resource | Endpoints | Notes |
| --- | --- | --- |
| Configuration items | `GET/POST /configuration-items`, `GET/PATCH/DELETE /configuration-items/{id}` | Every CI has the core `ident` (generated, e.g. `CI-7K3M9Q2X`; only administrators may set or change it, `403` otherwise), `validFrom`, `validUntil`, the derived `active` and `label` (the value of the class's title attribute, or the ident); everything else, name and status included, is an attribute of its class (see [the data model](data-model.md#the-ci-core-ident-validity-and-label)). Filters: `classId` (includes subclasses unless `includeSubclasses=false`), `active=true\|false\|all` (default `true`: only CIs inside their validity period), `lookupValueId` (lookup list value ids: one of the values of each list, e.g. a status and an environment), `ipWithin` (CIDR, over IP attributes), `deleted=exclude\|include\|only`. Sort by `label` (default), `ident`, `className`, `validFrom`, `validUntil`, `createdAt`, `updatedAt`, or by an attribute with `sort=attributes.<key>` (`-` for descending) when `classId` is given: the key must be the same attribute on every class in `classId` (inherited ones count) and not a reference; text sorts case-insensitively, IP/CIDR by address, lookups by list order, CIs without a value last (`400` on the field `sort` otherwise). Items embed `class` and carry `attributes` (a key → value map; unset attributes are absent) and `attributeReferences` (`{id, name, deleted, hidden}` for reference attributes) in both the list and the detail view, so a list view can show attribute columns. The `name` of a reference is the referenced CI's label. A reference into a class the caller may not view comes back with `hidden: true`, `name: null` and `deleted: false`; setting a reference to such a CI fails with the same `not_found` as a missing one. DELETE is a soft delete and also soft-deletes the CI's relationships. |
| Graph | `GET /configuration-items/{id}/graph?depth=1..6&direction=both\|outgoing\|incoming&relationshipTypeId=&maxNodes=` | Returns `{ nodes[], edges[], truncated }` in one call. `nodes[].depth` is the number of hops from the root. Each edge embeds its type and labels. |
| Search | `GET /search?q=` | Results are ranked (exact label or ident first, then label prefix, then similarity), each with `matches[]` naming the field that hit (`label`, `ident`, `attributes.hostname`, …). It takes the same filters as the CI list, so it returns active CIs unless `active=false\|all`. |
| Relationships | `GET/POST /relationships`, `GET/PATCH/DELETE /relationships/{id}` | Filters: `ciId` (either end), `sourceCiId`, `targetCiId`, `relationshipTypeId`, `deleted`. Each edge embeds `type`, `source` and `target`. PATCH changes only `notes` or `relationshipTypeId`. DELETE is a soft delete. |
| Areas | `GET/POST /areas`, `GET/PATCH/DELETE /areas/{id}`, `POST /areas/{id}/purge` | An area is a menu tab and a PostgreSQL schema ("Bestand" → `bestand`). `key` is derived from `name` unless given, and immutable. DELETE archives; purge (`{"confirm": "<key>"}`) drops the empty schema. Writes need `datamodel.manage`. |
| CI classes (types) | `GET/POST /ci-classes`, `GET/PATCH/DELETE /ci-classes/{id}`, `GET /ci-classes/{id}/attributes`, `GET /ci-classes/{id}/usage`, `POST /ci-classes/{id}/purge` | Each type has a table `<area>.<key>` and a reporting view `<area>.v_<key>`. `areaId` is immutable; left out on create, the type goes into its parent's area, or a root type into `infrastruktur` (created if missing). DELETE archives; purge drops the table with the type's CIs. `/attributes` returns every attribute a CI of this class can carry, inherited ones included, so the UI can render the CI form from it. A class has `name`, `parentId`, `isAbstract`, `icon`, `color` (`#rrggbb`), `sortOrder`, `isActive` (archive) and `titleAttributeId` (the field of the class or an ancestor that labels its CIs; a new subtype takes its parent's). `key` is immutable. Filters: `parentId` (`none` for roots), `descendantOf`, `isAbstract`, `isActive`; `sort=sortOrder` for menus. |
| Attribute definitions (fields) | `GET/POST /attribute-definitions`, `GET/PATCH/DELETE /attribute-definitions/{id}`, `GET /attribute-definitions/{id}/usage`, `POST /attribute-definitions/{id}/purge` | Each field is a typed column of its type's table. Filters: `classId`, `effectiveForClassId`, `dataType`. A definition carries `label`, `isRequired`, `enumValues`, `validation`, `groupName` (the form section), `sortOrder` (order within the section), `helpText` and `defaultValue`. `classId`, `key`, `referenceClassId` and `lookupListId` cannot change after creation; `dataType` can, between the scalar types, after a dry run of every stored value. DELETE archives; purge drops the column. |
| Schema changes | `GET /schema-changes`, `GET /schema-changes/{id}`, `POST /schema-changes/preview`, `POST /schema-changes/reconcile`, `GET /technical-names?name=&kind=` | Needs `datamodel.manage`. The history of every DDL plan (actor, time, exact statements, impact). `preview` runs any data model operation in a transaction that is rolled back and returns its DDL and impact. `reconcile` brings the catalog and reporting grants in line with the metadata. `technical-names` previews the key a display name maps to. |
| Relationship types | `GET/POST /relationship-types`, `GET/PATCH/DELETE /relationship-types/{id}`, `GET /relationship-types/{id}/usage` | `?sourceClassId=&targetClassId=` returns only the types legal between two classes, which is what the "add relationship" picker needs. |
| Relationship rules | `GET/POST /relationship-rules`, `GET/PATCH/DELETE /relationship-rules/{id}`, `GET /relationship-rules/{id}/usage` | Define which classes each type may connect. A rule also covers the subclasses of its classes. Deleting a rule keeps existing relationships; its usage counts them. |
| Statuses, environments, locations, owners (deprecated, read-only) | `GET /{statuses\|environments\|locations\|owners}`, `GET /…/{id}`, `GET /…/{id}/usage` | **Deprecated since migration 0016:** CIs no longer refer to these rows; status, environment, owner and location are lookup attributes on the lists `status`, `environment`, `owner` and `location`, whose values kept the ids of these rows. Change those values through `/lookup-list-values`. `POST`, `PATCH` and `DELETE` here answer `410 GONE` (after the usual `401`/`403` checks) and change nothing; the reads stay for history until a later release. Filters include `isActive`, `isOperational` (statuses), `parentId` and `locationType` (locations), and `kind` (owners). |
| Lookup lists | `GET/POST /lookup-lists`, `GET/PATCH/DELETE /lookup-lists/{id}`, `GET/POST /lookup-list-values`, `GET/PATCH/DELETE /lookup-list-values/{id}`, `GET /…/{id}/usage` | Lists an administrator defines (e.g. "Support contract": Gold, Silver). Values have `key`, `name`, `color`, `sortOrder`, `isActive`; filter values by `listId`. A `lookup` attribute stores one value by id. A list can be deleted with its values only while no attribute uses it and no list depends on it. Dependent lists: a list's `parentListId`, each value's `parentValueId` (filter `parentValueId=<id>|none`) and a field's `parentAttributeId`; see [Dependent lookup lists](data-model.md#dependent-lookup-lists). |
| Templates | `GET /admin/templates`, `POST /admin/templates/{key}/install` | Needs `datamodel.manage`. Lists the starter templates (today `it_infrastructure`: classes, attributes, relationship types and rules, lookup lists and their values) with what each brings, how much of it exists already and a `status` (`not_installed`, `partial`, `installed`). Install adds every missing row in one transaction and leaves existing ones alone, so it is idempotent; the response counts `created` and `existing` rows. Every created row is audited with the installing user. |
| UI settings | `GET/PUT /ui-settings`, `GET /ui-settings/branding`, `GET /ui-settings/versions`, `GET /ui-settings/versions/{version}`, `POST /ui-settings/versions/{version}/restore`, `GET/PUT/DELETE /ui-settings/assets/{logo\|favicon}` | One settings document for every user: branding, navigation, dashboard widgets, list views and detail/form layouts per class. Any signed-in user reads it; writes need `customization.manage`. `branding` and the images are public (login page). See [Customization](#customization-and-configuration-exportimport). |
| Configuration export/import | `GET /admin/config/export`, `POST /admin/config/import?mode=dry_run\|apply` | Needs `config.export_import`; importing a non-empty `dataModel` or `lookups` section also needs `datamodel.manage`, a `uiSettings` section `customization.manage`, and a non-empty `permissionProfiles` section `profiles.manage` (403 otherwise, dry run included). Import needs a session (API tokens get `403`); export accepts tokens, and only includes `permissionProfiles` when the caller also holds `profiles.manage` or `users.manage`. One JSON file with the data model, lookups, permission profiles and UI settings (no users, passwords or CIs). See [Customization](#customization-and-configuration-exportimport). |
| Audit log | `GET /audit-log` | Read-only, needs `audit.view`. Filters: `entityType`, `entityId`, `action`, `actorId`, `actorName`, `requestId`, `from`, `to`. Also records authentication events (`entityType=sessions`; actions `login.success`, `login.failure`, `login.locked`, `logout`, `session.revoke`) with the client `ipAddress` and `userAgent` in `newValue` (plus `peerIpAddress` when the TCP peer differs from the forwarded address); a failed sign-in has no actor id and stores only the attempted username. See [data model](data-model.md#auditing). |
| Setup | `GET /setup`, `POST /setup` | `setupRequired` is true while no user exists. `POST` creates the first user with the Administrator profile and signs them in; it needs `setupToken`, the one-time setup token from the server log (`403` if wrong; `429` while wrong tokens have locked setup for the client's network); `409` once any user exists, never throttled. |
| Authentication | `POST /auth/login`, `POST /auth/login/mfa`, `POST /auth/logout`, `GET /auth/me`, `PUT /auth/password` | `login` and `me` return `{ user, permissions, mfa, csrfToken }`. `permissions` is the union of the user's profiles: `administrator`, `global[]`, `allClasses` and per-class `classes[]`. Changing your own password needs `currentPassword`, ends your other sessions and revokes your API tokens. |
| Enterprise sign-in | `GET /auth/providers`, `GET /auth/oidc/{id}/start`, `GET /auth/oidc/callback` | Public. The sign-in page's OIDC buttons and whether a directory is enabled; the OIDC redirect flow (browser navigations, not fetches). See [Enterprise sign-in](#enterprise-sign-in). |
| Identity providers | `GET/POST /admin/identity-providers`, `GET/PATCH/DELETE /admin/identity-providers/{id}`, `POST /admin/identity-providers/{id}/test` | Administrator profile only (`users.manage` alone is `403`); `POST`, `PATCH`, `DELETE` and the connection test also need a session (API tokens get `403`). OIDC providers and LDAP/AD directories with their group-to-profile mappings; secrets are write-only. See [Enterprise sign-in](#enterprise-sign-in). |
| Two-factor authentication | `GET /auth/mfa`, `POST/DELETE /auth/mfa/totp`, `POST /auth/mfa/totp/confirm`, `POST /auth/mfa/recovery-codes` | One's own TOTP set-up; needs a session. See [Two-factor authentication](#two-factor-authentication). |
| Users | `GET/POST /admin/users`, `GET/PATCH/DELETE /admin/users/{id}`, `PUT /admin/users/{id}/password`, `DELETE /admin/users/{id}/mfa` | Needs `users.manage`. `PATCH` renames, disables (`isActive: false`, which ends the user's sessions) and assigns profiles (`profileIds` replaces the set). `PUT …/password` sets a new password, ends the user's sessions and revokes their API tokens (`revokedBy` is the administrator), and also the working tokens the user created for other owners. `DELETE …/mfa` turns off a user's two-factor authentication (lost device). A user shows `mfaEnabled` and `identityProvider` (null for a local account). Filters: `q`, `isActive`, `profileId`. |
| API tokens | `GET/POST /admin/api-tokens`, `GET/DELETE /admin/api-tokens/{id}` | Needs `users.manage` and a session. `POST {name, profileId, expiresAt, userId?}` answers `201 { token, secret }`; the secret is in that response only. `DELETE` revokes (the token stays listed with `status: revoked`). Filters: `q`, `userId` (owner), `createdBy` (the creating user, `createdByUserId`), `status` (`active`, `expired`, `revoked`). See [API tokens](#api-tokens). |
| Permission profiles | `GET/POST /admin/profiles`, `GET/PATCH/DELETE /admin/profiles/{id}`, `POST /admin/profiles/{id}/clone` | Writes need `profiles.manage` and a session (API tokens get `403`); reading also works with `users.manage`. A profile is `{ name, description, globalPermissions[], classPermissions[], requireMfa }`; `PATCH` replaces whichever list it sends. `requireMfa` makes two-factor authentication mandatory for its holders. The built-in Administrator profile is read-only except for `requireMfa` (`409`) and listed first. |
| Health | `GET /healthz`, `GET /readyz` | `/readyz` returns `503` when the database is unreachable or migrations are pending, and reports `migrations: { applied, expected, upToDate }`. `database` is `ok`, `unreachable` (no connection), `authentication_failed` (credentials refused), `permission_denied` (connected, but the role may not read the schema) or `error` (see the server log). |

## Authentication and permissions

**Signing in.** Users are local accounts with argon2id password hashes (at least 12 characters).
`POST /api/v1/auth/login {username, password}` (the username is case-insensitive) sets two cookies:

| Cookie | Attributes | Purpose |
| --- | --- | --- |
| `shadoucmdb_session` | `HttpOnly; SameSite=Lax; Path=/`, `Secure` behind HTTPS | 256-bit random token. The server stores only its SHA-256 in `sessions`. |
| `shadoucmdb_csrf` | `SameSite=Lax; Path=/`, `Secure` behind HTTPS, readable by the UI | The session's CSRF token (also `csrfToken` in the login and `/auth/me` responses). |

Behind HTTPS (whenever the cookies get `Secure`) they are named `__Host-shadoucmdb_session` and
`__Host-shadoucmdb_csrf`. A browser keeps a `__Host-` cookie only if it is `Secure`, has `Path=/` and no `Domain`,
so another host under the same domain cannot plant one ("cookie tossing"). When a request carries both names, the
server reads only the `__Host-` cookie, whatever the order. A client that reads the CSRF cookie must likewise prefer
`__Host-shadoucmdb_csrf`, or use `csrfToken` from the response. Sessions opened under the plain names before this
change keep working over HTTPS for one more release: the first answer to such a session sets the `__Host-` cookies
and deletes the plain ones.

Every `POST`, `PUT`, `PATCH` and `DELETE` must echo the token in `X-CSRF-Token`, or it is rejected with `403
CSRF_TOKEN_INVALID` before anything else happens. Login and setup need no token: they accept only
`application/json`, which a cross-site form cannot send.

A session ends on logout, after `SESSION_IDLE_TIMEOUT_MINUTES` without a request (default 12 h), after
`SESSION_MAX_AGE_HOURS` in total (default 7 days), when the user is disabled, and when an administrator resets the
user's password. `Secure` is added when the request reached the reverse proxy over HTTPS (`X-Forwarded-Proto: https`
or `Forwarded: proto=https`); `COOKIE_SECURE=always|never` overrides that.

**Login backoff.** After 5 failed sign-ins for a username, each further failure locks that username for 1 s, 2 s,
4 s, … up to 15 min. While locked, login answers `429 RATE_LIMITED` with `Retry-After`, without checking the
password. A success resets the counter. Unknown usernames are throttled the same way and cost the same argon2
work, so neither the answer nor its timing reveals which usernames exist.

The limits hold for concurrent requests too: an attempt is reserved when it passes the check and counts as if it
had failed until it finishes. A username admits only as many attempts at once as it has free failures left (one at a
time once it has been locked); further attempts that arrive meanwhile get `429` with `Retry-After: 1`. Attempts in
progress also count towards the server-wide budget below.

On top of that, the server counts failed sign-ins for all usernames together. Once 300 fall within any
10 minutes, sign-in is slowed down, not refused: attempts wait in a single queue that lets one through every
2 s, so guessing across all accounts stays at about 300 per 10 minutes while a correct password still signs in
(after a short wait). This caps password spraying (a guess or two for each of many usernames), which the
per-username lock does not see. Only when 64 attempts are already queued is the next one answered `429` with
`Retry-After`. The server logs a warning (at most once per 10 minutes) when the budget is exceeded. Sessions
that already exist are unaffected.

`PUT /api/v1/auth/password` has the same per-user backoff for wrong `currentPassword` values (5 free, then 1 s,
2 s, … up to 15 min), so a stolen session cannot be turned into the password by guessing. The counters live in
memory (per process, reset on restart).

**Two-factor authentication.** <a id="two-factor-authentication"></a>Any user can add an authenticator app (TOTP,
RFC 6238: SHA-1, 6 digits, 30 s; Google Authenticator, Microsoft Authenticator, 1Password, Aegis, … all work).

Local and LDAP/AD directory accounts can; for a directory account `currentPassword` below is the directory password,
checked against the account's own directory entry (`409` while that directory is disabled, `503
IDENTITY_PROVIDER_UNAVAILABLE` when it cannot be reached). OIDC accounts have no password here (`409`); their provider
runs the second factor.

1. `POST /api/v1/auth/mfa/totp {currentPassword}` answers `201 { secret, otpauthUri, algorithm, digits, period }`.
   Show `otpauthUri` as a QR code (or let the user type `secret`). Calling it again replaces an unconfirmed secret.
2. `POST /api/v1/auth/mfa/totp/confirm {code}` with a code from the app turns MFA on and answers
   `{ codes: [10 recovery codes] }`. They are shown only in this response (the server keeps their SHA-256); each
   signs in once in place of a code. `POST /api/v1/auth/mfa/recovery-codes {currentPassword, code}` replaces them.
3. From then on `POST /auth/login` with the right password (local or directory) answers `401 MFA_REQUIRED` and sets the
   `shadoucmdb_mfa` cookie (`HttpOnly`, `Path=/api/v1/auth`, 5 minutes). `POST /api/v1/auth/login/mfa {code}` with an
   authenticator code or a recovery code then signs in like login did before. A challenge takes at most 5 wrong
   codes; after that, or after 5 minutes, the password is asked for again.

Each authenticator code works once (a code already used, even within its 30 s, is refused), and one step of clock
drift either way is accepted. **Wrong codes are failed sign-ins**: they count towards the same per-username lock and
the same server-wide budget as wrong passwords (see *Login backoff*), and the right password alone does not reset
that count while a code is due. `DELETE /api/v1/auth/mfa/totp {currentPassword, code}` turns MFA off; the password
and code checks on these self-service routes share the per-user lock of `PUT /auth/password`. Once MFA is set up,
a right current password alone (on any of these routes or `PUT /auth/password`) does not reset that count either; only
a right password together with a right code does, so holding a session and the password does not buy unlimited
guesses at the code.

A permission profile with `requireMfa: true` (any profile, including the built-in Administrator) makes MFA mandatory
for its holders with a local or directory (LDAP/AD) account. OIDC accounts are exempt when their provider proved
the second factor at sign-in or is trusted to enforce it (see *Enterprise sign-in*; `mfa.required` is then false).
Local and directory users still sign in with their password, but until they have confirmed an authenticator every route
except sign-out, `/auth/me`, `PUT /auth/password` and the `/auth/mfa` set-up routes answers
`403 MFA_ENROLMENT_REQUIRED`; `/auth/me` shows `mfa.enrolmentRequired`. The requirement applies to sessions only: API
tokens are separate credentials and keep working. A user who lost their device and their recovery codes asks a
user manager for `DELETE /api/v1/admin/users/{id}/mfa`; if every administrator is locked out,
`shadoucmdb create-admin` creates a new one who can do that. Enrolment, turning MFA off, wrong codes and the use of
recovery codes are audited (`mfa.*`, see [data model](data-model.md#auditing)). WebAuthn / passkeys are not
supported yet.

**Enterprise sign-in.** <a id="enterprise-sign-in"></a>Besides local accounts, users can sign in through OpenID
Connect providers (Microsoft Entra ID, Okta, Keycloak, ADFS, Google Workspace, …) and LDAP / Active Directory
directories. An administrator (built-in Administrator profile) sets them up under `/api/v1/admin/identity-providers`
and maps the provider's groups to permission profiles.

- **Accounts.** The first sign-in of a person creates their account (`identityProvider` set, no password). Every
  sign-in sets its display name and e-mail from the provider and its profiles to exactly those its groups map to
  (compared case-insensitively). With no mapped group the sign-in is refused and an existing account loses its
  profiles. An account is never linked to a provider by username: if the name is taken, the sign-in is refused
  (`account_conflict`). Disabling an account here holds whatever the provider says. Changing an account's profiles
  by hand lasts until its next sign-in; change the mappings instead.
- **Break-glass.** Local accounts keep working next to any provider, including when the provider is down or
  misconfigured. Keep at least one local administrator (with two-factor authentication) and its password in your
  emergency procedure; `shadoucmdb create-admin` remains the last resort.
- **MFA for OIDC accounts.** OIDC accounts have no password here; their provider runs the second factor. Each OIDC
  provider has `oidc.mfaAssurance`: `verify` (the default for new providers) or `trustProvider`. With `verify`, a
  user holding a `requireMfa` profile is signed in only when the signed ID token proves a second factor: its `acr` is
  one of `oidc.requiredAcr` (then also sent as `acr_values`), or, without `requiredAcr`, its `amr` contains `mfa` or
  values of two RFC 8176 factor categories. Otherwise the sign-in ends at `/login?ssoError=mfa_not_enforced`. With
  `trustProvider` the token is not checked. A session whose sign-in did not prove MFA ends (`401`, audited as
  `session.revoke` with `reason: mfa_not_enforced`) on its next request once a profile of the user requires MFA or
  the provider is switched to `verify`: such an account cannot set up MFA here, so it gets no
  `MFA_ENROLMENT_REQUIRED` session. Providers that existed before this setting are `trustProvider`. See the
  [hardening guide](security/hardening.md#enterprise-sign-in) for per-provider notes.
- **Disabling or deleting a provider** ends the sessions of its accounts. A provider with accounts cannot be deleted
  (`409 IN_USE`): disable it.

*OIDC* (authorization code flow with PKCE S256, `state` and `nonce`; confidential or public clients). Set
`PUBLIC_URL` and register `{PUBLIC_URL}/api/v1/auth/oidc/callback` (shown as `oidc.redirectUri`) at the provider.
Request a `groups` claim in the ID token (Entra ID: *Groups assigned to the application*, which also avoids the
200-group overage; Keycloak: a group mapper, or `groupsClaim: realm_access.roles`). The web UI lists
`GET /api/v1/auth/providers` as buttons and **navigates** to `startUrl` (with `?returnTo=/path`). After the provider,
the callback sets the session cookies like `POST /auth/login` and redirects to `returnTo`; on a problem it redirects
to `/login?ssoError=<code>`: `expired`, `cancelled`, `failed`, `unavailable`, `not_configured`, `not_authorised`,
`account_conflict`, `account_disabled`, `invalid_username` or `last_administrator`. The ID token is verified in full:
its signature against the provider's published keys (RS256/384/512, PS256/384/512, ES256/384, EdDSA; never `none`
or HMAC), `iss`, `aud`/`azp`, `exp`, `iat`, `nbf`, the sign-in's `nonce`, and the callback's `iss` (RFC 9207) when
sent. The issuer must be `https://` (plain `http://` only for a test issuer on the same host). The username comes
from `usernameClaim` (default `preferred_username`) and must be a valid ShadouCMDB username.

*LDAP / Active Directory.* `ldaps://host[:port]`, or `ldap://host[:port]` with StartTLS (`startTls` defaults to
match the scheme); plain LDAP is refused. Certificates are always verified against the public roots and the
operating system's trust store; add a private CA with `caCertificate` (PEM). The service account (`bindDn`,
`bindPassword`; read-only is enough) searches `userBaseDn` with `userFilter` (default
`(&(objectClass=user)(sAMAccountName={username}))`; `{username}` is escaped), which must find exactly one entry;
then ShadouCMDB binds as that entry with the typed password (an empty password is refused before any bind). Groups
come from `groupAttribute` (default `memberOf`, direct membership; map each group, or resolve nested groups in the
filter with `LDAP_MATCHING_RULE_IN_CHAIN`). Directory users sign in with the normal username/password form: a name
no local account has is looked up in the enabled directories in `sortOrder`, and the first directory that knows the
name decides. Directory sign-ins share the login backoff of local ones. `503 IDENTITY_PROVIDER_UNAVAILABLE` means
the directory could not be asked.

`POST /api/v1/admin/identity-providers/{id}/test` checks the saved settings (OIDC: discovery and keys; LDAP: TLS,
service bind and, with `{"username": "..."}`, the entry, its groups and the profiles they map to) and answers
`200 { ok, message, details, user }` without changing anything. When no answer came back over verified TLS
(refused, timed out, TLS or StartTLS failed, not an LDAP server), `message` is the same generic text whatever the
cause, so its text no longer tells closed, filtered and non-TLS ports apart; the exact error is logged on the
server (`identity provider connection test failed`). The time to answer still differs (a refused connection fails at
once, a filtered one after the 10 s timeout); the test is for administrators only. The OIDC client secret and the LDAP bind password
are write-only (`clientSecretSet`, `bindPasswordSet`); they are stored in the database so the server can present
them, like the TOTP secrets, so protect database access and backups accordingly. SAML is not supported.

**API tokens.** <a id="api-tokens"></a>For scripts and services. A token belongs to a user (its owner; use a
dedicated account for a service) and is scoped to one permission profile: it may do exactly what **both** the owner
and that profile allow, so it never grants more than its owner holds, and taking a right from the owner takes it
from their tokens. Every token has an expiry (at most 366 days ahead) and can be revoked; it also stops working
while its owner is disabled or once its profile is deleted. Send it as `Authorization: Bearer scmdb_<64 hex>`.

- The secret is returned once, by `POST /api/v1/admin/api-tokens`. The server stores its SHA-256 and the first 14
  characters (`tokenPrefix`, to recognise a token found in a script or a log).
- With a `Bearer` header the request is authenticated by the token alone: cookies are ignored, and a bad token is
  `401`, never a fall-back to the session. That is why tokens need no CSRF token: a cross-site page cannot set the
  header (and `CORS_ORIGINS` does not allow it), and adding one cannot take a browser's session past the CSRF
  check. Other schemes (a proxy's `Basic` auth) are ignored and the session applies as usual.
- The same server-side checks apply as for a session: the route's global permission, then class permissions in the
  service. Sign-out, `/auth/me`, the password change, MFA administration, token administration and account
  changes (creating, updating, deleting a user and setting their password) and identity provider changes (adding,
  changing, including the group mappings, and deleting a provider), permission profile changes (creating,
  updating, including `requireMfa` on the built-in Administrator profile, cloning and deleting a profile) and
  configuration import need a session and answer tokens with `403 FORBIDDEN`, so a token cannot mint a credential
  (a token, an account, a password or a sign-in path) or widen or weaken what an account may do in a way that
  outlives its revocation. Listing and reading accounts, providers and profiles, the provider connection test and
  configuration export accept tokens.
- Managing tokens needs `users.manage`. As for accounts, a non-administrator can only create or revoke tokens for
  users whose permissions they hold themselves (their own tokens are always revocable), and a token for another
  user only with a profile whose permissions they hold themselves (`403` otherwise). A token created for another
  owner is also capped at its creator's current permissions: it may do only what the owner, the profile and the
  creator all allow, and nothing while the creator is disabled. Tokens created with the CLI, or whose creator is
  unknown, are capped by owner and profile only.
- A token records who created it (`createdByUserId`; `createdBy` is the name, for display). An administrator's
  reset of a user's password (`PUT /admin/users/{id}/password`) revokes the user's own tokens **and** every
  working token the user created for another owner, since the account may have been compromised. The user's own
  password change (`PUT /auth/password`) revokes only their own tokens. `GET /admin/api-tokens?createdBy=<id>`
  lists what a user created. Tokens created before this field existed have it only where their `create` audit
  row was still there when the upgrade ran; deleting the creator sets it to null and keeps the token. Disabling or
  deleting a user does not revoke the tokens they created for others, and single sign-on accounts have no password
  to reset: revoke those tokens by listing them with `createdBy` first.
- A token follows its owner's `requireMfa` (GH#200). While a permission profile the owner holds requires MFA (for
  OIDC accounts: and the provider is not trusted to enforce it), the token is accepted only if it was created from a
  session that proved a second factor (`mfaVerified`); otherwise requests get `401 UNAUTHENTICATED` and a
  `token.use` row with outcome `mfa_required`. The check runs on every request, so relaxing the policy makes such a
  token work again. For a service account under `requireMfa`, an administrator whose session proved a second factor
  creates the token. Creating a token that would be refused answers `403 MFA_REQUIRED_FOR_TOKEN`.
  `GET /admin/api-tokens?refusedForMfa=true` lists the working tokens refused this way (`refusedForMfa`).
- Every request made with a known token, accepted or refused, writes a `token.use` audit row; creating and revoking
  write `create` and `update` rows (see [data model](data-model.md#auditing)). A token that can no longer
  authenticate (revoked, expired, owner disabled, profile deleted) is recorded at most once a minute per outcome;
  the next row counts the requests left out in `unrecordedRefusals`.

**First run.** While there are no users, `GET /api/v1/setup` returns `{"setupRequired": true}` and
`POST /api/v1/setup` creates the first administrator and signs them in. Its body carries `setupToken`: the
one-time token the server writes to its log and setup token file, or the operator's `SETUP_TOKEN`
(see [deployment](deployment.md#the-setup-token)); a missing or wrong token answers `403 FORBIDDEN` with a
`setupToken` field detail. Wrong tokens are throttled like wrong passwords (5 free per client network,
then a doubling lock up to 15 min; 15 from several networks lock every network): while locked, `429
RATE_LIMITED` with `Retry-After`, and the token is not checked. `shadoucmdb create-admin` does the same
from the command line, and also works later to regain access (see [deployment](deployment.md#the-first-administrator)).

**Permission profiles.** There are no fixed roles. A profile is a named set of permissions, and a user can hold
several; their effective permissions are the union.

| Permission | Allows |
| --- | --- |
| `users.manage` | `/admin/users`: create, edit, disable, delete users, reset passwords, assign profiles |
| `profiles.manage` | `/admin/profiles`: create, edit, clone, delete profiles |
| `datamodel.manage` | Writes to CI classes, attribute definitions, relationship types and rules and lookup lists; `/admin/templates` |
| `customization.manage` | Branding, navigation, dashboard and layouts (phase 3) |
| `config.export_import` | Configuration export and import (phase 3) |
| `audit.view` | `GET /audit-log` |
| Class `view` / `create` / `edit` / `delete` | CIs of one class, or of every class with the `classId: null` wildcard. Any write right implies `view`. |

- A class grant applies to exactly that class, not its subclasses; the wildcard also covers classes added later.
- Every signed-in user can read the data model and lookups: the UI needs them to render CIs.
- CI lists, search and the graph only contain CIs of classes the user may view (the graph does not traverse
  through hidden CIs). A CI of a class the user may not view answers `404 NOT_FOUND` by id, exactly like a
  missing one, so the status does not reveal that it exists; changing a CI the user may view but not edit or
  delete answers `403`.
- Relationships: reading needs `view` on both CIs' classes; creating, changing or removing one needs `edit` on
  the source CI's class and `view` on the target's. A relationship with an endpoint the user may not view
  answers `404`, and creating one to such a CI fails with the same `not_found` field error as a missing CI.
- The `GET …/{id}/usage` routes of the data model and lookups need `datamodel.manage`, like the changes they
  prepare: their counts span every CI class.
- Moving a CI to another class needs `edit` on the old class and `create` on the new one.

**The built-in Administrator profile** holds every permission, including every class. It cannot be changed or
deleted (cloning it gives an editable copy). The database refuses any change that would leave no active user
holding it (`409 LAST_ADMINISTRATOR`), and nobody can disable or delete their own account.

**No escalation through delegation.** A user manager who is not an administrator can only assign profiles, and
act on accounts, whose permissions they hold themselves. A profile manager can only create or change profiles
within their own permissions. Otherwise `users.manage` or `profiles.manage` alone would lead to everything.

## Attribute values

Attribute values are sent and returned as JSON scalars, keyed by attribute key:

| `dataType` | JSON |
| --- | --- |
| `text`, `enum` | string (`enum` must be one of `enumValues`; `text` honours `validation.pattern` / `maxLength` and is stored exactly as sent, line breaks (`\n`, `\r\n`) and surrounding whitespace included; `validation.multiline: true` tells forms to edit it in a text area) |
| `number`, `integer` | number (`validation.min` / `max` apply) |
| `boolean` | boolean |
| `date` / `datetime` | `"2025-03-01"` / ISO 8601 with offset |
| `ip` / `cidr` | `"10.0.0.5"` / `"10.0.0.0/24"` (host bits must be zero) |
| `reference` | the id of a live CI of `referenceClassId` (or one of its subclasses) |
| `lookup` | the id of an active value of the attribute's `lookupListId` |

In `PATCH`, `attributes` is merged into the stored values, and `null` clears a value. Required attributes are
checked against the final state. An error on one value is reported at `attributes.<key>`.

On create, every active attribute the body leaves out gets its `defaultValue`, if it has one. A default is
validated like a value when the definition is written; `reference` attributes cannot have one.

## Changing the data model safely

A fresh install has no areas, classes, attributes, relationship types or lookups (`shadoucmdb seed` loads system
rows only). An administrator builds the model through the endpoints above or installs a starter template.

Areas, types and fields are real database objects (schema, table, column; see
[data-model.md](data-model.md#areas-type-tables-and-the-ddl-engine)). Every write that changes them runs its
DDL in the same transaction, serialised by an advisory lock, and is recorded in `/schema-changes` and the audit
log. Send the same body to `POST /schema-changes/preview` first to see the DDL and how many rows it touches.

- **Delete archives** areas, types and fields (`isActive: false`): the schema, table or column and every value
  stay; nothing new is accepted and the UI hides it. `PATCH {"isActive": true}` restores it. **Purge**
  (`POST …/{id}/purge` with `{"confirm": "<key>"}`) is the only way to drop them, and only once archived.
- Other data model and lookup resources are **deleted** only while nothing refers to them. `GET …/{id}/usage`
  returns `{ inUse, data: [{ kind, label, count, blocking }] }`; a blocking count makes `DELETE` answer `409
  IN_USE` with the same counts in `details[]`. Deleted CIs and relationships count too: they are kept for history.
  Archive them with `PATCH {"isActive": false}` instead.
- **Data-loss guards** answer `422 SCHEMA_CHANGE_REFUSED` and change nothing: a `dataType` change that some
  stored value would not survive (up to five are named); `isRequired: true` (a `NOT NULL` column) while any
  asset, deleted ones included, has no value; removing enum values that are stored; re-parenting a type whose CIs
  hold values in a table they would leave.
- Technical names (`key`) are immutable once created, because imports, reports and SQL depend on them; renaming
  changes only the display name.

## Customization and configuration export/import

**UI settings** (`/api/v1/ui-settings`) are one JSON document that the web UI applies for every user. Its schema is
`UiSettingsDocument` in the spec; every section is optional and `{}` means the built-in UI.

| Section | What it holds |
| --- | --- |
| `branding` | `appName`, `primaryColor` / `accentColor` (`#rrggbb`), `defaultTheme` (`light`, `dark`, `system`). |
| `navigation.entries[]` | Menu order. `type: page` (`dashboard`, `inventory`, `search`, `audit_log`, `administration`), `type: class` (`classKey`) or `type: section` (`key`, `label`, `items[]` of classes). Each entry can be renamed (`label`) and `hidden`. Pages and classes not listed follow in their default order. |
| `dashboard.widgets[]` | Widgets in order: `count_by_class` (optional `classKeys`), `count_by_lookup` (`lookupListKey`: CIs per value of that list, e.g. `status`), `recent_changes` (`limit`), `saved_search` (`search`: `classKeys`, `includeSubclasses`, `filters`, `sort`). `null` keeps the built-in dashboard. |
| `listViews[]` | Per class: `columns` (built-in fields `label`, `ident`, `class`, `validFrom`, `validUntil`, `active`, `createdAt`, `updatedAt`, or `attributes.<key>`), `defaultSort`, `defaultFilters` (`q`, and `lookups`: lookup list key → value keys), `pageSize`. |
| `layouts[]` | Per class (layout format v2): `tabs[]` (`key`, `label`, `sections[]`), each section `key` (unique in the layout), `label`, `kind` (`fields` by default; `note` with `text`, or a built-in panel `relations`, `history` or `audit`, each once per layout), `width` on the tab's 12-column grid (1–12, default 12: sections fill rows in order, so two of width 6 sit side by side), `newRow`, `minHeight` (field rows, 1–50), `columns` of its field grid (1–12, default 3), `collapsed` and `fields[]` of `{ field, width }` (1–12 columns, at most the section's), plus `hiddenFields` and `readOnlyFields`. A tab with `placement: "free"` places each section by a `frame` (`x`, `w` as fractions of the tab width, `y`, `h` in px, stacking order `z`, optional `minH`) and windows may overlap; the API normalises free tabs on save (missing frames from the grid positions, `z` 1..n, sections in reading order y then x) and converts a grid tab sent with frames back to the grid (see [data model](data-model.md#detail-and-form-layouts-ui-settings-layout-format-v2)). Fields no section places follow at the end of the first tab, grouped by attribute group. `ident`, `validFrom` and `validUntil` cannot be hidden. `hiddenFields` and `readOnlyFields` are presentation only, applied by the web UI: `GET /configuration-items/{id}` still returns a hidden attribute and `PATCH /configuration-items/{id}` still accepts a read-only one. They are not access control; restrict who can read or change a class's CIs with [permission profiles](#authentication-and-permissions). The older `panels[]` format is still accepted and converted to one "General" tab (see [data model](data-model.md#detail-and-form-layouts-ui-settings-layout-format-v2)). |

- **References are keys.** Classes, attributes and lookups are named by key, so a document moves between installs.
  A reference to something that does not exist is accepted: `GET` returns the *effective* settings without it and
  lists it in `issues[]` (`unknown_class`, `unknown_attribute`, `unknown_lookup_list`, `unknown_lookup_value`, with a path into the stored
  document). A required attribute that a layout hides or makes read-only is flagged as
  `required_field_not_editable`; a hidden core field in a restored older version as `core_field_hidden` (it is
  shown anyway). The stored document keeps every reference, so a class that comes back (e.g. from
  an import) reappears.
- **Versioned and audited.** `PUT` takes `{ version, settings, comment? }`: the version you loaded, or `409
  VERSION_CONFLICT`. Each save is a new version kept in history (`GET /ui-settings/versions`); `POST
  /ui-settings/versions/{n}/restore` saves version *n* again as the newest. An unchanged document is not saved
  again. Every save has an `audit_log` row (`entity_type = ui_settings`) with the old and new version.
- **Logo and favicon.** `PUT /ui-settings/assets/{logo|favicon}` with `{ contentType, data }` (base64). Logo: PNG,
  JPEG, WebP or SVG up to 512 KiB; favicon: PNG, ICO or SVG up to 128 KiB. The bytes must match the declared type;
  an SVG is parsed and must use only allowlisted drawing elements and attributes (shapes, text, gradients,
  patterns, masks, filters, CSS). Scripts, event handlers, animation, links, `<foreignObject>`, DTD subsets,
  processing instructions, CSS `@import`/escapes, and any reference outside the file (other than embedded PNG,
  JPEG, GIF or WebP data on `<image>`) are refused with `unsafe_content`. `GET` serves them without a
  session, with an ETag (`If-None-Match` answers 304), `nosniff` and a sandboxing Content-Security-Policy. The
  `url` in the settings carries a content hash (`?v=`). Uploads and removals are audited (`entity_type = ui_assets`).

**Export** (`GET /api/v1/admin/config/export`) downloads one file (`format: "shadoucmdb.config"`, `formatVersion:
3`) with `dataModel` (classes, attributes, relationship types and rules), `lookups` (statuses, environments,
locations, owners, lookup lists with their values), `permissionProfiles` (all but the built-in Administrator; only
when the caller also holds `profiles.manage` or `users.manage`, the key is left out otherwise) and `uiSettings` (the
stored document plus the images, base64). It never contains users, passwords, sessions, CIs or relationships.
Parents come before children; every reference is a key (a lookup attribute's default is the value's key; a list's
`parent`, a value's `parent` and an attribute's `parentAttribute` are keys too). Files of versions 1 and 2 are still
read; they carry no parents, so rows that exist in the target keep theirs.

**Import** (`POST /api/v1/admin/config/import?mode=dry_run|apply`, body: such a file, up to 16 MiB; needs a
signed-in session, API tokens get `403`):

- Every section is optional. Rows are matched by key (owners by kind and name, profiles by name, both
  case-insensitive), then created or updated field by field. **Nothing is deleted**: rows missing from the file are
  kept and counted as `notInFile`. The `uiSettings` section is the exception: it replaces the document (as a new
  version) and the images (a `null` logo removes the current one).
- The whole file is checked first and every problem is reported in one `400 VALIDATION_ERROR` with paths into the
  file (`dataModel.attributes.4.lookupList`): duplicates, unknown references, cycles, attribute rules, changes to
  immutable fields (an attribute's `dataType`, `referenceClass`, `lookupList`; a relationship type's
  `isDirectional`), invalid images.
- The import then runs in **one transaction** through the same code as the admin API, so every rule applies
  (e.g. making an attribute required while CIs lack a value is `409` with `code: values_missing`, prefixed with the
  file path). A profile cannot grant more than the importing user holds (`403`). Every change is audited with the
  importing user.
- `mode=dry_run` runs exactly the same writes and rolls back, so its result is what `apply` would do: `summary[]`
  (created, updated, deleted, unchanged, notInFile per section), `changes[]` (section, key, action, and for updates
  the changed `fields` with `from` and `to`), `warnings[]` (e.g. the built-in profile in the file is skipped) and
  `uiSettingsIssues[]`. Importing an install's own export reports no changes.

## Layers and extension seams

The server is the Rust binary `shadoucmdb` (Axum + Tokio + sqlx) in `backend/`:

```
src/http/*      transport: middleware (request id, CORS, body limit), error envelope, fallback, shutdown
src/api/*       route table, access rules, request validation (schema-driven), OpenAPI document, caller context, PG error mapping
src/auth/*      sessions and cookies, CSRF, API tokens, argon2id passwords, login backoff, permissions, the create-admin command
src/modules/*   routes and services (rules, transactions, audit) per resource
src/data/*      SQL only: compile-time checked sqlx queries (offline data in backend/.sqlx) and QueryBuilder lists
```

- **Access control:** each route declares who may call it (`.public()`, the default "signed in", or
  `.requires(GlobalPermission::…)`) in the same place as its path and schemas, so the router, the OpenAPI
  `security` entries and the documented 401/403 responses cannot disagree. The session is resolved and CSRF and
  the global permission are checked before the request is validated. Services check class permissions through
  `RequestContext::require_class` / `class_scope` (`src/api/context.rs`), because only they know a CI's class.
- **Other sign-in methods (SSO, LDAP, OIDC):** add a login route that verifies the identity and calls the same
  session start as `POST /auth/login`; everything after the session cookie stays the same.
- **Discovery and imports:** call the services directly with `RequestContext::import(source, run_id)`.
  Validation and audit then apply exactly as they do for the UI, with `actor_type = import`.
- **Integrations and reporting:** add a module under `src/modules/` and append its `routes()` in `api::routes()`
  (`src/api/mod.rs`). They show up in the spec and the router automatically; a route cannot exist without a spec entry.
- **Changing a fixed query:** the `sqlx::query!` macros are checked against `backend/.sqlx`. After changing one,
  rebuild once against a migrated database with `SQLX_OFFLINE_DIR=$PWD/.sqlx DATABASE_URL=... cargo build` and commit
  the updated files (delete stale ones first).

## Smoke test

`API_URL=http://localhost:3000 node tools/smoke/smoke.ts` (Node.js 22.18+, no dependencies, `npm run smoke` does
the same) runs every operation in the spec against a running API with a seeded database (`shadoucmdb seed --demo`),
covering the success paths and the error paths. On a database without users it completes first-run setup itself;
otherwise set `SMOKE_USERNAME` and `SMOKE_PASSWORD` to an administrator account. It also calls every non-public
operation without a session (expects 401), every permission-guarded operation as a user without profiles (expects
403), and checks class scoping, CSRF, the login backoff, the escalation guards, the last-Administrator guard, UI settings
versioning and images, and a configuration export/import round trip (dry run, apply, re-import without changes). It fails on any unexpected status, any 5xx, any spec operation it
did not call, or any response body that does not match the schema the spec declares for it. It works against any
URL: a local binary, a container or a remote host.

## Differences from the SHAA-3 Node API

The Rust server replaced the Node API in SHAA-9 with the same contract. `node tools/openapi-diff.mjs old.json new.json`
compares two specs semantically. Against the SHAA-3 `backend/openapi.json`, the only differences are:

- **Not-blank strings are published as `pattern: "\\S"`** (names, labels, `serialNumber`, `externalRef`, `enumValues`
  items). SHAA-3 trimmed these and required at least one character, which zod could not express in the schema.
- **`enumValues` publishes `uniqueItems: true`.** SHAA-3 enforced uniqueness without saying so in the schema.
- **Single-value literals are `enum: ["none"]` / `enum: ["ok"]`** instead of `const` (`parentId=none`, `Liveness.status`).
  Equivalent.
- Spellings that mean the same thing: nullable types as `type: [T, "null"]` or `oneOf`, `int32`/`int64` formats,
  and no regex patterns next to `uuid`/`date-time`/`ipv4` formats.

Behaviour differences, all deliberate:

- **Search ranking:** the exact-match and name-prefix criteria sort `NULLS LAST`. In SHAA-3 a CI with an empty
  hostname or serial number could rank above an exact match.
- **Changing a CI's class:** `"attributes": {"<old key>": null}` for keys the new class does not define is accepted
  as "clear". SHAA-3 rejected it as an unknown attribute, although its own error message suggested exactly that.
- **Validation details:** when a body already fails its schema (for example an unknown key), cross-field rules such
  as "Provide at least one field to update" are not reported in the same response. Malformed JSON is reported as
  `Body is not valid JSON: <parser detail>` with code `invalid_json`.
- **`q` that looks like a CIDR with an impossible prefix** (`10.0.0.0/99`) is searched as text instead of failing.
- **`validation.pattern` of text attributes uses Rust regex syntax** (no look-around or back-references). Patterns
  that do not compile are rejected when the definition is created or updated, as before.
- **`X-Actor-Name`** is no longer read (SHAA-28): the audit actor is the signed-in user.
- JSON key order inside objects can differ (JSON objects are unordered).
