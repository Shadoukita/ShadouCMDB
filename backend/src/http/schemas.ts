import { z } from 'zod';
import { KEY_PATTERN } from '../db/schema/common.js';

/**
 * Named response components for the OpenAPI document. Only response-side
 * schemas are registered here; request schemas are emitted inline per route.
 */
export const components: { id: string; schema: z.ZodType }[] = [];

export function component<T extends z.ZodType>(id: string, schema: T): T {
  if (components.some((c) => c.id === id)) throw new Error(`Duplicate OpenAPI component id ${id}`);
  components.push({ id, schema });
  return schema;
}

// ---------------------------------------------------------------------------
// Scalars
// ---------------------------------------------------------------------------

export const Uuid = z.uuid();
export const Key = z
  .string()
  .regex(new RegExp(KEY_PATTERN), 'Must be lower_snake_case: a letter, then letters, digits or _ (max 63)')
  .describe('Stable machine key, lower_snake_case');
export const Name = z.string().trim().min(1, 'Must not be blank').max(200);
export const Description = z.string().max(4000);
export const Timestamp = z.iso.datetime({ offset: true });
export const IdParams = z.object({ id: Uuid });

/** Query-string boolean: only the literal strings "true" / "false". */
export const QueryBool = z.enum(['true', 'false']).transform((v) => v === 'true');

/** Comma-separated list of uuids (repeated keys are joined with commas first). */
export const QueryUuidList = z
  .string()
  .transform((s) => s.split(',').map((v) => v.trim()).filter(Boolean))
  .pipe(z.array(Uuid).min(1).max(100))
  .describe('One or more ids, comma-separated');

// ---------------------------------------------------------------------------
// Pagination, sort, search
// ---------------------------------------------------------------------------

export const MAX_PAGE_SIZE = 200;

export const PageQuery = {
  limit: z.coerce.number().int().min(1).max(MAX_PAGE_SIZE).default(50).describe(`Page size (1-${MAX_PAGE_SIZE})`),
  offset: z.coerce.number().int().min(0).max(1_000_000).default(0).describe('Rows to skip'),
};

export const SearchQuery = {
  q: z.string().trim().min(1).max(200).optional().describe('Case-insensitive substring search'),
};

/** `sort=name` ascending, `sort=-name` descending; only the listed fields are accepted. */
export function sortParam<const F extends readonly [string, ...string[]]>(fields: F, fallback: string) {
  const values = fields.flatMap((f) => [f, `-${f}`]) as [string, ...string[]];
  return z
    .enum(values)
    .default(fallback)
    .describe(`Sort field; prefix with "-" for descending. One of: ${fields.join(', ')}`)
    .transform((s) => ({ field: s.replace(/^-/, '') as F[number], desc: s.startsWith('-') }));
}

export type Sort<F extends string> = { field: F; desc: boolean };

export const PageMeta = component(
  'PageMeta',
  z.object({
    limit: z.number().int(),
    offset: z.number().int(),
    total: z.number().int().describe('Total rows matching the filters'),
  }),
);

export function listOf<T extends z.ZodType>(id: string, item: T) {
  return component(id, z.object({ data: z.array(item), page: PageMeta }));
}

export interface Page<T> {
  data: T[];
  page: { limit: number; offset: number; total: number };
}

/** Escape LIKE wildcards so user input is matched literally. */
export const likePattern = (q: string) => `%${q.replace(/[\\%_]/g, (c) => `\\${c}`)}%`;

// ---------------------------------------------------------------------------
// Small embedded references (avoid N+1 lookups in the UI)
// ---------------------------------------------------------------------------

export const LookupRef = component(
  'LookupRef',
  z.object({ id: Uuid, key: z.string(), name: z.string() }).describe('Compact reference to a lookup row'),
);
export const OwnerRef = component('OwnerRef', z.object({ id: Uuid, name: z.string(), kind: z.enum(['person', 'team']) }));
