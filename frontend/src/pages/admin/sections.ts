import type { GlobalPermission } from "../../api/admin";
import { t } from "../../i18n";

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

const ACCESS = t("admin.group.access");
const DATA_MODEL = t("admin.group.dataModel");
const PROCESSES = t("admin.group.processes");
const SYSTEM = t("admin.group.system");

export const ADMIN_SECTIONS: AdminSection[] = [
  { key: "users", label: t("admin.section.users"), group: ACCESS, to: "/admin/users", permissions: ["users.manage"] },
  { key: "groups", label: t("groups.nav"), group: ACCESS, to: "/admin/groups", permissions: ["users.manage"] },
  // users.manage may read profiles too, to know what they assign.
  { key: "profiles", label: t("admin.section.profiles"), group: ACCESS, to: "/admin/profiles", permissions: ["profiles.manage", "users.manage"] },
  { key: "api-tokens", label: t("admin.section.apiTokens"), group: ACCESS, to: "/admin/api-tokens", permissions: ["users.manage"] },
  // Someone else's approvals for a time window, while they are away (the API needs users.manage).
  { key: "approval-delegations", label: t("delegations.adminTitle"), group: ACCESS, to: "/admin/approval-delegations", permissions: ["users.manage"] },
  // Who may sign in, and with which profiles: the API allows only the Administrator profile, not users.manage alone.
  { key: "identity-providers", label: t("admin.section.identityProviders"), group: ACCESS, to: "/admin/identity-providers", permissions: [], administratorOnly: true },
  { key: "areas", label: t("admin.section.areas"), group: DATA_MODEL, to: "/admin/areas", permissions: ["datamodel.manage"] },
  { key: "classes", label: t("admin.section.classes"), group: DATA_MODEL, to: "/admin/classes", permissions: ["datamodel.manage"] },
  { key: "relationships", label: t("admin.section.relationships"), group: DATA_MODEL, to: "/admin/relationships", permissions: ["datamodel.manage"] },
  { key: "dropdowns", label: t("admin.section.dropdowns"), group: DATA_MODEL, to: "/admin/dropdowns", permissions: ["datamodel.manage"] },
  // Named as the page's heading (audit A2).
  { key: "templates", label: t("admin.section.templates"), group: DATA_MODEL, to: "/admin/templates", permissions: ["datamodel.manage"] },
  { key: "workflows", label: t("admin.section.workflows"), group: PROCESSES, to: "/admin/workflows", permissions: ["workflows.manage"] },
  { key: "customization", label: t("admin.section.customization"), group: SYSTEM, to: "/admin/customization", permissions: ["customization.manage"] },
  // Switches bulk import on for the instance: the API allows only the Administrator profile.
  { key: "import", label: t("admin.section.import"), group: SYSTEM, to: "/admin/import", permissions: [], administratorOnly: true },
  { key: "config", label: t("admin.section.config"), group: SYSTEM, to: "/admin/config", permissions: ["config.export_import"] },
  { key: "audit", label: t("admin.section.audit"), group: SYSTEM, to: "/admin/audit", permissions: ["audit.view"] },
];

/**
 * The breadcrumbs of an admin page: Administration, the section's group, the section, then `rest`
 * (a record). Every section names its group the same way (audit S7); the section links only when
 * something follows it.
 */
export function adminCrumbs(key: string, ...rest: { label: string; to?: string }[]): { label: string; to?: string }[] {
  const s = ADMIN_SECTIONS.find((x) => x.key === key);
  if (!s) return [{ label: t("common.administration"), to: "/admin" }, ...rest];
  return [
    { label: t("common.administration"), to: "/admin" },
    { label: s.group },
    rest.length > 0 ? { label: s.label, to: s.to } : { label: s.label },
    ...rest,
  ];
}

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
