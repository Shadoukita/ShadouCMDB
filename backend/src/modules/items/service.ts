import { eq, sql } from 'drizzle-orm';
import { z } from 'zod';
import type { Database } from '../../db/client.js';
import { ciClasses, configurationItems } from '../../db/schema/index.js';
import { effectiveAttributes, type EffectiveAttributeRow } from '../../data/classes.js';
import { type Executor, type Tx, selectById, serialise, writeAudit } from '../../data/crud.js';
import {
  type EdgeRow,
  type ItemFilters,
  type ItemSortField,
  type StoredValueRow,
  type SummaryRow,
  type ValueColumns,
  attributeValues,
  deleteAttributeValues,
  edgesTouching,
  isIpOrCidr,
  itemSummaries,
  itemSummary,
  listItems,
  liveItems,
  searchItems,
  softDeleteEdgesOf,
  upsertAttributeValue,
  withDescendantClasses,
} from '../../data/items.js';
import type { RequestContext } from '../../http/context.js';
import { AppError, mapPgError, type FieldError } from '../../http/errors.js';
import type { Page } from '../../http/schemas.js';
import type {
  ConfigurationItem,
  ConfigurationItemSummary,
  CreateItemBody,
  Graph,
  GraphQuery,
  ListItemsQuery,
  SearchQuery,
  SearchResults,
  UpdateItemBody,
} from './schemas.js';

type Summary = z.output<typeof ConfigurationItemSummary>;
type Detail = z.output<typeof ConfigurationItem>;
type AttrInput = Record<string, string | number | boolean | null>;
type CiRow = typeof configurationItems.$inferSelect;

export function summaryDto(r: SummaryRow): Summary {
  return {
    ...(serialise(r.ci) as Omit<Summary, 'class' | 'status' | 'environment' | 'owner' | 'location'>),
    class: r.class,
    status: r.status,
    environment: r.environment,
    owner: r.owner,
    location: r.location,
  };
}

function valueToJson(v: StoredValueRow): string | number | boolean | null {
  return (
    v.value_text ??
    v.value_number ??
    v.value_boolean ??
    v.value_date ??
    (v.value_datetime ? new Date(v.value_datetime).toISOString() : null) ??
    v.value_ip ??
    v.value_cidr ??
    v.value_ref_ci_id
  );
}

// ---------------------------------------------------------------------------
// Attribute validation
// ---------------------------------------------------------------------------

type Rules = { min?: number; max?: number; maxLength?: number; pattern?: string };

function valueSchema(def: EffectiveAttributeRow): z.ZodType {
  const rules = (def.validation ?? {}) as Rules;
  const withRange = (s: z.ZodNumber) => {
    let out = s;
    if (rules.min !== undefined) out = out.min(rules.min);
    if (rules.max !== undefined) out = out.max(rules.max);
    return out;
  };
  switch (def.data_type) {
    case 'text': {
      let s = z.string().max(rules.maxLength ?? 10_000);
      if (rules.pattern) s = s.regex(new RegExp(rules.pattern), `Must match ${rules.pattern}`);
      return s;
    }
    case 'enum':
      return z.enum((def.enum_values ?? []) as [string, ...string[]]);
    case 'number':
      return withRange(z.number());
    case 'integer':
      return withRange(z.number().int());
    case 'boolean':
      return z.boolean();
    case 'date':
      return z.iso.date();
    case 'datetime':
      return z.iso.datetime({ offset: true });
    case 'ip':
      return z.union([z.ipv4(), z.ipv6()], { error: 'Must be an IPv4 or IPv6 address' });
    case 'cidr':
      return z.union([z.cidrv4(), z.cidrv6()], { error: 'Must be a CIDR block, e.g. 10.0.0.0/24' });
    case 'reference':
      return z.uuid();
    default:
      return z.never();
  }
}

function toColumns(def: EffectiveAttributeRow, v: string | number | boolean): ValueColumns {
  switch (def.data_type) {
    case 'text':
    case 'enum':
      return { valueText: v as string };
    case 'number':
    case 'integer':
      return { valueNumber: String(v) };
    case 'boolean':
      return { valueBoolean: v as boolean };
    case 'date':
      return { valueDate: v as string };
    case 'datetime':
      return { valueDatetime: new Date(v as string) };
    case 'ip':
      return { valueIp: v as string };
    case 'cidr':
      return { valueCidr: v as string };
    case 'reference':
      return { valueRefCiId: v as string };
    default:
      throw new Error(`unknown data type ${def.data_type}`);
  }
}

interface PreparedAttributes {
  set: { def: EffectiveAttributeRow; columns: ValueColumns }[];
  clear: EffectiveAttributeRow[];
}

async function prepareAttributes(
  tx: Executor,
  defs: EffectiveAttributeRow[],
  input: AttrInput | undefined,
  className: string,
  selfId: string | undefined,
): Promise<PreparedAttributes> {
  const byKey = new Map(defs.map((d) => [d.key, d]));
  const errors: FieldError[] = [];
  const prepared: PreparedAttributes = { set: [], clear: [] };
  const refs: { def: EffectiveAttributeRow; id: string }[] = [];

  for (const [key, value] of Object.entries(input ?? {})) {
    const field = `attributes.${key}`;
    const def = byKey.get(key);
    if (!def) {
      errors.push({ in: 'body', field, message: `Class "${className}" has no attribute "${key}"`, code: 'unknown_attribute' });
      continue;
    }
    if (value === null) {
      prepared.clear.push(def);
      continue;
    }
    if (!def.is_active) {
      errors.push({ in: 'body', field, message: 'This attribute is retired and cannot receive new values', code: 'attribute_inactive' });
      continue;
    }
    const parsed = valueSchema(def).safeParse(value);
    if (!parsed.success) {
      for (const issue of parsed.error.issues) errors.push({ in: 'body', field, message: issue.message, code: issue.code });
      continue;
    }
    if (def.data_type === 'reference') refs.push({ def, id: value as string });
    prepared.set.push({ def, columns: toColumns(def, value) });
  }

  if (refs.length) {
    const live = await liveItems(tx, refs.map((r) => r.id));
    for (const r of refs) {
      const field = `attributes.${r.def.key}`;
      if (r.id === selfId) errors.push({ in: 'body', field, message: 'A CI cannot reference itself', code: 'reference_self' });
      else if (!live.has(r.id)) errors.push({ in: 'body', field, message: 'Referenced CI does not exist or is deleted', code: 'not_found' });
    }
  }

  if (errors.length) throw AppError.validation(errors);
  return prepared;
}

async function writeAttributes(tx: Tx, ciId: string, set: PreparedAttributes['set']) {
  for (const { def, columns } of set) {
    try {
      // A savepoint keeps the transaction usable so the error can be reported per field.
      await tx.transaction(async (sp) => upsertAttributeValue(sp, ciId, def.id, columns));
    } catch (err) {
      throw mapPgError(err, `attributes.${def.key}`) ?? err;
    }
  }
}

async function checkRequired(tx: Executor, ciId: string, defs: EffectiveAttributeRow[]) {
  const present = new Set((await attributeValues(tx, [ciId])).map((v) => v.key));
  const missing = defs.filter((d) => d.is_required && d.is_active && !present.has(d.key));
  if (missing.length) {
    throw AppError.validation(
      missing.map((d) => ({ in: 'body' as const, field: `attributes.${d.key}`, message: `${d.label} is required`, code: 'required' })),
    );
  }
}

// ---------------------------------------------------------------------------

export class ItemService {
  constructor(private readonly db: Database) {}

  private async filters(q: z.output<typeof ListItemsQuery> | z.output<typeof SearchQuery>): Promise<ItemFilters> {
    return {
      classIds: q.classId ? (q.includeSubclasses ? await withDescendantClasses(this.db, q.classId) : q.classId) : undefined,
      statusIds: q.statusId,
      environmentIds: q.environmentId,
      ownerIds: q.ownerId,
      locationIds: q.locationId,
      ipWithin: q.ipWithin,
      deleted: q.deleted,
    };
  }

  async list(q: z.output<typeof ListItemsQuery>): Promise<Page<Summary>> {
    const f = { ...(await this.filters(q)), q: q.q };
    const { rows, total } = await listItems(this.db, f, q.sort as { field: ItemSortField; desc: boolean }, q.limit, q.offset);
    return { data: rows.map(summaryDto), page: { limit: q.limit, offset: q.offset, total } };
  }

  async search(q: z.output<typeof SearchQuery>): Promise<z.output<typeof SearchResults>> {
    const { rows, total } = await searchItems(this.db, q.q, await this.filters(q), q.limit, q.offset);
    const values = await attributeValues(this.db, rows.map((r) => r.ci.id));
    const needle = q.q.toLowerCase();
    const hit = (s: string | null | undefined) => !!s && s.toLowerCase().includes(needle);
    const words = needle.match(/[\p{L}\p{N}]+/gu) ?? [];

    return {
      data: rows.map((r) => {
        const matches: z.output<typeof SearchResults>['data'][number]['matches'] = [];
        const c = r.ci;
        if (hit(c.name)) matches.push({ field: 'name', label: 'Name', value: c.name });
        if (hit(c.hostname)) matches.push({ field: 'hostname', label: 'Hostname', value: c.hostname! });
        if (hit(c.serialNumber)) matches.push({ field: 'serialNumber', label: 'Serial number', value: c.serialNumber! });
        if (c.ipAddress && (c.ipAddress.startsWith(q.q) || isIpOrCidr(q.q)))
          matches.push({ field: 'ipAddress', label: 'IP address', value: c.ipAddress });
        if (c.notes && (hit(c.notes) || (words.length && words.every((w) => c.notes!.toLowerCase().includes(w)))))
          matches.push({ field: 'notes', label: 'Notes', value: c.notes.length > 200 ? `${c.notes.slice(0, 200)}…` : c.notes });
        for (const v of values) {
          if (v.ci_id !== c.id) continue;
          const text = v.value_text ?? v.value_ip ?? v.value_cidr;
          if (text && (hit(text) || ((v.value_ip || v.value_cidr) && text.startsWith(q.q))))
            matches.push({ field: `attributes.${v.key}`, label: v.label, value: text });
        }
        return { item: summaryDto(r), matches };
      }),
      page: { limit: q.limit, offset: q.offset, total },
    };
  }

  async detail(db: Executor, id: string): Promise<Detail | undefined> {
    const row = await itemSummary(db, id);
    if (!row) return undefined;
    const values = await attributeValues(db, [id]);
    const attributes: Detail['attributes'] = {};
    const attributeReferences: Detail['attributeReferences'] = {};
    for (const v of values) {
      const json = valueToJson(v);
      if (json === null) continue;
      attributes[v.key] = json;
      if (v.value_ref_ci_id) {
        attributeReferences[v.key] = { id: v.value_ref_ci_id, name: v.ref_name ?? '', deleted: !!v.ref_deleted };
      }
    }
    return { ...summaryDto(row), attributes, attributeReferences };
  }

  async get(id: string): Promise<Detail> {
    const d = await this.detail(this.db, id);
    if (!d) throw AppError.notFound('Configuration item', id);
    return d;
  }

  private async classInfo(tx: Executor, classId: string) {
    const [cls] = await tx
      .select({ key: ciClasses.key, isAbstract: ciClasses.isAbstract, isActive: ciClasses.isActive })
      .from(ciClasses)
      .where(eq(ciClasses.id, classId));
    if (!cls) throw AppError.field('classId', 'CI class does not exist', 'not_found');
    return cls;
  }

  async create(ctx: RequestContext, input: z.output<typeof CreateItemBody>): Promise<Detail> {
    return this.db.transaction(async (tx) => {
      const cls = await this.classInfo(tx, input.classId);
      const defs = await effectiveAttributes(tx, input.classId);
      const prepared = await prepareAttributes(tx, defs, input.attributes, cls.key, undefined);

      const { attributes: _a, ...core } = input;
      const [row] = await tx.insert(configurationItems).values(core).returning({ id: configurationItems.id });
      const id = row!.id;
      await writeAttributes(tx, id, prepared.set);
      await checkRequired(tx, id, defs);

      const dto = (await this.detail(tx, id))!;
      await writeAudit(tx, ctx, { action: 'create', entityType: 'configuration_items', entityId: id, newValue: dto });
      return dto;
    });
  }

  async update(ctx: RequestContext, id: string, input: z.output<typeof UpdateItemBody>): Promise<Detail> {
    return this.db.transaction(async (tx) => {
      const before = await selectById<CiRow>(tx, configurationItems, id, true);
      if (!before) throw AppError.notFound('Configuration item', id);
      if (before.deletedAt) throw new AppError('CONFLICT', 'This configuration item is deleted and cannot be modified');
      if (input.version !== undefined && input.version !== before.version) {
        throw new AppError(
          'VERSION_CONFLICT',
          `The item was changed by someone else (you sent version ${input.version}, current is ${before.version}). Reload and retry.`,
          [{ in: 'body', field: 'version', message: `Current version is ${before.version}`, code: 'stale' }],
        );
      }
      const beforeDto = (await this.detail(tx, id))!;

      const classId = input.classId ?? before.classId;
      const cls = await this.classInfo(tx, classId);
      const defs = await effectiveAttributes(tx, classId);
      const prepared = await prepareAttributes(tx, defs, input.attributes, cls.key, id);

      if (classId !== before.classId) {
        // Values the new class does not define must be cleared in the same request.
        const keep = new Set(defs.map((d) => d.key));
        const cleared = new Set(Object.entries(input.attributes ?? {}).filter(([, v]) => v === null).map(([k]) => k));
        const orphaned = Object.keys(beforeDto.attributes).filter((k) => !keep.has(k) && !cleared.has(k));
        if (orphaned.length) {
          throw AppError.field(
            'classId',
            `The new class does not define: ${orphaned.join(', ')}. Clear them in the same request ("attributes": {"${orphaned[0]}": null}).`,
            'attributes_outside_class',
          );
        }
        const current = await attributeValues(tx, [id]);
        await deleteAttributeValues(
          tx,
          id,
          current.filter((v) => !keep.has(v.key)).map((v) => v.attribute_id),
        );
      }
      await deleteAttributeValues(tx, id, prepared.clear.map((d) => d.id));

      const { attributes: _a, version: _v, ...core } = input;
      await tx
        .update(configurationItems)
        .set({ ...core, version: sql`${configurationItems.version} + 1` })
        .where(eq(configurationItems.id, id));
      await writeAttributes(tx, id, prepared.set);
      await checkRequired(tx, id, defs);

      const dto = (await this.detail(tx, id))!;
      await writeAudit(tx, ctx, { action: 'update', entityType: 'configuration_items', entityId: id, oldValue: beforeDto, newValue: dto });
      return dto;
    });
  }

  /** Soft delete: the CI and its live relationships get deleted_at; history keeps resolving. */
  async remove(ctx: RequestContext, id: string): Promise<void> {
    await this.db.transaction(async (tx) => {
      const before = await selectById<CiRow>(tx, configurationItems, id, true);
      if (!before || before.deletedAt) throw AppError.notFound('Configuration item', id);
      const beforeDto = (await this.detail(tx, id))!;
      const edges = await softDeleteEdgesOf(tx, id);
      await tx
        .update(configurationItems)
        .set({ deletedAt: sql`now()`, version: sql`${configurationItems.version} + 1` })
        .where(eq(configurationItems.id, id));
      await writeAudit(tx, ctx, [
        ...edges.map((e) => ({
          action: 'delete' as const,
          entityType: 'ci_relationships',
          entityId: e.id,
          oldValue: serialise({ ...e, deletedAt: null }),
        })),
        { action: 'delete', entityType: 'configuration_items', entityId: id, oldValue: beforeDto },
      ]);
    });
  }

  /**
   * Breadth-first expansion from a root CI: one query per hop (not per CI),
   * then one query for all node summaries.
   */
  async graph(rootId: string, q: z.output<typeof GraphQuery>): Promise<z.output<typeof Graph>> {
    const root = await itemSummary(this.db, rootId);
    if (!root) throw AppError.notFound('Configuration item', rootId);

    const depthOf = new Map<string, number>([[rootId, 0]]);
    const edges = new Map<string, EdgeRow>();
    let frontier = [rootId];
    let truncated = false;

    for (let hop = 1; hop <= q.depth && frontier.length > 0; hop++) {
      const found = await edgesTouching(this.db, frontier, q.direction, q.relationshipTypeId);
      const next: string[] = [];
      for (const e of found) {
        for (const other of [e.sourceCiId, e.targetCiId]) {
          if (depthOf.has(other)) continue;
          if (depthOf.size >= q.maxNodes) {
            truncated = true;
            continue;
          }
          depthOf.set(other, hop);
          next.push(other);
        }
        if (depthOf.has(e.sourceCiId) && depthOf.has(e.targetCiId)) edges.set(e.id, e);
      }
      frontier = next;
    }
    // Edges between nodes discovered at the last hop (e.g. app -> db when both
    // hang off the same server) are included too, so the picture is complete.
    if (frontier.length) {
      for (const e of await edgesTouching(this.db, frontier, 'both', q.relationshipTypeId)) {
        if (depthOf.has(e.sourceCiId) && depthOf.has(e.targetCiId)) edges.set(e.id, e);
      }
    }

    const summaries = await itemSummaries(this.db, [...depthOf.keys()]);
    const nodes = summaries
      .map((s) => ({ ...summaryDto(s), depth: depthOf.get(s.ci.id)! }))
      .sort((a, b) => a.depth - b.depth || a.name.localeCompare(b.name));

    return {
      rootId,
      depth: q.depth,
      direction: q.direction,
      nodes,
      edges: [...edges.values()].map((e) => ({
        id: e.id,
        relationshipTypeId: e.relationshipTypeId,
        type: { key: e.typeKey, name: e.typeName, forwardLabel: e.forwardLabel, reverseLabel: e.reverseLabel, isDirectional: e.isDirectional },
        sourceCiId: e.sourceCiId,
        targetCiId: e.targetCiId,
        notes: e.notes,
      })),
      truncated,
    };
  }
}
