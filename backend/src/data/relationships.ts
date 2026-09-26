import { and, asc, count, desc, eq, ilike, inArray, isNotNull, isNull, or, sql, type SQL } from 'drizzle-orm';
import { type PgColumn, alias } from 'drizzle-orm/pg-core';
import { ciClasses, ciRelationships, configurationItems, relationshipTypes } from '../db/schema/index.js';
import { likePattern } from '../http/schemas.js';
import type { Executor } from './crud.js';

const rel = ciRelationships;
const src = alias(configurationItems, 'src');
const tgt = alias(configurationItems, 'tgt');
const srcClass = alias(ciClasses, 'src_class');
const tgtClass = alias(ciClasses, 'tgt_class');

function relationshipSelect(db: Executor) {
  return db
    .select({
      rel: {
        id: rel.id,
        relationshipTypeId: rel.relationshipTypeId,
        sourceCiId: rel.sourceCiId,
        targetCiId: rel.targetCiId,
        notes: rel.notes,
        createdAt: rel.createdAt,
        updatedAt: rel.updatedAt,
        deletedAt: rel.deletedAt,
      },
      type: {
        id: relationshipTypes.id,
        key: relationshipTypes.key,
        name: relationshipTypes.name,
        forwardLabel: relationshipTypes.forwardLabel,
        reverseLabel: relationshipTypes.reverseLabel,
        isDirectional: relationshipTypes.isDirectional,
      },
      source: { id: src.id, name: src.name, classKey: srcClass.key, className: srcClass.name, deletedAt: src.deletedAt },
      target: { id: tgt.id, name: tgt.name, classKey: tgtClass.key, className: tgtClass.name, deletedAt: tgt.deletedAt },
    })
    .from(rel)
    .innerJoin(relationshipTypes, eq(relationshipTypes.id, rel.relationshipTypeId))
    .innerJoin(src, eq(src.id, rel.sourceCiId))
    .innerJoin(srcClass, eq(srcClass.id, src.classId))
    .innerJoin(tgt, eq(tgt.id, rel.targetCiId))
    .innerJoin(tgtClass, eq(tgtClass.id, tgt.classId));
}

export type RelationshipRow = Awaited<ReturnType<ReturnType<typeof relationshipSelect>['execute']>>[number];

export const RELATIONSHIP_SORT_FIELDS = ['createdAt', 'updatedAt', 'sourceName', 'targetName', 'typeName'] as const;
export type RelationshipSortField = (typeof RELATIONSHIP_SORT_FIELDS)[number];

const sortColumns: Record<RelationshipSortField, SQL | PgColumn> = {
  createdAt: rel.createdAt,
  updatedAt: rel.updatedAt,
  sourceName: sql`lower(${src.name})`,
  targetName: sql`lower(${tgt.name})`,
  typeName: sql`lower(${relationshipTypes.name})`,
};

export interface RelationshipFilters {
  q?: string;
  ciIds?: string[];
  sourceCiIds?: string[];
  targetCiIds?: string[];
  relationshipTypeIds?: string[];
  deleted: 'exclude' | 'include' | 'only';
}

export async function listRelationships(
  db: Executor,
  f: RelationshipFilters,
  sort: { field: RelationshipSortField; desc: boolean },
  limit: number,
  offset: number,
): Promise<{ rows: RelationshipRow[]; total: number }> {
  const where = and(
    f.deleted === 'exclude' ? isNull(rel.deletedAt) : f.deleted === 'only' ? isNotNull(rel.deletedAt) : undefined,
    f.ciIds ? or(inArray(rel.sourceCiId, f.ciIds), inArray(rel.targetCiId, f.ciIds)) : undefined,
    f.sourceCiIds ? inArray(rel.sourceCiId, f.sourceCiIds) : undefined,
    f.targetCiIds ? inArray(rel.targetCiId, f.targetCiIds) : undefined,
    f.relationshipTypeIds ? inArray(rel.relationshipTypeId, f.relationshipTypeIds) : undefined,
    f.q
      ? or(
          ilike(src.name, likePattern(f.q)),
          ilike(tgt.name, likePattern(f.q)),
          ilike(rel.notes, likePattern(f.q)),
          ilike(relationshipTypes.name, likePattern(f.q)),
        )
      : undefined,
  );
  const col = sortColumns[sort.field];
  const [rows, totals] = await Promise.all([
    relationshipSelect(db)
      .where(where)
      .orderBy(sort.desc ? desc(col) : asc(col), asc(rel.id))
      .limit(limit)
      .offset(offset),
    db
      .select({ n: count() })
      .from(rel)
      .innerJoin(relationshipTypes, eq(relationshipTypes.id, rel.relationshipTypeId))
      .innerJoin(src, eq(src.id, rel.sourceCiId))
      .innerJoin(tgt, eq(tgt.id, rel.targetCiId))
      .where(where),
  ]);
  return { rows, total: totals[0]?.n ?? 0 };
}

export async function getRelationship(db: Executor, id: string): Promise<RelationshipRow | undefined> {
  const rows = await relationshipSelect(db).where(eq(rel.id, id));
  return rows[0];
}
