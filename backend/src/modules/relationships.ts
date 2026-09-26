import { eq, inArray, sql } from 'drizzle-orm';
import { z } from 'zod';
import type { Database } from '../db/client.js';
import { ciRelationships, configurationItems } from '../db/schema/index.js';
import { type Executor, selectById, serialise, writeAudit } from '../data/crud.js';
import {
  RELATIONSHIP_SORT_FIELDS,
  type RelationshipRow,
  getRelationship,
  listRelationships,
} from '../data/relationships.js';
import type { RequestContext } from '../http/context.js';
import { AppError } from '../http/errors.js';
import { defineRoute, type RouteSpec } from '../http/route.js';
import {
  Description,
  IdParams,
  PageQuery,
  QueryUuidList,
  SearchQuery,
  Timestamp,
  Uuid,
  component,
  listOf,
  sortParam,
  type Page,
} from '../http/schemas.js';
import { nonEmptyPatch } from './simple-resource.js';

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

const Endpoint = z.object({
  id: Uuid,
  name: z.string(),
  classKey: z.string(),
  className: z.string(),
  deleted: z.boolean(),
});

const Relationship = component(
  'Relationship',
  z.object({
    id: Uuid,
    relationshipTypeId: Uuid,
    type: z.object({
      id: Uuid,
      key: z.string(),
      name: z.string(),
      forwardLabel: z.string(),
      reverseLabel: z.string(),
      isDirectional: z.boolean(),
    }),
    sourceCiId: Uuid,
    source: Endpoint,
    targetCiId: Uuid,
    target: Endpoint,
    notes: z.string().nullable(),
    createdAt: Timestamp,
    updatedAt: Timestamp,
    deletedAt: Timestamp.nullable().describe('Set when the relationship was removed (soft delete)'),
  }),
);
const RelationshipList = listOf('RelationshipList', Relationship);

const ListQuery = z.strictObject({
  ...PageQuery,
  ...SearchQuery,
  sort: sortParam(RELATIONSHIP_SORT_FIELDS, '-createdAt'),
  ciId: QueryUuidList.optional().describe('Relationships where any of these CIs is source or target'),
  sourceCiId: QueryUuidList.optional(),
  targetCiId: QueryUuidList.optional(),
  relationshipTypeId: QueryUuidList.optional(),
  deleted: z.enum(['exclude', 'include', 'only']).default('exclude').describe('Removed relationships: exclude (default), include, or only'),
});

const CreateBody = z
  .strictObject({
    relationshipTypeId: Uuid,
    sourceCiId: Uuid.describe('Reads "source <forwardLabel> target", e.g. application runs on server'),
    targetCiId: Uuid,
    notes: Description.nullable().optional(),
  })
  .refine((b) => b.sourceCiId !== b.targetCiId, { path: ['targetCiId'], message: 'A CI cannot be related to itself' });

// Endpoints are immutable: re-pointing an edge is a delete plus a create, which keeps history honest.
const UpdateBody = nonEmptyPatch(
  z.strictObject({ relationshipTypeId: Uuid.optional(), notes: Description.nullable().optional() }),
);

type Dto = z.output<typeof Relationship>;

function toDto(r: RelationshipRow): Dto {
  const endpoint = (e: RelationshipRow['source']) => ({
    id: e.id,
    name: e.name,
    classKey: e.classKey,
    className: e.className,
    deleted: e.deletedAt !== null,
  });
  return {
    ...(serialise(r.rel) as Pick<Dto, 'id' | 'relationshipTypeId' | 'sourceCiId' | 'targetCiId' | 'notes' | 'createdAt' | 'updatedAt' | 'deletedAt'>),
    type: r.type,
    source: endpoint(r.source),
    target: endpoint(r.target),
  };
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

class RelationshipService {
  constructor(private readonly db: Database) {}

  async list(q: z.output<typeof ListQuery>): Promise<Page<Dto>> {
    const { rows, total } = await listRelationships(
      this.db,
      {
        q: q.q,
        ciIds: q.ciId,
        sourceCiIds: q.sourceCiId,
        targetCiIds: q.targetCiId,
        relationshipTypeIds: q.relationshipTypeId,
        deleted: q.deleted,
      },
      q.sort,
      q.limit,
      q.offset,
    );
    return { data: rows.map(toDto), page: { limit: q.limit, offset: q.offset, total } };
  }

  private async load(db: Executor, id: string): Promise<Dto> {
    const row = await getRelationship(db, id);
    if (!row) throw AppError.notFound('Relationship', id);
    return toDto(row);
  }

  get(id: string): Promise<Dto> {
    return this.load(this.db, id);
  }

  async create(ctx: RequestContext, input: z.output<typeof CreateBody>): Promise<Dto> {
    return this.db.transaction(async (tx) => {
      // Missing endpoints get a precise field error here; everything else
      // (duplicates, endpoint class rules, deleted CIs) is enforced by the
      // database and its errors map to field-level 400/409s.
      const found = await tx
        .select({ id: configurationItems.id })
        .from(configurationItems)
        .where(inArray(configurationItems.id, [input.sourceCiId, input.targetCiId]));
      const missing = (['sourceCiId', 'targetCiId'] as const).filter((f) => !found.some((r) => r.id === input[f]));
      if (missing.length) {
        throw AppError.validation(missing.map((field) => ({ in: 'body', field, message: 'Configuration item does not exist', code: 'not_found' })));
      }
      const [row] = await tx.insert(ciRelationships).values(input).returning({ id: ciRelationships.id });
      const dto = await this.load(tx, row!.id);
      await writeAudit(tx, ctx, { action: 'create', entityType: 'ci_relationships', entityId: dto.id, newValue: dto });
      return dto;
    });
  }

  async update(ctx: RequestContext, id: string, input: z.output<typeof UpdateBody>): Promise<Dto> {
    return this.db.transaction(async (tx) => {
      const locked = await selectById<{ deletedAt: Date | null }>(tx, ciRelationships, id, true);
      if (!locked) throw AppError.notFound('Relationship', id);
      if (locked.deletedAt) throw new AppError('CONFLICT', 'This relationship was removed and cannot be modified');
      const before = await this.load(tx, id);
      await tx.update(ciRelationships).set(input).where(eq(ciRelationships.id, id));
      const dto = await this.load(tx, id);
      await writeAudit(tx, ctx, { action: 'update', entityType: 'ci_relationships', entityId: id, oldValue: before, newValue: dto });
      return dto;
    });
  }

  async remove(ctx: RequestContext, id: string): Promise<void> {
    await this.db.transaction(async (tx) => {
      const locked = await selectById<{ deletedAt: Date | null }>(tx, ciRelationships, id, true);
      if (!locked || locked.deletedAt) throw AppError.notFound('Relationship', id);
      const before = await this.load(tx, id);
      await tx.update(ciRelationships).set({ deletedAt: sql`now()` }).where(eq(ciRelationships.id, id));
      await writeAudit(tx, ctx, { action: 'delete', entityType: 'ci_relationships', entityId: id, oldValue: before });
    });
  }
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

const TAG = 'Relationships';
const BASE = '/api/v1/relationships';

export function relationshipRoutes(db: Database): RouteSpec[] {
  const svc = new RelationshipService(db);
  return [
    defineRoute({
      method: 'GET',
      url: BASE,
      operationId: 'listRelationships',
      tag: TAG,
      summary: 'List relationships (paginated, filterable by CI, direction and type)',
      description: '`q` matches source/target CI name, type name and notes. Use `ciId` for all edges of a CI.',
      query: ListQuery,
      response: RelationshipList,
      handler: async ({ query }) => svc.list(query),
    }),
    defineRoute({
      method: 'GET',
      url: `${BASE}/:id`,
      operationId: 'getRelationship',
      tag: TAG,
      summary: 'Get one relationship',
      params: IdParams,
      response: Relationship,
      errors: ['NOT_FOUND'],
      handler: async ({ params }) => svc.get(params.id),
    }),
    defineRoute({
      method: 'POST',
      url: BASE,
      operationId: 'createRelationship',
      tag: TAG,
      summary: 'Create a typed, directional relationship between two CIs',
      description:
        'Rejected with 400 when the type does not allow these CI classes, when source equals target, or when a CI is deleted; 409 when the same live edge (or, for symmetric types, its reverse) exists.',
      body: CreateBody,
      status: 201,
      response: Relationship,
      errors: ['CONFLICT'],
      handler: async ({ body, actor, requestId }) => svc.create({ actor, requestId }, body),
    }),
    defineRoute({
      method: 'PATCH',
      url: `${BASE}/:id`,
      operationId: 'updateRelationship',
      tag: TAG,
      summary: 'Update notes or type of a relationship (endpoints are immutable)',
      params: IdParams,
      body: UpdateBody,
      response: Relationship,
      errors: ['NOT_FOUND', 'CONFLICT'],
      handler: async ({ params, body, actor, requestId }) => svc.update({ actor, requestId }, params.id, body),
    }),
    defineRoute({
      method: 'DELETE',
      url: `${BASE}/:id`,
      operationId: 'deleteRelationship',
      tag: TAG,
      summary: 'Remove a relationship (soft delete; the same edge can be created again later)',
      params: IdParams,
      errors: ['NOT_FOUND'],
      handler: async ({ params, actor, requestId }) => svc.remove({ actor, requestId }, params.id),
    }),
  ];
}
