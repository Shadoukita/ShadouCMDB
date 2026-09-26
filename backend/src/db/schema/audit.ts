import { sql } from 'drizzle-orm';
import { bigint, check, index, jsonb, pgTable, text, timestamp, uuid } from 'drizzle-orm/pg-core';

export const AUDIT_ACTOR_TYPES = ['system', 'user', 'api_client', 'import'] as const;
export const AUDIT_ACTIONS = ['create', 'update', 'delete', 'restore'] as const;

/**
 * Append-only change history. The API writes one row per change in the same
 * transaction as the change itself (it is the only layer that knows the actor).
 * UPDATE and DELETE are rejected by the audit_log_append_only trigger.
 *
 * actor_* are free-form until authentication exists; a later auth milestone
 * fills actor_id with the user id without a schema change.
 */
export const auditLog = pgTable(
  'audit_log',
  {
    id: bigint('id', { mode: 'number' }).primaryKey().generatedAlwaysAsIdentity(),
    occurredAt: timestamp('occurred_at', { withTimezone: true }).notNull().defaultNow(),
    actorType: text('actor_type', { enum: AUDIT_ACTOR_TYPES }).notNull(),
    actorId: text('actor_id'),
    actorName: text('actor_name'),
    action: text('action', { enum: AUDIT_ACTIONS }).notNull(),
    // Table name of the changed entity, e.g. 'configuration_items', 'ci_relationships'.
    entityType: text('entity_type').notNull(),
    entityId: uuid('entity_id').notNull(),
    oldValue: jsonb('old_value'),
    newValue: jsonb('new_value'),
    // Correlates all rows written by one API request.
    requestId: text('request_id'),
  },
  (t) => [
    check('audit_log_actor_type_valid', sql`${t.actorType} IN (${sql.raw(AUDIT_ACTOR_TYPES.map((v) => `'${v}'`).join(', '))})`),
    check('audit_log_action_valid', sql`${t.action} IN (${sql.raw(AUDIT_ACTIONS.map((v) => `'${v}'`).join(', '))})`),
    check(
      'audit_log_values_present',
      sql`(${t.action} = 'create' AND ${t.oldValue} IS NULL AND ${t.newValue} IS NOT NULL)
          OR (${t.action} = 'update' AND ${t.oldValue} IS NOT NULL AND ${t.newValue} IS NOT NULL)
          OR (${t.action} IN ('delete', 'restore') AND ${t.oldValue} IS NOT NULL)`,
    ),
    index('audit_log_entity_idx').on(t.entityType, t.entityId, t.occurredAt.desc()),
    index('audit_log_occurred_idx').on(t.occurredAt.desc()),
    index('audit_log_actor_idx').on(t.actorId, t.occurredAt.desc()).where(sql`${t.actorId} IS NOT NULL`),
  ],
);
