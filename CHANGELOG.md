# Changelog

Changes operators need to act on. Everything else is in the generated notes of each
[GitHub release](https://github.com/Shadoukita/ShadouCMDB/releases).

## Unreleased

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
`serialNumber` and `statusName` are gone). Search matches report `label`, `ident` or
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
