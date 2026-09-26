import { eq, ilike, or, type SQL } from 'drizzle-orm';
import type { PgColumn } from 'drizzle-orm/pg-core';
import { z } from 'zod';
import type { Database } from '../db/client.js';
import {
  type AnyTable,
  type Tx,
  deleteRow,
  insertRow,
  order,
  selectById,
  selectPage,
  serialise,
  updateRow,
  writeAudit,
} from '../data/crud.js';
import type { RequestContext } from '../http/context.js';
import { AppError } from '../http/errors.js';
import { defineRoute, type RouteSpec } from '../http/route.js';
import { IdParams, PageQuery, SearchQuery, likePattern, listOf, sortParam, type Page } from '../http/schemas.js';

type Row = Record<string, unknown> & { id: string };
type Shape = Record<string, z.ZodType>;

/**
 * Configuration for a plain resource: one table, list/get/create/update/delete,
 * an audit row per change. Lookups, classes, attribute definitions and
 * relationship types are all built from this; the configuration-item and
 * relationship modules have their own services because they carry more rules.
 */
export interface SimpleResourceConfig<S extends readonly [string, ...string[]]> {
  table: AnyTable;
  /** audit_log.entity_type (the table name). */
  entityType: string;
  /** Human name for messages, e.g. "Status". */
  label: string;
  basePath: string;
  tag: string;
  names: { singular: string; plural: string };
  dto: z.ZodObject;
  createBody: z.ZodType<Record<string, unknown>>;
  updateBody: z.ZodType<Record<string, unknown>>;
  /** Extra list filters (added to limit/offset/q/sort). */
  filterShape?: Shape;
  filters?: (query: Record<string, unknown>) => (SQL | undefined)[];
  searchColumns: PgColumn[];
  sortFields: S;
  sortColumns: Record<S[number], PgColumn | SQL>;
  defaultSort: string;
  /** Runs inside the write transaction after insert/update; throw AppError to roll back. */
  afterWrite?: (tx: Tx, row: Row, previous: Row | undefined) => Promise<void>;
  /** Runs inside the delete transaction before the row is removed. */
  beforeDelete?: (tx: Tx, row: Row) => Promise<void>;
  deleteDescription?: string;
  toDto?: (row: Row) => Record<string, unknown>;
}

export class SimpleResourceService {
  constructor(
    private readonly db: Database,
    private readonly cfg: SimpleResourceConfig<readonly [string, ...string[]]>,
  ) {}

  dto(row: Row): Record<string, unknown> {
    return this.cfg.toDto ? this.cfg.toDto(row) : serialise(row);
  }

  async list(query: Record<string, unknown> & { limit: number; offset: number; q?: string; sort: { field: string; desc: boolean } }): Promise<Page<Record<string, unknown>>> {
    const { cfg } = this;
    const search = query.q
      ? or(...cfg.searchColumns.map((c) => ilike(c, likePattern(query.q!))))
      : undefined;
    const sortCol = cfg.sortColumns[query.sort.field as keyof typeof cfg.sortColumns]!;
    const { rows, total } = await selectPage<Row>(this.db, cfg.table, {
      where: [search, ...(cfg.filters?.(query) ?? [])],
      orderBy: [order(sortCol, query.sort.desc), order(cfg.table.id, false)],
      limit: query.limit,
      offset: query.offset,
    });
    return { data: rows.map((r) => this.dto(r)), page: { limit: query.limit, offset: query.offset, total } };
  }

  async get(id: string): Promise<Record<string, unknown>> {
    const row = await selectById<Row>(this.db, this.cfg.table, id);
    if (!row) throw AppError.notFound(this.cfg.label, id);
    return this.dto(row);
  }

  async create(ctx: RequestContext, input: Record<string, unknown>): Promise<Record<string, unknown>> {
    return this.db.transaction(async (tx) => {
      const row = await insertRow<Row>(tx, this.cfg.table, input);
      await this.cfg.afterWrite?.(tx, row, undefined);
      const dto = this.dto(row);
      await writeAudit(tx, ctx, { action: 'create', entityType: this.cfg.entityType, entityId: row.id, newValue: dto });
      return dto;
    });
  }

  async update(ctx: RequestContext, id: string, patch: Record<string, unknown>): Promise<Record<string, unknown>> {
    return this.db.transaction(async (tx) => {
      const before = await selectById<Row>(tx, this.cfg.table, id, true);
      if (!before) throw AppError.notFound(this.cfg.label, id);
      const row = await updateRow<Row>(tx, this.cfg.table, id, patch);
      await this.cfg.afterWrite?.(tx, row, before);
      const dto = this.dto(row);
      await writeAudit(tx, ctx, {
        action: 'update',
        entityType: this.cfg.entityType,
        entityId: id,
        oldValue: this.dto(before),
        newValue: dto,
      });
      return dto;
    });
  }

  async remove(ctx: RequestContext, id: string): Promise<void> {
    await this.db.transaction(async (tx) => {
      const before = await selectById<Row>(tx, this.cfg.table, id, true);
      if (!before) throw AppError.notFound(this.cfg.label, id);
      await this.cfg.beforeDelete?.(tx, before);
      await deleteRow(tx, this.cfg.table, id);
      await writeAudit(tx, ctx, { action: 'delete', entityType: this.cfg.entityType, entityId: id, oldValue: this.dto(before) });
    });
  }
}

/** Require at least one field in a PATCH body. */
export const nonEmptyPatch = <T extends z.ZodObject>(schema: T) =>
  schema.refine((o) => Object.keys(o).length > 0, { message: 'Provide at least one field to update' });

export function simpleResourceRoutes<S extends readonly [string, ...string[]]>(
  service: SimpleResourceService,
  cfg: SimpleResourceConfig<S>,
): RouteSpec[] {
  const { singular, plural } = cfg.names;
  const cap = (s: string) => s[0]!.toUpperCase() + s.slice(1);
  const listSchema = listOf(`${cap(singular)}List`, cfg.dto);
  const listQuery = z.strictObject({
    ...PageQuery,
    ...(cfg.searchColumns.length ? SearchQuery : {}),
    sort: sortParam(cfg.sortFields, cfg.defaultSort),
    ...(cfg.filterShape ?? {}),
  });
  const byId = `${cfg.basePath}/:id`;

  return [
    defineRoute({
      method: 'GET',
      url: cfg.basePath,
      operationId: `list${cap(plural)}`,
      tag: cfg.tag,
      summary: `List ${cfg.label.toLowerCase()} records (paginated, searchable, sortable)`,
      ...(cfg.searchColumns.length
        ? { description: `\`q\` matches ${cfg.searchColumns.map((c) => c.name).join(', ')} (case-insensitive substring).` }
        : {}),
      query: listQuery,
      response: listSchema,
      handler: async ({ query }) => (await service.list(query as never)) as z.output<typeof listSchema>,
    }),
    defineRoute({
      method: 'GET',
      url: byId,
      operationId: `get${cap(singular)}`,
      tag: cfg.tag,
      summary: `Get one ${cfg.label.toLowerCase()}`,
      params: IdParams,
      response: cfg.dto,
      errors: ['NOT_FOUND'],
      handler: async ({ params }) => (await service.get(params.id)) as never,
    }),
    defineRoute({
      method: 'POST',
      url: cfg.basePath,
      operationId: `create${cap(singular)}`,
      tag: cfg.tag,
      summary: `Create a ${cfg.label.toLowerCase()}`,
      body: cfg.createBody,
      status: 201,
      response: cfg.dto,
      errors: ['CONFLICT'],
      handler: async ({ body, actor, requestId }) => (await service.create({ actor, requestId }, body)) as never,
    }),
    defineRoute({
      method: 'PATCH',
      url: byId,
      operationId: `update${cap(singular)}`,
      tag: cfg.tag,
      summary: `Update a ${cfg.label.toLowerCase()} (partial)`,
      params: IdParams,
      body: cfg.updateBody,
      response: cfg.dto,
      errors: ['NOT_FOUND', 'CONFLICT'],
      handler: async ({ params, body, actor, requestId }) =>
        (await service.update({ actor, requestId }, params.id, body)) as never,
    }),
    defineRoute({
      method: 'DELETE',
      url: byId,
      operationId: `delete${cap(singular)}`,
      tag: cfg.tag,
      summary: `Delete a ${cfg.label.toLowerCase()}`,
      description:
        cfg.deleteDescription ??
        'Hard delete, allowed only while nothing references the row. A referenced row returns 409 IN_USE; retire it with `PATCH {"isActive": false}` instead so history keeps resolving.',
      params: IdParams,
      errors: ['NOT_FOUND', 'IN_USE'],
      handler: async ({ params, actor, requestId }) => {
        await service.remove({ actor, requestId }, params.id);
      },
    }),
  ];
}

/** Common filter: `isActive=true|false`. */
export const activeFilter = (col: PgColumn) => (value: unknown) =>
  typeof value === 'boolean' ? eq(col, value) : undefined;
