// A business service's members (spec SHAA-927 §5.3, §5.5, §2): the Members tab's URL state, the member
// picker's per-item errors, and the Impact tab's pinned "Affected business services" section.
import type { LocationQuery, LocationQueryRaw } from "vue-router";
import type { Schemas } from "../api/client";
import { t, type MessageKey } from "../i18n/index";

export type MemberKind = "" | "ci" | "service";
export type MemberSort = "name" | "-name" | "class" | "-class" | "criticality" | "-criticality" | "addedAt" | "-addedAt";

/** The Members tab's state, as the URL holds it: `?tab=members&mq=&mclass=&mkind=&msort=&mpage=`. */
export interface MembersState {
  q: string;
  /** Class ids (comma-separated in `mclass`). */
  classIds: string[];
  kind: MemberKind;
  sort: MemberSort;
  /** 1-based. */
  page: number;
}

export const MEMBERS_PAGE_SIZE = 50;
export const DEFAULT_MEMBERS_STATE: MembersState = { q: "", classIds: [], kind: "", sort: "name", page: 1 };
const SORTS: MemberSort[] = ["name", "-name", "class", "-class", "criticality", "-criticality", "addedAt", "-addedAt"];
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
/** The URL keys the Members tab owns; everything else in the query is left alone. */
export const MEMBER_PARAMS = ["mq", "mclass", "mkind", "msort", "mpage"] as const;

const one = (q: LocationQuery | LocationQueryRaw, k: string): string => {
  const v = q[k];
  const s = Array.isArray(v) ? v[0] : v;
  return typeof s === "string" ? s : s == null ? "" : String(s);
};

/** The state a URL asks for; a value that cannot be used falls back to its default. */
export function parseMembersQuery(query: LocationQuery | LocationQueryRaw): MembersState {
  const s: MembersState = { ...DEFAULT_MEMBERS_STATE, classIds: [] };
  s.q = one(query, "mq").slice(0, 200);
  s.classIds = [...new Set(one(query, "mclass").split(",").filter((id) => UUID.test(id)))].slice(0, 50);
  const kind = one(query, "mkind");
  if (kind === "ci" || kind === "service") s.kind = kind;
  const sort = one(query, "msort") as MemberSort;
  if (SORTS.includes(sort)) s.sort = sort;
  const page = Number(one(query, "mpage"));
  if (Number.isInteger(page) && page > 1) s.page = page;
  return s;
}

/** `query` with the Members tab's keys set from `s`: only what differs from the defaults, so a plain link stays plain. */
export function membersUrlQuery(query: LocationQuery | LocationQueryRaw, s: MembersState): LocationQueryRaw {
  const out: LocationQueryRaw = { ...query };
  for (const k of MEMBER_PARAMS) delete out[k];
  if (s.q) out.mq = s.q;
  if (s.classIds.length > 0) out.mclass = s.classIds.join(",");
  if (s.kind) out.mkind = s.kind;
  if (s.sort !== DEFAULT_MEMBERS_STATE.sort) out.msort = s.sort;
  if (s.page > 1) out.mpage = String(s.page);
  return out;
}

/** The member list (and CSV export) parameters for a state. */
export function memberFilters(s: MembersState): { q?: string; classId?: string; kind?: "ci" | "service"; sort: MemberSort } {
  return {
    ...(s.q ? { q: s.q } : {}),
    ...(s.classIds.length > 0 ? { classId: s.classIds.join(",") } : {}),
    ...(s.kind ? { kind: s.kind } : {}),
    sort: s.sort,
  };
}

export function memberListQuery(s: MembersState) {
  return { ...memberFilters(s), limit: MEMBERS_PAGE_SIZE, offset: (s.page - 1) * MEMBERS_PAGE_SIZE };
}

export const hasMemberFilters = (s: MembersState) => !!s.q || s.classIds.length > 0 || !!s.kind;

// ---------- The member picker's errors ----------

export interface PickerError {
  code: string;
  message: string;
}

interface ErrorDetail {
  field: string;
  message: string;
  code?: string;
}

const KNOWN_CODES = ["not_found", "membership_self", "membership_cycle", "membership_nesting_depth", "member_limit"] as const;

/** The operator-facing text of one error code; the API's own message for a code this UI does not know. */
export function pickerErrorMessage(code: string | undefined, apiMessage: string, params: { member?: string; maxNesting?: number; maxMembers?: number }): string {
  if (!code || !(KNOWN_CODES as readonly string[]).includes(code)) return apiMessage;
  return t(`services.error.${code}` as MessageKey, {
    member: params.member,
    max: code === "member_limit" ? params.maxMembers : params.maxNesting,
  });
}

/**
 * The per-item errors of a refused add (`memberIds[<index>]`, an index into `submitted`), keyed by CI id, and the
 * errors about the request as a whole (`member_limit` on `memberIds`, anything unexpected).
 */
export function pickerErrors(
  details: readonly ErrorDetail[],
  submitted: readonly string[],
  nameOf: (id: string) => string,
  limits: { maxNesting?: number; maxMembers?: number },
): { byId: Map<string, PickerError>; general: PickerError[] } {
  const byId = new Map<string, PickerError>();
  const general: PickerError[] = [];
  for (const d of details) {
    const m = /^memberIds\[(\d+)\]$/.exec(d.field);
    const id = m ? submitted[Number(m[1])] : undefined;
    const message = pickerErrorMessage(d.code, d.message, { member: id ? nameOf(id) : undefined, ...limits });
    if (id) {
      if (!byId.has(id)) byId.set(id, { code: d.code ?? "", message });
    } else {
      general.push({ code: d.code ?? "", message });
    }
  }
  return { byId, general };
}

// ---------- The Impact tab's pinned section ----------

type ImpactItem = Schemas["ImpactItem"];

/** The business services in an impact result: by criticality rank (not set last), then hops, then name. */
export function affectedServices(items: readonly ImpactItem[], serviceClassId: string | null | undefined): ImpactItem[] {
  if (!serviceClassId) return [];
  const rank = (i: ImpactItem) => i.criticality?.rank ?? Number.MAX_SAFE_INTEGER;
  return items
    .filter((i) => i.classId === serviceClassId)
    .sort((a, b) => rank(a) - rank(b) || a.hops - b.hops || a.name.localeCompare(b.name, undefined, { sensitivity: "base" }));
}
