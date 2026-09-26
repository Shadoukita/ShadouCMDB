# ShadouCMDB data model

The schema is a **generic CI model**. CI types, their attributes and the legal
relationships between them are all rows, so new asset types need no
migration and no code change. Integrity is enforced in PostgreSQL itself
(foreign keys, `NOT NULL`, unique and check constraints, plus triggers for rules
that need to look at other rows), not only in the API.

Migrations: [`sql/migrations/`](../sql/migrations/)
(`0000_extensions`, `0001_core_schema`, `0002_integrity_triggers`,
`0003_users_and_permission_profiles`, `0004_data_model_admin`).
SQL that reads and writes them: `backend/src/data/`.

## Tables

```
ci_classes ─┬─< ci_attribute_definitions >─── (reference_class_id) ─> ci_classes
  (parent)  │         └── (lookup_list_id) ─> lookup_lists ─< lookup_list_values
            │                                                      ^ (value_lookup_id)
            └─< configuration_items ─┬─< ci_attribute_values >── ci_attribute_definitions
                  │ │ │ │            │        (value_ref_ci_id) ──> configuration_items
   statuses ──────┘ │ │ │            └─< ci_relationships (source / target)
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
| `ci_classes` | CI types in a single-inheritance tree (`parent_id`). `is_abstract` classes group attributes and rules but hold no CIs. `icon`, `color` (`#rrggbb`) and `sort_order` drive menus and badges. | unique `key`; key format check; colour format; no self-parent; **no cycles** (trigger) |
| `ci_attribute_definitions` | Typed attribute per class, inherited by descendant classes. Types: `text`, `number`, `integer`, `boolean`, `enum`, `date`, `datetime`, `ip`, `cidr`, `reference`, `lookup`. `group_name` is the form section, `sort_order` the order within it; `help_text` is shown on forms; `default_value` (jsonb, same shape as an API value) is applied to new CIs. | unique (`class_id`, `key`); `enum_values` required iff enum; `reference_class_id` required iff reference; `lookup_list_id` required iff lookup (FK, RESTRICT); a default is never JSON null and never on a reference |
| `lookup_lists` | Lists an administrator defines, e.g. "Support contract". | unique `key`; key format; non-blank name |
| `lookup_list_values` | The values of a list (`key`, `name`, `color`, `sort_order`, `is_active`). | unique (`list_id`, `key`); cascades with the list; cannot move to another list (trigger) |
| `configuration_items` | CI instances with the common core: name, class, status, owner, location, environment, hostname, `ip_address inet`, serial, notes, plus `version` for optimistic locking. | FKs to class and every lookup; class must be concrete and active (trigger); non-blank name; hostname format |
| `ci_attribute_values` | One row per CI and attribute, with a typed column per data type (`value_text`, `value_number`, `value_boolean`, `value_date`, `value_datetime`, `value_ip`, `value_cidr`, `value_ref_ci_id`, `value_lookup_id`). | unique (`ci_id`, `attribute_id`); exactly one value column set; `value_lookup_id` FK (RESTRICT); trigger checks the attribute belongs to the CI's class lineage, the column matches `data_type`, integers are whole, enum values are allowed, references point at the right class, and lookup values come from the attribute's list |
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
| `audit_log` | actor (`actor_type`, `actor_id`, `actor_name`), `action`, `entity_type`, `entity_id`, `occurred_at`, `old_value`, `new_value` (jsonb), `request_id`. | action/actor checks; old/new presence per action; **UPDATE/DELETE rejected** (trigger) |

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

## Soft-delete decisions

| Table | Strategy | Why |
| --- | --- | --- |
| `configuration_items` | **Soft delete** (`deleted_at`) | A decommissioned server must still resolve in last quarter's report, in audit entries, and in historic relationships. All live-inventory indexes are partial on `deleted_at IS NULL`. |
| `ci_relationships` | **Soft delete** (`deleted_at`) | Answers "what did this app run on before the migration?" Uniqueness applies only to live edges, so a removed edge can be re-created. When the API soft-deletes a CI, it also soft-deletes that CI's live relationships in the same transaction. |
| `ci_attribute_values` | **Hard delete** | A value is part of the CI's current state. Clearing a field deletes the row, and the old value is kept in `audit_log`. Rows cascade if a CI is ever purged. |
| `ci_classes`, `ci_attribute_definitions`, `relationship_types`, `statuses`, `environments`, `locations`, `owners`, `lookup_lists`, `lookup_list_values` | **Retire, don't delete** (`is_active = false`) | These are referenced by history. FKs are `ON DELETE RESTRICT`, so a referenced row cannot be hard-deleted (the API checks first and answers `409 IN_USE` with the counts, see `GET …/{id}/usage`); inactive rows keep resolving for old CIs and are hidden from pickers. Inactive classes cannot receive new CIs, inactive attributes and lookup values cannot receive new values, and inactive relationship types cannot receive new edges. An unused lookup list is deleted together with its values. |
| `relationship_type_rules` | **Hard delete** | Pure configuration. Removing a rule blocks new edges and leaves existing edges alone. |
| `audit_log` | **Never deleted** | Append-only by trigger. Retention/archival is an operator decision for a later milestone. |
| `users` | **Disable** (`is_active = false`), hard delete allowed | Disabling is the normal way to remove access and ends the user's sessions. A hard delete is allowed because nothing references a user by foreign key: `audit_log` keeps `actor_id` and `actor_name` as text, so history still names them. |
| `permission_profiles` and their permission rows, `user_permission_profiles` | **Hard delete** | Pure configuration; every change is in `audit_log` (a profile's before/after includes its permissions, a user's includes their profiles). Deleting a profile removes it from its holders. |
| `ui_settings`, `ui_settings_versions` | **Replaced, never deleted** | Saving creates a new version; history is append-only, so any earlier layout can be looked at and restored. |
| `ui_assets` | **Hard delete** | An image is current state only; the audit log keeps its metadata (type, size, hash). |
| `sessions` | **Hard delete** | Logout, disabling, password resets and expiry remove rows; expired rows are purged at each login. Every session the API ends (not expiry) leaves a `logout` or `session.revoke` row in `audit_log`. |

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
for the proxy it assumes. Nothing alerts on these rows yet.

## Indexes for UI queries

| Query | Index |
| --- | --- |
| Inventory list sorted by name / recently updated | `configuration_items_live_name_idx` (`lower(name), id`), `configuration_items_live_updated_idx`; both partial on live rows, keyset-pagination friendly |
| Filter by class / status / owner / location / environment | `configuration_items_{class,status,owner,location,environment}_idx` (partial, live rows) |
| Global search | `search_vector` (generated tsvector over name, hostname, serial, notes) with GIN; trigram GIN on `name`, `hostname`, `serial_number` for `ILIKE '%…%'` |
| IP lookup and subnet containment | GiST `inet_ops` on `ip_address` (`ip_address << '10.0.0.0/8'`) |
| Relationship traversal (outgoing / incoming) | `ci_relationships_source_idx`, `ci_relationships_target_idx` (partial, live edges) |
| Filter by attribute value | `ci_attribute_values_{text,number}_idx` on (`attribute_id`, value); `ci_attribute_values_ref_idx` for reverse references |
| Entity history | `audit_log_entity_idx` (`entity_type, entity_id, occurred_at desc`) |

Helper SQL functions for the API: `ci_class_lineage(class_id)` returns the class and its
ancestors with depth, and is used to collect inherited attribute definitions.
`ci_class_is_a(class_id, ancestor_id)` checks class membership.

## Extending the model without migrations

Adding a "Load Balancer" class with its own fields is three inserts:

```sql
INSERT INTO ci_classes (key, name, parent_id)
VALUES ('load_balancer', 'Load balancer', (SELECT id FROM ci_classes WHERE key = 'network_device'));

INSERT INTO ci_attribute_definitions (class_id, key, label, data_type, enum_values)
VALUES ((SELECT id FROM ci_classes WHERE key = 'load_balancer'), 'algorithm', 'Algorithm', 'enum',
        '["round_robin","least_conn"]');

INSERT INTO ci_attribute_definitions (class_id, key, label, data_type)
VALUES ((SELECT id FROM ci_classes WHERE key = 'load_balancer'), 'vip', 'Virtual IP', 'ip');
```

Load balancer CIs automatically inherit the `network_device` and `hardware` attributes and
the `located_in` / `connected_to` rules. `shadoucmdb verify` exercises exactly this.

## Known limits, deliberately deferred

- An attribute key redefined on a child class that already exists on an ancestor is not
  rejected by the database; the API rejects it (`409`).
- `is_required`, `validation` (min/max/pattern) and `default_value` are enforced by the API at
  write time, not by the database: requiredness depends on the whole CI payload, not a single row.
  The API refuses to make an attribute required while live CIs lack a value.
- Re-parenting a class that already has CIs is checked by the API: values of attributes outside
  the new lineage are refused.
- Reserved seams, not built: SSO/LDAP/OIDC sign-in (a new login route that starts the same
  kind of session), linking `owners` to `users` (via `owners.external_ref`), discovery/import
  (`audit_log.actor_type = 'import'`), integrations and reporting.
- Class permissions do not inherit down the class tree: a grant on `hardware` does not cover
  `server`. Use the wildcard or grant each class.
