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

/** Administration › Audit log: every change, who made it, newest first. Filters live in the URL. */
useDocumentTitle("Audit log");
type EntityType = NonNullable<AuditListQuery["entityType"]>;
type Action = NonNullable<AuditListQuery["action"]>;

const ENTITY_TYPES: { value: EntityType; label: string }[] = [
  { value: "configuration_items", label: "Configuration item" },
  { value: "ci_relationships", label: "Relationship" },
  { value: "ci_classes", label: "CI class" },
  { value: "ci_attribute_definitions", label: "Attribute definition" },
  { value: "relationship_types", label: "Relationship type" },
  { value: "relationship_type_rules", label: "Relationship rule" },
  { value: "statuses", label: "Status" },
  { value: "environments", label: "Environment" },
  { value: "locations", label: "Location" },
  { value: "owners", label: "Owner" },
  { value: "users", label: "User" },
  { value: "permission_profiles", label: "Permission profile" },
];
const ACTIONS: Action[] = ["create", "update", "delete", "restore"];
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

const filtered = computed(() => ["actorId", "actorName", "entityType", "action"].some((k) => get(k)));
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);

function clearFilters() {
  actorText.value = "";
  update({ actorId: undefined, actorName: undefined, entityType: undefined, action: undefined });
}

/** A readable name for the changed record, taken from its before/after snapshot. */
function recordName(e: AuditEntry): string {
  const snap = (e.newValue ?? e.oldValue) as Record<string, unknown> | null;
  if (snap && typeof snap === "object") {
    for (const k of ["name", "username", "label", "key"]) if (typeof snap[k] === "string") return snap[k] as string;
    if (e.entityType === "ci_relationships") {
      const s = snap.source as { name?: string } | undefined;
      const t = snap.target as { name?: string } | undefined;
      if (s?.name && t?.name) return `${s.name} → ${t.name}`;
    }
  }
  return e.entityId.slice(0, 8);
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

const actionTone = (action: string) => (action === "delete" ? "danger" : action === "create" ? "ok" : "");
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
          <span class="badge">{{ actorUser.data.value?.displayName ?? get("actorId") }}</span>
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
      {{ filtered ? "Adjust or clear the filters above." : "Every create, update and delete is recorded here with the user who made it." }}
    </EmptyState>

    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th scope="col" :aria-sort="lq.ariaSort('occurredAt')">
                <button type="button" class="sort" @click="lq.toggleSort('occurredAt')">When {{ lq.sortIndicator("occurredAt") }}</button>
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
              <td>
                <RouterLink v-if="recordLink(e)" :to="recordLink(e)!">{{ recordName(e) }}</RouterLink>
                <template v-else>{{ recordName(e) }}</template>
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
