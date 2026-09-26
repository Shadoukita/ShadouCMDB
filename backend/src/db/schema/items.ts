import { sql } from 'drizzle-orm';
import {
  type AnyPgColumn,
  boolean,
  check,
  customType,
  date,
  index,
  inet,
  cidr,
  integer,
  numeric,
  pgTable,
  text,
  timestamp,
  unique,
  uuid,
} from 'drizzle-orm/pg-core';
import { id, timestamps } from './common.js';
import { ciAttributeDefinitions, ciClasses } from './classes.js';
import { environments, locations, owners, statuses } from './lookups.js';

const tsvector = customType<{ data: string }>({ dataType: () => 'tsvector' });

/**
 * CI instances. Holds the common core every CI shares; class-specific values
 * live in ci_attribute_values.
 *
 * Soft delete: deleted_at IS NOT NULL means the CI is decommissioned/removed but
 * kept so history, audit entries and past relationships still resolve. Every
 * "live" index is partial on deleted_at IS NULL.
 */
export const configurationItems = pgTable(
  'configuration_items',
  {
    id: id(),
    classId: uuid('class_id')
      .notNull()
      .references(() => ciClasses.id, { onDelete: 'restrict' }),
    name: text('name').notNull(),
    statusId: uuid('status_id')
      .notNull()
      .references(() => statuses.id, { onDelete: 'restrict' }),
    environmentId: uuid('environment_id').references(() => environments.id, { onDelete: 'restrict' }),
    ownerId: uuid('owner_id').references(() => owners.id, { onDelete: 'restrict' }),
    locationId: uuid('location_id').references(() => locations.id, { onDelete: 'restrict' }),
    hostname: text('hostname'),
    ipAddress: inet('ip_address'),
    serialNumber: text('serial_number'),
    notes: text('notes'),
    // Optimistic-locking counter; the API increments it on every update.
    version: integer('version').notNull().default(1),
    ...timestamps(),
    deletedAt: timestamp('deleted_at', { withTimezone: true }),
    searchVector: tsvector('search_vector').generatedAlwaysAs(
      sql`to_tsvector('simple',
            coalesce(name, '') || ' ' || coalesce(hostname, '') || ' ' ||
            coalesce(serial_number, '') || ' ' || coalesce(notes, ''))`,
    ),
  },
  (t) => [
    check('configuration_items_name_not_blank', sql`length(btrim(${t.name})) > 0`),
    check('configuration_items_version_positive', sql`${t.version} > 0`),
    check(
      'configuration_items_hostname_format',
      sql`${t.hostname} IS NULL OR ${t.hostname} ~ '^[A-Za-z0-9]([A-Za-z0-9._-]{0,252})$'`,
    ),
    // Inventory listing (default sort) and per-dimension filters, live rows only.
    index('configuration_items_live_name_idx').on(sql`lower(${t.name})`, t.id).where(sql`${t.deletedAt} IS NULL`),
    index('configuration_items_live_updated_idx').on(t.updatedAt.desc(), t.id).where(sql`${t.deletedAt} IS NULL`),
    index('configuration_items_class_idx').on(t.classId, sql`lower(${t.name})`).where(sql`${t.deletedAt} IS NULL`),
    index('configuration_items_status_idx').on(t.statusId).where(sql`${t.deletedAt} IS NULL`),
    index('configuration_items_owner_idx').on(t.ownerId).where(sql`${t.deletedAt} IS NULL`),
    index('configuration_items_location_idx').on(t.locationId).where(sql`${t.deletedAt} IS NULL`),
    index('configuration_items_environment_idx').on(t.environmentId).where(sql`${t.deletedAt} IS NULL`),
    // Global search: full-text plus trigram for substring / fuzzy matches.
    index('configuration_items_search_idx').using('gin', t.searchVector),
    index('configuration_items_name_trgm_idx').using('gin', sql`${t.name} gin_trgm_ops`),
    index('configuration_items_hostname_trgm_idx').using('gin', sql`${t.hostname} gin_trgm_ops`),
    index('configuration_items_serial_trgm_idx').using('gin', sql`${t.serialNumber} gin_trgm_ops`),
    // IP lookups, including subnet containment (ip_address << '10.0.0.0/8').
    index('configuration_items_ip_idx').using('gist', sql`${t.ipAddress} inet_ops`),
  ],
);

/**
 * Typed per-class attribute values (one row per CI x attribute). Exactly one
 * value_* column is populated; ci_attribute_values_validate() checks that it
 * matches the definition's data_type, that the attribute belongs to the CI's
 * class or one of its ancestors, that enum values are allowed, and that
 * references point at a CI of the right class.
 *
 * Hard delete: clearing a value deletes its row; the previous value is kept in
 * audit_log.
 */
export const ciAttributeValues = pgTable(
  'ci_attribute_values',
  {
    id: id(),
    ciId: uuid('ci_id')
      .notNull()
      .references(() => configurationItems.id, { onDelete: 'cascade' }),
    attributeId: uuid('attribute_id')
      .notNull()
      .references(() => ciAttributeDefinitions.id, { onDelete: 'restrict' }),
    valueText: text('value_text'), // text, enum
    valueNumber: numeric('value_number'), // number, integer
    valueBoolean: boolean('value_boolean'),
    valueDate: date('value_date'),
    valueDatetime: timestamp('value_datetime', { withTimezone: true }),
    valueIp: inet('value_ip'),
    valueCidr: cidr('value_cidr'),
    valueRefCiId: uuid('value_ref_ci_id').references((): AnyPgColumn => configurationItems.id, {
      onDelete: 'restrict',
    }),
    ...timestamps(),
  },
  (t) => [
    unique('ci_attribute_values_ci_attribute_uq').on(t.ciId, t.attributeId),
    check(
      'ci_attribute_values_exactly_one_value',
      sql`num_nonnulls(${t.valueText}, ${t.valueNumber}, ${t.valueBoolean}, ${t.valueDate},
                       ${t.valueDatetime}, ${t.valueIp}, ${t.valueCidr}, ${t.valueRefCiId}) = 1`,
    ),
    // Filter inventory by attribute value.
    index('ci_attribute_values_text_idx').on(t.attributeId, t.valueText).where(sql`${t.valueText} IS NOT NULL`),
    index('ci_attribute_values_number_idx').on(t.attributeId, t.valueNumber).where(sql`${t.valueNumber} IS NOT NULL`),
    index('ci_attribute_values_ref_idx').on(t.valueRefCiId).where(sql`${t.valueRefCiId} IS NOT NULL`),
    index('ci_attribute_values_attribute_idx').on(t.attributeId),
  ],
);
