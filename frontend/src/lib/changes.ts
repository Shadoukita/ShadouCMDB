/**
 * The entries of `body` that differ from `before` (compared as JSON), for a PATCH
 * that sends only what the user changed. The API checks what is written: a stored
 * value it would now refuse (e.g. a line break in a name from an older import,
 * GH#289) must not block changing another field of the same row.
 */
export function changedFields<T extends Record<string, unknown>>(body: T, before: Record<string, unknown>): Partial<T> {
  const out: Partial<T> = {};
  for (const k of Object.keys(body) as (keyof T & string)[]) {
    if (JSON.stringify(body[k]) !== JSON.stringify(before[k])) out[k] = body[k];
  }
  return out;
}
