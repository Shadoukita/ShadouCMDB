# ShadouCMDB data model

The schema is a **generic CI model with real tables per type**. CI types, their
fields and the legal relationships between them are defined as rows, and an
administrator's data model becomes real PostgreSQL objects: an **area**
("Bestand") is a schema (`bestand`), a **type** ("Netzwerk") is a table in it
(`bestand.netzwerk`) and a **field** is a typed column. The application creates
and alters them at run time through its DDL engine; no migration and no code
change is needed for a new asset type. Integrity is enforced in PostgreSQL itself
(foreign keys, `NOT NULL`, unique and check constraints, plus triggers for rules
that need to look at other rows), not only in the API.

Migrations: [`sql/migrations/`](../sql/migrations/)
(`0000_extensions`, `0001_core_schema`, `0002_integrity_triggers`,
`0003_users_and_permission_profiles`, `0004_data_model_admin`, `0005_ui_settings`, `0006_auth_audit`,
`0007_audit_retention`, `0008_cmdb_schema_and_areas`, `0009_type_tables`, `0010_api_tokens` …
`0014_enterprise_sign_in`, `0015_lookup_parent_lists`, `0016_core_ci_model`, `0017_layout_tabs`,
`0018_audit_hash_chain`, `0019_multiline_notes`, `0020_attribute_sorts`,
`0021_stateless_oidc_start`, `0022_api_token_creator`).
SQL that reads and writes them: `backend/src/data/`; the DDL engine: `backend/src/schema/`.

Every system table lives in the **`cmdb` schema** (the application connects with
`search_path = cmdb, public`; `public` keeps the pg_trgm functions and sqlx's
`_sqlx_migrations`). Area schemas sit next to it, so an administrator's names can
never collide with the application's.

## Tables

```
areas ─< ci_classes ─┬─< ci_attribute_definitions >─── (reference_class_id) ─> ci_classes
(schema)  (parent)   │  (parent_attribute_id)   └── (lookup_list_id) ─> lookup_lists ─< lookup_list_values
(title_attribute_id) │                                             (parent_list_id)   (parent_value_id)
                     │                                                      ^ (lookup fields)
                     └─< configuration_items ─┬─── <area>.<type> (id = configuration_items.id)
                         (ident, validity,    │        (reference fields) ──> configuration_items
                          label)              └─< ci_relationships (source / target)
                                                   │
                                    relationship_types ─< relationship_type_rules >─ ci_classes (source / target)
statuses, environments, owners, locations (deprecated since 0016: no longer referenced by CIs)
audit_log (append-only; entity_type + entity_id point at any row)

users ─< user_permission_profiles >─ permission_profiles ─┬─< permission_profile_global_permissions
  │                                                        └─< permission_profile_class_permissions >─ ci_classes (NULL = all)
  ├─< sessions
  ├─< user_totp (0..1), user_recovery_codes, mfa_challenges
  ├─< api_tokens >─ permission_profiles (scope; NULL once deleted)
  └── identity_providers (0..1; the provider the account signs in through)
         └─< identity_provider_group_mappings >─ permission_profiles
server_keys (secrets the server generates for itself, one row per purpose)

ui_settings (one row) ── (version) ─> ui_settings_versions (append-only)
ui_assets (logo, favicon)
```

| Table | Purpose | Key constraints |
| --- | --- | --- |
| `areas` | The top-level groups shown as menu tabs. `key` is the PostgreSQL schema holding the tables of the area's types. `is_active = false` archives the area. | unique `key`; key format `^[a-z][a-z0-9_]{0,62}$`; not a system schema (`cmdb`, `public`, `information_schema`, `pg_*`, `cmdb_*`); `key` immutable (trigger) |
| `ci_classes` | CI types in a single-inheritance tree (`parent_id`). `is_abstract` classes group attributes and rules but hold no CIs. `icon`, `color` (`#rrggbb`) and `sort_order` drive menus and badges. `area_id` is the area whose schema holds the type's table, named after `key`. `title_attribute_id` is the field whose value labels the type's CIs (see [The CI core](#the-ci-core-ident-validity-and-label)). | unique `key` (across areas); key format check; at most 61 characters and no `v_` prefix (the reporting view is `v_<key>`); colour format; no self-parent; **no cycles** (trigger); `key` and `area_id` immutable (trigger); the title attribute is a field of the class or an ancestor, of type text, enum, number, integer, date, datetime, ip or cidr (trigger; FK `ON DELETE SET NULL`) |
| `ci_attribute_definitions` | Typed attribute per class, inherited by descendant classes. Types: `text`, `number`, `integer`, `boolean`, `enum`, `date`, `datetime`, `ip`, `cidr`, `reference`, `lookup`. `group_name` is the form section (none: the General section, after the CI core), `sort_order` the order within it; `help_text` is shown on forms; `default_value` (jsonb, same shape as an API value) is applied to new CIs. | unique (`class_id`, `key`); `enum_values` required iff enum; `reference_class_id` required iff reference; `lookup_list_id` required iff lookup (FK, RESTRICT); `parent_attribute_id` only on a lookup field whose list has a parent list, pointing at a lookup field on that parent list, defined on the same class or an ancestor (trigger; FK); a default is never JSON null and never on a reference; `key` (the column name) and `class_id` immutable (trigger); `key` is never `id` |
| `lookup_lists` | Lists an administrator defines, e.g. "Support contract". `parent_list_id` makes a list depend on another one ("Model" depends on "Manufacturer"), see [Dependent lookup lists](#dependent-lookup-lists). | unique `key`; key format; non-blank name; no self-parent, **no cycles** (trigger); a parent list cannot be deleted (FK, RESTRICT); after a change of `parent_list_id` no value or field of the list may keep a parent from another list (deferred constraint trigger) |
| `lookup_list_values` | The values of a list (`key`, `name`, `color`, `sort_order`, `is_active`). `parent_value_id` is the value of the parent list it belongs to. | unique (`list_id`, `key`); cascades with the list; cannot move to another list (trigger); `parent_value_id` belongs to the list's parent list, is required on new values of a list with a parent list and cannot be cleared once assigned (trigger); a value other values belong to cannot be deleted (FK) |
| `configuration_items` | The registry of every CI, with only what every CI has whatever its class: `class_id`, `ident`, `valid_from`, `valid_until`, the derived `label`, `version` for optimistic locking, timestamps and `deleted_at`. Everything else (name, status, hostname, …) is a class field in the type tables. | FK to the class; class must be concrete and active (trigger); `ident` unique regardless of case (`lower(ident)`) and of the form `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`; `valid_until` after `valid_from`; non-blank `label` |
| `<area>.<type>` (type tables) | One per type, e.g. `bestand.netzwerk`: `id uuid PRIMARY KEY REFERENCES cmdb.configuration_items (id) ON DELETE CASCADE`, then one column per field of the type. A CI has a row in the table of its type **and of every ancestor type** (class table inheritance: a server's inherited `hardware` fields are in `infrastruktur.hardware`). Column types: text → `text`, enum → `text` with a CHECK on the allowed values, number → `numeric`, integer → `bigint`, boolean → `boolean`, date → `date`, datetime → `timestamptz`, ip → `inet`, cidr → `cidr`, reference → `uuid` FK to `configuration_items` (NO ACTION), lookup → `uuid` FK to `lookup_list_values` (RESTRICT). | PK/FK to the registry; enum CHECK `ck_<field id>_<hash of the values>`; FKs `fk_<field id>` with index `ix_<field id>`; a required, active field is `NOT NULL`. Constraint names never contain user text. That a reference points at a CI of the right type, that a lookup value belongs to the field's list, and min/max/pattern rules are checked by the API |
| `<area>.v_<type>` (reporting views) | Read-only view per type: the registry columns (`id`, `ident`, `label`, `type`, `valid_from`, `valid_until`, `active`, `record_version`, `created_at`, `updated_at`, `deleted_at`) plus every field of the type and its ancestors (the former fixed columns `name`, `status`, `hostname`, … among them); lookup fields show the value's key. Deleted CIs are included (filter on `deleted_at IS NULL` for the live inventory). | rebuilt by the DDL engine when the type or an ancestor changes; marked with a `shadoucmdb:<hash>` comment so the engine never touches a view it did not create: a view of the same name without that comment is left in place (the type then has no reporting view) and a schema change that would rebuild or drop it reports a `warning` instead |
| `schema_changes` | Every DDL plan the application ran: actor, time, request id, a one-line summary, the exact statements in order, and their impact on data (rows converted, values dropped, warnings). | **UPDATE/DELETE rejected** (trigger); at least one statement |
| `relationship_types` | `runs_on`, `depends_on`, `located_in`, `connected_to`, …, with `forward_label` / `reverse_label` and `is_directional`. | unique `key` |
| `relationship_type_rules` | Legal (source class, target class) pairs per type. A rule matches the named class **and all its descendants**. | unique triple |
| `ci_relationships` | Typed, directional edge `source_ci_id → target_ci_id`. | **no self-edges** (check); **no duplicate live edges** (partial unique index); for non-directional types the reverse edge also counts as a duplicate (trigger + advisory lock); endpoints must satisfy a rule and must not be soft-deleted (trigger) |
| `statuses` | **Deprecated (0016).** CI lifecycle as it was before the barebone core; `is_operational` flagged "live" statuses. Migration 0016 copied the rows into the lookup list `status` with the same ids; CIs hold those values. Kept read-only for history (the API's writes answer `410 GONE`); will be removed in a later release. The web UI shows these four tables read only under *Data model › Lookups*, each linking to its Dropdowns list. | unique `key` |
| `environments` | **Deprecated (0016)**, copied into the lookup list `environment`. | unique `key` |
| `locations` | **Deprecated (0016)**, copied into the lookup list `location` (flat; the hierarchy, type and address stay here). | unique `key`; `location_type` check; no cycles (trigger) |
| `owners` | **Deprecated (0016)**, copied into the lookup list `owner` (key derived from the name; kind and e-mail in the value's description). This is not a login table (see `users`). | `kind` check; unique `external_ref`; email format |
| `users` | Accounts: `username`, `display_name`, `email`, `is_active`, argon2id `password_hash` (PHC string, never returned by the API), `password_changed_at`, `last_login_at`. An account created by an identity provider has `identity_provider_id` and `external_id` (the OIDC `sub`, or the directory entry's `objectGUID`/`entryUUID`/DN) and no password. | unique `lower(username)`; username format; `password_hash LIKE '$argon2id$%'`; a password **iff** local (check); `identity_provider_id` and `external_id` together, unique as a pair; provider FK `RESTRICT`; email format |
| `identity_providers` | OIDC providers (`kind = 'oidc'`: `issuer_url`, `client_id`, `client_secret`, `scopes`, `username_claim`, `groups_claim`) and LDAP/AD directories (`kind = 'ldap'`: `ldap_url`, `start_tls`, `bind_dn`, `bind_password`, `user_base_dn`, `user_filter`, attribute names), with `name`, `is_enabled`, `sort_order` and an optional `ca_certificate` (PEM). Secrets are stored as is (the server presents them) and never returned by the API. | unique `lower(name)`; the columns of its kind required and the other kind's NULL (checks); issuer `https://` (or loopback `http://`); `ldaps://`, or `ldap://` with `start_tls` (check); bind DN and password together; filter contains `{username}`; `kind` immutable (trigger) |
| `identity_provider_group_mappings` | A group the provider reports (`group_name`: an OIDC groups-claim value or an LDAP group DN) grants a permission profile. | unique (`provider_id`, `lower(group_name)`, `profile_id`); cascades with the provider and the profile |
| `server_keys` | Secrets the server generates for itself, one row per `purpose`, with the `key_id` byte sent in front of every value sealed with it. `oidc_state` seals the `shadoucmdb_oidc` cookie: an OIDC sign-in between the redirect to the provider and its callback (provider, `state`, `nonce`, PKCE verifier, return path, expiry after 10 minutes), encrypted and authenticated with AES-256-GCM, so starting a sign-in stores nothing (migration 0021 replaced the former `oidc_login_states` table). The first API process that needs a key inserts it; all processes then share it. Never backed up. | PK `purpose`; `key_id` 0–255; `secret` exactly 32 bytes; the API role may only `SELECT` and `INSERT` |
| `permission_profiles` | Named sets of permissions. `is_builtin` marks the one Administrator profile (created by the migration), which holds every permission implicitly. `require_mfa`: holders must set up two-factor authentication. | unique `lower(name)`; at most one built-in; built-in cannot be updated (except `require_mfa`) or deleted (trigger) |
| `permission_profile_global_permissions` | (`profile_id`, `permission`) for `users.manage`, `profiles.manage`, `datamodel.manage`, `customization.manage`, `config.export_import`, `audit.view`. | PK; permission check; no rows for the built-in profile (trigger) |
| `permission_profile_class_permissions` | `can_view` / `can_create` / `can_edit` / `can_delete` per profile and class; `class_id` NULL is the "all classes" wildcard. | one row per (profile, class) and one wildcard per profile (partial unique indexes); `can_view` required; cascades with the class and the profile |
| `user_permission_profiles` | Which profiles each user holds (any number). | PK (`user_id`, `profile_id`); **never zero active users holding the Administrator profile** (deferred constraint trigger, serialised by an advisory lock) |
| `api_tokens` | API tokens: `name`, owner `user_id`, scope `profile_id`, SHA-256 of the secret (`token_hash`), `token_prefix` (first 14 characters), `expires_at` (required), `revoked_at`/`revoked_by`, `last_used_at`/`last_used_ip` (evidence only), `created_by` (the creator's name, for display) and `created_by_user_id` (the creating user; NULL for the CLI, a deleted creator, or an older token whose audit `create` row was purged). An administrator's password reset revokes the working tokens they created for other users. | unique `token_hash` (32 bytes); expiry after creation; `revoked_at` and `revoked_by` set together; cascades with the owner, `profile_id` set NULL when the profile is deleted, `created_by_user_id` set NULL when the creator is deleted |
| `user_totp` | A user's authenticator: the 160-bit TOTP `secret` (stored as is: checking a code needs it; whoever reads it still needs the password), `confirmed_at` (NULL while the set-up is unconfirmed, which does not count as MFA), `last_used_step` (the last accepted 30 s step, so no code works twice). | PK `user_id`, cascades with the user; secret exactly 20 bytes |
| `user_recovery_codes` | Ten one-time codes per confirmed authenticator: SHA-256 of each (`code_hash`, 80 random bits per code), `used_at`. | unique (`user_id`, `code_hash`); 32-byte hash; cascades with the user |
| `mfa_challenges` | A sign-in whose password was right and whose code is due: SHA-256 of the `shadoucmdb_mfa` cookie token, `expires_at` (5 minutes), `failed_attempts`. Never backed up. | unique `token_hash` (32 bytes); cascades with the user |
| `sessions` | Server-side login sessions: SHA-256 of the cookie token, `csrf_token`, `last_seen_at` (idle timeout), `expires_at` (absolute lifetime), `user_agent`, `ip_address` (`inet`, client address at sign-in; evidence only). | unique `token_hash` (32 bytes); cascades with the user |
| `ui_settings` | The one current UI settings document (`settings` jsonb, validated by the API against `UiSettingsDocument`), its `version`, and who saved it. Classes, attributes and lookups are referenced by key inside the document, not by FK, so it survives export/import; the API reports references that do not resolve. | exactly one row (`singleton` check + unique); `settings` is an object; `version` must exist in `ui_settings_versions` (deferred FK) |
| `ui_settings_versions` | Every saved version of the document with actor, time and an optional comment. | PK `version`; **UPDATE/DELETE rejected** (trigger); comment at most 500 characters |
| `ui_assets` | Logo and favicon bytes with `content_type` and `sha256` (ETag). Stored in the database so no shared file storage is needed and backups include them. | unique `kind` (`logo`, `favicon`); content type allowlist; size 1 byte to 512 KiB (logo) / 128 KiB (favicon); `sha256` format |
| `audit_log` | actor (`actor_type`, `actor_id`, `actor_name`), `action`, `entity_type`, `entity_id`, `occurred_at`, `old_value`, `new_value` (jsonb), `request_id`. | action/actor checks; old/new presence per action; **UPDATE/DELETE/TRUNCATE rejected** (trigger; the purge function is the only exception) |

All primary keys are `uuid` (`gen_random_uuid()`), except `audit_log.id`, which is a
`bigint` identity column for cheap append ordering. `created_at`/`updated_at` are
`timestamptz`; `updated_at` is maintained by a trigger on every mutable table.

### How the reference examples map

- **Server → Application → Database:** `Application runs_on Server` and `Application depends_on Database`
  (also `Database runs_on Server|VM` and `VM runs_on Server`).
- **Device → Location:** `<any hardware> located_in <Location CI>`. The rule is declared on the
  abstract `hardware` class, so servers, network devices and any future hardware class are covered.
  Hardware also has a `location` lookup field (list `location`) for filtering by site or rack.

## The CI core: ident, validity and label

Since migration `0016_core_ci_model` (SHAA-267) a CI has only a barebone core; every other field
comes from its class.

- **`ident`**: a short, readable identifier, unique regardless of case. The database generates it
  (`cmdb.new_ci_ident()`: `CI-` and 8 Crockford base32 characters from 40 random bits, e.g.
  `CI-7K3M9Q2X`). It is immutable for everyone but users holding the **Administrator** profile: the
  API answers `403 FORBIDDEN` to anyone else who sends a different ident on create or update (resending
  the current one is fine). An administrator may set any `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$` (a legacy
  asset number, say); a taken ident is `409 CONFLICT`. The change is in the CI's `update` audit row
  (`oldValue.ident`, `newValue.ident`).
- **`valid_from`** (default: now) and **`valid_until`** (optional, after `valid_from`): the validity
  period. A CI is **active** while `valid_from <= now() < valid_until` (open-ended without
  `valid_until`), so a future `valid_until` schedules the deactivation. `active` is derived at query
  time (`schema::ACTIVE_SQL`), never stored. Lists and search show active CIs unless
  `active=false|all`. Inactive is not deleted: the CI stays editable and linkable, and soft delete
  (`deleted_at`) is separate.
- **`label`**: the display name used in lists (and their default sort), references, relationships,
  the graph and search. It is the value of the class's **title attribute**
  (`ci_classes.title_attribute_id`) as text (at most 500 characters; an IP without its `/32`, a
  datetime in UTC), or the ident when the class has none or the CI has no value. The API recomputes it
  in the same transaction as every change that affects it (a CI's values, a class's title attribute or
  parent, a title field's type change or purge); clients never write it. A new subtype takes its
  parent's title attribute.

**In the web UI** (SHAA-269) the CI form and detail page of every class start with a **General**
section: ident, valid from, valid until (and, on the detail page, whether the CI is active), then the
class's fields without a `group_name`. Each `group_name` follows as its own section, in `sort_order`;
there is no catch-all "Other" section. The detail page ends with a **Record** section (class, created,
updated, id). A new CI's valid from is filled in with the current local time; double-clicking any
date or date-and-time input sets the current local date and time. The ident is read-only for anyone
without the Administrator profile. An active CI with a future `valid_until` says "deactivates on …" in
lists and on its detail page, and an inactive one with a future `valid_from` says when it activates.

**Former fixed columns.** Until 0016 every CI had `name`, `status_id`, `environment_id`, `owner_id`,
`location_id`, `hostname`, `ip_address`, `serial_number` and `notes`. The migration made them class
fields and dropped the columns:

| Column | Field | Placed on |
| --- | --- | --- |
| `name` | `name`, text, required, max. 200 | every root class; it is the title attribute of every class |
| `status_id` | `status`, lookup (list `status`), required | the topmost classes whose CIs held a value |
| `environment_id`, `owner_id`, `location_id` | `environment`, `owner`, `location`, lookups (lists of the same names) | the same rule |
| `hostname`, `ip_address`, `serial_number`, `notes` | `hostname` (text, hostname pattern), `ip_address` (ip), `serial_number` (text, max. 200), `notes` (text, max. 4000, multi-line since migration 0019) | the same rule |

"Topmost" means: a class gets the field if its own CIs (deleted ones included) held a value and no
ancestor already got it, so each CI's lineage has exactly one such field. A key already used in that
lineage gets a suffix (`hostname_2`) and a warning in `schema_changes`. The lookup lists get the rows of
`statuses`, `environments`, `owners` and `locations` **with the same ids** (a list key already taken
gets a suffix too), so an id stored anywhere keeps its meaning. For every field the number of values
written must equal the number of CIs that held one, or the migration fails and nothing changes.
`updated_at` of CIs and classes is kept (the backfill is not an edit), and stored UI settings are
rewritten to the new field names in a new settings version. `sql/checks/core_ci_upgrade_1_before.sql`
and `_2_after.sql` compare every value before and after the upgrade.

**Dashboards and operational status.** Status is now an ordinary lookup field, so "counts by status"
keep working through it: the built-in dashboard and `count_by_lookup` widgets count CIs per value of
a lookup list with `lookupValueId` (the migration turned `count_by_status` / `count_by_environment`
widgets into `count_by_lookup` widgets on the migrated lists, and saved-search status, environment and
location filters into `lookups` filters). The `is_operational` flag of the old `statuses` table is no
longer used: whether a CI is live is its validity (`active`), which is independent of any class
field. Reports that want "operational" should filter on `active` in the reporting views.

### Bare start and the IT infrastructure starter template

`shadoucmdb seed` loads only system rows (today it checks the built-in Administrator profile that
migration 0003 creates). A fresh install therefore has no classes, attributes, relationship types or
lookups. What `seed` loaded before SHAA-31 is now the **`it_infrastructure` starter template**
(`backend/src/modules/templates/`), installed through `POST /api/v1/admin/templates/it_infrastructure/install`
or `shadoucmdb seed --template it_infrastructure`, or with one click under *Administration › Templates* in
the web UI. Installing matches rows by key, adds only what is missing, never changes existing rows and audits
every row it creates, so it is idempotent. Everything the template adds (and anything else in the data model)
can then be changed under *Administration › CI classes, Relationship types and Dropdowns*. Databases
seeded before migration 0004 keep all their rows; the template then reports `installed`.

The template contains:

- Classes: `hardware` (abstract) › `server`, `network_device`; `virtual_machine`, `application`,
  `database`, `service`, `location`. There are 69 attribute definitions across them, including an
  `application.primary_database` **reference** attribute. Every root class has `name` (the title
  attribute), `status` and, where it makes sense, `environment`, `owner` and `notes`; hardware also
  `location`, `hostname`, `ip_address` and `serial_number`, virtual machines `hostname` and `ip_address`.
  A template field whose key a class of the same lineage already has (e.g. `status` on `server` after the
  0016 upgrade) counts as present.
- Relationship rules: `runs_on` (app→server/VM, db→server/VM, VM→server), `depends_on`
  (app→db/app, service→app/service), `located_in` (hardware→location, location→location),
  `connected_to` (hardware↔hardware).
- Lookup lists `status` (planned, in service, maintenance, retired, disposed), `environment`,
  `owner` (empty) and `location` (EMEA, FRA1, its room and rack, Americas, NYC1, AWS eu-central-1).
- `seed --demo` installs the template, then adds three owner teams to the `owner` list, 8 CIs with
  their names, statuses and other values, and 8 relationships (a CRM service down to its rack).
- It installs into the area **Infrastruktur** (schema `infrastruktur`), creating the area if needed,
  and builds the type tables and reporting views in the same transaction.

## Detail and form layouts (UI settings, layout format v2)

A class's layout lives in the UI settings document (`ui_settings.settings.layouts[]`, one per class,
validated by the API as `UiClassLayout`) and applies to both the CI form and the detail page. It is
presentation, not data, so it is JSON in the settings document rather than tables, and it names classes
and fields by key. Administrators edit it in **Administration › Customization › Detail and form layout**
(the form designer). The designer has the same section handles as the layout editor below (resize by the
edges, place side by side by the grip, **+** next to a section), and its **Properties** panel sets a
section's width, **Start a new row** and its columns from the keyboard. Its preview frame is resized by
the grip on its right edge; **Full width** / **Laptop** / **Tablet** / **Phone** are shortcuts.

```
layouts[]: { classKey, tabs[], hiddenFields[], readOnlyFields[] }
  tabs[]:     { key, label, sections[] }                   key unique among the layout's tabs
  sections[]: { key, label, kind (default fields),         key unique across the whole layout
                width 1–12 (default 12), newRow, minHeight 1–50,   placement on the tab's 12-column grid
                columns 1–12 (default 3), collapsed,       columns of the section's own field grid
                fields[],                                  kind fields only
                text }                                     kind note only: 1–4,000 characters
  fields[]:   { field, width 1–12 (default 1) }            field placed once; width ≤ the section's columns
```

- **Section kinds.** `kind` says what a section shows. `fields` (the default when `kind` is absent) is a
  grid of fields. `note` is static text an administrator writes in `text`: plain text or limited
  Markdown, which the web UI renders without ever rendering raw HTML. `relations`, `history` and `audit`
  are the detail page's built-in panels (relationships, version history, audit trail); placing one in a
  section shows it there, in any tab, under the section's label and honouring `collapsed`. Each panel
  can be placed once per layout, and a panel the layout does not place keeps its usual position on the
  detail page. Notes and panels have no `fields`, and panels no `text`. The API writes `kind` only
  for sections that are not `fields`, so layouts saved before section kinds existed round-trip
  unchanged and need no migration.
- **Sections on the tab's grid.** Every tab is a grid of 12 columns. A section spans `width` of them
  (default 12, the full width) and sections fill the grid row by row in the order given: two sections
  of width 6 sit side by side, a third one starts the next row. `newRow: true` starts a new row even if
  the section would still fit next to the previous one. `minHeight` is optional: the section is at least
  that many field rows tall (one field row is the height of a row of fields, so it scales with the font
  size); without it a section is as tall as its content.
- **Responsive, never in pixels.** Widths are fractions of the available width, never pixel sizes, so a
  layout works on every screen. Below the tablet breakpoint (820 px of content width, which includes
  tablets in portrait at 768 px and every phone) sections stack at the full width in their order and
  `newRow` has no effect; the side-by-side arrangement is the desktop layout.
- `field` is a core field (`ident`, `validFrom`, `validUntil`), a detail-page field (`label`, `class`,
  `active`, `createdAt`, `updatedAt`) or `attributes.<key>`. Fields fill a section's grid row by row in
  the order given; `width` is the number of the section's `columns` a field spans. A grid of 12 columns
  allows fine sizes (a field of width 4 is a third of the section); the earlier 1–4 values keep their
  meaning. A section's field grid narrows with the section's own width: at most two columns below
  820 px (a field then takes both when it spans more than half of the section, and grids of up to 4
  columns keep their earlier rule) and one below 520 px. A half-width section on a wide screen therefore
  gets the grid it would get on a tablet.
- **Unplaced fields are never lost.** Anything the tabs do not place and that is not hidden (an
  attribute added to the class later, for example) follows at the end of the first tab: a General
  section with the core fields and the attributes without a group, then the attribute groups. The
  detail page's "Active" follows "Valid until" wherever that is placed.
- **Core fields can be moved, not hidden.** `hiddenFields` may not contain `ident`, `validFrom` or
  `validUntil` (the API answers `400` with the path, e.g. `settings.layouts.0.hiddenFields.1`).
  They can still be read-only on the form. A required attribute that a layout hides or makes read-only
  stays editable on a new CI and is reported as the issue `required_field_not_editable`.
- **Server-side validation** on `PUT /api/v1/ui-settings` and configuration import: the schema (key
  patterns, section widths, columns and field widths of 1–12, `minHeight` of 1–50, at most 20 tabs, 50 sections per tab, 200 fields per section) and
  the cross-field rules above (unique keys, a field placed once, width within the section's columns,
  core fields not hidden, each built-in panel once, `fields` and `text` only on sections of their kind,
  note text not blank). References to attributes that do not exist are accepted, dropped from the
  effective settings and listed as `issues`, like everywhere else in the document.

**Layouts saved before the grid** (no section `width`, `columns` and field widths of 1–4) stay valid
unchanged and render as before: every section is 12 wide, so they stack at the full width. There is
no migration; the API fills in `width: 12` when it returns a layout or saves it again. `newRow` and
`minHeight` are only written when set.

### Editing a layout on the CI page

Users with **customization.manage** can also edit a class's layout on a real CI page: **Edit layout** on
a CI's detail page, its edit form or the new-CI form opens the layout editor in a separate browser
window. The designer's **Open on a CI** opens it too, on the class's first CI or, without CIs, on an
empty form of the class. There is one editor window per class (named `layout-editor-<class key>`): a
second click brings the open window to the front instead of loading it again. The page the editor was
opened from stays as it is.

The editor has its own route: the page's path plus `/layout-editor` (`/cis/<id>/layout-editor`,
`/cis/<id>/edit/layout-editor`, `/cis/new/layout-editor?classId=<id>`). It shows the real page, with the
CI's values, framed as being edited; the bar on top names the class, since the layout applies to all of
its CIs. Users without the permission are sent to the page itself. If a popup blocker refuses the window,
the editor opens in the same tab with a notice, and **Done** returns to the page.

| To… | With the mouse | From the keyboard |
|---|---|---|
| Add a tab | **+ Tab** at the end of the tab bar; type its name | same (a button) |
| Add a section | **+ Section** below a section (on hover) or at the end of the tab; type its name | same (buttons) |
| Add a section next to another | the **+** on a section's right edge (on hover): the new section shares the row | same (a button) |
| Resize a section | drag its right edge; it snaps to the tab's 12 columns and shows e.g. "6 / 12" while you drag. Between two sections in a row, drag the left edge of the second to share the row differently | on the section's grip: Alt+← / Alt+→; the toolbar's width (1–12 / 12) |
| Place a section beside another | drag it by the grip on its top edge onto the other section's left or right edge (a bar shows where it goes) | the toolbar's width and **New row**, and Alt+↑ / Alt+↓ on the grip for the order |
| Move a section up or down, or to another tab | drag its grip above or below another section, or onto a tab | on the grip: Alt+↑ / Alt+↓; toolbar ↑ / ↓ and the tab select |
| Rename a tab or section | click its name (a tab: click the selected tab) | Enter on the name, or **✎** |
| Move a field | drag it (by its grip) within a section, into another section, or onto a tab | on the grip: Alt+↑ / Alt+↓; the field's toolbar: **Move to section** |
| Resize a field | drag its right edge | on the grip: Alt+← / Alt+→; toolbar ⇤ / ⇥ |
| Hide / show a field | drag it onto **Hidden fields**, or **Hide**; **Show** in the tray | Delete on the grip |
| Section columns, collapsed, remove | the section's toolbar (hover) | Tab into the toolbar |
| Try another screen width | drag the grip on the preview's right edge to any width | on the grip: ← / → (10 px, Shift: 50 px), Home, End |
| Read-only on the form | the field's toolbar (form only) | same |

Placing a section beside another fits it into the row: it takes the columns the row leaves free (a 6
next to a 6), and when the row is full the section it is dropped on gives up half of its width. Every
drag is one undo step, however far the edge travelled.

**Undo** / **Redo** (Ctrl+Z, Ctrl+Shift+Z or Ctrl+Y outside text fields), **Desktop** / **Tablet** /
**Phone** as shortcuts for the preview width, **Reset to built-in layout** (drops the class's own layout from the draft; undoable),
**Save layout** with an optional note, **Discard** and **Done** (closes the editor's window) are in the
bar. Leaving the editor or closing its window with unsaved changes asks first. After a save, the editor
tells the other open windows of the web UI (BroadcastChannel `layout-updated`), which load the new
settings; in a browser without BroadcastChannel, the page that opened the editor offers a reload. Saving goes through `PUT /api/v1/ui-settings` with the version the
editor started from, like **Customization**: it creates a settings version (listed in **Customization ›
History** with the note, and audited), and a `409 VERSION_CONFLICT` (someone saved in between) is shown
with **Load the latest version**, which discards the draft. The same rules apply as in the designer
(lib/layoutDesign in the web UI, and the API's validation). Users without the permission see the normal
page at the editor's URL.

**Layout format v1 and migration 0017.** Before 0017 a layout was `panels[]` (`key`, `label`,
ordered `fields`, `collapsed`). Migration `0017_layout_tabs` converts the stored settings: the panels
become the sections of one tab "General" (key `general`), in order, each with 3 columns and every field
1 column wide; `ident`, `validFrom` and `validUntil` are taken out of `hiddenFields`; nothing else in
the document changes. The result is saved as a new settings version by `migration 0017`, so the
previous version stays in the history. The API still accepts `panels` (older exports, API clients,
restoring a version saved before 0017) and converts them the same way; it never returns them, and a
layout may not send both `panels` and `tabs`. Only the settings document changes; no table does.

## Areas, type tables and the DDL engine

**Technical names.** Areas, types and fields have a display name, which can be changed at any time,
and a technical name (`key`), which is the schema, table or column name and never changes once
created. The API derives it from the display name unless one is given: lower case, `ä`→`ae`,
`ö`→`oe`, `ü`→`ue`, `ß`→`ss` (and other accents dropped), every other character becomes `_`, runs of
`_` collapse, a leading digit gets a prefix (`type_2024_…`). "Virtuelle Maschinen" →
`virtuelle_maschinen`, "Größe" → `groesse`. `GET /api/v1/technical-names` previews it. A name must
match `^[a-z][a-z0-9_]{0,62}$` (types: at most 61 characters, since the view adds `v_`), so it never
needs quoting in a report. Refused with `422 INVALID_NAME` and the reason: reserved SQL keywords,
`pg_` prefixes, system schemas (`cmdb`, `public`, `information_schema`, `cmdb_*`), the database
role prefix `shadoucmdb_*` for areas, `v_` for types, registry column names of the reporting views
(`id`, `ident`, `label`, `type`, `valid_from`, `valid_until`, `active`, `record_version`, `created_at`,
`updated_at`, `deleted_at`) for fields, and names already taken (types are unique across areas; a field is unique
within its type's lineage; an area may not match an existing schema or database role, since a schema
named after a role comes first on that role's default search_path).

**The engine** (`backend/src/schema/`). A data model write changes the metadata rows, then asks the
engine to bring the catalog in line, **in the same transaction**:

1. It takes the advisory lock `hashtext('shadoucmdb:schema')`, so schema changes are serialised: a
   second administrator waits for the first to commit, then plans against the new state.
2. It compares the metadata with `pg_catalog` and plans the DDL: `CREATE SCHEMA`, `CREATE TABLE`,
   `ADD COLUMN`, `ALTER COLUMN … TYPE … USING`, CHECK and FK constraints, `SET/DROP NOT NULL`,
   reporting views, grants.
3. It runs the **data-loss guards** before anything executes (all answer `422 SCHEMA_CHANGE_REFUSED`
   naming the request field and the reason):
   - a field type change is dry-run over every stored value (`cmdb.value_castable`) and refused if
     any would not convert, listing up to five of them; reference and lookup fields cannot change type
     while they hold values;
   - making a field required (`NOT NULL`) is refused while any asset, deleted ones included, has no value;
   - removing enum values that are still stored is refused;
   - nothing is ever dropped except by an explicit purge.
4. It executes the plan with `lock_timeout = 10s` (a busy table gives `409 CONFLICT` rather than a
   queue behind a long report), rebuilds affected views, and records the plan in `schema_changes`
   plus an `audit_log` row.

Every identifier in the DDL comes from a validated technical name and is quoted; the only literals
are enum values, quoted by PostgreSQL's `format('%L')`. User text never reaches SQL.
`POST /api/v1/schema-changes/preview` runs any data model operation exactly as its endpoint would,
inside a transaction that is always rolled back, and returns the DDL and its impact.

**Archive and purge.** Deleting an area, type or field **archives** it (`is_active = false`): the UI
hides it, its schema, table or column and every value stay, no new CIs or values are accepted, and
`PATCH {"isActive": true}` restores it. An archived field is never `NOT NULL`. A separate **purge**
(`POST …/{id}/purge` with the technical name typed as `confirm`) drops the column, the table (with the
type's CIs, their relationships, fields and rules) or the empty schema. Purge is refused while the
object is active, while a type has subtypes or other types' reference fields point at it, and while an
area still holds types.

**Reporting role.** If a role named `cmdb_reporting` exists, the engine grants it `USAGE` on every
area schema and `SELECT` on every reporting view, and nothing else: it cannot read the `cmdb` tables
or the type tables directly. `POST /api/v1/schema-changes/reconcile` (and every `shadoucmdb migrate`)
catches up after the role is created later. See `sql/bootstrap/00_create_role_and_database.sql`.

**The move from `ci_attribute_values` (migration 0009).** Existing classes go into the area
`infrastruktur`; each gets its table, each attribute its column, and every value is copied into it.
For every attribute the number of values written must equal its number of rows in
`ci_attribute_values`, and the totals must match, or the migration fails and nothing changes. The
EAV table is then dropped. A required attribute whose CIs (deleted ones included) lack values stays
nullable and is reported as a warning in `schema_changes`. `sql/checks/` has scripts to compare every
value before and after the upgrade.

## Soft-delete decisions

| Table | Strategy | Why |
| --- | --- | --- |
| `configuration_items` | **Soft delete** (`deleted_at`) | A decommissioned server must still resolve in last quarter's report, in audit entries, and in historic relationships. All live-inventory indexes are partial on `deleted_at IS NULL`. |
| `ci_relationships` | **Soft delete** (`deleted_at`) | Answers "what did this app run on before the migration?" Uniqueness applies only to live edges, so a removed edge can be re-created. When the API soft-deletes a CI, it also soft-deletes that CI's live relationships in the same transaction. |
| Type tables (`<area>.<type>`) | **Follow the registry** | A CI's rows live as long as its `configuration_items` row (so a soft-deleted CI keeps its values). Clearing a field sets the column to NULL; the old value is kept in `audit_log`. |
| `areas`, `ci_classes`, `ci_attribute_definitions` | **Archive, then purge** (`is_active = false`) | They are database objects. DELETE archives and keeps all data; only a purge, typed to confirm, drops the schema, table or column. `schema_changes` and `audit_log` keep the history of both; purging a type also writes a `delete` row with the last state of every CI and relationship it removes. |
| `relationship_types`, `lookup_lists`, `lookup_list_values` (and the deprecated `statuses`, `environments`, `locations`, `owners`, which no CI references any more) | **Retire, don't delete** (`is_active = false`) | These are referenced by history. FKs are `ON DELETE RESTRICT`, so a referenced row cannot be hard-deleted (the API checks first and answers `409 IN_USE` with the counts, see `GET …/{id}/usage`); inactive rows keep resolving for old CIs and are hidden from pickers. Inactive classes cannot receive new CIs, inactive attributes and lookup values cannot receive new values, and inactive relationship types cannot receive new edges. An unused lookup list is deleted together with its values. |
| `relationship_type_rules` | **Hard delete** | Pure configuration. Removing a rule blocks new edges and leaves existing edges alone. |
| `audit_log` | **Append-only; pruned by age only** | UPDATE, DELETE and TRUNCATE are rejected by trigger and not granted to the API role. The only deletion path is the operator's `shadoucmdb prune-audit`, see [Retention and personal data](#retention-and-personal-data). |
| `users` | **Disable** (`is_active = false`), hard delete allowed | Disabling is the normal way to remove access and ends the user's sessions. A hard delete is allowed because nothing references a user by foreign key: `audit_log` keeps `actor_id` and `actor_name` as text, so history still names them. |
| `permission_profiles` and their permission rows, `user_permission_profiles` | **Hard delete** | Pure configuration; every change is in `audit_log` (a profile's before/after includes its permissions, a user's includes their profiles). Deleting a profile removes it from its holders. |
| `ui_settings`, `ui_settings_versions` | **Replaced, never deleted** | Saving creates a new version; history is append-only, so any earlier layout can be looked at and restored. |
| `ui_assets` | **Hard delete** | An image is current state only; the audit log keeps its metadata (type, size, hash). |
| `api_tokens` | **Revoke** (`revoked_at`), row kept | A revoked or expired token stays listed next to its audit rows. Deleting the owner deletes their tokens; the API writes a `delete` audit row for each first. |
| `user_totp`, `user_recovery_codes`, `mfa_challenges` | **Hard delete** | Turning MFA off (by the user or an administrator) deletes the authenticator and codes; the `mfa.disable` audit row is the history. A used recovery code keeps its row (`used_at`) until the codes are replaced. Challenges are deleted when used, after 5 wrong codes, at the next sign-in once expired, and by `prune-audit --scope auth`. |
| `identity_providers`, `identity_provider_group_mappings` | **Disable** (`is_enabled = false`), hard delete only without accounts | Disabling stops sign-ins through the provider and ends its accounts' sessions. A provider that accounts still belong to cannot be deleted (FK `RESTRICT`, checked by the API first: `409 IN_USE`). Mappings are configuration: replaced as a whole, history in `audit_log`. |
| `server_keys` | **Never changed by the API** | The API only reads and adds keys. To rotate the OIDC sign-in key, delete its row as the owner role (`DELETE FROM cmdb.server_keys WHERE purpose = 'oidc_state';`) and restart every API process: they generate a new key, and sign-ins in progress end with "expired" once. A restore does the same, since the table is not backed up. |
| `sessions` | **Hard delete** | Logout, disabling, password resets and expiry remove rows; expired rows are purged at each login, and `prune-audit` removes any left 30 days after their expiry. Every session the API ends (not expiry) leaves a `logout` or `session.revoke` row in `audit_log`. |

## Auditing

The API writes `audit_log` rows **in the same transaction** as the change, because only
the API knows the actor and the request. Database triggers are not used for auditing:
they cannot see the actor, and would log raw row images instead of API-level changes.
Changes made through the API record the signed-in user (`actor_type = 'user'`, `actor_id` =
the user's id, `actor_name` = their username). First-run setup, `create-admin` and `seed`
record `actor_type = 'system'`; a starter template install records the installing user (or `system` / `seed`
from the CLI), one `create` row per created row, sharing one `request_id`. Users and permission profiles are audited like everything
else (`entity_type` `users` / `permission_profiles`); password hashes never appear in it. UI settings saves are
audited as `ui_settings` updates (old and new version with their documents), logo and favicon changes as
`ui_assets` (metadata only, not the bytes). A configuration import writes one row per created or updated row with
the importing user as the actor; a dry run writes none.

Authentication events are audit rows too, with `entity_type = 'sessions'`, `old_value` NULL
and the details in `new_value` (every one also has `ipAddress` and `userAgent` of the request,
and `peerIpAddress`, the TCP peer, when that differs from `ipAddress`):

| `action` | Actor | `entity_id` | `new_value` |
| --- | --- | --- | --- |
| `login.success` | the user | the new session | `userId`, `username`, `method` (`password`, `totp`, `recovery_code`, `setup`, `oidc` or `ldap`) |
| `login.failure` | anonymous (`api_client`, no id) | a fresh id for the attempt | `attemptedUsername` (first 64 characters, as typed) |
| `login.locked` | anonymous | the failed attempt that set the lock | `attemptedUsername`, `lockedForSeconds` |
| `logout` | the user | the session | `userId`, `username`, `session` (`createdAt`, `ipAddress`, `userAgent`) |
| `session.revoke` | whoever caused it (an administrator, the user, `system`) | the ended session | as for `logout`, plus `reason`: `user_disabled`, `user_deleted`, `password_reset`, `password_changed`, `replaced` (a new sign-in in the same browser) or `provider_disabled` (its identity provider was disabled) |

A failed sign-in never says whether the username exists (a wrong password, an unknown name
and a disabled account look the same, and all three count towards the same login lock), so reading the audit log does not reveal account
names. Sign-ins refused with 429 while a name is locked are not recorded: they cost the
server nothing, and recording them would let an anonymous client grow `audit_log` at will.
Passwords, session tokens, token hashes and CSRF tokens are never written. The IP address is
evidence, not an access control: see [deployment](deployment.md#https-and-session-cookies)
for the proxy it assumes. Nothing alerts on these rows yet. With `AUDIT_CAPTURE_CLIENT_IP=false` or
`AUDIT_CAPTURE_USER_AGENT=false` those fields are `null` here and in `sessions`, and nothing logs them.

### Tamper evidence

`audit_log` rejects `UPDATE`, `DELETE` and `TRUNCATE` (triggers), and every row is hash-chained
(migration 0018). A `BEFORE INSERT` trigger sets `chain_seq` (1, 2, 3, … in commit order),
`prev_hash` (the previous row's `row_hash`; 32 zero bytes for the first) and

```
row_hash = sha256(prev_hash || jsonb_build_array(chain_seq, occurred_at (UTC, µs), actor_type, actor_id,
                  actor_name, action, entity_type, entity_id, old_value, new_value, request_id)::text)
```

whatever the inserting statement supplied. Inserts serialise on the one row of
`audit_log_chain_head` until they commit; a rolled-back insert leaves no gap.
`audit_log_verify()` (and `shadoucmdb audit-verify`, which exits non-zero) reports rows whose content no
longer matches their hash (`altered`), broken links (`relinked`), missing `chain_seq` values (`gap`) and a
deleted tail (`tail`). A superuser can still rewrite the whole chain consistently; the defence against that
is the off-host copy: `AUDIT_EXPORT` sends every row with its `rowHash` to a SIEM, and `audit-verify`
prints the chain head to compare with it. Rows that existed before migration 0018 were chained in `id`
order when it ran.

Two-factor authentication events have `entity_type = 'users'` and the user's id as `entity_id`, `old_value` NULL,
and `userId`, `username`, `ipAddress`, `userAgent` in `new_value`:

| `action` | Actor | `new_value` also has |
| --- | --- | --- |
| `mfa.enrol` | the user | `method` (`totp`), `recoveryCodes` (how many were issued) |
| `mfa.disable` | the user, or the administrator who reset it | `reason`: `self_service` or `admin_reset` |
| `mfa.failure` | anonymous at sign-in, else the user | `stage`: `login`, `disable` or `recovery_codes` (a wrong or replayed code); outside sign-in `lockedForSeconds` when it set the lock |
| `mfa.recovery_code_used` | the user | `stage`, `recoveryCodesRemaining` |
| `mfa.recovery_codes` | the user | `recoveryCodes` (new codes replaced the old ones) |

Identity providers (`entity_type = 'identity_providers'`) record `create`, `update` and `delete` with the API
representation (settings and group mappings; secrets only as `clientSecretSet` / `bindPasswordSet`). The account
changes a sign-in through a provider makes (creating the account, new name, e-mail or profiles) are `create` and
`update` rows on `users` with `actor_type = 'system'` and `actor_name = 'identity provider "<name>"'`. A sign-in
the provider vouched for but ShadouCMDB refused (no mapped group, name taken, account disabled) is a
`login.failure` with the name the provider sent; a callback without this browser's pending sign-in is not recorded.

A wrong code at sign-in that sets the username's lock also writes `login.locked`. No TOTP secret, code, recovery
code or hash is ever written.

API tokens (`entity_type = 'api_tokens'`, `entity_id` = the token): creating one is a `create` row and revoking it
an `update` row (old and new token, never the secret or its hash). **Every request made with a known token** is a
`token.use` row whose `new_value` has `tokenName`, `tokenPrefix`, `userId`, `username`, `outcome` (`accepted`,
`revoked`, `expired`, `owner_disabled`, `no_scope`, `session_only` or `forbidden`), `method`, `path`,
`operationId`, `ipAddress` and `userAgent`. Its actor is the owner with `actor_type = 'api_client'`, as for the
changes the request makes, which share its `request_id`. A made-up token matches no row and is not recorded
(anyone could grow the table that way); the server logs a warning instead. A class-permission refusal inside a
service is recorded as `accepted` (the route let the token in); the response says `403`.

## Retention and personal data

**Personal data.** An IP address or user agent tied to a user is personal data (GDPR Art. 4(1)).
These fields hold it:

| Where | Fields |
| --- | --- |
| `audit_log`, `entity_type = 'sessions'` (`login.*`, `logout`, `session.revoke`) | `new_value.ipAddress`, `new_value.peerIpAddress`, `new_value.userAgent`, `new_value.session.ipAddress`, `new_value.session.userAgent`, and the user named in `actor_*`, `new_value.username` / `attemptedUsername` |
| `audit_log`, `mfa.*` rows (`entity_type = 'users'`) | `new_value.ipAddress`, `new_value.peerIpAddress`, `new_value.userAgent`, `new_value.username` |
| `audit_log`, `entity_type = 'api_tokens'` (`token.use`) | `new_value.ipAddress`, `new_value.userAgent`, `new_value.username`, and the owner in `actor_*` |
| `sessions` | `ip_address`, `user_agent` |
| `api_tokens` | `last_used_ip` |
| `users` | `username`, `display_name`, `email` |
| `audit_log`, `entity_type = 'users'` and every row's `actor_name` | the same user details, as history |

**Retention policy** (decided in SHAA-54):

| Data | Kept | How it goes |
| --- | --- | --- |
| Authentication events (including `mfa.*`) and `token.use` rows in `audit_log` | **180 days** | `shadoucmdb prune-audit --older-than 180d --execute`, run by the operator (scope `auth`, the default) |
| CI and configuration change history in `audit_log` (`create`, `update`, `delete`, `restore`) | **Indefinitely** | Only if an operator explicitly runs `prune-audit --scope changes` |
| `sessions` rows | Until **30 days after expiry**; revoked sessions are deleted at once | Deleted at sign-in once expired; `prune-audit` (scope `auth`) removes any older than 30 days past expiry |
| `audit.purge` rows | **Forever** | Never deleted, not even by the purge |

Nothing is deleted automatically: no timer, no setting. The operator schedules the command
(cron, a systemd timer, Task Scheduler) if they want it regular; see
[deployment](deployment.md#audit-log-retention).

**How the purge stays safe.** `prune_audit_log()` is a `SECURITY DEFINER` function owned by
the schema owner. Only `shadoucmdb_maintenance` may execute it; the API role cannot, and has
no UPDATE, DELETE or TRUNCATE on `audit_log`. The function deletes by age only (no other
filter), refuses a window whose cutoff is under **30 days** ago (checked on the cutoff, so a
`1 month` window counts as the calendar month it is), so recent evidence of an attack cannot be
removed through it, and writes an `audit.purge` row in the same transaction: `scope`,
`olderThan`, `cutoff`, `deleted` (rows per action), `sessionsDeleted`, `databaseUser` and
`clientAddress` (from the connection) and `operator` (the OS user the command reports). The
append-only trigger lets a DELETE through only while the function runs as the table owner,
and never for an `audit.purge` row. Because the function runs with the owner's rights, its
`search_path` is `pg_catalog, pg_temp` and it names its tables by schema: a function or
aggregate another role planted in `public` can never be resolved in its place. For the same
reason no role but the owner may create objects in `public` (PostgreSQL 14 allows it by
default; the bootstrap scripts and migration 0007 revoke it). Migration 0008 moved the function into
`cmdb` with the other system objects (`cmdb.prune_audit_log`); migration 0010 added `token.use` to the `auth` scope, and 0013 the `mfa.*` events (and the clean-up of expired `mfa_challenges`). The schema owner remains able to change anything, which
is why its credentials belong to migrations only, not to the running server.

**Erasure for one person (GDPR Art. 17) is not supported.** It conflicts with an append-only
audit trail: deleting or rewriting one user's rows is exactly what the trail exists to
prevent. Until that is decided separately, a person's authentication records leave with the
180-day window, and their change history (`actor_name`, user snapshots) stays. Deleting a
user removes their account and sessions, not their history.

## Dependent lookup lists

A lookup list can depend on another list (migration `0015_lookup_parent_lists`): each value of the
child list names the value of the parent list it belongs to. The web UI then offers, in a child
dropdown, only the values of the chosen parent value (`GET /api/v1/lookup-list-values?parentValueId=…`)
and keeps the child empty until the parent is chosen.

```
lookup_lists:        manufacturer  <── (parent_list_id) ──  model
lookup_list_values:  cisco         <── (parent_value_id) ── c9300, c9500
                     hpe           <── (parent_value_id) ── dl380
fields of "server":  manufacturer  <── (parent_attribute_id) ── model
```

- **Fields.** A lookup field on a child list names its parent field (`parent_attribute_id`): the
  lookup field bound to the parent list, on the same class or an ancestor. When a CI is created or
  updated, the API refuses (`400`, per field) a child value whose parent value is not the CI's value
  of the parent field (`lookup_parent_mismatch`), and a child value while the parent field is empty
  (`lookup_parent_missing`). A child may always be empty. The check covers the fields a request sets
  or clears and those whose parent field it sets or clears, so editing another field of a CI stored
  before the rule existed is not refused. The parent field is optional: a child-list field without
  one accepts any active value of its list, as before.
- **A list gets another parent list** (or none): in the same transaction the API unassigns every
  value's `parent_value_id` and every bound field's `parent_attribute_id` (each change audited as an
  `update`); the administrator then reassigns them. An unassigned value cannot be chosen on a field
  with a parent field. Existing CI values are not changed.
- **Retiring a parent value** (`isActive: false`) retires every active value that belongs to it,
  down the chain of dependent lists, each audited. It is refused with `409 IN_USE` while CIs store one
  of those dependent values, because the CIs would keep a model whose manufacturer can no longer be
  chosen; change those CIs first. Reactivating a parent does not reactivate its dependents, and a
  value whose parent is retired can be neither created nor reactivated (`parent_value_inactive`).
- **Deleting a parent value** is refused (`409 IN_USE`, usage kind `childValues`) while any value
  belongs to it; so is deleting a list other lists depend on (`childLists`) and purging a field that
  other fields name as their parent field.
- **Moving a type** to another parent type is refused while one of its fields (or of its subtypes)
  would lose its parent field from the lineage (`parent_field_outside_lineage`).

**In the web UI** (*Administration › Data model › Dropdowns*, which replaces *Lookups › Lists*; the old
address `/admin/lookups/lists` redirects there):

- A list's **Parent list** is set in its edit dialog; the lists table shows it. Lists that depend on the
  list being edited are not offered, since they would form a cycle.
- The values of a child list show the parent value each **belongs to**. The value dialog picks it, and a
  new value added while the table is filtered starts with that parent value. The values can be filtered
  by parent value (or *Not assigned*); the filter is part of the URL (`?list=…&parent=<id>|none`) and is
  applied by the API. Reordering a filtered table keeps the positions those values had among the others.
- The attribute dialog of a lookup field on a child list offers **Parent field**: the lookup fields of the
  class and its ancestors bound to the parent list. With exactly one candidate, a new field starts with it.
- On the CI form a child dropdown is disabled with "Choose <parent field> first" until the parent field
  has a value, and then lists only the values of that parent value. Choosing another parent value clears a
  child value the new parent does not offer; clearing the parent clears the child. A stored child value
  that does not belong to the current parent value (saved before the rule existed) is shown as such
  rather than silently dropped.

The database enforces the list, value and field rules (triggers and foreign keys, see `verify`);
that a CI's value belongs to its parent field's value is checked by the API, because the values
live in the per-type tables.

## Indexes for UI queries

| Query | Index |
| --- | --- |
| Inventory list sorted by label / recently updated | `configuration_items_live_label_idx` (`lower(label), id`), `configuration_items_live_updated_idx`; both partial on live rows, keyset-pagination friendly |
| Filter by class (sorted by label) | `configuration_items_class_label_idx` (`class_id, lower(label)`, live rows) |
| Active / inactive | `configuration_items_validity_idx` (`valid_until, valid_from`, live rows) |
| Ident lookup | `configuration_items_ident_uq` (unique, `lower(ident)`) and trigram GIN on `ident` |
| Global search | `search_vector` (generated tsvector over label and ident) with GIN; trigram GIN on `label` for `ILIKE '%…%'`; field values (hostname, notes, …) through the type tables, see below |
| Filter by status, environment, owner, location or any lookup value | `lookupValueId`: the lookup columns' `ix_<field id>` indexes in the type tables |
| IP lookup and subnet containment | `ipWithin` and IP search scan the ip columns of the type tables (`<<=`); add a GiST `inet_ops` index on a hot column by hand if needed |
| Relationship traversal (outgoing / incoming) | `ci_relationships_source_idx`, `ci_relationships_target_idx` (partial, live edges) |
| Field values | Each type table is keyed by `id`; reference and lookup columns have an index (`ix_<field id>`) for reverse lookups and deletes. Search over text/enum/ip/cidr columns scans the type tables (`OR ci.id IN (SELECT id FROM <table> WHERE …)`); add an index on a hot column by hand if needed, the engine leaves foreign indexes alone |
| Entity history | `audit_log_entity_idx` (`entity_type, entity_id, occurred_at desc`) |

Helper SQL functions for the API: `cmdb.ci_class_lineage(class_id)` returns the class and its
ancestors with depth, and is used to collect inherited attribute definitions.
`cmdb.ci_class_is_a(class_id, ancestor_id)` checks class membership. `cmdb.type_table(class_id)`
returns the quoted table name (`"bestand"."netzwerk"`), `cmdb.attribute_value_count(attribute_id)` and
`cmdb.lookup_value_count(value_id)` count stored values.

## Extending the model without migrations

Adding a "Load Balancer" type with its own fields is three API calls; the engine does the DDL:

```sh
POST /api/v1/ci-classes             {"name": "Load balancer", "areaId": "<infrastruktur>", "parentId": "<network_device>"}
  -> CREATE TABLE "infrastruktur"."load_balancer" (id uuid PRIMARY KEY REFERENCES cmdb.configuration_items (id) ON DELETE CASCADE)
POST /api/v1/attribute-definitions  {"classId": "<load_balancer>", "label": "Algorithm", "dataType": "enum", "enumValues": ["round_robin", "least_conn"]}
  -> ALTER TABLE "infrastruktur"."load_balancer" ADD COLUMN "algorithm" text
  -> ALTER TABLE "infrastruktur"."load_balancer" ADD CONSTRAINT "ck_…" CHECK ("algorithm" = ANY ('{round_robin,least_conn}'::text[]))
POST /api/v1/attribute-definitions  {"classId": "<load_balancer>", "key": "vip", "label": "Virtual IP", "dataType": "ip"}
  -> ALTER TABLE "infrastruktur"."load_balancer" ADD COLUMN "vip" inet
```

Each call also (re)creates the reporting view `infrastruktur.v_load_balancer`. Load balancer CIs
inherit the `network_device` and `hardware` fields (stored in those types' tables) and the
`located_in` / `connected_to` rules. `shadoucmdb verify` exercises exactly this.

Never create or alter type tables by hand: the engine compares the catalog with the metadata and
would report or undo the difference. Views, indexes and tables a DBA adds in an area schema under
other names are left alone.

## Known limits, deliberately deferred

- An attribute key redefined on a child class that already exists on an ancestor is not
  rejected by the database; the API rejects it (`422 INVALID_NAME`, `name_taken`).
- `is_required` is enforced by the database (`NOT NULL`) for active fields; `validation`
  (min/max/pattern), `default_value`, the class of a referenced CI and the list of a lookup value
  are enforced by the API at write time.
- Re-parenting a type that already has CIs moves their rows between the ancestor tables in the same
  transaction; it is refused while values exist in a table the CIs would leave, or while the new
  ancestors have required fields.
- A type cannot move to another area, and a field cannot become a reference or lookup field (add a
  new field instead).
- Reserved seams, not built: SSO/LDAP/OIDC sign-in (a new login route that starts the same
  kind of session), linking `owners` to `users` (via `owners.external_ref`), discovery/import
  (`audit_log.actor_type = 'import'`), integrations and reporting.
- Class permissions do not inherit down the class tree: a grant on `hardware` does not cover
  `server`. Use the wildcard or grant each class.
- The label is maintained by the API. A value written into a type table by hand (never do that)
  leaves the label stale until the CI is next saved or its class's title attribute changes.
- Only the Administrator profile may change idents; there is no separate permission for it.
