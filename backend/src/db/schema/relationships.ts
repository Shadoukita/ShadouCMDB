import { sql } from 'drizzle-orm';
import { boolean, check, index, pgTable, text, timestamp, unique, uniqueIndex, uuid } from 'drizzle-orm/pg-core';
import { KEY_PATTERN, lookupColumns, id, timestamps } from './common.js';
import { ciClasses } from './classes.js';
import { configurationItems } from './items.js';

/**
 * Relationship classes (runs_on, depends_on, located_in, connected_to, ...).
 * A relationship always reads source -> target using forward_label
 * ("app runs on server"); reverse_label is the view from the target
 * ("server hosts app"). Non-directional types (connected_to) are stored once
 * per pair; the reverse edge is rejected as a duplicate.
 */
export const relationshipTypes = pgTable(
  'relationship_types',
  {
    ...lookupColumns(),
    forwardLabel: text('forward_label').notNull(),
    reverseLabel: text('reverse_label').notNull(),
    isDirectional: boolean('is_directional').notNull().default(true),
  },
  (t) => [check('relationship_types_key_format', sql`${t.key} ~ ${sql.raw(`'${KEY_PATTERN}'`)}`)],
);

/**
 * Legal endpoint classes per relationship type. A rule matches a CI whose class
 * is the rule's class or any descendant of it, so a rule on the abstract
 * "hardware" class covers servers, network devices and any hardware class
 * added later. A relationship with no matching rule is rejected.
 */
export const relationshipTypeRules = pgTable(
  'relationship_type_rules',
  {
    id: id(),
    relationshipTypeId: uuid('relationship_type_id')
      .notNull()
      .references(() => relationshipTypes.id, { onDelete: 'cascade' }),
    sourceClassId: uuid('source_class_id')
      .notNull()
      .references(() => ciClasses.id, { onDelete: 'restrict' }),
    targetClassId: uuid('target_class_id')
      .notNull()
      .references(() => ciClasses.id, { onDelete: 'restrict' }),
    ...timestamps(),
  },
  (t) => [
    unique('relationship_type_rules_uq').on(t.relationshipTypeId, t.sourceClassId, t.targetClassId),
    index('relationship_type_rules_source_idx').on(t.sourceClassId),
    index('relationship_type_rules_target_idx').on(t.targetClassId),
  ],
);

/**
 * Typed, directional edges between CIs.
 *
 * Soft delete: removing a relationship sets deleted_at, so "what did this app
 * run on last quarter" stays answerable. Uniqueness only applies to live edges,
 * so an edge can be removed and later re-created.
 */
export const ciRelationships = pgTable(
  'ci_relationships',
  {
    id: id(),
    relationshipTypeId: uuid('relationship_type_id')
      .notNull()
      .references(() => relationshipTypes.id, { onDelete: 'restrict' }),
    sourceCiId: uuid('source_ci_id')
      .notNull()
      .references(() => configurationItems.id, { onDelete: 'restrict' }),
    targetCiId: uuid('target_ci_id')
      .notNull()
      .references(() => configurationItems.id, { onDelete: 'restrict' }),
    notes: text('notes'),
    ...timestamps(),
    deletedAt: timestamp('deleted_at', { withTimezone: true }),
  },
  (t) => [
    check('ci_relationships_no_self_edge', sql`${t.sourceCiId} <> ${t.targetCiId}`),
    uniqueIndex('ci_relationships_live_edge_uq')
      .on(t.relationshipTypeId, t.sourceCiId, t.targetCiId)
      .where(sql`${t.deletedAt} IS NULL`),
    // Traversal in both directions (outgoing from a CI, incoming to a CI).
    index('ci_relationships_source_idx').on(t.sourceCiId, t.relationshipTypeId).where(sql`${t.deletedAt} IS NULL`),
    index('ci_relationships_target_idx').on(t.targetCiId, t.relationshipTypeId).where(sql`${t.deletedAt} IS NULL`),
  ],
);
