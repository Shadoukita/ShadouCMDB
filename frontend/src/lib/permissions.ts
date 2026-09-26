import type { EffectivePermissions, GlobalPermission } from "../api/admin";

/**
 * Client-side reading of GET /auth/me → permissions, used to hide or disable
 * actions. It mirrors the server's rules; the server still enforces every one.
 */
export type ClassRight = "view" | "create" | "edit" | "delete";

/** Global permissions with operator-facing wording. Order is the order of the profile editor. */
export const GLOBAL_PERMISSIONS: { key: GlobalPermission; label: string; hint: string }[] = [
  { key: "users.manage", label: "Manage users", hint: "Create, edit, disable and delete users; reset passwords; assign profiles" },
  { key: "profiles.manage", label: "Manage permission profiles", hint: "Create, edit, clone and delete permission profiles" },
  { key: "datamodel.manage", label: "Manage the data model", hint: "CI classes, attributes, relationship types and rules, lookups" },
  { key: "customization.manage", label: "Manage customization", hint: "Branding, navigation, dashboards and layouts" },
  { key: "config.export_import", label: "Export and import configuration", hint: "Export or import the whole configuration" },
  { key: "audit.view", label: "View the audit log", hint: "Read the change history of every record" },
];

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
