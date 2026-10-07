import type { EffectivePermissions, GlobalPermission } from "../api/admin";
import { t } from "../i18n";

/**
 * Client-side reading of GET /auth/me → permissions, used to hide or disable
 * actions. It mirrors the server's rules; the server still enforces every one.
 */
export type ClassRight = "view" | "create" | "edit" | "delete";

/** Global permissions with operator-facing wording. Order is the order of the profile editor. */
export const GLOBAL_PERMISSIONS: { key: GlobalPermission; label: string; hint: string }[] = (
  [
    "users.manage",
    "profiles.manage",
    "datamodel.manage",
    "customization.manage",
    "config.export_import",
    "audit.view",
    "cis.import",
    "views.share",
    "workflows.manage",
  ] as const
).map((key) => ({ key, label: t(`permission.${key}`), hint: t(`permission.${key}.hint`) }));

export const CLASS_RIGHTS: ClassRight[] = ["view", "create", "edit", "delete"];

export function hasGlobal(p: EffectivePermissions | undefined, perm: GlobalPermission): boolean {
  return !!p && (p.administrator || p.global.includes(perm));
}

/** Rights on one class: the wildcard grant or that exact class's grant (class grants are not inherited). */
export function canClass(p: EffectivePermissions | undefined, classId: string | undefined, right: ClassRight): boolean {
  if (!p) return false;
  if (p.administrator || allows(p.allClasses, right)) return true;
  if (!classId) return false;
  const grant = p.classes.find((c) => c.classId === classId);
  return !!grant && allows(grant, right);
}

/** True when the right is held on at least one class (e.g. to show "+ New CI" at all). */
export function canAnyClass(p: EffectivePermissions | undefined, right: ClassRight): boolean {
  if (!p) return false;
  return p.administrator || allows(p.allClasses, right) || p.classes.some((c) => allows(c, right));
}

function allows(r: Record<ClassRight, boolean>, right: ClassRight): boolean {
  // Write rights imply view, as on the server.
  return right === "view" ? r.view || r.create || r.edit || r.delete : r[right];
}

/**
 * The classes worth offering to a user: every concrete class they may view, plus each abstract ancestor of
 * one (filtering by an abstract class lists its subclasses). Any other class would only ever list zero CIs,
 * which reads as "empty" rather than "not yours". Grants are per exact class, so an abstract class's own
 * grant shows nothing by itself.
 */
export function viewableClasses<C extends { id: string; parentId: string | null; isAbstract: boolean }>(
  all: C[],
  canView: (classId: string) => boolean,
): C[] {
  const byId = new Map(all.map((c) => [c.id, c]));
  const keep = new Set<string>();
  for (const c of all) {
    if (c.isAbstract || !canView(c.id)) continue;
    for (let p: C | undefined = c; p && !keep.has(p.id); p = p.parentId ? byId.get(p.parentId) : undefined) keep.add(p.id);
  }
  return all.filter((c) => keep.has(c.id));
}
