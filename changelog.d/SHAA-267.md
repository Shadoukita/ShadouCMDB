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
