import type { GlobalPermission } from "../../api/admin";

/**
 * The Administration area's sections, in sub-navigation order and grouped under
 * headings. A section is shown to users holding any of its permissions
 * (administrators hold all); an `administratorOnly` section only to holders of the
 * built-in Administrator profile, whatever else they hold.
 */
export interface AdminSection {
  key: string;
  label: string;
  group: string;
  to: string;
  permissions: GlobalPermission[];
  administratorOnly?: boolean;
}

/** What the signed-in user may open: their global permissions, and whether they hold the Administrator profile. */
export interface AdminAccess {
  can: (p: GlobalPermission) => boolean;
  isAdministrator: boolean;
}

export function sectionAllowed(s: Pick<AdminSection, "permissions" | "administratorOnly">, access: AdminAccess): boolean {
  if (s.administratorOnly) return access.isAdministrator;
  return s.permissions.length === 0 || s.permissions.some(access.can);
}

export const ADMIN_SECTIONS: AdminSection[] = [
  { key: "users", label: "Users", group: "Access", to: "/admin/users", permissions: ["users.manage"] },
  // users.manage may read profiles too, to know what they assign.
  { key: "profiles", label: "Permission profiles", group: "Access", to: "/admin/profiles", permissions: ["profiles.manage", "users.manage"] },
  { key: "api-tokens", label: "API tokens", group: "Access", to: "/admin/api-tokens", permissions: ["users.manage"] },
  // Who may sign in, and with which profiles: the API allows only the Administrator profile, not users.manage alone.
  { key: "identity-providers", label: "Identity providers", group: "Access", to: "/admin/identity-providers", permissions: [], administratorOnly: true },
  { key: "areas", label: "Areas", group: "Data model", to: "/admin/areas", permissions: ["datamodel.manage"] },
  { key: "classes", label: "CI classes", group: "Data model", to: "/admin/classes", permissions: ["datamodel.manage"] },
  { key: "relationships", label: "Relationship types", group: "Data model", to: "/admin/relationships", permissions: ["datamodel.manage"] },
  { key: "dropdowns", label: "Dropdowns", group: "Data model", to: "/admin/dropdowns", permissions: ["datamodel.manage"] },
  { key: "templates", label: "Templates", group: "Data model", to: "/admin/templates", permissions: ["datamodel.manage"] },
  { key: "customization", label: "Customization", group: "System", to: "/admin/customization", permissions: ["customization.manage"] },
  // Switches bulk import on for the instance: the API allows only the Administrator profile.
  { key: "import", label: "Import", group: "System", to: "/admin/import", permissions: [], administratorOnly: true },
  { key: "config", label: "Export / import", group: "System", to: "/admin/config", permissions: ["config.export_import"] },
  { key: "audit", label: "Audit log", group: "System", to: "/admin/audit", permissions: ["audit.view"] },
];

export function visibleSections(access: AdminAccess): AdminSection[] {
  return ADMIN_SECTIONS.filter((s) => sectionAllowed(s, access));
}

/** Visible sections under their group headings, in order. */
export function groupedSections(access: AdminAccess): { group: string; sections: AdminSection[] }[] {
  const out: { group: string; sections: AdminSection[] }[] = [];
  for (const s of visibleSections(access)) {
    const last = out[out.length - 1];
    if (last?.group === s.group) last.sections.push(s);
    else out.push({ group: s.group, sections: [s] });
  }
  return out;
}
