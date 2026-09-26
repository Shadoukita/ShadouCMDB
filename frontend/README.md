# frontend

The ShadouCMDB web UI: Vue 3 + Vite + TypeScript, with Vue Router, Pinia and TanStack Query for Vue.

It talks **only** to the backend API (`/api/v1`). It never opens a database connection, never embeds
SQL and never receives database credentials. There is nothing to configure here except where the API is.

## Run it

```sh
npm ci                                   # from the repo root
cp frontend/.env.example frontend/.env   # optional
# point the dev server at a running backend (proxied, so no CORS setup is needed):
API_PROXY_TARGET=http://<api-host>:3000 npm run dev -w frontend      # http://localhost:5173
```

Production build: `npm run build -w frontend` type-checks (`vue-tsc`) and writes static files to `frontend/dist/`.
Serve them from any static web server with an SPA fallback (unknown paths → `index.html`), or embed them in the
backend binary. With no configuration the UI calls the API on its own origin at `/api/v1`.

## Configuration

The API base URL is the only setting. It is never hardcoded. It resolves in this order:

| Where | When it applies | Example |
| --- | --- | --- |
| `dist/config.js` → `window.__SHADOUCMDB_CONFIG__.apiBaseUrl` | At runtime. Overwrite the file at deploy time (for example from a container entrypoint) to repoint a built UI without rebuilding. | `window.__SHADOUCMDB_CONFIG__ = { apiBaseUrl: "https://cmdb-api.example.com" };` |
| `VITE_API_BASE_URL` | At build time | `VITE_API_BASE_URL=https://cmdb-api.example.com npm run build -w frontend` |
| _(neither set)_ | Same origin as the UI. Use this behind a reverse proxy that routes `/api` to the backend, or with `API_PROXY_TARGET` in dev and preview. | |

When the UI and API are on different origins, add the UI origin to the backend's `CORS_ORIGINS`.
`API_PROXY_TARGET` and `WEB_PORT` only affect `npm run dev` and `npm run preview`. All variables are listed in [`.env.example`](.env.example).

## Screens

| Route | Screen |
| --- | --- |
| `/` | Dashboard: total CIs, counts by class and by status (server-side counts), recently changed CIs, "+ New" per class |
| `/cis` | Inventory: dense table with server-side search, filters (class, status, environment, owner, location, deleted), sortable columns and pagination. **All state is in the URL** (e.g. `/cis?classId=…&statusId=…&q=fra1&sort=-updatedAt&offset=50`), so views survive reload and can be bookmarked or shared. |
| `/cis/new?classId=…` | Create form |
| `/cis/:id` | Detail: general fields, class attributes (reference attributes are links), relationships (add/remove), relationship map (multi-hop graph), history (audit log with field diffs) |
| `/cis/:id/edit` | Edit form (sends `version` for optimistic locking and handles `409 VERSION_CONFLICT`) |
| `/search?q=…` | Global search results, ranked by the API with the field that matched. The header search box has type-ahead; press `/` to focus it. |

### Forms are generated from the API

The CI form has two parts:

- The core fields every CI has: name, status, environment, owner, location, hostname, IP, serial and notes.
- The class attributes, rendered from `GET /ci-classes/{id}/attributes`. These include inherited
  definitions, grouped by `groupName` and ordered by `sortOrder`.

Each `dataType` maps to one input: `text`, `number`/`integer` (min/max), `boolean`, `enum`, `date`, `datetime`,
`ip`, `cidr`, and `reference` (a type-ahead CI picker restricted to `referenceClassId`). No field list is written
per class, so a CI class added through the API gets a working form, detail view, sidebar entry and dashboard
row with no frontend change.

### Errors, states and navigation

- API validation errors (`details[].field`, e.g. `hostname` or `attributes.cpu_cores`) render next to their field.
- Every list and panel has designed loading, empty and error states. An unreachable API names the URL it
  tried, and an empty inventory offers "Create your first configuration item".
- Deleting a CI opens a confirmation that lists every relationship that will break. Removing a relationship
  also confirms.
- Related CIs are links. Walking from CI to CI builds a trail in the breadcrumb, for example
  `Inventory › CRM › crm-app-01 › fra1-esx-01 › FRA1 Rack A01`.

## Code layout

```
src/config.ts          the only place deploy-time config is read
src/api/schema.d.ts    types generated from backend/openapi.json (do not edit)
src/api/client.ts      the one HTTP client (openapi-fetch) + ApiError / error-envelope handling
src/api/queries.ts     TanStack Query composables and cache keys; components never call fetch
src/router.ts          routes (Vue Router, HTML5 history)
src/stores/            Pinia stores (the one-shot "Created …/Saved …" notice)
src/components/        shell, breadcrumbs, global search, pickers, dialogs, state views
src/pages/             one component per screen; detail/, form/ and dashboard/ hold their parts
src/lib/               formatting, attribute value conversion, the breadcrumb walk trail
src/styles/tokens.css  design tokens (spacing, type scale, colours); app.css uses only these
e2e/                   Playwright end-to-end walk (see below)
```

After an API change, regenerate the types with `npm run api:types -w frontend`. This reads
`../backend/openapi.json`, so the backend spec must be present. `npm run api:check -w frontend` fails if the
committed types are stale. `npm run typecheck` then flags every call site that no longer matches the contract.

## End-to-end tests

`e2e/` is a Playwright walk of every screen against a real API: create a CI with field-level validation, find
it through URL-backed filters (and after a reload), edit it and read the History diff, provoke a
`409 VERSION_CONFLICT`, add relationships in both directions, walk `CRM › crm-app-01 › fra1-esx-01 › FRA1 Rack A01`
by clicking, delete with the "relationships that will break" confirmation, create a CI class through the API and
use it without a frontend change, and check the empty, not-found and API-unreachable states. Any page error or Vue
warning fails the test.

The tests expect the demo inventory (`shadoucmdb seed --demo`) and create their own uniquely named records.

```sh
npx playwright install chromium                                           # once
API_PROXY_TARGET=http://<api-host>:3000 npm run test:e2e -w frontend      # starts a dev server on :5199
E2E_BASE_URL=http://localhost:4173 npm run test:e2e -w frontend           # or test an already-served build
```

Set `E2E_SCREENSHOT_DIR=<dir>` to save a screenshot of each step.
