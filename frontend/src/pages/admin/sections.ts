import type { GlobalPermission } from "../../api/admin";

/**
 * The Administration area's sections, in sub-navigation order. A section is
 * shown to users holding any of its permissions (administrators hold all).
 * Sections planned for later phases (data model, lookups, templates,
 * customization, export/import) are added here with their route when built.
 */
export interface AdminSection {
  key: string;
  label: string;
  to: string;
  permissions: GlobalPermission[];
}

export const ADMIN_SECTIONS: AdminSection[] = [
  { key: "users", label: "Users", to: "/admin/users", permissions: ["users.manage"] },
  // users.manage may read profiles too, to know what they assign.
  { key: "profiles", label: "Permission profiles", to: "/admin/profiles", permissions: ["profiles.manage", "users.manage"] },
  { key: "audit", label: "Audit log", to: "/admin/audit", permissions: ["audit.view"] },
];

export function visibleSections(can: (p: GlobalPermission) => boolean): AdminSection[] {
  return ADMIN_SECTIONS.filter((s) => s.permissions.some(can));
}
