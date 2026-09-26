import type { PermissionProfile } from "../../api/admin";
import { CLASS_RIGHTS, GLOBAL_PERMISSIONS } from "../../lib/permissions";

/** One-line description of what a profile grants, for the profiles list. */
export function summarise(p: PermissionProfile): string {
  if (p.isBuiltin) return "Everything (built-in)";
  const parts: string[] = [];
  const all = p.classPermissions.find((c) => c.classId === null);
  if (all) parts.push(`All classes: ${CLASS_RIGHTS.filter((r) => all[r]).join(", ")}`);
  const specific = p.classPermissions.filter((c) => c.classId !== null).length;
  if (specific > 0) parts.push(`${specific} ${specific === 1 ? "class" : "classes"}`);
  const globals = GLOBAL_PERMISSIONS.filter((g) => p.globalPermissions.includes(g.key)).map((g) => g.label);
  parts.push(...globals);
  return parts.length > 0 ? parts.join(" · ") : "Nothing";
}
