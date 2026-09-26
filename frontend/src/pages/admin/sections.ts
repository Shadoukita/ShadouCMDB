import type { GlobalPermission } from "../../api/admin";

/**
 * The Administration area's sections, in sub-navigation order and grouped under
 * headings. A section is shown to users holding any of its permissions
 * (administrators hold all).
 */
export interface AdminSection {
  key: string;
  label: string;
  group: string;
  to: string;
  permissions: GlobalPermission[];
}

export const ADMIN_SECTIONS: AdminSection[] = [
  { key: "users", label: "Users", group: "Access", to: "/admin/users", permissions: ["users.manage"] },
  // users.manage may read profiles too, to know what they assign.
  { key: "profiles", label: "Permission profiles", group: "Access", to: "/admin/profiles", permissions: ["profiles.manage", "users.manage"] },
  { key: "classes", label: "CI classes", group: "Data model", to: "/admin/classes", permissions: ["datamodel.manage"] },
  { key: "relationships", label: "Relationship types", group: "Data model", to: "/admin/relationships", permissions: ["datamodel.manage"] },
  { key: "lookups", label: "Lookups", group: "Data model", to: "/admin/lookups", permissions: ["datamodel.manage"] },
  { key: "templates", label: "Templates", group: "Data model", to: "/admin/templates", permissions: ["datamodel.manage"] },
  { key: "customization", label: "Customization", group: "System", to: "/admin/customization", permissions: ["customization.manage"] },
  { key: "config", label: "Export / import", group: "System", to: "/admin/config", permissions: ["config.export_import"] },
  { key: "audit", label: "Audit log", group: "System", to: "/admin/audit", permissions: ["audit.view"] },
];

export function visibleSections(can: (p: GlobalPermission) => boolean): AdminSection[] {
  return ADMIN_SECTIONS.filter((s) => s.permissions.some(can));
}

/** Visible sections under their group headings, in order. */
export function groupedSections(can: (p: GlobalPermission) => boolean): { group: string; sections: AdminSection[] }[] {
  const out: { group: string; sections: AdminSection[] }[] = [];
  for (const s of visibleSections(can)) {
    const last = out[out.length - 1];
    if (last?.group === s.group) last.sections.push(s);
    else out.push({ group: s.group, sections: [s] });
  }
  return out;
}
