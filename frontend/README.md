# frontend

The ShadouCMDB web UI: Vue 3 + Vite + TypeScript, with Vue Router, Pinia and TanStack Query for Vue.

It talks **only** to the backend API (`/api/v1`). It never opens a database connection, never embeds
SQL and never receives database credentials. There is nothing to configure here except where the API is.

## Run it

Node.js 22.18 or newer is required (the repo root `.nvmrc` pins `22`).

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
| `/` | Dashboard: total CIs, counts by class, counts by status (only if a lookup list with the key `status` exists; server-side counts), recently changed CIs, and a `New <class name>` link per class; or the widgets chosen under Customization |
| `/cis` | Inventory: dense table with server-side search, filters, sortable columns and pagination. The filters are class, lookup values (`lookupValueId`, set by links such as the dashboard counts and shown as a removable chip), IP within (`ipWithin`, likewise a chip), validity (active only, show inactive, only inactive) and deleted CIs (hide, include, only deleted). The **Columns** popover chooses and reorders the columns (built-in fields and, when one class is selected, its attributes). **All state is in the URL** (e.g. `/cis?classId=…&lookupValueId=…&q=fra1&sort=-updatedAt&columns=label,ident&offset=50`), so views survive reload and can be bookmarked or shared. |
| `/cis/new?classId=…` | Create form |
| `/cis/:id` | Detail: one tab per tab of the class layout (core fields and class attributes; reference attributes are links), then a "Relationship map" tab (multi-hop graph) and, with `audit.view` and unless the layout places it, a "History" tab (audit log with field diffs). Relationships can be added and removed. Holders of `customization.manage` also see "Edit layout". The fields open as inputs: once something was changed a bar offers **Save** and **Discard**; a save sends only the changed fields with the `version` the page loaded (a `409 VERSION_CONFLICT` offers to load the current version), and leaving the CI with unsaved changes asks first. Without the edit right on the class, on a deleted CI, and for read-only or managed fields (a Person's Email linked to a sign-in account) the values are shown read-only in the same place. |
| `/cis/:id/edit` | Edit form, used by the business service page (sends `version` for optimistic locking and handles `409 VERSION_CONFLICT`) |
| `/cis/new/layout-editor`, `/cis/:id/layout-editor`, `/cis/:id/edit/layout-editor` | The in-page layout editor, opened in its own window from "Edit layout" on the pages above. Needs `customization.manage`; without it the route shows the plain page. |
| `/search?q=…` | Global search results, ranked by the API with the field that matched. The header search box has type-ahead; press `/` to focus it. |
| `/login` | Sign-in. `?redirect=/cis?…` returns there afterwards (only same-app paths are followed). Each enabled OpenID Connect identity provider adds an "Enterprise sign-in" button; a failed attempt comes back as `?ssoError=<code>` with a message. For users with two-factor authentication a second step asks for the authenticator code, or a recovery code. |
| `/account` | My account (the name in the header): change your own password, and two-factor authentication. Set up an authenticator app (password, then a QR code of `otpauthUri` rendered in the browser plus the setup key, then a code), see the 10 recovery codes once (copy or download as `.txt`), replace them, or turn two-factor off. |
| `/two-factor-setup` | Forced enrolment: while a profile the user holds requires two-factor authentication they have not set up (`/auth/me` `mfa.enrolmentRequired`), every other route leads here, without the app shell. `?redirect=` returns to the page they asked for afterwards. Without a pending enrolment the route redirects to `/account`. |
| `/setup` | First-run setup: creates the first administrator and signs them in. It asks for the one-time setup token the server wrote to its log and setup token file at start-up. Shown only while `GET /setup` says no user exists. |
| `/admin` | Administration, with its own sub-navigation. Opens the first section the user may use. |
| `/admin/users` | Users: search, status and profile filters, sortable columns, paging (all in the URL). `/admin/users/new` creates one; `/admin/users/:id` edits it, assigns profiles, disables/enables it, resets the password or two-factor authentication, or deletes it. The list and the user page show whether two-factor is on. |
| `/admin/profiles` | Permission profiles: list, clone. `/admin/profiles/new` and `/admin/profiles/:id` edit the global permissions, the per-class view/create/edit/delete matrix and "Require two-factor authentication" (the only setting the built-in Administrator profile accepts); delete confirms and names the users who lose the profile. |
| `/admin/api-tokens` | API tokens (`users.manage`): search, filters, sort and paging in the URL; create a token (its secret is shown once, in the create dialog). |
| `/admin/identity-providers` | Identity providers (Administrator profile only): OpenID Connect providers and LDAP / Active Directory directories. `/admin/identity-providers/new` and `/admin/identity-providers/:id` edit one, with write-only secrets and group mappings to permission profiles. |
| `/admin/areas` | Data model › Areas (`datamodel.manage`): an area is a menu tab and a database schema holding the tables of its classes. Reorder; delete archives (schema and data are kept), and only a purge, typed to confirm, drops the schema. |
| `/admin/classes` | Data model › CI classes: the class tree in menu order. Drag a row (or use ↑/↓) to reorder among its siblings; archive/restore. "Show archived classes" (`?archived=show`) lists archived classes and the Area filter (`?areaId=…`) narrows the tree to one area. |
| `/admin/classes/new`, `/admin/classes/:id` | Class editor: name, key and area (both fixed after creation), description, parent, abstract, icon, colour and title attribute; archive/restore, and purge for an archived class (drops its table and CIs). Every change is previewed as the DDL it will run before it is applied. Below it, the **attribute editor**: every attribute defined on the class by form section, with drag-and-drop (or ↑/↓) ordering that also moves an attribute into another section; add/edit type, required, enum values, lookup list, reference class, validation, default value, help text and section; archive/restore, and purge for an archived attribute (drops its column), also previewed as DDL. Inherited attributes are listed read-only with a link to the class that defines them. |
| `/admin/relationships` | Relationship types (reorder, edit labels, archive, delete) and, for the selected type (`?type=…`), its rules: which source and target classes it may connect. |
| `/admin/lookups`, `/admin/lookups/:kind` | Former Lookups section; redirects to `/admin/dropdowns` (`/admin/lookups/lists?list=…` keeps its list). Status, environment, location and owner are the lookup lists of the same name. |
| `/admin/dropdowns` | Data model › Dropdowns (`datamodel.manage`): the administrator's own lookup lists, used by lookup attributes (and the place where status, environment, owner and location values are edited), with their ordered, coloured values. The selected list is `?list=…`. A list can depend on a parent list: each of its values then names a parent value, forms offer only the values of the chosen parent, and `?parent=…` filters the values. |
| `/admin/templates` | Starter templates: what each contains and how much already exists; one click installs the IT infrastructure starter (idempotent). On an empty install it explains that the CMDB has no data model yet. |
| `/admin/customization/:section` | Customization (`customization.manage`): `branding`, `navigation`, `dashboard`, `list-views`, `layouts` (detail and form layout) and `history` (the saved versions, with restore). `/admin/customization` opens `branding`. See [Customization](#customization) below. The class a per-class section edits is in the URL (`?class=server`). |
| `/admin/config` | Export / import (`config.export_import`): download the configuration file; upload one to see the dry run (summary per section, every change with its old and new values, warnings), then apply it. |
| `/admin/audit` | Audit log: every change with the user who made it, filterable by actor, record type and action. `?actorId=…` shows one user's changes. |
| any other path | Not-found page. |

### Forms are generated from the API

The CI form has two parts:

- The core fields every CI has: ident, valid from and valid until. Only administrators set the ident (others get a generated one); valid from is required and defaults to now.
- The class attributes, rendered from `GET /ci-classes/{id}/attributes`. These include inherited
  definitions, grouped by `groupName` and ordered by `sortOrder`. Everything else, name and status included,
  is a class attribute.

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
  Administration sections (Users and API tokens: `users.manage`; Permission profiles: `profiles.manage`, or read-only with
  `users.manage`; Identity providers: the Administrator profile only; Areas, CI classes, Relationship types, Dropdowns,
  Lookups and Templates: `datamodel.manage`; Customization: `customization.manage`; Export / import:
  `config.export_import`; Audit log: `audit.view`), and "Edit layout" (`customization.manage`). The API enforces every rule; the UI only avoids offering what it
  would refuse. The rules live in `src/lib/permissions.ts` and mirror the server's: class grants apply to exactly
  that class, the "all classes" row to every class, and create/edit/delete imply view.
- Administration sections are listed, grouped (Access, Data model, System), in `src/pages/admin/sections.ts`
  with their permission; the router uses the same list.

### Customization

One settings document (`GET /ui-settings`) applies to every user. The screens read the
*effective* settings; anything a section leaves out keeps the built-in behaviour, so `{}` is the stock UI.

- **Branding** (`src/stores/branding.ts`): app name, logo and favicon come from the public
  `GET /ui-settings/branding`, so the sign-in and setup pages are branded too. The primary and accent colours
  are set as the values of known CSS variables (`--c-primary`, `--c-accent`, …) after a `#rrggbb` check; links
  and focus rings are adjusted until they read on the background, and button text switches to dark on light
  colours. Nothing from the settings is ever injected as CSS or HTML. `defaultTheme` picks light, dark or the
  operating system's theme for users who have not chosen one in their user menu (kept in `localStorage`).
- **Navigation** (`src/components/MainNav.vue`): the menu follows `navigation.entries` (order, names, hidden
  entries, sections of classes). Pages and classes the settings do not mention follow in the built-in order, so a
  new class appears by itself. Pages a user may not open are never shown. Classes sit under collapsible tabs, one per
  area (the folded tabs are remembered in `localStorage`), and below 820 px the sidebar becomes a drawer. The user
  menu has a Theme selector.
- **Dashboard** (`src/pages/dashboard/`): with `dashboard.widgets` set, the dashboard shows those widgets in
  order (counts by class, counts by the values of a lookup list, recently changed CIs, saved searches); otherwise the built-in one.
- **List views** (`InventoryPage.vue`): per class, the columns (built-in fields or `attributes.<key>`), default sort
  and page size, and default filters. Default filters are written into the URL when the operator navigates to the
  class list without filters (menu, links); a reload or Back keeps the URL as it is, so a cleared filter stays
  cleared. Attribute columns read the values the list API returns with each CI.
- **Detail and form layout** (`detail/LayoutPanels.vue`, `form/CiForm.vue`, `lib/uiSettings.ts`
  `resolveLayout`): a layout template (`lib/layoutTemplates.ts`): each class's default, or the CI's own layout
  or another template chosen for it (`GET /configuration-items/{id}/layout`, which the detail page and the edit
  form read; the create form uses the class's default). A layout is tabs of sections, each section a window with its own position and size
  (`lib/freeLayout.ts`; windows may overlap) and a grid of 1–12 columns whose fields span some of them
  (collapsed sections start closed on the detail page), hidden fields, and fields read-only on the form. Fields
  no section places follow below the windows of the first tab, in a General section and their attribute groups.
  A tab saved on the earlier 12-column grid (or sent that way, e.g. by an older export) is shown as windows where
  its sections were on the grid (`makeFree`, the same estimate the API stores). The grids answer to the width of
  their container (CSS container queries): windows stack in reading order below 820 px of tab width, and a
  section's field grid narrows to two columns and then one with the section's own width. The form keeps every
  tab in the page, shows the tab of the first missing or rejected field and counts errors per tab. Required
  fields stay editable on a new CI whatever the layout says, or it could not be saved.

The editor (`src/pages/admin/customization/`) works on the *stored* document of the current version, which keeps
references to classes that do not exist right now, and saves the whole document with the version it loaded
(`409 VERSION_CONFLICT` if someone saved in between). Its History section lists every saved version and restores
one by saving it again as the newest. Branding and navigation preview live in the real header and
menu while the editor is open; the dashboard and list view sections preview inline. The layout section
(`admin/customization/LayoutsSection.vue`) lists every class with its default template (an inline select in the
draft; search, template filter and sort in the URL, paged in the browser: classes are metadata, not inventory)
and every template with who uses it (`GET /ui-settings/layout-templates/usage` for the CIs), with New, Rename,
Duplicate and Delete in the draft, and Edit, which opens the layout editor with `?template=<key>` on a CI that
shows it. **Edit CI…** opens a class's panel (`ClassCiEditor.vue`) with **Edit CI: <name>** on the class's default
template (the most recently updated CI, or another found by name; a class without CIs links to the create form's
editor). The layout editor edits a target (`lib/layoutEditor.ts`): a template, or the CI's own layout, and saves
to the template (a new settings version, confirmed with who it reaches), as a new template or for this CI only
(`PUT /configuration-items/{id}/layout`); **Use template…** and **Reset to class default** change the CI's
layout at once. The layout editor is the CI page itself (`components/layoutEdit/`, `lib/layoutEditor.ts`,
edits in `lib/layoutDesign.ts`), in a window of its own: fields are dragged between sections and tabs (or onto a
tab) and resized by dragging their right edge; each section is a window (`FreeWindow.vue`) moved and resized
anywhere, snapping to the other windows and an 8 px grid, and stacked in layers. Everything has a keyboard path,
announced to screen readers. Core fields (ident, valid from, valid until) can be moved but not hidden. The editor
saves itself. Leaving Customization or reloading with unsaved changes asks first. The issues the API
reports (unknown classes, a required attribute hidden by a layout) are listed above the sections.

### Errors, states and navigation

- API validation errors (`details[].field`, e.g. `validFrom` or `attributes.cpu_cores`) render next to their field.
- Every list and panel has designed loading, empty and error states. An unreachable API names the URL it
  tried, and an empty inventory offers "Create your first configuration item".
- Deleting a CI opens a confirmation that lists every relationship that will break. Removing a relationship
  also confirms.
- Deleting a relationship type or rule, a dropdown list or value, or a legacy lookup value first asks the API what refers to it
  (`GET …/{id}/usage`) and lists it. Classes and attributes are archived, and purged only after that (see above). A row still in use cannot be deleted: the dialog offers to archive it instead.
- Related CIs are links. Walking from CI to CI builds a trail in the breadcrumb, for example
  `Inventory › CRM › crm-app-01 › fra1-esx-01 › FRA1 Rack A01`.
- Confirmations ("Saved crm-app-01.") are toasts in the bottom-right corner that close after 6 s. **F8** moves
  focus to the newest one (its clock stops while it has focus) and **F8** again goes back; **Esc** closes the
  focused toast. Focus then moves to the next toast, or back to where it was before, never to the top of the page.

## Code layout

```
src/config.ts          the only place deploy-time config is read
src/api/schema.d.ts    types generated from backend/openapi.json (do not edit)
src/api/client.ts      the one HTTP client (openapi-fetch) + ApiError / error-envelope handling
src/api/queries.ts     TanStack Query composables and cache keys; components never call fetch
src/api/admin.ts       the same for sign-in, users, permission profiles, API tokens, own password and the audit log
src/api/datamodel.ts   the same for areas, classes, attributes, relationship types/rules, lookups, lists and templates
src/api/uiSettings.ts  the same for UI settings, their versions and images, and configuration export/import
src/api/               also identity providers, two-factor authentication, schema changes, the CSRF token and the query client
src/lib/uiSettings.ts  how the settings document is applied: menu merge, list columns, layout tabs and windows
src/lib/layoutDesign.ts the layout editor's edits on a class layout (move, resize, tabs, sections, notes, panels)
src/lib/freeLayout.ts  the windows of a layout tab: frames, snapping, layers, a grid tab made free
src/lib/permissions.ts permission checks mirrored from the server
src/router.ts          routes (Vue Router, HTML5 history) and the setup → sign-in → app guard
src/stores/            Pinia stores (the session, branding and theme, the one-shot "Created …/Saved …" notice)
src/components/        shell, breadcrumbs, global search, pickers, dialogs, state views;
                       layoutEdit/ holds the in-page layout editor
src/pages/             one component per screen; detail/, form/ and dashboard/ hold their parts;
                       admin/datamodel/ holds the data model editors,
                       admin/customization/ and admin/config/ the customization and export/import screens
src/lib/               pure helpers and composables: formatting, attribute values, list and inventory URL state,
                       the breadcrumb trail, reordering, class trees and icons, layout editing, navigation
src/styles/tokens.css  design tokens (spacing, type scale, colours); app.css builds on them
unit/                  unit tests (`npm run test:unit -w frontend`)
e2e/                   Playwright end-to-end specs (see below)
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

`a11y.spec.ts` runs axe-core (WCAG 2.1 A and AA rules) on sign-in, the inventory (and its Columns popover, in the light and dark theme), a CI detail page with its delete
dialog and edit form, the class and attribute editor, users and profiles, My account and the two-factor enrolment
step. Critical and serious violations fail it; the rest is reported. No rule is turned off; one may only be turned off
for a single screen, with a comment in the spec saying why.

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

The remaining specs in `e2e/` each cover one feature, and the file name says which (API tokens, dropdowns, identity
providers, inventory columns, in-page layout editing, responsive layout, release regressions, security checks, and so
on). Two depend on the environment: `core-upgrade.spec.ts` runs only when `E2E_UPGRADE_SNAPSHOT` points at the
snapshot of an upgraded instance (the upgrade workflow sets it), and the identity provider spec skips its sign-in
button test unless the API has `PUBLIC_URL` set.

The tests expect the demo inventory (`shadoucmdb seed --demo`) and create their own uniquely named records.
They run signed in: `e2e/global-setup.ts` completes first-run setup on a database without users (it sends the setup
token, `E2E_SETUP_TOKEN` or else `SETUP_TOKEN`), or signs in as `E2E_USERNAME` / `E2E_PASSWORD` (an Administrator
account, e.g. from `shadoucmdb create-admin`). Without them it uses `e2e-admin` / `e2e-admin-password`.

```sh
npx playwright install chromium                                           # once
export E2E_USERNAME=admin E2E_PASSWORD=...                                # unless the database has no users yet
export E2E_SETUP_TOKEN=...                                                # only while the database has no users: the API's SETUP_TOKEN
API_PROXY_TARGET=http://<api-host>:3000 npm run test:e2e -w frontend      # starts a dev server on :5199
E2E_BASE_URL=http://localhost:4173 npm run test:e2e -w frontend           # or test an already-served build
E2E_FRESH_BASE_URL=http://localhost:3001 ...                              # also run first-run.spec.ts
E2E_BARE_BASE_URL=http://localhost:3002 E2E_IMPORT_BASE_URL=http://localhost:3003 ...  # also run fresh-install.spec.ts
E2E_AREAS_BASE_URL=http://localhost:3004 E2E_AREAS_PGDATABASE=... E2E_AREAS_IMPORT_BASE_URL=http://localhost:3005 E2E_AREAS_IMPORT_PGDATABASE=... ...  # also run area-tables.spec.ts
```

Set `E2E_SCREENSHOT_DIR=<dir>` to save a screenshot of each step.
