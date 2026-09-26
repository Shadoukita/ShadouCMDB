import { isIP } from 'node:net';
import { and, asc, count, desc, eq, inArray, isNotNull, isNull, sql, type SQL } from 'drizzle-orm';
import { type PgColumn, alias } from 'drizzle-orm/pg-core';
import {
  ciAttributeValues,
  ciClasses,
  ciRelationships,
  configurationItems,
  environments,
  locations,
  owners,
  relationshipTypes,
  statuses,
} from '../db/schema/index.js';
import { likePattern } from '../http/schemas.js';
import type { Executor } from './crud.js';

/*
 * SQL for configuration items: inventory list, detail, global search,
 * attribute values and relationship-graph expansion.
 */

// ---------------------------------------------------------------------------
// Summary rows (CI + embedded class / status / environment / owner / location)
// ---------------------------------------------------------------------------

const ci = configurationItems;

const summaryColumns = {
  ci: {
    id: ci.id,
    name: ci.name,
    classId: ci.classId,
    statusId: ci.statusId,
    environmentId: ci.environmentId,
    ownerId: ci.ownerId,
    locationId: ci.locationId,
    hostname: ci.hostname,
    ipAddress: ci.ipAddress,
    serialNumber: ci.serialNumber,
    notes: ci.notes,
    version: ci.version,
    createdAt: ci.createdAt,
    updatedAt: ci.updatedAt,
    deletedAt: ci.deletedAt,
  },
  class: { id: ciClasses.id, key: ciClasses.key, name: ciClasses.name },
  status: { id: statuses.id, key: statuses.key, name: statuses.name },
  environment: { id: environments.id, key: environments.key, name: environments.name },
  owner: { id: owners.id, name: owners.name, kind: owners.kind },
  location: { id: locations.id, key: locations.key, name: locations.name },
};

function summarySelect(db: Executor) {
  return db
    .select(summaryColumns)
    .from(ci)
    .innerJoin(ciClasses, eq(ciClasses.id, ci.classId))
    .innerJoin(statuses, eq(statuses.id, ci.statusId))
    .leftJoin(environments, eq(environments.id, ci.environmentId))
    .leftJoin(owners, eq(owners.id, ci.ownerId))
    .leftJoin(locations, eq(locations.id, ci.locationId));
}

export type SummaryRow = Awaited<ReturnType<ReturnType<typeof summarySelect>['execute']>>[number];

export const ITEM_SORT_FIELDS = [
  'name',
  'hostname',
  'ipAddress',
  'serialNumber',
  'className',
  'statusName',
  'createdAt',
  'updatedAt',
] as const;
export type ItemSortField = (typeof ITEM_SORT_FIELDS)[number];

const sortColumns: Record<ItemSortField, SQL | PgColumn> = {
  name: sql`lower(${ci.name})`,
  hostname: sql`lower(${ci.hostname})`,
  ipAddress: sql`${ci.ipAddress}`,
  serialNumber: sql`${ci.serialNumber}`,
  className: sql`lower(${ciClasses.name})`,
  statusName: statuses.sortOrder,
  createdAt: ci.createdAt,
  updatedAt: ci.updatedAt,
};

export interface ItemFilters {
  q?: string;
  classIds?: string[];
  statusIds?: string[];
  environmentIds?: string[];
  ownerIds?: string[];
  locationIds?: string[];
  ipWithin?: string;
  ids?: string[];
  deleted: 'exclude' | 'include' | 'only';
}

/** Class ids plus every descendant class, so filtering by "hardware" finds servers too. */
export async function withDescendantClasses(db: Executor, classIds: string[]): Promise<string[]> {
  const res = await db.execute<{ id: string }>(sql`
    WITH RECURSIVE down AS (
      SELECT id, 0 AS depth FROM ci_classes WHERE id = ANY(${sql.param(classIds)}::uuid[])
      UNION
      SELECT c.id, down.depth + 1 FROM ci_classes c JOIN down ON c.parent_id = down.id WHERE down.depth < 64
    )
    SELECT DISTINCT id FROM down`);
  return res.rows.map((r) => r.id);
}

/** Words of a query turned into a prefix tsquery ("web prod" -> 'web:* & prod:*'). */
function prefixTsQuery(q: string): string | undefined {
  const words = q.toLowerCase().match(/[\p{L}\p{N}]+/gu);
  return words?.length ? words.slice(0, 8).map((w) => `${w}:*`).join(' & ') : undefined;
}

export const isIpOrCidr = (s: string) => isIP(s) !== 0 || isCidr(s);

const isCidr = (s: string) => {
  const [addr, bits, extra] = s.split('/');
  return extra === undefined && !!addr && isIP(addr) !== 0 && bits !== undefined && /^\d{1,3}$/.test(bits);
};

/**
 * The search predicate shared by the inventory list and global search: name,
 * hostname and serial (substring, trigram-indexed), notes (word prefix, via the
 * tsvector), IP address (prefix, or containment when q is an IP/CIDR) and
 * attribute values (text/enum substring, IP/CIDR prefix).
 */
export function searchCondition(q: string): SQL {
  const pattern = likePattern(q);
  const prefix = `${q.replace(/[\\%_]/g, (c) => `\\${c}`)}%`;
  const tsq = prefixTsQuery(q);
  const parts: SQL[] = [
    sql`${ci.name} ILIKE ${pattern}`,
    sql`${ci.hostname} ILIKE ${pattern}`,
    sql`${ci.serialNumber} ILIKE ${pattern}`,
    sql`host(${ci.ipAddress}) LIKE ${prefix}`,
    sql`EXISTS (SELECT 1 FROM ci_attribute_values v WHERE v.ci_id = ${ci.id}
          AND (v.value_text ILIKE ${pattern} OR host(v.value_ip) LIKE ${prefix} OR v.value_cidr::text LIKE ${prefix}))`,
  ];
  if (tsq) parts.push(sql`${ci.searchVector} @@ to_tsquery('simple', ${tsq})`);
  if (isIpOrCidr(q)) parts.push(sql`${ci.ipAddress} <<= ${q}::inet`);
  return sql`(${sql.join(parts, sql` OR `)})`;
}

function filterConditions(f: ItemFilters): (SQL | undefined)[] {
  return [
    f.deleted === 'exclude' ? isNull(ci.deletedAt) : f.deleted === 'only' ? isNotNull(ci.deletedAt) : undefined,
    f.q ? searchCondition(f.q) : undefined,
    f.ids ? inArray(ci.id, f.ids) : undefined,
    f.classIds ? inArray(ci.classId, f.classIds) : undefined,
    f.statusIds ? inArray(ci.statusId, f.statusIds) : undefined,
    f.environmentIds ? inArray(ci.environmentId, f.environmentIds) : undefined,
    f.ownerIds ? inArray(ci.ownerId, f.ownerIds) : undefined,
    f.locationIds ? inArray(ci.locationId, f.locationIds) : undefined,
    f.ipWithin ? sql`${ci.ipAddress} <<= ${f.ipWithin}::inet` : undefined,
  ];
}

export async function listItems(
  db: Executor,
  f: ItemFilters,
  sort: { field: ItemSortField; desc: boolean },
  limit: number,
  offset: number,
): Promise<{ rows: SummaryRow[]; total: number }> {
  const where = and(...filterConditions(f).filter(Boolean));
  const col = sortColumns[sort.field];
  const primary = sort.desc ? sql`${col} DESC NULLS LAST` : sql`${col} ASC NULLS LAST`;
  const [rows, totals] = await Promise.all([
    summarySelect(db).where(where).orderBy(primary, asc(ci.id)).limit(limit).offset(offset),
    db.select({ n: count() }).from(ci).where(where),
  ]);
  return { rows, total: totals[0]?.n ?? 0 };
}

/** Global search: same predicate as the list, ranked by exact / prefix / trigram similarity. */
export async function searchItems(
  db: Executor,
  q: string,
  f: Omit<ItemFilters, 'q'>,
  limit: number,
  offset: number,
): Promise<{ rows: SummaryRow[]; total: number }> {
  const where = and(...filterConditions({ ...f, q }).filter(Boolean));
  const lower = q.toLowerCase();
  const rank = [
    desc(sql`(lower(${ci.name}) = ${lower} OR lower(${ci.hostname}) = ${lower} OR lower(${ci.serialNumber}) = ${lower} OR host(${ci.ipAddress}) = ${q})`),
    desc(sql`(lower(${ci.name}) LIKE ${`${lower.replace(/[\\%_]/g, (c) => `\\${c}`)}%`})`),
    desc(
      sql`greatest(similarity(${ci.name}, ${q}), similarity(coalesce(${ci.hostname}, ''), ${q}), similarity(coalesce(${ci.serialNumber}, ''), ${q}))`,
    ),
    asc(sql`lower(${ci.name})`),
    asc(ci.id),
  ];
  const [rows, totals] = await Promise.all([
    summarySelect(db).where(where).orderBy(...rank).limit(limit).offset(offset),
    db.select({ n: count() }).from(ci).where(where),
  ]);
  return { rows, total: totals[0]?.n ?? 0 };
}

export async function itemSummaries(db: Executor, ids: string[]): Promise<SummaryRow[]> {
  if (ids.length === 0) return [];
  return summarySelect(db).where(inArray(ci.id, ids));
}

export async function itemSummary(db: Executor, id: string): Promise<SummaryRow | undefined> {
  const rows = await summarySelect(db).where(eq(ci.id, id));
  return rows[0];
}

// ---------------------------------------------------------------------------
// Attribute values
// ---------------------------------------------------------------------------

export interface StoredValueRow {
  ci_id: string;
  attribute_id: string;
  key: string;
  label: string;
  data_type: string;
  value_text: string | null;
  value_number: number | null;
  value_boolean: boolean | null;
  value_date: string | null;
  value_datetime: Date | null;
  value_ip: string | null;
  value_cidr: string | null;
  value_ref_ci_id: string | null;
  ref_name: string | null;
  ref_deleted: boolean | null;
}

export async function attributeValues(db: Executor, ciIds: string[]): Promise<StoredValueRow[]> {
  if (ciIds.length === 0) return [];
  const res = await db.execute<StoredValueRow & Record<string, unknown>>(sql`
    SELECT v.ci_id, v.attribute_id, d.key, d.label, d.data_type,
           v.value_text, v.value_number::float8 AS value_number, v.value_boolean,
           v.value_date::text AS value_date, v.value_datetime,
           host(v.value_ip) AS value_ip, v.value_cidr::text AS value_cidr, v.value_ref_ci_id,
           r.name AS ref_name, (r.deleted_at IS NOT NULL) AS ref_deleted
    FROM ci_attribute_values v
    JOIN ci_attribute_definitions d ON d.id = v.attribute_id
    LEFT JOIN configuration_items r ON r.id = v.value_ref_ci_id
    WHERE v.ci_id = ANY(${sql.param(ciIds)}::uuid[])
    ORDER BY d.sort_order, d.key`);
  return res.rows;
}

export type ValueColumns = Partial<
  Pick<
    typeof ciAttributeValues.$inferInsert,
    'valueText' | 'valueNumber' | 'valueBoolean' | 'valueDate' | 'valueDatetime' | 'valueIp' | 'valueCidr' | 'valueRefCiId'
  >
>;

const EMPTY_VALUE: Required<{ [K in keyof ValueColumns]: null }> = {
  valueText: null,
  valueNumber: null,
  valueBoolean: null,
  valueDate: null,
  valueDatetime: null,
  valueIp: null,
  valueCidr: null,
  valueRefCiId: null,
};

export async function upsertAttributeValue(db: Executor, ciId: string, attributeId: string, value: ValueColumns) {
  const cols = { ...EMPTY_VALUE, ...value };
  await db
    .insert(ciAttributeValues)
    .values({ ciId, attributeId, ...cols })
    .onConflictDoUpdate({ target: [ciAttributeValues.ciId, ciAttributeValues.attributeId], set: cols });
}

export async function deleteAttributeValues(db: Executor, ciId: string, attributeIds: string[]) {
  if (attributeIds.length === 0) return;
  await db
    .delete(ciAttributeValues)
    .where(and(eq(ciAttributeValues.ciId, ciId), inArray(ciAttributeValues.attributeId, attributeIds)));
}

/** Referenced CIs that exist and are not deleted, with their class. */
export async function liveItems(db: Executor, ids: string[]): Promise<Map<string, string>> {
  if (ids.length === 0) return new Map();
  const rows = await db
    .select({ id: ci.id, classId: ci.classId })
    .from(ci)
    .where(and(inArray(ci.id, ids), isNull(ci.deletedAt)));
  return new Map(rows.map((r) => [r.id, r.classId]));
}

// ---------------------------------------------------------------------------
// Relationship graph
// ---------------------------------------------------------------------------

export interface EdgeRow {
  id: string;
  relationshipTypeId: string;
  sourceCiId: string;
  targetCiId: string;
  notes: string | null;
  typeKey: string;
  typeName: string;
  forwardLabel: string;
  reverseLabel: string;
  isDirectional: boolean;
}

/** Live edges touching any of the given CIs, in the requested direction(s). */
export async function edgesTouching(
  db: Executor,
  ciIds: string[],
  direction: 'outgoing' | 'incoming' | 'both',
  typeIds: string[] | undefined,
): Promise<EdgeRow[]> {
  if (ciIds.length === 0) return [];
  const out = inArray(ciRelationships.sourceCiId, ciIds);
  const inc = inArray(ciRelationships.targetCiId, ciIds);
  // Symmetric types (connected_to) are followed both ways whatever the direction.
  const dirCond =
    direction === 'both'
      ? sql`(${out} OR ${inc})`
      : direction === 'outgoing'
        ? sql`(${out} OR (NOT ${relationshipTypes.isDirectional} AND ${inc}))`
        : sql`(${inc} OR (NOT ${relationshipTypes.isDirectional} AND ${out}))`;
  return db
    .select({
      id: ciRelationships.id,
      relationshipTypeId: ciRelationships.relationshipTypeId,
      sourceCiId: ciRelationships.sourceCiId,
      targetCiId: ciRelationships.targetCiId,
      notes: ciRelationships.notes,
      typeKey: relationshipTypes.key,
      typeName: relationshipTypes.name,
      forwardLabel: relationshipTypes.forwardLabel,
      reverseLabel: relationshipTypes.reverseLabel,
      isDirectional: relationshipTypes.isDirectional,
    })
    .from(ciRelationships)
    .innerJoin(relationshipTypes, eq(relationshipTypes.id, ciRelationships.relationshipTypeId))
    .where(
      and(
        isNull(ciRelationships.deletedAt),
        dirCond,
        typeIds ? inArray(ciRelationships.relationshipTypeId, typeIds) : undefined,
      ),
    )
    .orderBy(asc(ciRelationships.createdAt), asc(ciRelationships.id));
}

/** Soft-delete every live edge of a CI; returns the removed edges for auditing. */
export async function softDeleteEdgesOf(db: Executor, ciId: string) {
  return db
    .update(ciRelationships)
    .set({ deletedAt: sql`now()` })
    .where(
      and(
        isNull(ciRelationships.deletedAt),
        sql`(${ciRelationships.sourceCiId} = ${ciId} OR ${ciRelationships.targetCiId} = ${ciId})`,
      ),
    )
    .returning();
}

export const sourceCi = alias(configurationItems, 'source_ci');
export const targetCi = alias(configurationItems, 'target_ci');
