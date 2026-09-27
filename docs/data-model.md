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
`0008_cmdb_schema_and_areas`, `0009_type_tables`).
SQL that reads and writes them: `backend/src/data/`; the DDL engine: `backend/src/schema/`.

Every system table lives in the **`cmdb` schema** (the application connects with
`search_path = cmdb, public`; `public` keeps the pg_trgm functions and sqlx's
`_sqlx_migrations`). Area schemas sit next to it, so an administrator's names can
never collide with the application's.

## Tables

```
areas ─< ci_classes ─┬─< ci_attribute_definitions >─── (reference_class_id) ─> ci_classes
(schema)  (parent)   │         └── (lookup_list_id) ─> lookup_lists ─< lookup_list_values
                     │                                                      ^ (lookup fields)
                     └─< configuration_items ─┬─── <area>.<type> (id = configuration_items.id)
                  │ │ │ │                     │        (reference fields) ──> configuration_items
   statuses ──────┘ │ │ │                     └─< ci_relationships (source / target)
   environments ────┘ │ │                        │
   owners ────────────┘ │           relationship_types ─< relationship_type_rules >─ ci_classes (source / target)
   locations (parent) ──┘
audit_log (append-only; entity_type + entity_id point at any row)

users ─< user_permission_profiles >─ permission_profiles ─┬─< permission_profile_global_permissions
  │                                                        └─< permission_profile_class_permissions >─ ci_classes (NULL = all)
  └─< sessions

ui_settings (one row) ── (version) ─> ui_settings_versions (append-only)
ui_assets (logo, favicon)
```

| Table | Purpose | Key constraints |
| --- | --- | --- |
| `areas` | The top-level groups shown as menu tabs. `key` is the PostgreSQL schema holding the tables of the area's types. `is_active = false` archives the area. | unique `key`; key format `^[a-z][a-z0-9_]{0,62}$`; not a system schema (`cmdb`, `public`, `information_schema`, `pg_*`, `cmdb_*`); `key` immutable (trigger) |
| `ci_classes` | CI types in a single-inheritance tree (`parent_id`). `is_abstract` classes group attributes and rules but hold no CIs. `icon`, `color` (`#rrggbb`) and `sort_order` drive menus and badges. `area_id` is the area whose schema holds the type's table, named after `key`. | unique `key` (across areas); key format check; at most 61 characters and no `v_` prefix (the reporting view is `v_<key>`); colour format; no self-parent; **no cycles** (trigger); `key` and `area_id` immutable (trigger) |
| `ci_attribute_definitions` | Typed attribute per class, inherited by descendant classes. Types: `text`, `number`, `integer`, `boolean`, `enum`, `date`, `datetime`, `ip`, `cidr`, `reference`, `lookup`. `group_name` is the form section, `sort_order` the order within it; `help_text` is shown on forms; `default_value` (jsonb, same shape as an API value) is applied to new CIs. | unique (`class_id`, `key`); `enum_values` required iff enum; `reference_class_id` required iff reference; `lookup_list_id` required iff lookup (FK, RESTRICT); a default is never JSON null and never on a reference; `key` (the column name) and `class_id` immutable (trigger); `key` is never `id` |
| `lookup_lists` | Lists an administrator defines, e.g. "Support contract". | unique `key`; key format; non-blank name |
| `lookup_list_values` | The values of a list (`key`, `name`, `color`, `sort_order`, `is_active`). | unique (`list_id`, `key`); cascades with the list; cannot move to another list (trigger) |
| `configuration_items` | CI instances with the common core: name, class, status, owner, location, environment, hostname, `ip_address inet`, serial, notes, plus `version` for optimistic locking. | FKs to class and every lookup; class must be concrete and active (trigger); non-blank name; hostname format |
| `<area>.<type>` (type tables) | One per type, e.g. `bestand.netzwerk`: `id uuid PRIMARY KEY REFERENCES cmdb.configuration_items (id) ON DELETE CASCADE`, then one column per field of the type. A CI has a row in the table of its type **and of every ancestor type** (class table inheritance: a server's inherited `hardware` fields are in `infrastruktur.hardware`). Column types: text → `text`, enum → `text` with a CHECK on the allowed values, number → `numeric`, integer → `bigint`, boolean → `boolean`, date → `date`, datetime → `timestamptz`, ip → `inet`, cidr → `cidr`, reference → `uuid` FK to `configuration_items` (NO ACTION), lookup → `uuid` FK to `lookup_list_values` (RESTRICT). | PK/FK to the registry; enum CHECK `ck_<field id>_<hash of the values>`; FKs `fk_<field id>` with index `ix_<field id>`; a required, active field is `NOT NULL`. Constraint names never contain user text. That a reference points at a CI of the right type, that a lookup value belongs to the field's list, and min/max/pattern rules are checked by the API |
| `<area>.v_<type>` (reporting views) | Read-only view per type: the registry columns (`id`, `name`, `type`, `status`, `environment`, `owner`, `location`, `hostname`, `ip_address`, `serial_number`, `notes`, `record_version`, `created_at`, `updated_at`, `deleted_at`) plus every field of the type and its ancestors; lookup fields show the value's key. Deleted CIs are included (filter on `deleted_at IS NULL` for the live inventory). | rebuilt by the DDL engine when the type or an ancestor changes; marked with a `shadoucmdb:<hash>` comment so the engine never touches a view it did not create |
| `schema_changes` | Every DDL plan the application ran: actor, time, request id, a one-line summary, the exact statements in order, and their impact on data (rows converted, values dropped, warnings). | **UPDATE/DELETE rejected** (trigger); at least one statement |
| `relationship_types` | `runs_on`, `depends_on`, `located_in`, `connected_to`, …, with `forward_label` / `reverse_label` and `is_directional`. | unique `key` |
| `relationship_type_rules` | Legal (source class, target class) pairs per type. A rule matches the named class **and all its descendants**. | unique triple |
| `ci_relationships` | Typed, directional edge `source_ci_id → target_ci_id`. | **no self-edges** (check); **no duplicate live edges** (partial unique index); for non-directional types the reverse edge also counts as a duplicate (trigger + advisory lock); endpoints must satisfy a rule and must not be soft-deleted (trigger) |
| `statuses` | CI lifecycle (`planned`, `in_service`, `maintenance`, `retired`, `disposed`). `is_operational` flags "live" statuses for reporting. | unique `key` |
| `environments` | `production`, `staging`, `test`, `development`, `disaster_recovery`. | unique `key` |
| `locations` | Location hierarchy (region › site › building › floor › room › rack, plus `cloud_region`). | unique `key`; `location_type` check; no cycles (trigger) |
| `owners` | Accountable people or teams (`kind` = `person` / `team`). This is not a login table (see `users`). `external_ref` is the seam for a later directory/IdP link. | `kind` check; unique `external_ref`; email format |
| `users` | Local accounts: `username`, `display_name`, `email`, `is_active`, argon2id `password_hash` (PHC string, never returned by the API), `password_changed_at`, `last_login_at`. | unique `lower(username)`; username format; `password_hash LIKE '$argon2id$%'`; email format |
| `permission_profiles` | Named sets of permissions. `is_builtin` marks the one Administrator profile (created by the migration), which holds every permission implicitly. | unique `lower(name)`; at most one built-in; built-in cannot be updated or deleted (trigger) |
| `permission_profile_global_permissions` | (`profile_id`, `permission`) for `users.manage`, `profiles.manage`, `datamodel.manage`, `customization.manage`, `config.export_import`, `audit.view`. | PK; permission check; no rows for the built-in profile (trigger) |
| `permission_profile_class_permissions` | `can_view` / `can_create` / `can_edit` / `can_delete` per profile and class; `class_id` NULL is the "all classes" wildcard. | one row per (profile, class) and one wildcard per profile (partial unique indexes); `can_view` required; cascades with the class and the profile |
| `user_permission_profiles` | Which profiles each user holds (any number). | PK (`user_id`, `profile_id`); **never zero active users holding the Administrator profile** (deferred constraint trigger, serialised by an advisory lock) |
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
  A Location-class CI sets its core `location_id` to the matching `locations` row, which ties the
  relationship graph to the lookup hierarchy used for filtering.

### Bare start and the IT infrastructure starter template

`shadoucmdb seed` loads only system rows (today it checks the built-in Administrator profile that
migration 0003 creates). A fresh install therefore has no classes, attributes, relationship types or
lookups. What `seed` loaded before SHAA-31 is now the **`it_infrastructure` starter template**
(`backend/src/modules/templates/`), installed through `POST /api/v1/admin/templates/it_infrastructure/install`
or `shadoucmdb seed --template it_infrastructure`, or with one click under *Administration › Templates* in
the web UI. Installing matches rows by key, adds only what is missing, never changes existing rows and audits
every row it creates, so it is idempotent. Everything the template adds (and anything else in the data model)
can then be changed under *Administration › CI classes, Relationship types and Lookups*. Databases
seeded before migration 0004 keep all their rows; the template then reports `installed`.

The template contains:

- Classes: `hardware` (abstract) › `server`, `network_device`; `virtual_machine`, `application`,
  `database`, `service`, `location`. There are 35 attribute definitions across them, including an
  `application.primary_database` **reference** attribute.
- Relationship rules: `runs_on` (app→server/VM, db→server/VM, VM→server), `depends_on`
  (app→db/app, service→app/service), `located_in` (hardware→location, location→location),
  `connected_to` (hardware↔hardware).
- Statuses, environments and a small location tree (EMEA › FRA1 › room › rack, Americas › NYC1,
  AWS eu-central-1).
- `seed --demo` installs the template, then adds three owner teams, 8 CIs, 24 attribute values and
  8 relationships (a CRM service down to its rack).
- It installs into the area **Infrastruktur** (schema `infrastruktur`), creating the area if needed,
  and builds the type tables and reporting views in the same transaction.

## Areas, type tables and the DDL engine

**Technical names.** Areas, types and fields have a display name, which can be changed at any time,
and a technical name (`key`), which is the schema, table or column name and never changes once
created. The API derives it from the display name unless one is given: lower case, `ä`→`ae`,
`ö`→`oe`, `ü`→`ue`, `ß`→`ss` (and other accents dropped), every other character becomes `_`, runs of
`_` collapse, a leading digit gets a prefix (`type_2024_…`). "Virtuelle Maschinen" →
`virtuelle_maschinen`, "Größe" → `groesse`. `GET /api/v1/technical-names` previews it. A name must
match `^[a-z][a-z0-9_]{0,62}$` (types: at most 61 characters, since the view adds `v_`), so it never
needs quoting in a report. Refused with `422 INVALID_NAME` and the reason: reserved SQL keywords,
`pg_` prefixes, system schemas (`cmdb`, `public`, `information_schema`, `cmdb_*`), `v_` for types,
registry column names (`id`, `name`, `status`, …) for fields, and names already taken (types are
unique across areas; a field is unique within its type's lineage).

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
| `areas`, `ci_classes`, `ci_attribute_definitions` | **Archive, then purge** (`is_active = false`) | They are database objects. DELETE archives and keeps all data; only a purge, typed to confirm, drops the schema, table or column. `schema_changes` and `audit_log` keep the history of both. |
| `relationship_types`, `statuses`, `environments`, `locations`, `owners`, `lookup_lists`, `lookup_list_values` | **Retire, don't delete** (`is_active = false`) | These are referenced by history. FKs are `ON DELETE RESTRICT`, so a referenced row cannot be hard-deleted (the API checks first and answers `409 IN_USE` with the counts, see `GET …/{id}/usage`); inactive rows keep resolving for old CIs and are hidden from pickers. Inactive classes cannot receive new CIs, inactive attributes and lookup values cannot receive new values, and inactive relationship types cannot receive new edges. An unused lookup list is deleted together with its values. |
| `relationship_type_rules` | **Hard delete** | Pure configuration. Removing a rule blocks new edges and leaves existing edges alone. |
| `audit_log` | **Append-only; pruned by age only** | UPDATE, DELETE and TRUNCATE are rejected by trigger and not granted to the API role. The only deletion path is the operator's `shadoucmdb prune-audit`, see [Retention and personal data](#retention-and-personal-data). |
| `users` | **Disable** (`is_active = false`), hard delete allowed | Disabling is the normal way to remove access and ends the user's sessions. A hard delete is allowed because nothing references a user by foreign key: `audit_log` keeps `actor_id` and `actor_name` as text, so history still names them. |
| `permission_profiles` and their permission rows, `user_permission_profiles` | **Hard delete** | Pure configuration; every change is in `audit_log` (a profile's before/after includes its permissions, a user's includes their profiles). Deleting a profile removes it from its holders. |
| `ui_settings`, `ui_settings_versions` | **Replaced, never deleted** | Saving creates a new version; history is append-only, so any earlier layout can be looked at and restored. |
| `ui_assets` | **Hard delete** | An image is current state only; the audit log keeps its metadata (type, size, hash). |
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
| `login.success` | the user | the new session | `userId`, `username`, `method` (`password` or `setup`) |
| `login.failure` | anonymous (`api_client`, no id) | a fresh id for the attempt | `attemptedUsername` (first 64 characters, as typed) |
| `login.locked` | anonymous | the failed attempt that set the lock | `attemptedUsername`, `lockedForSeconds` |
| `logout` | the user | the session | `userId`, `username`, `session` (`createdAt`, `ipAddress`, `userAgent`) |
| `session.revoke` | whoever caused it (an administrator, the user, `system`) | the ended session | as for `logout`, plus `reason`: `user_disabled`, `user_deleted`, `password_reset`, `password_changed` or `replaced` (a new sign-in in the same browser) |

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
(migration 0010). A `BEFORE INSERT` trigger sets `chain_seq` (1, 2, 3, … in commit order),
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
prints the chain head to compare with it. Rows that existed before migration 0010 were chained in `id`
order when it ran.

## Retention and personal data

**Personal data.** An IP address or user agent tied to a user is personal data (GDPR Art. 4(1)).
These fields hold it:

| Where | Fields |
| --- | --- |
| `audit_log`, `entity_type = 'sessions'` (`login.*`, `logout`, `session.revoke`) | `new_value.ipAddress`, `new_value.peerIpAddress`, `new_value.userAgent`, `new_value.session.ipAddress`, `new_value.session.userAgent`, and the user named in `actor_*`, `new_value.username` / `attemptedUsername` |
| `sessions` | `ip_address`, `user_agent` |
| `users` | `username`, `display_name`, `email` |
| `audit_log`, `entity_type = 'users'` and every row's `actor_name` | the same user details, as history |

**Retention policy** (decided in SHAA-54):

| Data | Kept | How it goes |
| --- | --- | --- |
| Authentication events in `audit_log` | **180 days** | `shadoucmdb prune-audit --older-than 180d --execute`, run by the operator (scope `auth`, the default) |
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
`cmdb` with the other system objects (`cmdb.prune_audit_log`). The schema owner remains able to change anything, which
is why its credentials belong to migrations only, not to the running server.

**Erasure for one person (GDPR Art. 17) is not supported.** It conflicts with an append-only
audit trail: deleting or rewriting one user's rows is exactly what the trail exists to
prevent. Until that is decided separately, a person's authentication records leave with the
180-day window, and their change history (`actor_name`, user snapshots) stays. Deleting a
user removes their account and sessions, not their history.

## Indexes for UI queries

| Query | Index |
| --- | --- |
| Inventory list sorted by name / recently updated | `configuration_items_live_name_idx` (`lower(name), id`), `configuration_items_live_updated_idx`; both partial on live rows, keyset-pagination friendly |
| Filter by class / status / owner / location / environment | `configuration_items_{class,status,owner,location,environment}_idx` (partial, live rows) |
| Global search | `search_vector` (generated tsvector over name, hostname, serial, notes) with GIN; trigram GIN on `name`, `hostname`, `serial_number` for `ILIKE '%…%'` |
| IP lookup and subnet containment | GiST `inet_ops` on `ip_address` (`ip_address << '10.0.0.0/8'`) |
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
