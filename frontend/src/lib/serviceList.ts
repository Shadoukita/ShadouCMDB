// The business service list's URL state (spec SHAA-927 §5.2):
//   ?q=&criticality=&owner=&role=&mine=1&ownerState=&inactive=0&sort=&page=&limit=
// so a filtered view survives a reload, can be bookmarked and walks back with Back. Values the URL
// cannot hold (a hand-edited link) fall back to their default instead of reaching the API.
import type { LocationQuery, LocationQueryRaw } from "vue-router";
import type { OwnerRole, PrincipalRef, ServiceListQuery } from "../api/services";

export const SERVICE_SORTS = ["name", "criticality", "memberCount", "updatedAt"] as const;
export type ServiceSortField = (typeof SERVICE_SORTS)[number];
/** Most critical first, then by name (the API breaks ties by name). */
export const DEFAULT_SERVICE_SORT = "criticality";
export const DEFAULT_SERVICE_LIMIT = 50;
/** `criticality=none`: services whose criticality is not set. */
export const CRITICALITY_NONE = "none";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
/** The API's limit on criticalityValueId. */
const MAX_CRITICALITY = 50;

export interface ServiceListState {
  q: string;
  /** Criticality value ids, or CRITICALITY_NONE. */
  criticality: string[];
  /** A user or group id. */
  owner: string;
  role: OwnerRole | "";
  mine: boolean;
  ownerState: "none" | "disabled" | "";
  includeInactive: boolean;
  sort: string;
  /** 1-based. */
  page: number;
  limit: number;
}

export const DEFAULT_SERVICE_LIST: ServiceListState = {
  q: "",
  criticality: [],
  owner: "",
  role: "",
  mine: false,
  ownerState: "",
  includeInactive: true,
  sort: DEFAULT_SERVICE_SORT,
  page: 1,
  limit: DEFAULT_SERVICE_LIMIT,
};

const one = (q: LocationQuery | LocationQueryRaw, k: string): string => {
  const v = q[k];
  return typeof v === "string" ? v : "";
};

function int(raw: string, fallback: number, min: number, max: number): number {
  if (!/^\d+$/.test(raw)) return fallback;
  return Math.min(max, Math.max(min, Number(raw)));
}

export function parseServiceListQuery(query: LocationQuery | LocationQueryRaw): ServiceListState {
  const s: ServiceListState = { ...DEFAULT_SERVICE_LIST, criticality: [] };
  s.q = one(query, "q").slice(0, 200);
  s.criticality = [...new Set(one(query, "criticality").split(",").filter((v) => v === CRITICALITY_NONE || UUID.test(v)))].slice(0, MAX_CRITICALITY);
  const owner = one(query, "owner");
  if (UUID.test(owner)) s.owner = owner;
  const role = one(query, "role");
  if (role === "technical" || role === "business") s.role = role;
  s.mine = one(query, "mine") === "1";
  const ownerState = one(query, "ownerState");
  if (ownerState === "none" || ownerState === "disabled") s.ownerState = ownerState;
  s.includeInactive = one(query, "inactive") !== "0";
  const sort = one(query, "sort");
  if (SERVICE_SORTS.includes(sort.replace(/^-/, "") as ServiceSortField)) s.sort = sort;
  s.page = int(one(query, "page"), 1, 1, 100_000);
  s.limit = int(one(query, "limit"), DEFAULT_SERVICE_LIMIT, 1, 200);
  return s;
}

/** The URL query for a state: only what differs from the defaults, so a plain link stays plain. */
export function serviceListUrl(s: ServiceListState): Record<string, string> {
  const q: Record<string, string> = {};
  if (s.q) q.q = s.q;
  if (s.criticality.length) q.criticality = s.criticality.join(",");
  if (s.owner) q.owner = s.owner;
  if (s.role) q.role = s.role;
  if (s.mine) q.mine = "1";
  if (s.ownerState) q.ownerState = s.ownerState;
  if (!s.includeInactive) q.inactive = "0";
  if (s.sort !== DEFAULT_SERVICE_SORT) q.sort = s.sort;
  if (s.page > 1) q.page = String(s.page);
  if (s.limit !== DEFAULT_SERVICE_LIMIT) q.limit = String(s.limit);
  return q;
}

/** The request for a state (GET /business-services). The role only narrows an owner filter or "My services". */
export function serviceListParams(s: ServiceListState): ServiceListQuery {
  const p: ServiceListQuery = { limit: s.limit, offset: (s.page - 1) * s.limit, sort: s.sort as ServiceListQuery["sort"] };
  if (s.q.trim()) p.q = s.q.trim();
  if (s.criticality.length) p.criticalityValueId = s.criticality.join(",");
  if (s.owner) p.ownerId = s.owner;
  if (s.role && (s.owner || s.mine)) p.ownerRole = s.role;
  if (s.mine) p.mine = "true";
  if (s.ownerState) p.ownerState = s.ownerState;
  if (!s.includeInactive) p.includeInactive = "false";
  return p;
}

/** Whether any filter (not the sort or page) narrows the list: "no match" instead of "no services yet". */
export function hasServiceFilters(s: ServiceListState): boolean {
  return !!(s.q.trim() || s.criticality.length || s.owner || s.mine || s.ownerState || !s.includeInactive);
}

/** An owner cell: at most `max` names, then how many more. */
export function ownerCell(owners: PrincipalRef[], max = 2): { shown: PrincipalRef[]; more: number } {
  return { shown: owners.slice(0, max), more: Math.max(0, owners.length - max) };
}
