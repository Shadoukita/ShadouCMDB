import { sql } from 'drizzle-orm';
import type { Executor } from './crud.js';

/**
 * Class-hierarchy queries. They lean on the ci_class_lineage() /
 * ci_class_is_a() SQL functions from migration 0002 so the database and the
 * API agree on what "inherits" means.
 */

export interface EffectiveAttributeRow {
  id: string;
  class_id: string;
  key: string;
  label: string;
  description: string | null;
  data_type: string;
  is_required: boolean;
  enum_values: string[] | null;
  reference_class_id: string | null;
  validation: Record<string, unknown> | null;
  group_name: string | null;
  sort_order: number;
  is_active: boolean;
  created_at: Date;
  updated_at: Date;
  depth: number;
  defined_on_key: string;
  defined_on_name: string;
}

/** Attribute definitions of a class and all its ancestors (ancestors first, then sort order). */
export async function effectiveAttributes(db: Executor, classId: string): Promise<EffectiveAttributeRow[]> {
  const res = await db.execute<EffectiveAttributeRow & Record<string, unknown>>(sql`
    SELECT d.*, l.depth, c.key AS defined_on_key, c.name AS defined_on_name
    FROM ci_class_lineage(${classId}) l
    JOIN ci_attribute_definitions d ON d.class_id = l.class_id
    JOIN ci_classes c ON c.id = d.class_id
    ORDER BY l.depth DESC, d.sort_order, d.key`);
  return res.rows;
}

/** Another definition with the same key on an ancestor or descendant would shadow it. */
export async function attributeKeyClash(db: Executor, classId: string, key: string, exceptId: string) {
  const res = await db.execute<{ class_key: string }>(sql`
    SELECT c.key AS class_key
    FROM ci_attribute_definitions d JOIN ci_classes c ON c.id = d.class_id
    WHERE d.key = ${key} AND d.id <> ${exceptId}
      AND (ci_class_is_a(${classId}, d.class_id) OR ci_class_is_a(d.class_id, ${classId}))
    LIMIT 1`);
  return res.rows[0]?.class_key;
}

/** Enum values currently stored for an attribute that are not in the allowed list. */
export async function enumValuesInUse(db: Executor, attributeId: string, allowed: string[]): Promise<string[]> {
  const res = await db.execute<{ v: string }>(sql`
    SELECT DISTINCT value_text AS v FROM ci_attribute_values
    WHERE attribute_id = ${attributeId} AND NOT (value_text = ANY(${sql.param(allowed)}::text[]))
    ORDER BY 1 LIMIT 10`);
  return res.rows.map((r) => r.v);
}

/** Live CIs whose class is exactly this one. */
export async function classHasItems(db: Executor, classId: string): Promise<boolean> {
  const res = await db.execute(sql`
    SELECT 1 FROM configuration_items WHERE class_id = ${classId} AND deleted_at IS NULL LIMIT 1`);
  return res.rows.length > 0;
}

/**
 * After re-parenting a class: attribute values on CIs of this class (or its
 * descendants) whose definition no longer sits in the CI's lineage.
 */
export async function orphanedAttributeValues(db: Executor, classId: string): Promise<string[]> {
  const res = await db.execute<{ key: string }>(sql`
    SELECT DISTINCT d.key
    FROM ci_attribute_values v
    JOIN ci_attribute_definitions d ON d.id = v.attribute_id
    JOIN configuration_items ci ON ci.id = v.ci_id
    WHERE ci_class_is_a(ci.class_id, ${classId}) AND NOT ci_class_is_a(ci.class_id, d.class_id)
    LIMIT 10`);
  return res.rows.map((r) => r.key);
}
