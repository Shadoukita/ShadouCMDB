/**
 * Whether the data model is still empty: no class but the built-in ones (a `systemRole`, such as Business service,
 * which every install has since migration 0033). A fresh install starts at the data model, not at a first CI.
 */
export function dataModelEmpty(classes: readonly { systemRole: string | null }[]): boolean {
  return !classes.some((c) => c.systemRole === null);
}
