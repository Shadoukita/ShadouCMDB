import { and, asc, count, desc, eq, gte, ilike, inArray, lt } from 'drizzle-orm';
import { z } from 'zod';
import type { Database } from '../db/client.js';
import { AUDIT_ACTIONS, AUDIT_ACTOR_TYPES, auditLog } from '../db/schema/index.js';
import { defineRoute, type RouteSpec } from '../http/route.js';
import { PageQuery, QueryUuidList, Timestamp, Uuid, component, likePattern, listOf, sortParam } from '../http/schemas.js';

/*
 * Read-only view of audit_log. Rows are written by the services in the same
 * transaction as each change; there is no write endpoint and the table is
 * append-only at the database level.
 */

const ENTITY_TYPES = [
  'configuration_items',
  'ci_relationships',
  'ci_classes',
  'ci_attribute_definitions',
  'relationship_types',
  'relationship_type_rules',
  'statuses',
  'environments',
  'locations',
  'owners',
] as const;

const AuditEntry = component(
  'AuditEntry',
  z.object({
    id: z.number().int(),
    occurredAt: Timestamp,
    actorType: z.enum(AUDIT_ACTOR_TYPES),
    actorId: z.string().nullable(),
    actorName: z.string().nullable(),
    action: z.enum(AUDIT_ACTIONS),
    entityType: z.string().describe('Table of the changed entity, e.g. configuration_items'),
    entityId: Uuid,
    oldValue: z.unknown().describe('API representation before the change (null for create)'),
    newValue: z.unknown().describe('API representation after the change (null for delete)'),
    requestId: z.string().nullable(),
  }),
);

const ListQuery = z.strictObject({
  ...PageQuery,
  sort: sortParam(['occurredAt'] as const, '-occurredAt'),
  entityType: z.enum(ENTITY_TYPES).optional(),
  entityId: QueryUuidList.optional().describe('History of these entities'),
  action: z.enum(AUDIT_ACTIONS).optional(),
  actorName: z.string().trim().min(1).max(200).optional().describe('Case-insensitive substring'),
  requestId: z.string().max(128).optional(),
  from: Timestamp.optional().describe('occurredAt >= from (ISO 8601)'),
  to: Timestamp.optional().describe('occurredAt < to (ISO 8601)'),
});

export function auditRoutes(db: Database): RouteSpec[] {
  return [
    defineRoute({
      method: 'GET',
      url: '/api/v1/audit-log',
      operationId: 'listAuditLog',
      tag: 'Audit log',
      summary: 'Change history (read-only, paginated, newest first by default)',
      query: ListQuery,
      response: listOf('AuditEntryList', AuditEntry),
      handler: async ({ query: q }) => {
        const where = and(
          q.entityType ? eq(auditLog.entityType, q.entityType) : undefined,
          q.entityId ? inArray(auditLog.entityId, q.entityId) : undefined,
          q.action ? eq(auditLog.action, q.action) : undefined,
          q.actorName ? ilike(auditLog.actorName, likePattern(q.actorName)) : undefined,
          q.requestId ? eq(auditLog.requestId, q.requestId) : undefined,
          q.from ? gte(auditLog.occurredAt, new Date(q.from)) : undefined,
          q.to ? lt(auditLog.occurredAt, new Date(q.to)) : undefined,
        );
        const dir = q.sort.desc ? desc : asc;
        const [rows, totals] = await Promise.all([
          db.select().from(auditLog).where(where).orderBy(dir(auditLog.occurredAt), dir(auditLog.id)).limit(q.limit).offset(q.offset),
          db.select({ n: count() }).from(auditLog).where(where),
        ]);
        return {
          data: rows.map((r) => ({ ...r, occurredAt: r.occurredAt.toISOString() })),
          page: { limit: q.limit, offset: q.offset, total: totals[0]?.n ?? 0 },
        };
      },
    }),
  ];
}
