<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, type RouteLocationRaw } from "vue-router";
import { useAuditList, useUser, type AuditListQuery } from "../../api/admin";
import type { AuditEntry } from "../../api/queries";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { useDebounced, useDocumentTitle } from "../../lib/composables";
import { formatDateTime } from "../../lib/format";
import { useListQuery } from "../../lib/listQuery";
import { useSessionStore } from "../../stores/session";
import AuditActor from "./AuditActor.vue";
import { clientTitle, str } from "./auditClient";
import SortIcon from "../../components/SortIcon.vue";

/** Administration › Audit log: every change, who made it, newest first. Filters live in the URL. */
useDocumentTitle("Audit log");
type EntityType = NonNullable<AuditListQuery["entityType"]>;
type Action = NonNullable<AuditListQuery["action"]>;

// Records keyed by the OpenAPI enums: a value added to the API fails the typecheck until it is offered here.
const ENTITY_LABELS: Record<EntityType, string> = {
  configuration_items: "Configuration item",
  ci_relationships: "Relationship",
  ci_classes: "CI class",
  ci_attribute_definitions: "Attribute definition",
  relationship_types: "Relationship type",
  relationship_type_rules: "Relationship rule",
  statuses: "Status",
  environments: "Environment",
  locations: "Location",
  owners: "Owner",
  users: "User",
  permission_profiles: "Permission profile",
  ui_settings: "UI settings",
  ui_assets: "UI asset",
  sessions: "Sign-in / session",
  audit_log: "Audit log",
  areas: "Area",
  schema_changes: "Schema change",
  api_tokens: "API token",
  identity_providers: "Identity provider",
  lookup_lists: "Lookup list",
  lookup_list_values: "Lookup list value",
  import_jobs: "Bulk import",
  import_settings: "Bulk import settings",
  import_mappings: "Import mapping",
  user_groups: "User group",
  saved_views: "Shared view",
  config: "Configuration file",
  ci_layout_overrides: "CI layout",
  workflow_definitions: "Workflow",
  workflow_approval_delegations: "Approval delegation",
};
const ENTITY_TYPES = (Object.keys(ENTITY_LABELS) as EntityType[]).map((value) => ({ value, label: ENTITY_LABELS[value] }));
const ACTION_SET: Record<Action, true> = {
  create: true,
  update: true,
  delete: true,
  restore: true,
  "login.success": true,
  "login.failure": true,
  "login.locked": true,
  logout: true,
  "session.revoke": true,
  "session.reauthenticate": true,
  "session.reauthentication_required": true,
  "audit.purge": true,
  "backup.restore": true,
  "token.use": true,
  "mfa.enrol": true,
  "mfa.disable": true,
  "mfa.failure": true,
  "mfa.recovery_code_used": true,
  "mfa.recovery_codes": true,
  "schema_change.refused": true,
  export: true,
  "import.commit": true,
  "import.report_read": true,
  "workflow.publish": true,
  "workflow.start": true,
  "workflow.cancel": true,
  "workflow.transition": true,
  "workflow.migrate": true,
  "workflow.force": true,
  "workflow.approval_request": true,
  "workflow.approval_decide": true,
  "workflow.approval_close": true,
  "workflow.approval_overdue": true,
};
const ACTIONS = Object.keys(ACTION_SET) as Action[];
const entityLabel = (t: string) => ENTITY_TYPES.find((e) => e.value === t)?.label ?? t;

const session = useSessionStore();
const lq = useListQuery({ sort: "-occurredAt" });
const { get, limit, offset, update } = lq;
const oneOf = <T extends string>(v: string, list: readonly T[]) => (list.includes(v as T) ? (v as T) : undefined);
const query = computed<AuditListQuery>(() => ({
  actorId: get("actorId") || undefined,
  actorName: get("actorName") || undefined,
  entityType: oneOf(get("entityType"), ENTITY_TYPES.map((e) => e.value)),
  action: oneOf(get("action"), ACTIONS),
  // Set by links from a record (for example an import's "View in audit log"); cleared with the other filters.
  entityId: get("entityId") || undefined,
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

const filtered = computed(() => ["actorId", "actorName", "entityType", "entityId", "action"].some((k) => get(k)));
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);

function clearFilters() {
  actorText.value = "";
  update({ actorId: undefined, actorName: undefined, entityType: undefined, entityId: undefined, action: undefined });
}


/** A readable name for the changed record, taken from its before/after snapshot. */
function recordName(e: AuditEntry): string {
  const snap = (e.newValue ?? e.oldValue) as Record<string, unknown> | null;
  if (snap && typeof snap === "object") {
    if (e.action === "audit.purge") {
      // An operator's prune-audit run: which scope and how far back it reached.
      const scope = str(snap.scope) ?? "?";
      const window = str(snap.olderThan);
      return window ? `Pruned ${scope} entries older than ${window}` : `Pruned ${scope} entries`;
    }
    if (e.action === "backup.restore") {
      // An operator's restore: when the backup it brought back was taken.
      const backup = (snap.backup ?? {}) as Record<string, unknown>;
      const taken = str(backup.createdAt);
      return taken ? `Restored a backup taken ${formatDateTime(taken)}` : "Restored a backup";
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
      const who = str(snap.username) ?? str(snap.attemptedUsername) ?? "(unknown user)";
      const ip = str(snap.ipAddress) ?? str(session.ipAddress);
      return ip ? `${who} from ${ip}` : who;
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

const actionTone = (action: string) =>
  ["delete", "login.failure", "login.locked"].includes(action) ? "danger" : ["create", "login.success"].includes(action) ? "ok" : "";
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'Audit log' }]" />
  <div class="page-header">
    <div class="title">
      <h1>Audit log</h1>
      <span v-if="list.data.value" class="muted">{{ total.toLocaleString() }} entries</span>
      <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" aria-label="Refreshing" />
    </div>
  </div>

  <section class="panel" aria-label="Audit log">
    <form class="toolbar" role="search" @submit.prevent>
      <div v-if="get('actorId')" class="field">
        <span class="label">Actor</span>
        <span class="checkbox-row">
          <span class="badge" dir="auto">{{ actorUser.data.value?.displayName ?? get("actorId") }}</span>
          <button type="button" class="btn btn-sm" @click="update({ actorId: undefined })">Any actor</button>
        </span>
      </div>
      <div v-else class="field search">
        <label for="a-actor">Actor name</label>
        <input id="a-actor" v-model="actorText" type="search" placeholder="Part of a user's name…" />
      </div>
      <div class="field">
        <label for="a-type">Record type</label>
        <select id="a-type" :value="get('entityType')" @change="update({ entityType: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Any type</option>
          <option v-for="t in ENTITY_TYPES" :key="t.value" :value="t.value">{{ t.label }}</option>
        </select>
      </div>
      <div class="field">
        <label for="a-action">Action</label>
        <select id="a-action" :value="get('action')" @change="update({ action: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Any action</option>
          <option v-for="a in ACTIONS" :key="a" :value="a">{{ a }}</option>
        </select>
      </div>
      <button v-if="filtered" type="button" class="btn" @click="clearFilters">Clear filters</button>
    </form>

    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <LoadingState v-if="list.isLoading.value" label="Loading audit log…" />
    <EmptyState v-if="list.data.value && total === 0" :title="filtered ? 'No changes match these filters' : 'No changes recorded yet'">
      {{ filtered ? "Adjust or clear the filters above." : "Every change and every sign-in is recorded here with the user who made it." }}
    </EmptyState>

    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th scope="col" :aria-sort="lq.ariaSort('occurredAt')">
                <button type="button" class="sort" @click="lq.toggleSort('occurredAt')">When <SortIcon :dir="lq.ariaSort('occurredAt')" /></button>
              </th>
              <th scope="col">Actor</th>
              <th scope="col">Action</th>
              <th scope="col">Record type</th>
              <th scope="col">Record</th>
              <th scope="col">Changed fields</th>
              <th scope="col">Request</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="e in rows" :key="e.id">
              <td>{{ formatDateTime(e.occurredAt) }}</td>
              <td><AuditActor :entry="e" /></td>
              <td><span :class="['badge', actionTone(e.action)]">{{ e.action }}</span></td>
              <td>{{ entityLabel(e.entityType) }}</td>
              <td :title="recordTitle(e)">
                <RouterLink v-if="recordLink(e)" :to="recordLink(e)!" dir="auto">{{ recordName(e) }}</RouterLink>
                <bdi v-else>{{ recordName(e) }}</bdi>
              </td>
              <td :title="changedFields(e).join(', ')">{{ changedFields(e).join(", ") }}</td>
              <td class="mono muted" :title="e.requestId ?? undefined">{{ e.requestId?.slice(0, 8) ?? "" }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
    </template>
  </section>
</template>
