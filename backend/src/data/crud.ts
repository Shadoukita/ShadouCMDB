import { and, asc, count, desc, eq, type SQL } from 'drizzle-orm';
import type { PgColumn, PgTable } from 'drizzle-orm/pg-core';
import type { Database } from '../db/client.js';
import { auditLog } from '../db/schema/index.js';
import type { RequestContext } from '../http/context.js';

/**
 * Data-access helpers shared by the resource repositories. Everything that
 * talks SQL lives under src/data; services call these and never build HTTP
 * responses, routes call services and never touch the database.
 */

export type Tx = Parameters<Parameters<Database['transaction']>[0]>[0];
export type Executor = Database | Tx;

// Drizzle's table generics do not compose well across helpers; the resource
// modules keep their own precise row types and cast at the boundary.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type AnyTable = PgTable & { id: PgColumn } & Record<string, any>;

export interface ListArgs {
  where: (SQL | undefined)[];
  orderBy: SQL[];
  limit: number;
  offset: number;
}

export async function selectPage<Row>(db: Executor, table: AnyTable, args: ListArgs): Promise<{ rows: Row[]; total: number }> {
  const where = and(...args.where.filter(Boolean));
  const [rows, totals] = await Promise.all([
    db.select().from(table).where(where).orderBy(...args.orderBy).limit(args.limit).offset(args.offset),
    db.select({ n: count() }).from(table).where(where),
  ]);
  return { rows: rows as Row[], total: totals[0]?.n ?? 0 };
}

export async function selectById<Row>(db: Executor, table: AnyTable, id: string, forUpdate = false): Promise<Row | undefined> {
  const q = db.select().from(table).where(eq(table.id, id));
  const rows = forUpdate ? await q.for('update') : await q;
  return rows[0] as Row | undefined;
}

export async function insertRow<Row>(tx: Executor, table: AnyTable, values: Record<string, unknown>): Promise<Row> {
  const rows = (await tx.insert(table).values(values).returning()) as Row[];
  return rows[0]!;
}

export async function updateRow<Row>(tx: Executor, table: AnyTable, id: string, values: Record<string, unknown>): Promise<Row> {
  const rows = (await tx.update(table).set(values).where(eq(table.id, id)).returning()) as Row[];
  return rows[0]!;
}

export async function deleteRow(tx: Executor, table: AnyTable, id: string): Promise<void> {
  await tx.delete(table).where(eq(table.id, id));
}

export const order = (col: PgColumn | SQL, isDesc: boolean): SQL => (isDesc ? desc(col) : asc(col));

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

export interface AuditEntry {
  action: 'create' | 'update' | 'delete' | 'restore';
  entityType: string;
  entityId: string;
  oldValue?: unknown;
  newValue?: unknown;
}

/** Append audit rows in the caller's transaction so a change and its audit commit together. */
export async function writeAudit(tx: Executor, ctx: RequestContext, entries: AuditEntry | AuditEntry[]): Promise<void> {
  const list = Array.isArray(entries) ? entries : [entries];
  if (list.length === 0) return;
  await tx.insert(auditLog).values(
    list.map((e) => ({
      actorType: ctx.actor.type,
      actorId: ctx.actor.id,
      actorName: ctx.actor.name,
      action: e.action,
      entityType: e.entityType,
      entityId: e.entityId,
      oldValue: e.oldValue ?? null,
      newValue: e.newValue ?? null,
      requestId: ctx.requestId,
    })),
  );
}

// ---------------------------------------------------------------------------
// Serialisation
// ---------------------------------------------------------------------------

/** Row -> JSON-ready object: Date -> ISO string, drops the given internal columns. */
export function serialise<T extends Record<string, unknown>>(row: T, omit: readonly string[] = []): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(row)) {
    if (omit.includes(k)) continue;
    out[k] = v instanceof Date ? v.toISOString() : v;
  }
  return out;
}

/** Keep only keys whose value is not undefined (PATCH semantics). */
export function defined<T extends Record<string, unknown>>(obj: T): Partial<T> {
  return Object.fromEntries(Object.entries(obj).filter(([, v]) => v !== undefined)) as Partial<T>;
}
