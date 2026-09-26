# ShadouCMDB API

The backend is the only database client. Everything the UI needs goes through this API.

- **Contract:** [`backend/openapi.json`](../backend/openapi.json) (OpenAPI 3.1). It is generated from the
  same zod schemas that validate requests. The running server also serves it at `GET /openapi.json`,
  with a browsable UI at `GET /docs`.
- **Base path:** `/api/v1`. The health probes `/healthz` and `/readyz` sit at the root.
- **Regenerate the contract** after changing a route: `npm run openapi -w backend`.
  `npm run openapi -w backend -- --check` fails if the committed file is stale.

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

```
src/modules/*   routes (zod schemas + handlers) and services (rules, transactions, audit)
src/data/*      SQL / Drizzle queries only
src/http/*      route registry, validation, error envelope, OpenAPI generation, request context
```

- **Authentication and RBAC:** `buildApp({ resolveActor })` in `src/app.ts`. Swap the resolver to verify a
  token. Services already receive the actor, and audit rows already record it.
- **Discovery and imports:** call the services with an actor of type `import`. Validation and audit then apply
  exactly as they do for the UI.
- **Integrations and reporting:** add a module under `src/modules/` and append its routes in `buildRoutes()`. They
  show up in the spec automatically.

## Smoke test

`API_URL=http://localhost:3000 npm run smoke -w backend` runs every operation in the spec against a running API,
covering the success paths and the error paths. It fails on any unexpected status, any 5xx, or any spec operation it
did not call. Run the API with `NODE_ENV=development` so each response is also checked against its schema.
