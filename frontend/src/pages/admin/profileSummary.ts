import type { PermissionProfile } from "../../api/admin";
import { t } from "../../i18n";
import { CLASS_RIGHTS, GLOBAL_PERMISSIONS } from "../../lib/permissions";

/** One-line description of what a profile grants, for the profiles list. */
export function summarise(p: PermissionProfile): string {
  if (p.isBuiltin) return t("admin.profiles.summary.everything");
  const parts: string[] = [];
  const all = p.classPermissions.find((c) => c.classId === null);
  if (all) parts.push(t("admin.profiles.summary.allClasses", { rights: CLASS_RIGHTS.filter((r) => all[r]).map((r) => t(`admin.right.${r}`)).join(", ") }));
  const specific = p.classPermissions.filter((c) => c.classId !== null).length;
  if (specific > 0) parts.push(t("admin.profiles.summary.classes", { n: specific }));
  const globals = GLOBAL_PERMISSIONS.filter((g) => p.globalPermissions.includes(g.key)).map((g) => g.label);
  parts.push(...globals);
  return parts.length > 0 ? parts.join(" · ") : t("admin.profiles.summary.nothing");
}
