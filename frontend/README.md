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
| `/` | Dashboard: total CIs, counts by class and by status (server-side counts), recently changed CIs, "+ New" per class; or the widgets chosen under Customization |
| `/cis` | Inventory: dense table with server-side search, filters (class, status, environment, owner, location, deleted), sortable columns and pagination. **All state is in the URL** (e.g. `/cis?classId=…&statusId=…&q=fra1&sort=-updatedAt&offset=50`), so views survive reload and can be bookmarked or shared. |
| `/cis/new?classId=…` | Create form |
| `/cis/:id` | Detail: general fields, class attributes (reference attributes are links), relationships (add/remove), relationship map (multi-hop graph), history (audit log with field diffs) |
| `/cis/:id/edit` | Edit form (sends `version` for optimistic locking and handles `409 VERSION_CONFLICT`) |
| `/search?q=…` | Global search results, ranked by the API with the field that matched. The header search box has type-ahead; press `/` to focus it. |
| `/login` | Sign-in. `?redirect=/cis?…` returns there afterwards (only same-app paths are followed). For users with two-factor authentication a second step asks for the authenticator code, or a recovery code. |
| `/account` | My account (the name in the header): two-factor authentication. Set up an authenticator app (password, then a QR code of `otpauthUri` rendered in the browser plus the setup key, then a code), see the 10 recovery codes once (copy or download as `.txt`), replace them, or turn two-factor off. |
| `/two-factor-setup` | Forced enrolment: while a profile the user holds requires two-factor authentication they have not set up (`/auth/me` `mfa.enrolmentRequired`), every other route leads here, without the app shell. `?redirect=` returns to the page they asked for afterwards. |
| `/setup` | First-run setup: creates the first administrator and signs them in. Shown only while `GET /setup` says no user exists. |
| `/admin` | Administration, with its own sub-navigation. Opens the first section the user may use. |
| `/admin/users` | Users: search, status and profile filters, sortable columns, paging (all in the URL). `/admin/users/new` creates one; `/admin/users/:id` edits it, assigns profiles, disables/enables it, resets the password or two-factor authentication, or deletes it. The list and the user page show whether two-factor is on. |
| `/admin/profiles` | Permission profiles: list, clone. `/admin/profiles/new` and `/admin/profiles/:id` edit the global permissions, the per-class view/create/edit/delete matrix and "Require two-factor authentication" (the only setting the built-in Administrator profile accepts); delete confirms and names the users who lose the profile. |
| `/admin/classes` | Data model › CI classes: the class tree in menu order. Drag a row (or use ↑/↓) to reorder among its siblings; archive/restore; `?archived=show` lists archived classes. |
| `/admin/classes/new`, `/admin/classes/:id` | Class editor: name, key (fixed after creation), parent, abstract, icon, colour; archive, delete (refused with the usage counts while anything refers to it). Below it, the **attribute editor**: every attribute defined on the class by form section, with drag-and-drop (or ↑/↓) ordering that also moves an attribute into another section; add/edit type, required, enum values, lookup list, reference class, validation, default value, help text and section; archive/restore/delete. Inherited attributes are listed read-only with a link to the class that defines them. |
| `/admin/relationships` | Relationship types (reorder, edit labels, archive, delete) and, for the selected type (`?type=…`), its rules: which source and target classes it may connect. |
| `/admin/lookups/:kind` | Lookups: `statuses` and `environments` (ordered, reorderable), `locations` and `owners` (searched, filtered, sorted and paged by the API; state in the URL), and `lists`, the administrator's own value lists (`?list=…`) with their ordered, coloured values. |
| `/admin/templates` | Starter templates: what each contains and how much already exists; one click installs the IT infrastructure starter (idempotent). On an empty install it explains that the CMDB has no data model yet. |
| `/admin/customization/:section` | Customization (`customization.manage`): `branding`, `navigation`, `dashboard`, `list-views`, `layouts` and `history`. See [Customization](#customization) below. The class a per-class section edits is in the URL (`?class=server`). |
| `/admin/config` | Export / import (`config.export_import`): download the configuration file; upload one to see the dry run (summary per section, every change with its old and new values, warnings), then apply it. |
| `/admin/audit` | Audit log: every change with the user who made it, filterable by actor, record type and action. `?actorId=…` shows one user's changes. |

### Forms are generated from the API

The CI form has two parts:

- The core fields every CI has: name, status, environment, owner, location, hostname, IP, serial and notes.
- The class attributes, rendered from `GET /ci-classes/{id}/attributes`. These include inherited
  definitions, grouped by `groupName` and ordered by `sortOrder`.

Each `dataType` maps to one input: `text`, `number`/`integer` (min/max), `boolean`, `enum`, `date`, `datetime`,
`ip`, `cidr`, `reference` (a type-ahead CI picker restricted to `referenceClassId`) and `lookup` (the active values
of the attribute's lookup list; the detail page shows the value's name and colour). A new CI starts from each
attribute's `defaultValue`, and `helpText` shows under the field. No field list is written per class, so a CI class
added under Administration (or through the API) gets a working form, detail view, sidebar entry and dashboard row
with no frontend change. Classes, statuses and the other lookups appear in the order the administrator set.

A fresh install has no classes. The dashboard, inventory, New CI page and sidebar then say so and send an
administrator (`datamodel.manage`) to Templates or the class editor; everyone else is told to ask one.

### Sign-in and permissions

- On load the UI asks `GET /auth/me`. Without a session it asks `GET /setup` and shows first-run setup or
  sign-in. The session is an HttpOnly cookie set by the API; the UI sends the session's CSRF token as
  `X-CSRF-Token` on every POST, PUT, PATCH and DELETE (`src/api/client.ts`).
- Any `401` to a signed-in request means the session ended (idle or absolute timeout, signed out elsewhere, account
  disabled). The UI goes to `/login?redirect=<current page>`, says the session ended, and returns there after
  sign-in. Signing in or out clears the query cache, so one user never sees another's data.
- Two-factor authentication: `POST /auth/login` answering `401 MFA_REQUIRED` switches the sign-in page to the
  code step (`POST /auth/login/mfa`); a challenge that expired or took too many wrong codes returns to the password
  form with the API's message. When `/auth/me` says `mfa.enrolmentRequired`, or any request answers
  `403 MFA_ENROLMENT_REQUIRED` (an administrator just made it mandatory), the UI re-reads the session and goes to
  `/two-factor-setup`. The TOTP secret and recovery codes are held only in the component that shows them, never in
  the query cache or the URL.
- The permissions from `/auth/me` hide actions the user cannot use: "+ New CI" and create links per class,
  Edit and Delete on a CI, adding and removing relationships, the History tab (needs `audit.view`), and the
  Administration sections (Users: `users.manage`; Permission profiles: `profiles.manage`, or read-only with
  `users.manage`; CI classes, Relationship types, Lookups and Templates: `datamodel.manage`; Audit log: `audit.view`). The API enforces every rule; the UI only avoids offering what it
  would refuse. The rules live in `src/lib/permissions.ts` and mirror the server's: class grants apply to exactly
  that class, the "all classes" row to every class, and create/edit/delete imply view.
- Administration sections are listed, grouped (Access, Data model, System), in `src/pages/admin/sections.ts`
  with their permission; the router uses the same list.

### Customization

One settings document (`GET /ui-settings`, see `docs/api.md`) applies to every user. The screens read the
*effective* settings; anything a section leaves out keeps the built-in behaviour, so `{}` is the stock UI.

- **Branding** (`src/stores/branding.ts`): app name, logo and favicon come from the public
  `GET /ui-settings/branding`, so the sign-in and setup pages are branded too. The primary and accent colours
  are set as the values of known CSS variables (`--c-primary`, `--c-accent`, …) after a `#rrggbb` check; links
  and focus rings are adjusted until they read on the background, and button text switches to dark on light
  colours. Nothing from the settings is ever injected as CSS or HTML. `defaultTheme` picks light, dark or the
  operating system's theme for users who have not chosen one in their user menu (kept in `localStorage`).
- **Navigation** (`src/components/MainNav.vue`): the menu follows `navigation.entries` (order, names, hidden
  entries, sections of classes). Pages and classes the settings do not mention follow in the built-in order, so a
  new class appears by itself. Pages a user may not open are never shown.
- **Dashboard** (`src/pages/dashboard/`): with `dashboard.widgets` set, the dashboard shows those widgets in
  order (counts by class, status or environment, recently changed CIs, saved searches); otherwise the built-in one.
- **List views** (`InventoryPage.vue`): per class, the columns (built-in fields or `attributes.<key>`), default sort
  and page size, and default filters. Default filters are written into the URL when the operator navigates to the
  class list without filters (menu, links); a reload or Back keeps the URL as it is, so a cleared filter stays
  cleared. Attribute columns read the values the list API returns with each CI.
- **Detail and form layout** (`detail/LayoutPanels.vue`, `form/CiForm.vue`, `lib/uiSettings.ts`
  `resolveLayout`): per class, tabs of sections on a 12-column grid (`sectionClass`: a section spans 1–12, so
  sections sit side by side), each section a grid of 1–12 columns whose fields span some of them (collapsed
  sections start closed on the detail page), hidden fields, and fields read-only on the form. Fields no section
  places follow at the end of the first tab, in a General section and their attribute groups. The grids answer to
  the width of their container (CSS container queries): sections stack below 820 px of tab width, and a
  section's field grid narrows to two columns and then one with the section's own width, on small screens and
  in the designers' previews alike. The form keeps every tab in the page, shows the tab of
  the first missing or rejected field and counts errors per tab. Required fields stay editable on a new CI
  whatever the layout says, or it could not be saved.

The editor (`src/pages/admin/customization/`) works on the *stored* document of the current version, which keeps
references to classes that do not exist right now, and saves the whole document with the version it loaded
(`409 VERSION_CONFLICT` if someone saved in between). Branding and navigation preview live in the real header and
menu while the editor is open; the dashboard and list view sections preview inline. The layout section is a
visual form designer (`admin/customization/LayoutsSection.vue`, `designer/`, edits in `lib/layoutDesign.ts`):
the canvas is the class's form with every field drawn as it will appear; fields are dragged between sections and
tabs (or onto a tab), resized by dragging their right edge; sections are resized on the tab's 12 columns by
their edges and placed side by side by their grip (`components/layoutEdit/SectionShell.vue`, shared with the
in-page layout editor), and the preview frame is resized by its grip (`PreviewResizeHandle.vue`), with laptop,
tablet and phone widths as shortcuts. Everything has a keyboard path: Enter selects a field, Alt+↑/↓ moves it,
Alt+←/→ changes its width, Delete hides it, and the properties bar offers the same as buttons and selects,
announced to screen readers. Core fields (ident, valid from, valid until) can be moved but not hidden. Leaving
Customization or reloading with unsaved changes asks first. The issues the API
reports (unknown classes, a required attribute hidden by a layout) are listed above the sections.

### Errors, states and navigation

- API validation errors (`details[].field`, e.g. `hostname` or `attributes.cpu_cores`) render next to their field.
- Every list and panel has designed loading, empty and error states. An unreachable API names the URL it
  tried, and an empty inventory offers "Create your first configuration item".
- Deleting a CI opens a confirmation that lists every relationship that will break. Removing a relationship
  also confirms.
- Deleting a class, attribute, relationship type or rule, or a lookup value first asks the API what refers to it
  (`GET …/{id}/usage`) and lists it. A row still in use cannot be deleted: the dialog offers to archive it instead.
- Related CIs are links. Walking from CI to CI builds a trail in the breadcrumb, for example
  `Inventory › CRM › crm-app-01 › fra1-esx-01 › FRA1 Rack A01`.

## Code layout

```
src/config.ts          the only place deploy-time config is read
src/api/schema.d.ts    types generated from backend/openapi.json (do not edit)
src/api/client.ts      the one HTTP client (openapi-fetch) + ApiError / error-envelope handling
src/api/queries.ts     TanStack Query composables and cache keys; components never call fetch
src/api/admin.ts       the same for sign-in, users, permission profiles and the audit log
src/api/datamodel.ts   the same for classes, attributes, relationship types/rules, lookups, lists and templates
src/api/uiSettings.ts  the same for UI settings, their versions and images, and configuration export/import
src/lib/uiSettings.ts  how the settings document is applied: menu merge, list columns, layout tabs and grid
src/lib/layoutDesign.ts the form designer's edits on a class layout (move, resize, tabs, sections, side by side)
src/lib/permissions.ts permission checks mirrored from the server
src/router.ts          routes (Vue Router, HTML5 history) and the setup → sign-in → app guard
src/stores/            Pinia stores (the session, branding and theme, the one-shot "Created …/Saved …" notice)
src/components/        shell, breadcrumbs, global search, pickers, dialogs, state views
src/pages/             one component per screen; detail/, form/ and dashboard/ hold their parts;
                       admin/datamodel/ and admin/lookups/ hold the data model editors,
                       admin/customization/ and admin/config/ the customization and export/import screens
src/lib/               formatting, attribute value conversion, the breadcrumb walk trail, drag-and-drop
                       reordering (reorder.ts), class trees, class icons
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
use it without a frontend change, and check the empty, not-found and API-unreachable states. The auth and
admin specs sign in and out, follow an expired session to sign-in and back, walk first-run setup, build a
permission profile in the matrix, clone and delete one, create a user holding it, sign in as that user to check
which actions are hidden, disable/enable the account, reset its password, and read who did it in the audit log.
`mfa.spec.ts` computes TOTP codes like an authenticator app: it sets one up from My account (QR code, setup key,
a wrong code, recovery codes with download), signs in with a code and with a recovery code, replaces the codes and
turns two-factor off; then it requires two-factor on a profile, follows a holder through forced enrolment, and
resets their two-factor from user management.
The data model spec builds a lookup list and a class with attributes in the editors (drag and arrow reordering,
moving between sections, an API error at its field), creates a CI from the generated form, archives the class,
adds a relationship type and rule, checks the lookup delete guards, and the fresh-install guidance. The
customization spec sets the app name, a colour, the dark default theme and a logo (and checks them on the sign-in
page), builds a menu section, dashboard widgets with a saved search, a class list view with a default filter and a
layout with hidden and read-only fields, restores an earlier version, and runs an export, a dry run with its diff,
rejected files and an applied import. It starts from and returns to the built-in settings.

`permissions.spec.ts` checks the same rules where they are enforced, at the API: a user with a restricted profile
gets `403 FORBIDDEN` from every administration and data-model endpoint and from class operations the profile
lacks (and the administrator reads back that nothing changed), anonymous calls get `401`, writes without the CSRF
token are refused, signing out, disabling the account or resetting its password ends the session on the server,
and repeated wrong passwords lock the username with `429`.
Any page error or Vue warning fails the test.

`a11y.spec.ts` runs axe-core (WCAG 2.1 A and AA rules) on sign-in, the inventory, a CI detail page with its delete
dialog and edit form, the class and attribute editor, users and profiles, My account and the two-factor enrolment
step. Critical and serious violations fail it; the rest is reported. What it covers and what it does not is in
[docs/accessibility.md](../docs/accessibility.md).

`first-run.spec.ts` walks the real first-run setup, nothing mocked, and only runs when `E2E_FRESH_BASE_URL` points
at a second API whose database is migrated but has no user. It creates the first administrator there, so give it
a newly migrated database for each run (CI starts one on port 3001).

`fresh-install.spec.ts` builds a CMDB up from a bare install and moves it to another one, nothing mocked. It runs
when `E2E_BARE_BASE_URL` and `E2E_IMPORT_BASE_URL` point at two more APIs, each on its own database that is
migrated and `seed`ed (system rows only) with no user; global setup creates the administrator `fresh-admin` on
both. On the bare install it checks that nothing but system rows exist, installs the IT infrastructure starter
template from the empty state (and checks that installing again adds nothing), builds a lookup list and a class
with a required number and a lookup attribute in the editors, creates two CIs, and reads them back in a list view,
the search and the API. It then exports the whole setup (template, own class, lookup list, profile, branding,
menu, dashboard, list view and layout) and imports it into the second install: dry run first (nothing written),
then apply. Exporting the second install again must give the same file, no CIs or users may have moved, and the
imported class, lookup list, menu, dashboard, list view, form layout and profile must work there. Give both
newly created databases for each run (CI starts them on ports 3002 and 3003).

`area-tables.spec.ts` checks that the data model is real PostgreSQL, by querying the database itself with `psql`
(connection from the `PG*` environment variables, as the owner of the app's databases). It runs when
`E2E_AREAS_BASE_URL` and `E2E_AREAS_IMPORT_BASE_URL` point at two more bare APIs like the ones above, and
`E2E_AREAS_PGDATABASE` / `E2E_AREAS_IMPORT_PGDATABASE` name their databases. In the UI it creates the area
“Bestand” and the types “Netzwerk” and “Virtuelle Maschinen” with typed fields, then asserts the schema
`bestand`, the tables `bestand.netzwerk` and `bestand.virtuelle_maschinen` column by column (types, the id's
`ON DELETE CASCADE` key to `cmdb.configuration_items`), the reporting views `bestand.v_*` and the
`cmdb.schema_changes` history. Assets created in the UI must be rows of the type tables and views. It converts a
field's type (text to whole number succeeds; a value that is no number refuses the other), is refused making a
field required while an asset has no value, archives a field (the column stays) and purges it, typed to confirm
(the column is gone). A user whose profile lacks `datamodel.manage` gets `403` from every one of these endpoints,
and the database is unchanged. Finally the export is imported into the second install: the dry run shows the DDL
and creates nothing, and applying it builds the same tables and views. Give both newly created databases for each
run (CI starts them on ports 3004 and 3005).

The tests expect the demo inventory (`shadoucmdb seed --demo`) and create their own uniquely named records.
They run signed in: `e2e/global-setup.ts` completes first-run setup on a database without users, or signs in as
`E2E_USERNAME` / `E2E_PASSWORD` (an Administrator account, e.g. from `shadoucmdb create-admin`).

```sh
npx playwright install chromium                                           # once
export E2E_USERNAME=admin E2E_PASSWORD=...                                # unless the database has no users yet
API_PROXY_TARGET=http://<api-host>:3000 npm run test:e2e -w frontend      # starts a dev server on :5199
E2E_BASE_URL=http://localhost:4173 npm run test:e2e -w frontend           # or test an already-served build
E2E_FRESH_BASE_URL=http://localhost:3001 ...                              # also run first-run.spec.ts
E2E_BARE_BASE_URL=http://localhost:3002 E2E_IMPORT_BASE_URL=http://localhost:3003 ...  # also run fresh-install.spec.ts
E2E_AREAS_BASE_URL=http://localhost:3004 E2E_AREAS_PGDATABASE=... E2E_AREAS_IMPORT_BASE_URL=http://localhost:3005 E2E_AREAS_IMPORT_PGDATABASE=... ...  # also run area-tables.spec.ts
```

Set `E2E_SCREENSHOT_DIR=<dir>` to save a screenshot of each step.
