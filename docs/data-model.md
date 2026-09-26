# ShadouCMDB data model

The schema is a **generic CI model**. CI types, their attributes and the legal
relationships between them are all rows, so new asset types need no
migration and no code change. Integrity is enforced in PostgreSQL itself
(foreign keys, `NOT NULL`, unique and check constraints, plus triggers for rules
that need to look at other rows), not only in the API.

Migrations: [`sql/migrations/`](../sql/migrations/)
(`0000_extensions`, `0001_core_schema`, `0002_integrity_triggers`).
SQL that reads and writes them: `backend/src/data/`.

## Tables

```
ci_classes ─┬─< ci_attribute_definitions >─── (reference_class_id) ─> ci_classes
  (parent)  │
            └─< configuration_items ─┬─< ci_attribute_values >── ci_attribute_definitions
                  │ │ │ │            │        (value_ref_ci_id) ──> configuration_items
   statuses ──────┘ │ │ │            └─< ci_relationships (source / target)
   environments ────┘ │ │                        │
   owners ────────────┘ │           relationship_types ─< relationship_type_rules >─ ci_classes (source / target)
   locations (parent) ──┘
audit_log (append-only; entity_type + entity_id point at any row)
```

| Table | Purpose | Key constraints |
| --- | --- | --- |
| `ci_classes` | CI types in a single-inheritance tree (`parent_id`). `is_abstract` classes group attributes and rules but hold no CIs. | unique `key`; key format check; no self-parent; **no cycles** (trigger) |
| `ci_attribute_definitions` | Typed attribute per class, inherited by descendant classes. Types: `text`, `number`, `integer`, `boolean`, `enum`, `date`, `datetime`, `ip`, `cidr`, `reference`. | unique (`class_id`, `key`); `enum_values` required iff enum; `reference_class_id` required iff reference |
| `configuration_items` | CI instances with the common core: name, class, status, owner, location, environment, hostname, `ip_address inet`, serial, notes, plus `version` for optimistic locking. | FKs to class and every lookup; class must be concrete and active (trigger); non-blank name; hostname format |
| `ci_attribute_values` | One row per CI and attribute, with a typed column per data type (`value_text`, `value_number`, `value_boolean`, `value_date`, `value_datetime`, `value_ip`, `value_cidr`, `value_ref_ci_id`). | unique (`ci_id`, `attribute_id`); exactly one value column set; trigger checks the attribute belongs to the CI's class lineage, the column matches `data_type`, integers are whole, enum values are allowed, and references point at the right class |
| `relationship_types` | `runs_on`, `depends_on`, `located_in`, `connected_to`, …, with `forward_label` / `reverse_label` and `is_directional`. | unique `key` |
| `relationship_type_rules` | Legal (source class, target class) pairs per type. A rule matches the named class **and all its descendants**. | unique triple |
| `ci_relationships` | Typed, directional edge `source_ci_id → target_ci_id`. | **no self-edges** (check); **no duplicate live edges** (partial unique index); for non-directional types the reverse edge also counts as a duplicate (trigger + advisory lock); endpoints must satisfy a rule and must not be soft-deleted (trigger) |
| `statuses` | CI lifecycle (`planned`, `in_service`, `maintenance`, `retired`, `disposed`). `is_operational` flags "live" statuses for reporting. | unique `key` |
| `environments` | `production`, `staging`, `test`, `development`, `disaster_recovery`. | unique `key` |
| `locations` | Location hierarchy (region › site › building › floor › room › rack, plus `cloud_region`). | unique `key`; `location_type` check; no cycles (trigger) |
| `owners` | Accountable people or teams (`kind` = `person` / `team`). This is not a login table: authentication is out of scope for Milestone 1. `external_ref` is the seam for a later directory/IdP link. | `kind` check; unique `external_ref`; email format |
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

### Seeded reference data (`shadoucmdb seed`)

- Classes: `hardware` (abstract) › `server`, `network_device`; `virtual_machine`, `application`,
  `database`, `service`, `location`. There are 35 attribute definitions across them, including an
  `application.primary_database` **reference** attribute.
- Relationship rules: `runs_on` (app→server/VM, db→server/VM, VM→server), `depends_on`
  (app→db/app, service→app/service), `located_in` (hardware→location, location→location),
  `connected_to` (hardware↔hardware).
- Statuses, environments, a small location tree (EMEA › FRA1 › room › rack, Americas › NYC1,
  AWS eu-central-1), and three owner teams.
- `--demo` adds 8 CIs, 24 attribute values and 8 relationships (a CRM service down to its rack).

## Soft-delete decisions

| Table | Strategy | Why |
| --- | --- | --- |
| `configuration_items` | **Soft delete** (`deleted_at`) | A decommissioned server must still resolve in last quarter's report, in audit entries, and in historic relationships. All live-inventory indexes are partial on `deleted_at IS NULL`. |
| `ci_relationships` | **Soft delete** (`deleted_at`) | Answers "what did this app run on before the migration?" Uniqueness applies only to live edges, so a removed edge can be re-created. When the API soft-deletes a CI, it also soft-deletes that CI's live relationships in the same transaction. |
| `ci_attribute_values` | **Hard delete** | A value is part of the CI's current state. Clearing a field deletes the row, and the old value is kept in `audit_log`. Rows cascade if a CI is ever purged. |
| `ci_classes`, `ci_attribute_definitions`, `relationship_types`, `statuses`, `environments`, `locations`, `owners` | **Retire, don't delete** (`is_active = false`) | These are referenced by history. FKs are `ON DELETE RESTRICT`, so a referenced row cannot be hard-deleted; inactive rows keep resolving for old CIs and are hidden from pickers. Inactive classes cannot receive new CIs, and inactive relationship types cannot receive new edges. |
| `relationship_type_rules` | **Hard delete** | Pure configuration. Removing a rule blocks new edges and leaves existing edges alone. |
| `audit_log` | **Never deleted** | Append-only by trigger. Retention/archival is an operator decision for a later milestone. |

## Auditing

The API writes `audit_log` rows **in the same transaction** as the change, because only
the API knows the actor and the request. Database triggers are not used for auditing:
they cannot see the actor, and would log raw row images instead of API-level changes.
Until authentication exists, `actor_type`/`actor_name` identify the caller, and a later
auth milestone fills `actor_id` without a schema change.

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
  rejected by the database. The API should reject it when definitions are managed.
- `is_required` and `validation` (min/max/pattern) are enforced by the API at write time, not
  by the database: requiredness depends on the whole CI payload, not a single row.
- Re-parenting a class that already has CIs is allowed. The API should check that existing
  attribute values stay within the new lineage.
- Reserved seams, not built: an auth `users` table (link via `owners.external_ref` /
  `audit_log.actor_id`), RBAC, discovery/import (`audit_log.actor_type = 'import'`),
  integrations and reporting.
