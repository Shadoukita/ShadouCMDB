# ShadouCMDB API

The backend is the only database client. Everything the UI needs goes through this API.

- **Contract:** [`backend/openapi.json`](../backend/openapi.json) (OpenAPI 3.1). It is generated from the code
  (utoipa): the same route table builds the router and the document, and requests are validated against the
  same JSON Schemas the document publishes. The running server also serves it at `GET /openapi.json`, with a
  browsable UI (Swagger UI, bundled in the binary) at `GET /docs`.
- **Base path:** `/api/v1`. The health probes `/healthz` and `/readyz` sit at the root.
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
| Audit | Every write adds an `audit_log` row in the same transaction. `X-Actor-Name` sets the actor label (it is unauthenticated until auth exists). `X-Request-Id` is echoed back and stored. |
| CORS | Off by default. Set `CORS_ORIGINS` when the UI is served from another origin. |

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
| 404 | `NOT_FOUND` | The id does not exist, or the route does not exist. |
| 409 | `CONFLICT` | A duplicate (unique key or live edge), or a write to a soft-deleted CI or relationship. |
| 409 | `IN_USE` | A hard delete of a row that is still referenced. Retire it with `PATCH {"isActive": false}` instead. |
| 409 | `VERSION_CONFLICT` | A stale `version` on a CI `PATCH`. |
| 413 / 415 | `PAYLOAD_TOO_LARGE` / `UNSUPPORTED_MEDIA_TYPE` | The body is over 1 MiB, or is not JSON. |
| 503 | `DATABASE_UNAVAILABLE` | PostgreSQL is unreachable. |
| 500 | `INTERNAL_ERROR` | A bug. The message is generic and the log carries `requestId`. |

## Endpoints

| Resource | Endpoints | Notes |
| --- | --- | --- |
| Configuration items | `GET/POST /configuration-items`, `GET/PATCH/DELETE /configuration-items/{id}` | Filters: `classId` (includes subclasses unless `includeSubclasses=false`), `statusId`, `environmentId`, `ownerId`, `locationId`, `ipWithin` (CIDR), `deleted=exclude\|include\|only`. List items are summaries with embedded `class`, `status`, `environment`, `owner` and `location`. The detail view adds `attributes` (a key → value map) and `attributeReferences`. DELETE is a soft delete and also soft-deletes the CI's relationships. |
| Graph | `GET /configuration-items/{id}/graph?depth=1..6&direction=both\|outgoing\|incoming&relationshipTypeId=&maxNodes=` | Returns `{ nodes[], edges[], truncated }` in one call. `nodes[].depth` is the number of hops from the root. Each edge embeds its type and labels. |
| Search | `GET /search?q=` | Results are ranked, each with `matches[]` naming the field that hit (`hostname`, `attributes.url`, …). It takes the same filters as the CI list. |
| Relationships | `GET/POST /relationships`, `GET/PATCH/DELETE /relationships/{id}` | Filters: `ciId` (either end), `sourceCiId`, `targetCiId`, `relationshipTypeId`, `deleted`. Each edge embeds `type`, `source` and `target`. PATCH changes only `notes` or `relationshipTypeId`. DELETE is a soft delete. |
| CI classes | `GET/POST /ci-classes`, `GET/PATCH/DELETE /ci-classes/{id}`, `GET /ci-classes/{id}/attributes` | `/attributes` returns every attribute a CI of this class can carry, inherited ones included, so the UI can render the CI form from it. Filters: `parentId` (`none` for roots), `descendantOf`, `isAbstract`, `isActive`. |
| Attribute definitions | `GET/POST /attribute-definitions`, `GET/PATCH/DELETE /attribute-definitions/{id}` | Filters: `classId`, `effectiveForClassId`, `dataType`. `classId`, `key` and `dataType` cannot change after creation. |
| Relationship types | `GET/POST /relationship-types`, `GET/PATCH/DELETE /relationship-types/{id}` | `?sourceClassId=&targetClassId=` returns only the types legal between two classes, which is what the "add relationship" picker needs. |
| Relationship rules | `GET/POST /relationship-rules`, `GET/PATCH/DELETE /relationship-rules/{id}` | Define which classes each type may connect. A rule also covers the subclasses of its classes. |
| Statuses, environments, locations, owners | `GET/POST /{statuses\|environments\|locations\|owners}`, `GET/PATCH/DELETE /…/{id}` | Filters include `isActive`, `isOperational` (statuses), `parentId` and `locationType` (locations), and `kind` (owners). |
| Audit log | `GET /audit-log` | Read-only. Filters: `entityType`, `entityId`, `action`, `actorName`, `requestId`, `from`, `to`. |
| Health | `GET /healthz`, `GET /readyz` | `/readyz` returns `503` when the database is unreachable or migrations are pending, and reports `migrations: { applied, expected, upToDate }`. |

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

In `PATCH`, `attributes` is merged into the stored values, and `null` clears a value. Required attributes are
checked against the final state. An error on one value is reported at `attributes.<key>`.

## Layers and extension seams

The server is the Rust binary `shadoucmdb` (Axum + Tokio + sqlx) in `backend/`:

```
src/http/*      transport: middleware (request id, CORS, body limit), error envelope, fallback, shutdown
src/api/*       route table, request validation (schema-driven), OpenAPI document, actor context, PG error mapping
src/modules/*   routes and services (rules, transactions, audit) per resource
src/data/*      SQL only: compile-time checked sqlx queries (offline data in backend/.sqlx) and QueryBuilder lists
```

- **Authentication and RBAC:** `AppState.actors` in `src/http/mod.rs` holds an `ActorResolver`
  (`src/api/context.rs`). Replace `AnonymousActorResolver` with one that verifies a token and returns the user,
  or rejects the request. Services already receive the actor, and audit rows already record it.
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
covering the success paths and the error paths. It fails on any unexpected status, any 5xx, any spec operation it
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
- **`X-Actor-Name`** must be visible ASCII to be recorded; other values are ignored.
- JSON key order inside objects can differ (JSON objects are unordered).
