import { sql } from 'drizzle-orm';
import {
  type AnyPgColumn,
  boolean,
  check,
  index,
  integer,
  jsonb,
  pgTable,
  text,
  unique,
  uuid,
} from 'drizzle-orm/pg-core';
import { KEY_PATTERN, id, timestamps } from './common.js';

/**
 * CI types. Classes form a single-inheritance tree via parent_id: a class
 * inherits every attribute definition of its ancestors. Adding a class
 * (e.g. "load_balancer" under "network_device") is an INSERT, never a migration.
 * Cycles are rejected by the ci_classes_prevent_cycle trigger.
 */
export const ciClasses = pgTable(
  'ci_classes',
  {
    id: id(),
    key: text('key').notNull().unique(),
    name: text('name').notNull(),
    description: text('description'),
    parentId: uuid('parent_id').references((): AnyPgColumn => ciClasses.id, { onDelete: 'restrict' }),
    // Abstract classes group attributes and relationship rules but cannot hold CIs.
    isAbstract: boolean('is_abstract').notNull().default(false),
    icon: text('icon'),
    isActive: boolean('is_active').notNull().default(true),
    ...timestamps(),
  },
  (t) => [
    check('ci_classes_key_format', sql`${t.key} ~ ${sql.raw(`'${KEY_PATTERN}'`)}`),
    check('ci_classes_not_own_parent', sql`${t.parentId} IS NULL OR ${t.parentId} <> ${t.id}`),
    index('ci_classes_parent_idx').on(t.parentId),
  ],
);

export const ATTRIBUTE_DATA_TYPES = [
  'text',
  'number',
  'integer',
  'boolean',
  'enum',
  'date',
  'datetime',
  'ip',
  'cidr',
  'reference',
] as const;
export type AttributeDataType = (typeof ATTRIBUTE_DATA_TYPES)[number];

/**
 * Typed attribute definitions attached to a class (and inherited by its
 * descendants). A custom field is a row here, never a new column.
 */
export const ciAttributeDefinitions = pgTable(
  'ci_attribute_definitions',
  {
    id: id(),
    classId: uuid('class_id')
      .notNull()
      .references(() => ciClasses.id, { onDelete: 'restrict' }),
    key: text('key').notNull(),
    label: text('label').notNull(),
    description: text('description'),
    dataType: text('data_type', { enum: ATTRIBUTE_DATA_TYPES }).notNull(),
    isRequired: boolean('is_required').notNull().default(false),
    // enum: JSON array of allowed string values, e.g. ["linux","windows"].
    enumValues: jsonb('enum_values').$type<string[]>(),
    // reference: the class (or ancestor) the referenced CI must belong to.
    referenceClassId: uuid('reference_class_id').references(() => ciClasses.id, { onDelete: 'restrict' }),
    // Optional extra validation for the API layer: {min, max, pattern, maxLength, unit}.
    validation: jsonb('validation').$type<Record<string, unknown>>(),
    // Optional UI grouping ("Hardware", "Software") and ordering.
    groupName: text('group_name'),
    sortOrder: integer('sort_order').notNull().default(0),
    isActive: boolean('is_active').notNull().default(true),
    ...timestamps(),
  },
  (t) => [
    unique('ci_attribute_definitions_class_key_uq').on(t.classId, t.key),
    check('ci_attribute_definitions_key_format', sql`${t.key} ~ ${sql.raw(`'${KEY_PATTERN}'`)}`),
    check(
      'ci_attribute_definitions_data_type_valid',
      sql`${t.dataType} IN (${sql.raw(ATTRIBUTE_DATA_TYPES.map((v) => `'${v}'`).join(', '))})`,
    ),
    check(
      'ci_attribute_definitions_enum_values',
      sql`(${t.dataType} = 'enum') = (${t.enumValues} IS NOT NULL)
          AND (${t.enumValues} IS NULL OR (jsonb_typeof(${t.enumValues}) = 'array' AND jsonb_array_length(${t.enumValues}) > 0))`,
    ),
    check(
      'ci_attribute_definitions_reference_class',
      sql`(${t.dataType} = 'reference') = (${t.referenceClassId} IS NOT NULL)`,
    ),
    check(
      'ci_attribute_definitions_validation_object',
      sql`${t.validation} IS NULL OR jsonb_typeof(${t.validation}) = 'object'`,
    ),
    index('ci_attribute_definitions_reference_class_idx').on(t.referenceClassId),
  ],
);
