# ShadouCMDB API

The backend is the only database client. Everything the UI needs goes through this API.

- **Contract:** [`backend/openapi.json`](../backend/openapi.json) (OpenAPI 3.1). It is generated from the code
  (utoipa): the same route table builds the router and the document, and requests are validated against the
  same JSON Schemas the document publishes. The running server also serves it at `GET /openapi.json`, with a
  browsable UI (Swagger UI, bundled in the binary) at `GET /docs`.
- **Base path:** `/api/v1`. The health probes `/healthz` and `/readyz` sit at the root.
- **Sign-in required:** every operation except `/healthz`, `/readyz`, `GET/POST /api/v1/setup` and
  `POST /api/v1/auth/login` needs a session (see [Authentication and permissions](#authentication-and-permissions)).
  The contract itself (`/openapi.json`, `/docs`) and the web UI's static files stay public.
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
| Search | `q` does a case-insensitive search. On CIs it covers name, hostname, serial, IP, notes and attribute values. |
| Id filters | Accept comma-separated lists (`statusId=a,b`) or repeated keys. |
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
| 401 | `UNAUTHENTICATED` | No session, an expired or idle session, a disabled user, an unknown, expired or revoked API token, or (on login) a wrong username or password. |
| 403 | `FORBIDDEN` | Signed in, but a global permission or a class permission is missing; or an API token on a route that needs a session. |
| 403 | `CSRF_TOKEN_INVALID` | A write without the session's `X-CSRF-Token` header. |
| 404 | `NOT_FOUND` | The id does not exist, or the route does not exist. |
| 409 | `CONFLICT` | A duplicate (unique key or live edge), or a write to a soft-deleted CI or relationship. |
| 409 | `IN_USE` | A hard delete of a row that is still referenced. `details[]` names each kind of reference and its count (`field` is the kind, e.g. `configurationItems`; `code` is `in_use`). Retire the row with `PATCH {"isActive": false}` instead. |
| 409 | `VERSION_CONFLICT` | A stale `version` on a CI `PATCH`. |
| 409 | `LAST_ADMINISTRATOR` | The change would leave no active user holding the Administrator profile. |
| 422 | `INVALID_NAME` | A technical name (area, type or field `key`) is malformed, reserved (SQL keyword, `pg_` or `shadoucmdb_` prefix, system schema, registry column) or already taken. `details[]` names the field and the reason. |
| 422 | `SCHEMA_CHANGE_REFUSED` | A data-loss guard stopped a schema change: a type change some stored values would not survive, `isRequired` while assets lack a value, removing stored enum values, or a purge that is not allowed yet (still active, wrong `confirm`, dependants). Nothing was changed. |
| 429 | `RATE_LIMITED` | Too many failed sign-ins (for this username, or on the whole server), or too many wrong current passwords on `PUT /auth/password`; wait for `Retry-After` seconds. |
| 413 / 415 | `PAYLOAD_TOO_LARGE` / `UNSUPPORTED_MEDIA_TYPE` | The body is over 1 MiB (16 MiB for a configuration import), or is not JSON. |
| 503 | `DATABASE_UNAVAILABLE` | PostgreSQL is unreachable. |
| 500 | `INTERNAL_ERROR` | A bug. The message is generic and the log carries `requestId`. |

## Endpoints

| Resource | Endpoints | Notes |
| --- | --- | --- |
| Configuration items | `GET/POST /configuration-items`, `GET/PATCH/DELETE /configuration-items/{id}` | Filters: `classId` (includes subclasses unless `includeSubclasses=false`), `statusId`, `environmentId`, `ownerId`, `locationId`, `ipWithin` (CIDR), `deleted=exclude\|include\|only`. Items embed `class`, `status`, `environment`, `owner` and `location`, and carry `attributes` (a key → value map; unset attributes are absent) and `attributeReferences` (`{id, name, deleted}` for reference attributes) in both the list and the detail view, so a list view can show attribute columns. DELETE is a soft delete and also soft-deletes the CI's relationships. |
| Graph | `GET /configuration-items/{id}/graph?depth=1..6&direction=both\|outgoing\|incoming&relationshipTypeId=&maxNodes=` | Returns `{ nodes[], edges[], truncated }` in one call. `nodes[].depth` is the number of hops from the root. Each edge embeds its type and labels. |
| Search | `GET /search?q=` | Results are ranked, each with `matches[]` naming the field that hit (`hostname`, `attributes.url`, …). It takes the same filters as the CI list. |
| Relationships | `GET/POST /relationships`, `GET/PATCH/DELETE /relationships/{id}` | Filters: `ciId` (either end), `sourceCiId`, `targetCiId`, `relationshipTypeId`, `deleted`. Each edge embeds `type`, `source` and `target`. PATCH changes only `notes` or `relationshipTypeId`. DELETE is a soft delete. |
| Areas | `GET/POST /areas`, `GET/PATCH/DELETE /areas/{id}`, `POST /areas/{id}/purge` | An area is a menu tab and a PostgreSQL schema ("Bestand" → `bestand`). `key` is derived from `name` unless given, and immutable. DELETE archives; purge (`{"confirm": "<key>"}`) drops the empty schema. Writes need `datamodel.manage`. |
| CI classes (types) | `GET/POST /ci-classes`, `GET/PATCH/DELETE /ci-classes/{id}`, `GET /ci-classes/{id}/attributes`, `GET /ci-classes/{id}/usage`, `POST /ci-classes/{id}/purge` | Each type has a table `<area>.<key>` and a reporting view `<area>.v_<key>`. `areaId` is immutable; left out on create, the type goes into its parent's area, or a root type into `infrastruktur` (created if missing). DELETE archives; purge drops the table with the type's CIs. `/attributes` returns every attribute a CI of this class can carry, inherited ones included, so the UI can render the CI form from it. A class has `name`, `parentId`, `isAbstract`, `icon`, `color` (`#rrggbb`), `sortOrder` and `isActive` (archive). `key` is immutable. Filters: `parentId` (`none` for roots), `descendantOf`, `isAbstract`, `isActive`; `sort=sortOrder` for menus. |
| Attribute definitions (fields) | `GET/POST /attribute-definitions`, `GET/PATCH/DELETE /attribute-definitions/{id}`, `GET /attribute-definitions/{id}/usage`, `POST /attribute-definitions/{id}/purge` | Each field is a typed column of its type's table. Filters: `classId`, `effectiveForClassId`, `dataType`. A definition carries `label`, `isRequired`, `enumValues`, `validation`, `groupName` (the form section), `sortOrder` (order within the section), `helpText` and `defaultValue`. `classId`, `key`, `referenceClassId` and `lookupListId` cannot change after creation; `dataType` can, between the scalar types, after a dry run of every stored value. DELETE archives; purge drops the column. |
| Schema changes | `GET /schema-changes`, `GET /schema-changes/{id}`, `POST /schema-changes/preview`, `POST /schema-changes/reconcile`, `GET /technical-names?name=&kind=` | Needs `datamodel.manage`. The history of every DDL plan (actor, time, exact statements, impact). `preview` runs any data model operation in a transaction that is rolled back and returns its DDL and impact. `reconcile` brings the catalog and reporting grants in line with the metadata. `technical-names` previews the key a display name maps to. |
| Relationship types | `GET/POST /relationship-types`, `GET/PATCH/DELETE /relationship-types/{id}`, `GET /relationship-types/{id}/usage` | `?sourceClassId=&targetClassId=` returns only the types legal between two classes, which is what the "add relationship" picker needs. |
| Relationship rules | `GET/POST /relationship-rules`, `GET/PATCH/DELETE /relationship-rules/{id}`, `GET /relationship-rules/{id}/usage` | Define which classes each type may connect. A rule also covers the subclasses of its classes. Deleting a rule keeps existing relationships; its usage counts them. |
| Statuses, environments, locations, owners | `GET/POST /{statuses\|environments\|locations\|owners}`, `GET/PATCH/DELETE /…/{id}`, `GET /…/{id}/usage` | Filters include `isActive`, `isOperational` (statuses), `parentId` and `locationType` (locations), and `kind` (owners). |
| Lookup lists | `GET/POST /lookup-lists`, `GET/PATCH/DELETE /lookup-lists/{id}`, `GET/POST /lookup-list-values`, `GET/PATCH/DELETE /lookup-list-values/{id}`, `GET /…/{id}/usage` | Lists an administrator defines (e.g. "Support contract": Gold, Silver). Values have `key`, `name`, `color`, `sortOrder`, `isActive`; filter values by `listId`. A `lookup` attribute stores one value by id. A list can be deleted with its values only while no attribute uses it. |
| Templates | `GET /admin/templates`, `POST /admin/templates/{key}/install` | Needs `datamodel.manage`. Lists the starter templates (today `it_infrastructure`) with what each brings, how much of it exists already and a `status` (`not_installed`, `partial`, `installed`). Install adds every missing row in one transaction and leaves existing ones alone, so it is idempotent; the response counts `created` and `existing` rows. Every created row is audited with the installing user. |
| UI settings | `GET/PUT /ui-settings`, `GET /ui-settings/branding`, `GET /ui-settings/versions`, `GET /ui-settings/versions/{version}`, `POST /ui-settings/versions/{version}/restore`, `GET/PUT/DELETE /ui-settings/assets/{logo\|favicon}` | One settings document for every user: branding, navigation, dashboard widgets, list views and detail/form layouts per class. Any signed-in user reads it; writes need `customization.manage`. `branding` and the images are public (login page). See [Customization](#customization-and-configuration-exportimport). |
| Configuration export/import | `GET /admin/config/export`, `POST /admin/config/import?mode=dry_run\|apply` | Needs `config.export_import`. One JSON file with the data model, lookups, permission profiles and UI settings (no users, passwords or CIs). See [Customization](#customization-and-configuration-exportimport). |
| Audit log | `GET /audit-log` | Read-only, needs `audit.view`. Filters: `entityType`, `entityId`, `action`, `actorId`, `actorName`, `requestId`, `from`, `to`. Also records authentication events (`entityType=sessions`; actions `login.success`, `login.failure`, `login.locked`, `logout`, `session.revoke`) with the client `ipAddress` and `userAgent` in `newValue` (plus `peerIpAddress` when the TCP peer differs from the forwarded address); a failed sign-in has no actor id and stores only the attempted username. See [data model](data-model.md#auditing). |
| Setup | `GET /setup`, `POST /setup` | `setupRequired` is true while no user exists. `POST` creates the first user with the Administrator profile and signs them in; `409` once any user exists. |
| Authentication | `POST /auth/login`, `POST /auth/logout`, `GET /auth/me`, `PUT /auth/password` | `login` and `me` return `{ user, permissions, csrfToken }`. `permissions` is the union of the user's profiles: `administrator`, `global[]`, `allClasses` and per-class `classes[]`. Changing your own password needs `currentPassword` and ends your other sessions. |
| Users | `GET/POST /admin/users`, `GET/PATCH/DELETE /admin/users/{id}`, `PUT /admin/users/{id}/password` | Needs `users.manage`. `PATCH` renames, disables (`isActive: false`, which ends the user's sessions) and assigns profiles (`profileIds` replaces the set). `PUT …/password` sets a new password and ends the user's sessions. Filters: `q`, `isActive`, `profileId`. |
| API tokens | `GET/POST /admin/api-tokens`, `GET/DELETE /admin/api-tokens/{id}` | Needs `users.manage` and a session. `POST {name, profileId, expiresAt, userId?}` answers `201 { token, secret }`; the secret is in that response only. `DELETE` revokes (the token stays listed with `status: revoked`). Filters: `q`, `userId`, `status` (`active`, `expired`, `revoked`). See [API tokens](#api-tokens). |
| Permission profiles | `GET/POST /admin/profiles`, `GET/PATCH/DELETE /admin/profiles/{id}`, `POST /admin/profiles/{id}/clone` | Writes need `profiles.manage`; reading also works with `users.manage`. A profile is `{ name, description, globalPermissions[], classPermissions[] }`; `PATCH` replaces whichever list it sends. The built-in Administrator profile is read-only (`409`) and listed first. |
| Health | `GET /healthz`, `GET /readyz` | `/readyz` returns `503` when the database is unreachable or migrations are pending, and reports `migrations: { applied, expected, upToDate }`. |

## Authentication and permissions

**Signing in.** Users are local accounts with argon2id password hashes (at least 12 characters).
`POST /api/v1/auth/login {username, password}` (the username is case-insensitive) sets two cookies:

| Cookie | Attributes | Purpose |
| --- | --- | --- |
| `shadoucmdb_session` | `HttpOnly; SameSite=Lax; Path=/`, `Secure` behind HTTPS | 256-bit random token. The server stores only its SHA-256 in `sessions`. |
| `shadoucmdb_csrf` | `SameSite=Lax; Path=/`, `Secure` behind HTTPS, readable by the UI | The session's CSRF token (also `csrfToken` in the login and `/auth/me` responses). |

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
  service. Sign-out, `/auth/me`, the password change and token administration need a session and answer tokens with
  `403 FORBIDDEN`, so a token cannot mint a longer-lived token.
- Managing tokens needs `users.manage`. As for accounts, a non-administrator can only create or revoke tokens for
  users whose permissions they hold themselves (their own tokens are always revocable).
- Every request made with a known token, accepted or refused, writes a `token.use` audit row; creating and revoking
  write `create` and `update` rows (see [data model](data-model.md#auditing)).

**First run.** While there are no users, `GET /api/v1/setup` returns `{"setupRequired": true}` and
`POST /api/v1/setup` creates the first administrator and signs them in. `shadoucmdb create-admin` does the same
from the command line, and also works later to regain access (see [deployment](deployment.md#the-first-administrator)).

**Permission profiles.** There are no fixed roles. A profile is a named set of permissions, and a user can hold
several; their effective permissions are the union.

| Permission | Allows |
| --- | --- |
| `users.manage` | `/admin/users`: create, edit, disable, delete users, reset passwords, assign profiles |
| `profiles.manage` | `/admin/profiles`: create, edit, clone, delete profiles |
| `datamodel.manage` | Writes to CI classes, attribute definitions, relationship types and rules, statuses, environments, locations, owners and lookup lists; `/admin/templates` |
| `customization.manage` | Branding, navigation, dashboard and layouts (phase 3) |
| `config.export_import` | Configuration export and import (phase 3) |
| `audit.view` | `GET /audit-log` |
| Class `view` / `create` / `edit` / `delete` | CIs of one class, or of every class with the `classId: null` wildcard. Any write right implies `view`. |

- A class grant applies to exactly that class, not its subclasses; the wildcard also covers classes added later.
- Every signed-in user can read the data model and lookups: the UI needs them to render CIs.
- CI lists, search and the graph only contain CIs of classes the user may view (the graph does not traverse
  through hidden CIs). Reading or changing one CI of another class answers `403`.
- Relationships: reading needs `view` on both CIs' classes; creating, changing or removing one needs `edit` on
  the source CI's class and `view` on the target's.
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
| `text`, `enum` | string (`enum` must be one of `enumValues`; `text` honours `validation.pattern` / `maxLength`) |
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
| `dashboard.widgets[]` | Widgets in order: `count_by_class` (optional `classKeys`), `count_by_status`, `count_by_environment`, `recent_changes` (`limit`), `saved_search` (`search`: `classKeys`, `includeSubclasses`, `filters`, `sort`). `null` keeps the built-in dashboard. |
| `listViews[]` | Per class: `columns` (built-in fields such as `name`, `status`, `hostname`, or `attributes.<key>`), `defaultSort`, `defaultFilters` (`q`, `statusKeys`, `environmentKeys`, `locationKeys`), `pageSize`. |
| `layouts[]` | Per class: `panels[]` (`key`, `label`, ordered `fields`, `collapsed`), `hiddenFields`, `readOnlyFields`. Fields not placed in a panel follow in a trailing panel grouped by attribute group. `name` can be neither hidden nor read-only. |

- **References are keys.** Classes, attributes and lookups are named by key, so a document moves between installs.
  A reference to something that does not exist is accepted: `GET` returns the *effective* settings without it and
  lists it in `issues[]` (`unknown_class`, `unknown_attribute`, `unknown_status`, …, with a path into the stored
  document). A required attribute that a layout hides or makes read-only is flagged as
  `required_field_not_editable`. The stored document keeps every reference, so a class that comes back (e.g. from
  an import) reappears.
- **Versioned and audited.** `PUT` takes `{ version, settings, comment? }`: the version you loaded, or `409
  VERSION_CONFLICT`. Each save is a new version kept in history (`GET /ui-settings/versions`); `POST
  /ui-settings/versions/{n}/restore` saves version *n* again as the newest. An unchanged document is not saved
  again. Every save has an `audit_log` row (`entity_type = ui_settings`) with the old and new version.
- **Logo and favicon.** `PUT /ui-settings/assets/{logo|favicon}` with `{ contentType, data }` (base64). Logo: PNG,
  JPEG, WebP or SVG up to 512 KiB; favicon: PNG, ICO or SVG up to 128 KiB. The bytes must match the declared type;
  SVGs with scripts, event handlers, `javascript:` URLs or embedded HTML are refused. `GET` serves them without a
  session, with an ETag (`If-None-Match` answers 304), `nosniff` and a sandboxing Content-Security-Policy. The
  `url` in the settings carries a content hash (`?v=`). Uploads and removals are audited (`entity_type = ui_assets`).

**Export** (`GET /api/v1/admin/config/export`) downloads one file (`format: "shadoucmdb.config"`, `formatVersion:
1`) with `dataModel` (classes, attributes, relationship types and rules), `lookups` (statuses, environments,
locations, owners, lookup lists with their values), `permissionProfiles` (all but the built-in Administrator) and
`uiSettings` (the stored document plus the images, base64). It never contains users, passwords, sessions, CIs or
relationships. Parents come before children; every reference is a key (a lookup attribute's default is the value's
key).

**Import** (`POST /api/v1/admin/config/import?mode=dry_run|apply`, body: such a file, up to 16 MiB):

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
