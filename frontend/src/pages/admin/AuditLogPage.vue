<script setup lang="ts">
import { adminCrumbs } from "./sections";
import { computed, ref, watch } from "vue";
import { RouterLink, type RouteLocationRaw } from "vue-router";
import { useAuditList, useUser, type AuditListQuery } from "../../api/admin";
import type { AuditEntry } from "../../api/queries";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import SkeletonRows from "../../components/SkeletonRows.vue";
import { formatNumber, t, type MessageKey } from "../../i18n";
import { actionLabel, actionTone, AUDIT_ACTIONS, EVENT_SOURCES, eventSource, formatUtc, sourceLabel, type AuditAction, type EventSource } from "../../lib/auditEvents";
import { useDebounced, useDocumentTitle } from "../../lib/composables";
import { formatDateTime } from "../../lib/format";
import { useListQuery } from "../../lib/listQuery";
import { useSessionStore } from "../../stores/session";
import AuditActor from "./AuditActor.vue";
import { clientTitle, str } from "./auditClient";
import SortIcon from "../../components/SortIcon.vue";

/**
 * Administration › Audit log as an event stream (design §2.7 "Event stream", audit A7): every change and
 * sign-in, newest first, with the time in UTC, the actor, the source it came through, the event as a toned
 * badge with a readable name, the record and the changed fields. Filters and source chips live in the URL;
 * the API filters and pages.
 */
useDocumentTitle(() => t("admin.section.audit"));
type EntityType = NonNullable<AuditListQuery["entityType"]>;
type Action = NonNullable<AuditListQuery["action"]>;

// Records keyed by the OpenAPI enums: a value added to the API fails the typecheck until it is offered here.
const ENTITY_LABELS: Record<EntityType, MessageKey> = {
  configuration_items: "audit.entity.configuration_items",
  ci_relationships: "audit.entity.ci_relationships",
  ci_classes: "audit.entity.ci_classes",
  ci_attribute_definitions: "audit.entity.ci_attribute_definitions",
  relationship_types: "audit.entity.relationship_types",
  relationship_type_rules: "audit.entity.relationship_type_rules",
  statuses: "audit.entity.statuses",
  environments: "audit.entity.environments",
  locations: "audit.entity.locations",
  owners: "audit.entity.owners",
  users: "audit.entity.users",
  permission_profiles: "audit.entity.permission_profiles",
  ui_settings: "audit.entity.ui_settings",
  ui_assets: "audit.entity.ui_assets",
  sessions: "audit.entity.sessions",
  audit_log: "audit.entity.audit_log",
  areas: "audit.entity.areas",
  schema_changes: "audit.entity.schema_changes",
  api_tokens: "audit.entity.api_tokens",
  identity_providers: "audit.entity.identity_providers",
  lookup_lists: "audit.entity.lookup_lists",
  lookup_list_values: "audit.entity.lookup_list_values",
  import_jobs: "audit.entity.import_jobs",
  import_settings: "audit.entity.import_settings",
  import_mappings: "audit.entity.import_mappings",
  user_groups: "audit.entity.user_groups",
  saved_views: "audit.entity.saved_views",
  config: "audit.entity.config",
  ci_layout_overrides: "audit.entity.ci_layout_overrides",
  workflow_definitions: "audit.entity.workflow_definitions",
  workflow_approval_delegations: "audit.entity.workflow_approval_delegations",
  inventory: "audit.entity.inventory",
};
const ENTITY_TYPES = (Object.keys(ENTITY_LABELS) as EntityType[]).map((value) => ({ value, label: t(ENTITY_LABELS[value]) }));
// Both ways: an action added to the API fails the typecheck until lib/auditEvents names it, and back.
const ACTIONS_MATCH_API: [Exclude<Action, AuditAction>, Exclude<AuditAction, Action>] extends [never, never] ? true : false = true;
void ACTIONS_MATCH_API;
const ACTIONS: readonly Action[] = AUDIT_ACTIONS;

const session = useSessionStore();
const lq = useListQuery({ sort: "-occurredAt" });
const { get, limit, offset, update } = lq;
/** Source chips (UI, API, Import, System): `?source=ui,api`, none selected shows every source. */
const sources = computed<EventSource[]>(() => {
  const chosen = new Set(get("source").split(","));
  return EVENT_SOURCES.filter((s) => chosen.has(s.source)).map((s) => s.source);
});
const sourceActorTypes = computed(() => EVENT_SOURCES.filter((s) => sources.value.includes(s.source)).map((s) => s.actorType));
function toggleSource(source: EventSource) {
  const next = sources.value.includes(source) ? sources.value.filter((s) => s !== source) : [...sources.value, source];
  update({ source: next.join(",") || undefined });
}

const oneOf = <T extends string>(v: string, list: readonly T[]) => (list.includes(v as T) ? (v as T) : undefined);
const query = computed<AuditListQuery>(() => ({
  actorId: get("actorId") || undefined,
  actorName: get("actorName") || undefined,
  entityType: oneOf(get("entityType"), ENTITY_TYPES.map((e) => e.value)),
  action: oneOf(get("action"), ACTIONS),
  // Set by links from a record (for example an import's "View in audit log"); cleared with the other filters.
  entityId: get("entityId") || undefined,
  actorType: sourceActorTypes.value.join(",") || undefined,
  sort: lq.sort.value === "occurredAt" ? "occurredAt" : "-occurredAt",
  limit: limit.value,
  offset: offset.value,
}));
const list = useAuditList(query);
// Name the user behind an actorId filter (only users.manage may read users; otherwise show the id).
const actorUser = useUser(() => (session.can("users.manage") ? get("actorId") || undefined : undefined));

const actorText = ref(get("actorName"));
const debounced = useDebounced(actorText, 300);
watch(debounced, (v) => v !== get("actorName") && update({ actorName: v || undefined }));
watch(
  () => get("actorName"),
  (v) => (actorText.value = v),
);

const filtered = computed(() => ["actorId", "actorName", "entityType", "entityId", "action", "source"].some((k) => get(k)));
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);

function clearFilters() {
  actorText.value = "";
  update({ actorId: undefined, actorName: undefined, entityType: undefined, entityId: undefined, action: undefined, source: undefined });
}


/** A readable name for the changed record, taken from its before/after snapshot. */
function recordName(e: AuditEntry): string {
  const snap = (e.newValue ?? e.oldValue) as Record<string, unknown> | null;
  if (snap && typeof snap === "object") {
    if (e.action === "audit.purge") {
      // An operator's prune-audit run: which scope and how far back it reached.
      const scope = str(snap.scope) ?? "?";
      const window = str(snap.olderThan);
      return window ? t("audit.record.prunedOlder", { scope, window }) : t("audit.record.pruned", { scope });
    }
    if (e.action === "backup.restore") {
      // An operator's restore: when the backup it brought back was taken.
      const backup = (snap.backup ?? {}) as Record<string, unknown>;
      const taken = str(backup.createdAt);
      return taken ? t("audit.record.restoredTaken", { at: formatDateTime(taken) }) : t("audit.record.restored");
    }
    if (e.action === "token.use") {
      // A request made with an API token: which token, what it called, and whether it was let in.
      const token = str(snap.tokenName) ?? str(snap.tokenPrefix) ?? "(token)";
      return `${token}: ${str(snap.method) ?? "?"} ${str(snap.path) ?? "?"} (${str(snap.outcome) ?? "?"})`;
    }
    if (e.entityType === "sessions") {
      // Sign-in events: the (attempted) username and the client address. Both are
      // attacker-controlled text, so they are only ever interpolated, never v-html.
      const session = (snap.session ?? {}) as Record<string, unknown>;
      const who = str(snap.username) ?? str(snap.attemptedUsername) ?? t("audit.record.unknownUser");
      const ip = str(snap.ipAddress) ?? str(session.ipAddress);
      return ip ? t("audit.record.from", { who, ip }) : who;
    }
    for (const k of ["name", "username", "label", "key"]) if (typeof snap[k] === "string") return snap[k] as string;
    if (e.entityType === "ci_relationships") {
      const s = snap.source as { name?: string } | undefined;
      const t = snap.target as { name?: string } | undefined;
      if (s?.name && t?.name) return `${s.name} → ${t.name}`;
    }
  }
  return e.entityId.slice(0, 8);
}

/** Hover text for purges, and for sign-in, MFA and token rows (see auditClient.ts). */
function recordTitle(e: AuditEntry): string | undefined {
  if (e.action === "audit.purge") return purgeTitle((e.newValue ?? {}) as Record<string, unknown>);
  return clientTitle(e);
}

/** Hover text for a purge: the cutoff and how many rows each action lost. */
function purgeTitle(snap: Record<string, unknown>): string | undefined {
  const deleted = (snap.deleted ?? {}) as Record<string, unknown>;
  const counts = Object.entries(deleted)
    .filter(([, n]) => typeof n === "number")
    .map(([action, n]) => `  ${action}: ${n}`);
  const parts = [
    str(snap.cutoff) && `Cutoff: ${formatDateTime(snap.cutoff as string)}`,
    counts.length ? `Deleted:\n${counts.join("\n")}` : "Deleted: nothing",
    typeof snap.sessionsDeleted === "number" && snap.sessionsDeleted > 0 && `Expired sessions deleted: ${snap.sessionsDeleted}`,
    str(snap.operator) && `Operator: ${snap.operator}`,
    str(snap.clientAddress) && `From: ${snap.clientAddress}`,
  ].filter((p): p is string => typeof p === "string");
  return parts.join("\n");
}

function recordLink(e: AuditEntry): RouteLocationRaw | undefined {
  if (e.entityType === "configuration_items") return `/cis/${e.entityId}`;
  if (e.action === "delete") return undefined;
  if (e.entityType === "users" && session.can("users.manage")) return `/admin/users/${e.entityId}`;
  if (e.entityType === "permission_profiles" && (session.can("profiles.manage") || session.can("users.manage")))
    return `/admin/profiles/${e.entityId}`;
  return undefined;
}

/** Top-level fields that differ between the snapshots (bookkeeping fields left out). */
function changedFields(e: AuditEntry): string[] {
  if (e.action !== "update") return [];
  const o = (e.oldValue ?? {}) as Record<string, unknown>;
  const n = (e.newValue ?? {}) as Record<string, unknown>;
  const skip = new Set(["updatedAt", "version"]);
  return [...new Set([...Object.keys(o), ...Object.keys(n)])].filter((k) => !skip.has(k) && JSON.stringify(o[k]) !== JSON.stringify(n[k]));
}
const pastEnd = computed(() => !!list.data.value && total.value > 0 && rows.value.length === 0);
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="adminCrumbs('audit')" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("admin.section.audit") }}</h1>
        <span v-if="list.data.value" class="count mono">{{ t("audit.entries", { n: total, count: formatNumber(total) }) }}</span>
        <span v-if="list.isFetching.value && !list.isPending.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
    </div>
    <p class="page-intro">{{ t("audit.intro") }}</p>
    <form class="toolbar" role="search" @submit.prevent>
      <div v-if="get('actorId')" class="field">
        <span class="label">{{ t("history.col.actor") }}</span>
        <span class="chip owner-chip">
          <bdi>{{ actorUser.data.value?.displayName ?? get("actorId") }}</bdi>
          <button type="button" class="chip-clear" :aria-label="t('audit.filter.anyActor')" @click="update({ actorId: undefined })"><Icon name="x" :size="14" /></button>
        </span>
      </div>
      <div v-else class="field search">
        <label for="a-actor">{{ t("audit.filter.actorName") }}</label>
        <div class="input-icon">
          <Icon name="search" />
          <input id="a-actor" v-model="actorText" type="search" :placeholder="t('audit.filter.actorPlaceholder')" />
        </div>
      </div>
      <div class="field">
        <label for="a-type">{{ t("audit.col.recordType") }}</label>
        <select id="a-type" :value="get('entityType')" @change="update({ entityType: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("audit.filter.anyType") }}</option>
          <option v-for="et in ENTITY_TYPES" :key="et.value" :value="et.value">{{ et.label }}</option>
        </select>
      </div>
      <div class="field">
        <label for="a-action">{{ t("audit.col.action") }}</label>
        <select id="a-action" :value="get('action')" @change="update({ action: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("audit.filter.anyAction") }}</option>
          <option v-for="a in ACTIONS" :key="a" :value="a">{{ actionLabel(a) }}</option>
        </select>
      </div>
      <div class="field">
        <span class="label">{{ t("history.col.source") }}</span>
        <div class="event-sources" role="group" :aria-label="t('history.sources')">
          <button
            v-for="s in EVENT_SOURCES"
            :key="s.source"
            type="button"
            :class="['chip', 'event-source-filter', { selected: sources.includes(s.source) }]"
            :aria-pressed="sources.includes(s.source)"
            @click="toggleSource(s.source)"
          >
            {{ sourceLabel(s.source) }}
          </button>
        </div>
      </div>
      <button v-if="filtered" type="button" class="btn btn-ghost" @click="clearFilters"><Icon name="x" />{{ t("admin.filter.clear") }}</button>
    </form>
  </div>

  <section class="panel explorer event-stream audit-stream" :aria-label="t('admin.section.audit')">

    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <SkeletonRows v-else-if="list.isPending.value" :label="t('audit.loading')" />
    <EmptyState v-else-if="total === 0 && filtered" icon="search" :title="t('audit.noMatch')">
      {{ t("admin.filter.noMatchBody") }}
      <template #actions><button type="button" class="btn" @click="clearFilters">{{ t("admin.filter.clear") }}</button></template>
    </EmptyState>
    <EmptyState v-else-if="total === 0" icon="scroll-text" :title="t('audit.empty.title')">{{ t("audit.empty.body") }}</EmptyState>
    <EmptyState v-else-if="pastEnd" :title="t('common.pastEnd')">
      <template #actions><button type="button" class="btn" @click="update({})">{{ t("common.firstPage") }}</button></template>
    </EmptyState>

    <template v-if="rows.length > 0 && !list.isError.value">
      <div class="table-wrap table-scroll">
        <table :class="['data', 'list-table', 'event-table', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th scope="col" :aria-sort="lq.ariaSort('occurredAt')">
                <button type="button" class="sort" @click="lq.toggleSort('occurredAt')">{{ t("history.col.time") }} <SortIcon :dir="lq.ariaSort('occurredAt')" /></button>
              </th>
              <th scope="col">{{ t("history.col.actor") }}</th>
              <th scope="col">{{ t("history.col.source") }}</th>
              <th scope="col">{{ t("history.col.event") }}</th>
              <th scope="col">{{ t("audit.col.recordType") }}</th>
              <th scope="col">{{ t("audit.col.record") }}</th>
              <th scope="col">{{ t("audit.col.changed") }}</th>
              <th scope="col">{{ t("audit.col.request") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="e in rows" :key="e.id">
              <td class="mono event-time"><time :datetime="e.occurredAt" :title="formatDateTime(e.occurredAt)">{{ formatUtc(e.occurredAt) }}</time></td>
              <td><AuditActor :entry="e" /></td>
              <td><span class="chip event-source">{{ sourceLabel(eventSource(e)) }}</span></td>
              <td><span :class="['badge', actionTone(e.action)]" :title="e.action">{{ actionLabel(e.action) }}</span></td>
              <td>{{ t(ENTITY_LABELS[e.entityType as EntityType] ?? "audit.entity.unknown", { type: e.entityType }) }}</td>
              <td :title="recordTitle(e)">
                <RouterLink v-if="recordLink(e)" :to="recordLink(e)!" class="list-name" dir="auto">{{ recordName(e) }}</RouterLink>
                <bdi v-else>{{ recordName(e) }}</bdi>
              </td>
              <td class="audit-fields" :title="changedFields(e).join(', ')">
                <code v-for="f in changedFields(e)" :key="f">{{ f }}</code>
              </td>
              <td class="mono muted" :title="e.requestId ?? undefined">{{ e.requestId?.slice(0, 8) ?? "" }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <div class="table-footer">
        <PaginationBar numbered :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
      </div>
    </template>
  </section>
</template>
