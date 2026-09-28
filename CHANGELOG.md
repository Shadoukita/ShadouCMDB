# Changelog

Changes operators need to act on. Everything else is in the generated notes of each
[GitHub release](https://github.com/Shadoukita/ShadouCMDB/releases).

## Unreleased

### Security: API tokens can no longer add, change or delete an identity provider

A token scoped to the Administrator profile could add an OIDC provider or LDAP directory, point an
existing one at another server or remap its groups to profiles, and the sign-ins that set up kept
working after the token was revoked ([SHAA-370], GitHub #137). These routes now need a signed-in
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

[SHAA-370]: docs/security/hardening.md#enterprise-sign-in

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

### Security: the audit log no longer shows CI values of classes the reader may not view

`GET /api/v1/audit-log` returned the full item in `oldValue`/`newValue` for every CI and relationship
entry, so a user with **View audit log** (`audit.view`) whose profile limits the classes they may view
could read the attribute values of CIs they get 403 on ([GH#121]). Entries about a CI of a class the
reader may not view, and relationship entries with such an endpoint, now keep their metadata (who,
when, what, which entity) but return `oldValue` and `newValue` as null with the new field
`redacted: true`. In the entries they may see, a reference attribute into a hidden CI keeps only its id,
as on the item endpoints. Administrators and profiles with View on all classes see everything as before.

**Upgrade:** nothing to do. The response gains the `redacted` field; no schema change.

[GH#121]: https://github.com/Shadoukita/ShadouCMDB/issues/121

### Security: starting an OIDC sign-in no longer stores anything (GH#122)

Anyone could open `GET /api/v1/auth/oidc/{id}/start` without signing in, and each request stored a
pending sign-in under a limit shared by all users; about 17 requests a second from one client were
enough to make OIDC sign-in answer "unavailable" for everybody ([#122]). The pending sign-in (provider,
`state`, `nonce`, PKCE verifier, return path, expiry) now travels in the `shadoucmdb_oidc` cookie
itself, encrypted and authenticated with AES-256-GCM under a key the server generates and keeps in the
new table `cmdb.server_keys`. There is nothing left to fill up. The callback refuses a cookie that was
changed, cut, sealed with another key or is older than 10 minutes (`ssoError=expired`) before it
contacts the provider. Migration 0021 creates `server_keys` and drops `oidc_login_states`.

**Upgrade:**

- Run `shadoucmdb migrate` as usual. A sign-in in progress while you upgrade ends once with
  "expired"; the user clicks the provider button again.
- The API role may only read and add rows in `server_keys`. Installs split with
  `sql/bootstrap/10_split_roles.sql` get this from migration 0021; if you re-run that script, use the
  one from this release.
- Several API processes behind a load balancer share the key through the database; no configuration.
- `server_keys` is not backed up: a restored server generates a new key. To rotate the key, see
  [data model › Soft-delete decisions](docs/data-model.md#soft-delete-decisions) (`DELETE FROM cmdb.server_keys WHERE purpose =
  'oidc_state'` as the owner role, then restart the API).
- Limit anonymous requests per client address at your reverse proxy
  ([deployment › Hardening settings](docs/deployment.md#hardening-settings)).

[#122]: https://github.com/Shadoukita/ShadouCMDB/issues/122

### Changed: *Data model › Lookups* is read only

Since migration 0016 the status, environment, owner and location of a CI are values of the lookup
lists under **Administration › Data model › Dropdowns**. The older statuses, environments, locations
and owners tables under **Lookups** are kept for history only, so values added there never appeared
on a CI form ([SHAA-336]). The web UI now shows these tables read only (no add, edit, reorder, archive
or delete), and each tab links to the Dropdowns list that replaced it. Maintain CI statuses,
environments, owners and locations under **Dropdowns**. The API refuses writes to the old tables in
this release as well (see the next entry).

[SHAA-336]: docs/data-model.md#tables

### Changed (breaking API change): legacy statuses, environments, locations and owners are read-only

Since migration `0016_core_ci_model`, CIs take their status, environment, location and owner from the
lookup lists `status`, `environment`, `location` and `owner` (same ids), so a row written to the old
tables never showed up on a CI ([GH#111]). Their write endpoints now refuse the change instead of
accepting it silently:

- `POST /api/v1/{statuses,environments,locations,owners}` and `PATCH`/`DELETE` on `…/{id}` answer
  **`410 GONE`** (new error code `GONE`) and change nothing. The message names the replacement. The
  usual checks still come first: `401` without a session or token, `403` without `datamodel.manage`
  or the CSRF token.
- `GET` on the list, `…/{id}` and `…/{id}/usage` is unchanged, for history and reports. It stays
  deprecated and will be removed in a later release.
- Configuration import/export still carries these rows, so exports from earlier versions import as
  before.

**Upgrade:** integrations that create or change these rows must write lookup list values instead:
find the list with `GET /api/v1/lookup-lists?q=status` (the key can differ if `status` was already
taken before 0016), then `POST /api/v1/lookup-list-values` with its `listId`, or `PATCH`
`/api/v1/lookup-list-values/{id}` with the id the old row had. No schema change.

[GH#111]: https://github.com/Shadoukita/ShadouCMDB/issues/111

### Fixed: the inventory sorts by attributes again (hostname, IP address, serial number, ...)

`GET /api/v1/configuration-items` takes `sort=attributes.<key>` (and `-attributes.<key>` for
descending) when the list is filtered by `classId` ([GH#112], [SHAA-335]). The attribute must be the
same one on every class in `classId` (its own or inherited, so `classId=<hardware>` sorts servers and
network devices by their shared `ip_address`). Text sorts case-insensitively, IP and CIDR by address
(`10.0.0.9` before `10.0.0.10`), numbers and dates by value, lookups by the list's value order; CIs
without a value come last. Reference attributes cannot be sorted on. A sort on an attribute without
`classId`, on an unknown attribute, or on a key that is a different attribute in the classes of
`classId` is a `400` with the field `sort` and the code `class_required`, `unknown_attribute`,
`ambiguous_attribute` or `not_sortable`.

Stored UI settings accept the same field in a list view's `defaultSort` and a saved search's `sort`.
Where the class has no such attribute, the effective settings drop the sort (the list sorts by label)
and report an `unknown_attribute` issue.

In the web UI ([SHAA-347]), **Administration › Customization › List views** offers the class's
attributes (not references) as the default sort, and a saved-search widget under **Dashboard** offers
the attributes every ticked class has. In the inventory of one class, the headers of attribute
columns sort the list; the sort is dropped when the class filter changes.

**Upgrade:** migration 0020 puts back the sorts migration 0016 had turned into label sorts. A list
view's default sort, or the sort of a saved-search widget on one class, that was on `hostname`,
`ipAddress`, `serialNumber` or `statusName` before 0016 becomes `attributes.hostname`,
`attributes.ip_address`, `attributes.serial_number` or `attributes.status` (or the suffixed key 0016
gave the attribute, e.g. `hostname_2`), as a new UI settings version by "migration 0020". A sort
changed since 0016, a saved search on several classes, and a class without the attribute keep their
current sort. Nothing to do otherwise.

[GH#112]: https://github.com/Shadoukita/ShadouCMDB/issues/112
[SHAA-335]: docs/api.md
[SHAA-347]: docs/api.md#customization-and-configuration-exportimport

### Added: resize sections by dragging and place them side by side in the layout editors

The form designer (**Customization › Detail and form layout**) and the layout editor window (**Edit
layout** on a CI) now edit the 12-column grid directly ([SHAA-304]):

- **Resize a section** by dragging its right edge; it snaps to the 12 columns and shows e.g. "6 / 12"
  while you drag. Between two sections in a row, the left edge of the second moves the border between
  them.
- **Place a section beside another** by dragging the grip on its top edge onto the other section's left
  or right edge (a bar shows where it goes). It takes the columns the row leaves free, or half of the
  other section when the row is full. The **+** on a section's right edge adds a new section next to it;
  **+ Section** below it adds one underneath.
- **Fields** can be resized on grids of up to 12 columns, with the same live guide.
- **Preview width:** the preview frame has a visible grip on its right edge to drag it to any width;
  the Full width / Laptop / Tablet / Phone buttons are shortcuts.
- **Keyboard:** on a section's grip, Alt+← / Alt+→ resize it and Alt+↑ / Alt+↓ move it; the section's
  toolbar and the designer's **Properties** set the width (1–12 / 12), **Start a new row** and the
  columns. The preview grip takes ← / →, Home and End. In the layout editor, one drag is one undo step.

On the CI detail page and the form, a section's field grid now narrows with the section's own width, so
a half-width section on a wide screen uses two columns instead of squeezing three.

**Upgrade:** nothing to do. No API or schema change; layouts saved before look the same.

[SHAA-304]: docs/data-model.md#editing-a-layout-on-the-ci-page

### Added: sections side by side and a finer grid in detail and form layouts

Each tab of a class layout is now a grid of 12 columns ([SHAA-303]). A section has a `width` (1–12,
default 12, the full width) and sections fill the grid row by row, so two sections of width 6 sit side
by side; `newRow: true` starts a new row early, and the optional `minHeight` keeps a section at least
that many field rows tall. A section's own field grid (`columns`) and field widths go up to 12, for
finer sizes than the earlier 1–4. Sizes are fractions of the width, never pixels: below the tablet
breakpoint (820 px) sections stack at the full width. The form designer's drag handles for this follow
separately.

**Upgrade:** nothing to do. Layouts and exports saved before stay valid and look the same (every
section is full width); there is no migration.

**API (additive):** `UiLayoutSection` gets `width` (always returned, default 12), `newRow` and
`minHeight` (returned only when set); the maximum of `columns` and `UiLayoutField.width` is now 12, and
a field's width must still fit its section's columns (`400` with the path otherwise). API clients that
rebuild a layout from its known keys should keep the new ones, or saving drops them.

[SHAA-303]: docs/data-model.md#detail-and-form-layouts-ui-settings-layout-format-v2

### Changed: API docs off by default; HTTP timeouts, audit hash chain and SIEM export

Backend hardening ([SHAA-80]):

- **`/openapi.json` and `/docs` are off by default** (`API_DOCS=off`). Set `API_DOCS=authenticated`
  (any signed-in user) or `public` if tools or people read the contract from the server. The contract
  is also in the repository as `backend/openapi.json`.
- Request headers must arrive within `HTTP_HEADER_READ_TIMEOUT_SECS` (default 10), and a whole
  request must be answered within `HTTP_REQUEST_TIMEOUT_SECS` (default 120); a slower one gets
  `408 REQUEST_TIMEOUT` and its transaction is rolled back. Raise the second for very large imports.
- `GET /api/v1/version` reports the build and the number of migrations it expects, without a database.
- Migration `0018_audit_hash_chain` hash-chains every `audit_log` row, existing rows included (in `id`
  order; on a large audit log, allow for it in the maintenance window). `shadoucmdb audit-verify`
  checks the chain and prints its head; after retention runs, use `audit-verify --allow-gaps`.
- `AUDIT_EXPORT` copies every new audit row to stdout, a file or syslog over UDP/TCP for a SIEM
  (default off). `AUDIT_CAPTURE_CLIENT_IP` and `AUDIT_CAPTURE_USER_AGENT` (default true) turn off
  recording the client's IP address or User-Agent where policy rules it out.
- A three-role install that runs `sql/bootstrap/10_split_roles.sql` after upgrading needs this
  release's version of the script (it also locks the API role out of the chain head).

[SHAA-80]: docs/deployment.md#hardening-settings

### Added: multi-line text fields; Notes keep their line breaks

Text attributes have a new validation rule `multiline` (`validation: {"multiline": true}`) that
tells the CI form to edit the value in a multi-line text area and the detail page to show its line
breaks ([GH#109]). Administrators set or clear it when creating or editing a text attribute
(`POST`/`PATCH /api/v1/attribute-definitions`; omitted from responses when false). It is only valid
for `text` attributes (`400` otherwise). Text values were and are stored exactly as sent, line breaks
included. In the class editor the option is **Multiline** on a text attribute; the CI form saves a
multi-line value exactly as typed (indentation and trailing line breaks included), where single-line
text is still trimmed.

**Upgrade:** migration `0019_multiline_notes` sets `multiline` on the **Notes** fields that migration
`0016_core_ci_model` created from the former `notes` column (`notes`, or `notes_<n>` where the key was
taken), identified by that migration's recorded schema change rather than by name; fields created by
administrators are not touched. Each change is in the audit log (actor `migration 0019`). A fresh
install's IT infrastructure template creates **Notes** as a multi-line field. Before this release the
form edited Notes in a single-line input, so saving a CI could drop line breaks from its notes: values
saved that way are not restored.

[GH#109]: https://github.com/Shadoukita/ShadouCMDB/issues/109

### Fixed: Dropdowns row actions visible on laptop screens

Administration › Data model › Dropdowns cut off the **Delete** button of each list at 1440 px and
pushed it off-screen at 1280 px ([GH#110]). The Description column now takes the remaining width and
truncates (the full text is in its tooltip), on the Lookups and dropdown value tables as well.

[GH#110]: https://github.com/Shadoukita/ShadouCMDB/issues/110

### Changed (breaking API change): barebone CI core, fixed fields become class attributes

Every CI now has a small core that is the same for every class, and everything else is a class
attribute ([SHAA-267]). Migration `0016_core_ci_model` does the move; `migrate` applies it as usual.

**New on every CI** (API: `ConfigurationItem`, `ConfigurationItemSummary`, graph nodes, search hits):

- `ident`: a short, unique, readable identifier such as `CI-7K3M9Q2X`, generated for every CI
  (existing ones included). Only users with the **Administrator** profile can set or change it (`403`
  for anyone else); unique regardless of case (`409`); changes are in the audit log.
- `validFrom` (defaults to the creation time; existing CIs get their `createdAt`) and `validUntil`
  (optional), and the derived `active` (`validFrom <= now < validUntil`).
- `label`: the display name, taken from the class's new **title attribute**
  (`ci_classes.titleAttributeId`, the migrated `name` field by default), or the ident.

**Removed from the CI API**: `name`, `statusId`/`status`, `environmentId`/`environment`,
`ownerId`/`owner`, `locationId`/`location`, `hostname`, `ipAddress`, `serialNumber` and `notes`, in
responses and in create/update bodies (sending them is now `400`). Their values are class attributes
now: `attributes.name`, `attributes.status`, `attributes.environment`, `attributes.owner`,
`attributes.location` (lookup attributes; the value is the same id as before), `attributes.hostname`,
`attributes.ip_address`, `attributes.serial_number`, `attributes.notes`. The migration adds `name` to
every root class and the other fields only to the classes whose CIs hold values, copies every value and
verifies the counts; nothing is lost. `sql/checks/core_ci_upgrade_1_before.sql` and `_2_after.sql`
compare every value before and after the upgrade.

**List and search** (`GET /api/v1/configuration-items`, `GET /api/v1/search`): only **active** CIs
are returned unless `active=false` or `active=all`. The filters `statusId`, `environmentId`, `ownerId`
and `locationId` are replaced by `lookupValueId` (lookup list value ids; the old ids still work, since
the values kept them). `ipWithin` searches the IP attributes. Sort fields are `label`, `ident`,
`className`, `validFrom`, `validUntil`, `createdAt` and `updatedAt` (`name`, `hostname`, `ipAddress`,
`serialNumber` and `statusName` are gone; attributes sort as `attributes.<key>` with `classId` since
GH#112). Search matches report `label`, `ident` or
`attributes.<key>`.

**Deprecated**: `/api/v1/statuses`, `/environments`, `/owners` and `/locations`. CIs no longer refer to
these tables; their rows were copied into the lookup lists `status`, `environment`, `owner` and
`location` (another key if one was taken) with the same ids. The tables and endpoints stay unchanged
for now and will be removed in a later release. A fresh install's IT infrastructure template creates
the lookup lists instead of these rows.

**Dashboards**: status counts keep working through the `status` attribute. `count_by_status` and
`count_by_environment` widgets are now `count_by_lookup` widgets with `lookupListKey`, and saved-search
filters `statusKeys`/`environmentKeys`/`locationKeys` are now `lookups` (list key → value keys); the
migration rewrote stored UI settings accordingly. The `is_operational` flag of statuses is no longer
used; whether a CI is live is its validity (`active`). List columns and layouts name the former fields
`attributes.<key>`; the built-in fields are `label`, `ident`, `class`, `validFrom`, `validUntil`,
`active`, `createdAt` and `updatedAt`. Settings versions saved before the upgrade stay in the history
but cannot be restored (`409`); the migration saved the converted settings as a new version.

**Reporting views** (`<area>.v_<type>`): the registry columns are now `id`, `ident`, `label`, `type`,
`valid_from`, `valid_until`, `active`, `record_version`, `created_at`, `updated_at`, `deleted_at`; the
former fixed columns appear as ordinary field columns (`name`, `status` as the value key, `hostname`,
…) of the types that have them.

**Action on upgrade**:

- Update API clients and integrations that read or write the removed fields or filters (see above).
  An integration that stored status, environment, owner or location ids keeps working with the lookup
  attributes, since the ids did not change.
- Reports on the reporting views that read `name`, `status`, `hostname`, … keep working for the types
  that have those fields; reports that joined `cmdb.configuration_items` directly must read the type
  tables or views instead.
- A view of your own that reads the dropped columns of `cmdb.configuration_items` makes the migration
  fail with a dependency error before anything changes: drop or rewrite it first.
- If you want an exact before/after comparison, run `sql/checks/core_ci_upgrade_1_before.sql` before
  and `sql/checks/core_ci_upgrade_2_after.sql` after `migrate`.

[SHAA-267]: docs/data-model.md#the-ci-core-ident-validity-and-label

### Added: a layout editor on the real CI page

Users with **customization.manage** get an **Edit layout** button on the CI detail page and on the CI
form (edit and new) ([SHAA-298]). It opens the layout editor in a separate browser window, one per class
(a second click brings the open window to the front), while the page it came from stays as it is. The
editor shows the real page, framed and with a sticky bar that names the class: the change applies to
every CI of that class. The page keeps showing the
CI's real values while tabs are added (**+ Tab** at the end of the tab bar), sections are added between
and after sections (**+ Section**), tabs and sections are renamed by clicking their name, fields are
dragged between sections and onto tabs and resized by their right edge, and a toolbar on each field and
section moves, collapses, sets columns, hides and removes. Hidden fields are listed in a tray to show
them again. The bar has undo and redo (Ctrl+Z, Ctrl+Shift+Z), desktop, tablet and phone widths,
**Reset to built-in layout**, **Save layout** with an optional note, **Discard** and **Done** (closes
the window); leaving or closing the window with unsaved changes asks first. Every drag has a keyboard
equivalent. After a save, the other open windows of the web UI show the new layout without a reload.
When a popup blocker refuses the window, the editor opens in the same tab and says so.

Saving creates a new UI settings version exactly as **Customization** does (same API, permission check,
history and audit trail). If someone else saved in the meantime, the save is refused with a message
and a **Load the latest version** button. The designer in **Administration › Customization › Detail and
form layout** stays available and gains **Open on a CI** (the class's first CI, or an empty form of the
class when it has none), in the same editor window. The editor's URLs (`/cis/<id>/layout-editor`,
`/cis/<id>/edit/layout-editor`, `/cis/new/layout-editor?classId=…`) show the normal page to users
without the permission. No API or database change.

[SHAA-298]: docs/data-model.md#editing-a-layout-on-the-ci-page

### Added: visual form designer; layouts get tabs, sections and a field grid

**Administration › Customization › Detail and form layout** is now a visual designer ([SHAA-271]). It
shows the class's form as it will look and lets administrators drag fields between sections and tabs,
resize them on a grid of 1–4 columns, add, rename, reorder and remove tabs and sections, hide fields and
make them read-only, and resize the preview (or pick laptop, tablet or phone width) to check smaller
screens. Every action also works from the keyboard. Ident, valid from and valid until can be moved but
not hidden. The CI form and the detail page show the layout's tabs; the detail page's relationship map
and history follow them.

**Upgrade:** migration `0017_layout_tabs` converts saved layouts to the new format (layout format v2)
as a new settings version: the old panels become the sections of one **General** tab, in the same order,
so forms and detail pages look as before apart from the field grid. Hidden ident and validity fields are
shown again. Nothing else in the settings changes, and the previous version stays in
**Customization › History**.

**API:** `UiClassLayout` has `tabs[]` → `sections[]` (`columns`) → `fields[]` (`{ field, width }`) instead
of `panels[]`, and the API validates them (unique tab and section keys, a field placed once, widths
within the section's columns, core fields not hidden; `400` with the path otherwise). `panels[]` is
deprecated: still accepted from older exports and API clients and converted to one General tab, but
never returned. New issue code `core_field_hidden`.

[SHAA-271]: docs/data-model.md#detail-and-form-layouts-ui-settings-layout-format-v2

### Added: layout sections can hold a note or a built-in panel (API)

A layout section has a new optional `kind` ([SHAA-299]): `fields` (the default, a grid of fields as
before), `note` (static text written by an administrator in `text`, at most 4,000 characters, plain
text or limited Markdown; the web UI never renders raw HTML from it), or one of the detail page's
built-in panels `relations`, `history` and `audit`, which can then be placed in any tab. Each panel can
be placed once per layout; a panel a layout does not place keeps its usual position. The API refuses a
panel placed twice, `fields` or `text` on a section of the wrong kind and an empty or over-long note
(`400` with the path). The editor for these sections follows in the web UI.

**Upgrade:** nothing to do. The change is additive to layout format v2: saved layouts, earlier settings
versions and configuration exports stay valid and are returned unchanged (`kind` is only written for
sections that are not `fields`).

[SHAA-299]: docs/data-model.md#detail-and-form-layouts-ui-settings-layout-format-v2

### Added: notes and built-in panels in the layout editor and the designer

The layout editor window and **Customization › Detail and form layout** place layout content blocks
([SHAA-310]): **+ Note** adds static text (plain text or limited Markdown: bold, italic, code, links and
lists; HTML is shown as text and only http, https and mailto links are kept), and **+ Panel** places the
detail page's Relationships, History or Audit trail panel in any tab, each once per layout. The detail
page shows them where the layout puts them; a panel the layout does not place keeps its usual position.
The CI form shows notes but not panels. History and Audit trail sections are shown only to users with
`audit.view`. When the API refuses a save, its messages about a section (for example
`settings.layouts.0.tabs.1.sections.0.kind`) are listed in that section.

[SHAA-310]: docs/data-model.md#detail-and-form-layouts-ui-settings-layout-format-v2

### Changed: CI form and detail page start with the General section

The CI form and detail page show the same core for every class ([SHAA-269]): a **General** section
with the ident, valid from, valid until and the class's fields without a form section, then the class's
own sections. The former fixed fields and the "Other" section are gone; a field without a section sits
on General. A new CI's valid from is pre-filled with the current local time, and double-clicking any
date or date-and-time input fills in the current local date and time. Only administrators can edit the
ident; for everyone else it is read-only. Lists and the detail page say when an active CI deactivates
("deactivates on …"); lists show active CIs by default, with **Validity › Show inactive** for the rest.
The class page's **Title attribute** picks the field that labels the class's CIs. Saved layouts keep
working: fields no panel places now fall into General and their sections.

[SHAA-269]: docs/data-model.md#the-ci-core-ident-validity-and-label

### Fixed: database roles and the database may have any name

The migrations granted to the fixed names `shadoucmdb_app` and `shadoucmdb_maintenance`, so an
install with its own role names stopped at migration 0008
([#42](https://github.com/Shadoukita/ShadouCMDB/issues/42)). `migrate` now grants to the users of
`DATABASE_URL` (API) and `MAINTENANCE_DATABASE_URL` (maintenance), prints both, and stops if one
does not exist. **Set both when you run `migrate`**, `restore`, `factory-reset` or
`decommission`. The bootstrap scripts take `-v owner_role=… -v app_role=… -v maintenance_role=…
-v db_name=…`; the defaults are unchanged. Nothing changes for installs with the default names. A
database migrated by a pre-release build since v0.1.0-rc.1 gets its migration records updated by
the next `migrate`, and its backups still restore.

### Added: dependent lookup lists (cascading dropdowns)

A lookup list can now depend on another list, e.g. "Model" on "Manufacturer": each model names the
manufacturer it belongs to, and a model field names its manufacturer field. The API then refuses a
model that does not belong to the CI's manufacturer. Retiring a manufacturer retires its models
(refused while CIs still use one of them); deleting it is refused while models belong to it. See
[docs/data-model.md](docs/data-model.md#dependent-lookup-lists).

Migration `0015_lookup_parent_lists` adds nullable columns (`lookup_lists.parent_list_id`,
`lookup_list_values.parent_value_id`, `ci_attribute_definitions.parent_attribute_id`) and their
integrity triggers; existing lists, values, fields and CI values are unchanged. `migrate` applies it
as usual; `verify` now runs 23 checks.

API: the audit log can now be filtered by `entityType=lookup_lists` and `lookup_list_values`. New nullable fields `parentListId`, `parentValueId`, `parentAttributeId` on lookup lists,
values and attribute definitions (responses and create/update bodies), list filters
`parentListId` and `parentValueId`, and new `409 IN_USE` cases. Configuration files are now written
as `formatVersion: 3` (with `parent` / `parentAttribute` keys); versions 1 and 2 are still imported
and leave existing parent links as they are. **Action** only for scripts that parse exported files
and check `formatVersion === 2`: accept 3.

Web UI: lookup lists moved from *Administration › Lookups › Lists* to the new *Administration › Data
model › Dropdowns* (old bookmarks redirect). There a list gets its parent list and each value its parent
value, values can be filtered by parent value, and the attribute dialog picks a lookup field's parent
field. On the CI form a child dropdown stays disabled until its parent field is set, offers only that
parent's values, and is cleared when the parent changes to one that does not offer it.

### Added: sign-in through OIDC providers and LDAP/AD directories

Users can now sign in through OpenID Connect providers (Entra ID, Okta, Keycloak, ADFS, …) and
LDAP / Active Directory directories (LDAPS or StartTLS, certificates always verified), with the
provider's groups mapped to permission profiles. Local accounts keep working as break-glass
access. Migration `0014_enterprise_sign_in` adds the tables; `migrate` applies it as usual.
Holders of the Administrator profile set providers up under **Administration › Identity
providers** (settings, group mappings, a connection test); the sign-in page shows a "Sign in with
…" button per enabled OIDC provider.

**Action** only if you want OIDC sign-in: set `PUBLIC_URL` to the address users open the web UI
at (e.g. `https://cmdb.example.com`) and register `{PUBLIC_URL}/api/v1/auth/oidc/callback` at the
provider. Allow outbound connections from the server to the provider or the directory. See
[docs/api.md](docs/api.md#enterprise-sign-in) and
[docs/security/hardening.md](docs/security/hardening.md#enterprise-sign-in).

### Changed: database TLS verifies the server certificate by default

`DATABASE_SSL` now defaults to `verify-full` instead of `require` ([#39]). With the old default the
connection was encrypted, but the server certificate and host name were not checked, so anything
on the network path to PostgreSQL could pose as the database.

**Action on upgrade** if `DATABASE_SSL` is not set in your environment:

- The certificate is from a public CA and matches the host name in `PGHOST` / `DATABASE_URL`:
  nothing to do.
- Private CA, or a managed service (Amazon RDS, Azure Database for PostgreSQL): set
  `DATABASE_SSL_CA_FILE` to the CA bundle.
- Connecting by IP address: use a host name that is in the certificate, or a certificate that
  contains the IP.
- No TLS on the database (local development only): set `DATABASE_SSL=disable`.

`DATABASE_SSL=require` still works as an explicit setting, and the server now logs a warning at
startup when it is used with a host that is not loopback. See
[docs/security/hardening.md](docs/security/hardening.md#database-tls).

[#39]: https://github.com/Shadoukita/ShadouCMDB/issues/39
