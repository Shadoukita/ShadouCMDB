/** Rows that form a tree through parentId (CI classes, locations). */
export interface TreeNode {
  id: string;
  parentId: string | null;
}

/**
 * Orders a parentId tree depth-first so lists and pickers can indent children
 * under their parent. Siblings keep the input order unless `compare` is given.
 * A row whose parent is not in the list is treated as a root.
 */
export function flattenTree<T extends TreeNode>(items: readonly T[], compare?: (a: T, b: T) => number): { item: T; depth: number }[] {
  const ids = new Set(items.map((i) => i.id));
  const byParent = new Map<string | null, T[]>();
  for (const i of items) {
    const parent = i.parentId && ids.has(i.parentId) ? i.parentId : null;
    byParent.set(parent, [...(byParent.get(parent) ?? []), i]);
  }
  const out: { item: T; depth: number }[] = [];
  const seen = new Set<string>();
  const walk = (parent: string | null, depth: number) => {
    const children = byParent.get(parent) ?? [];
    for (const i of compare ? [...children].sort(compare) : children) {
      if (seen.has(i.id)) continue;
      seen.add(i.id);
      out.push({ item: i, depth });
      walk(i.id, depth + 1);
    }
  };
  walk(null, 0);
  return out;
}

/** The ids of every row below `id` (not including it). */
export function descendantIds(items: readonly TreeNode[], id: string): Set<string> {
  const out = new Set<string>();
  const stack = [id];
  while (stack.length > 0) {
    const current = stack.pop()!;
    for (const i of items) {
      if (i.parentId === current && !out.has(i.id)) {
        out.add(i.id);
        stack.push(i.id);
      }
    }
  }
  return out;
}

/** Admin-defined order first, then name: the order of menus, pickers and the class list. */
export function bySortOrder<T extends { sortOrder: number; name: string }>(a: T, b: T): number {
  return a.sortOrder - b.sortOrder || a.name.localeCompare(b.name);
}
