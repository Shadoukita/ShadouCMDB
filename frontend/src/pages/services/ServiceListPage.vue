<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../api/client";
import { useCriticalityValues } from "../../api/queries";
import { useServiceList, useServiceSettings, type Principal, type PrincipalRef, type ServiceSummary } from "../../api/services";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import CriticalityBadge from "../../components/CriticalityBadge.vue";
import EmptyState from "../../components/EmptyState.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import PrincipalCombobox from "../../components/PrincipalCombobox.vue";
import { t, type MessageKey } from "../../i18n";
import { useDebounced, useDocumentTitle } from "../../lib/composables";
import { formatDateTime } from "../../lib/format";
import {
  CRITICALITY_NONE,
  DEFAULT_SERVICE_LIST,
  hasServiceFilters,
  ownerCell,
  parseServiceListQuery,
  serviceListParams,
  serviceListUrl,
  type ServiceListState,
} from "../../lib/serviceList";
import { useSessionStore } from "../../stores/session";
import ServiceError from "./ServiceError.vue";

/**
 * The business service list (spec §5.2): filters, sort and page in the URL (lib/serviceList), so a view
 * survives a reload, can be shared and walks back with Back; paged and filtered on the server.
 */
const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const settings = useServiceSettings();
useDocumentTitle(() => t("services.title"));

const state = computed(() => parseServiceListQuery(route.query));
const denied = computed(() => settings.data.value?.canView === false);
const list = useServiceList(
  () => serviceListParams(state.value),
  () => !!settings.data.value?.canView,
);
const rows = computed(() => list.data.value?.data ?? []);
const total = computed(() => list.data.value?.page.total ?? 0);
const filtered = computed(() => hasServiceFilters(state.value));
const classId = computed(() => settings.data.value?.classId);
const canCreate = computed(() => !!classId.value && session.canOnClass(classId.value, "create"));
const createTo = computed(() => ({ path: "/cis/new", query: { classId: classId.value, return: "services" } }));
/** The owner lookup needs edit on the service class or users.manage; others filter by "My services" only. */
const canPickOwner = computed(() => !!settings.data.value?.canEdit || session.can("users.manage"));
const forbidden = computed(() => denied.value || (list.error.value instanceof ApiError && list.error.value.code === "FORBIDDEN"));

/** Navigates to a state; a filter change starts at page 1. The search box replaces the entry instead of piling up history. */
function update(patch: Partial<ServiceListState>, opts: { keepPage?: boolean; replace?: boolean } = {}) {
  const next = { ...state.value, ...patch, ...(opts.keepPage ? {} : { page: 1 }) };
  const to = { path: "/services", query: serviceListUrl(next) };
  void (opts.replace ? router.replace(to) : router.push(to));
}

// The search box follows the URL (Back, Clear filters) and writes to it 300 ms after the last keystroke.
const qText = ref(state.value.q);
const qDebounced = useDebounced(qText, 300);
watch(qDebounced, (q) => q !== state.value.q && update({ q }, { replace: true }));
watch(
  () => state.value.q,
  (q) => {
    if (q !== qDebounced.value) qText.value = q;
  },
);

// Criticality: several values, plus "not set".
const criticality = useCriticalityValues();
const critChoices = computed(() => [
  ...(criticality.data.value ?? []).map((v) => ({ id: v.id, name: v.isActive ? v.name : `${v.name} ${t("services.filter.retired")}` })),
  { id: CRITICALITY_NONE, name: t("services.filter.criticality.none") },
]);
const critSummary = computed(() => {
  const chosen = state.value.criticality;
  if (chosen.length === 0) return t("services.filter.any");
  if (chosen.length === 1) return critChoices.value.find((c) => c.id === chosen[0])?.name ?? t("services.filter.selected", { n: 1 });
  return t("services.filter.selected", { n: chosen.length });
});
function toggleCriticality(id: string, on: boolean) {
  const set = new Set(state.value.criticality);
  if (on) set.add(id);
  else set.delete(id);
  update({ criticality: critChoices.value.map((c) => c.id).filter((c) => set.has(c)) });
}

// Owner: one user or group. Its name comes from the pick, or after a reload from the rows it owns.
const ownerNames = ref<Record<string, string>>({});
function pickOwner(p: Principal) {
  ownerNames.value = { ...ownerNames.value, [p.id]: p.displayName };
  update({ owner: p.id });
}
const ownerName = computed(() => {
  const id = state.value.owner;
  if (!id) return "";
  if (ownerNames.value[id]) return ownerNames.value[id];
  for (const r of rows.value) {
    const o = [...r.owners.technical, ...r.owners.business].find((p) => p.id === id);
    if (o) return o.displayName;
  }
  return t("services.filter.owner.unknown");
});

function clearFilters() {
  qText.value = "";
  update({ ...DEFAULT_SERVICE_LIST, criticality: [], sort: state.value.sort, limit: state.value.limit });
}

const SORTABLE: Record<string, string> = { name: "name", criticality: "criticality", members: "memberCount", updated: "updatedAt" };
function toggleSort(field: string) {
  update({ sort: state.value.sort === field ? `-${field}` : field });
}
const ariaSort = (field: string) => (state.value.sort === field ? "ascending" : state.value.sort === `-${field}` ? "descending" : "none");
const indicator = (field: string) => (state.value.sort === field ? "▲" : state.value.sort === `-${field}` ? "▼" : "");
const COLUMNS: [string, MessageKey][] = [
  ["name", "services.col.name"],
  ["ident", "services.col.ident"],
  ["criticality", "services.col.criticality"],
  ["technical", "services.col.technicalOwners"],
  ["business", "services.col.businessOwners"],
  ["members", "services.col.members"],
  ["nested", "services.col.nested"],
  ["active", "services.col.active"],
  ["updated", "services.col.updated"],
];
const numeric = new Set(["members", "nested"]);

const ownerText = (o: PrincipalRef) => (o.active ? o.displayName : `${o.displayName} ${t("services.owners.disabled")}`);
const cell = (s: ServiceSummary, role: "technical" | "business") => ownerCell(s.owners[role]);
const pastEnd = computed(() => !!list.data.value && total.value > 0 && rows.value.length === 0);
</script>

<template>
  <Breadcrumbs :items="[{ label: t('services.title') }]" />
  <div class="page-header">
    <div class="title">
      <h1>{{ t("services.title") }}</h1>
      <span v-if="list.data.value" class="muted">{{ t("services.count", { n: total }) }}</span>
      <span v-if="list.isFetching.value && !list.isPending.value" class="spinner" :aria-label="t('common.refreshing')" />
    </div>
    <div v-if="canCreate" class="actions">
      <RouterLink class="btn btn-primary" :to="createTo">{{ t("services.create") }}</RouterLink>
    </div>
  </div>

  <ServiceError v-if="settings.isError.value" :error="settings.error.value" :on-retry="() => settings.refetch()" />
  <LoadingState v-else-if="settings.isPending.value" :label="t('services.loading')" />
  <EmptyState v-else-if="forbidden" :title="t('services.forbiddenTitle')">
    {{ t("services.forbidden") }}
    <template #actions><RouterLink class="btn" to="/">{{ t("services.backToDashboard") }}</RouterLink></template>
  </EmptyState>

  <section v-else class="panel" :aria-label="t('services.title')">
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="svc-q">{{ t("services.filter.search") }}</label>
        <input id="svc-q" v-model="qText" type="search" maxlength="200" :placeholder="t('services.filter.searchPlaceholder')" />
      </div>
      <details class="field multi-select">
        <summary>
          <span id="svc-crit-label" class="label">{{ t("services.col.criticality") }}</span>
          <span class="select-like" aria-describedby="svc-crit-label">{{ critSummary }}</span>
        </summary>
        <fieldset class="popover">
          <legend class="sr-only">{{ t("services.col.criticality") }}</legend>
          <p v-if="criticality.isError.value" class="muted">{{ t("services.filter.criticality.failed") }}</p>
          <label v-for="v in critChoices" :key="v.id" class="checkbox-row">
            <input type="checkbox" :checked="state.criticality.includes(v.id)" @change="toggleCriticality(v.id, ($event.target as HTMLInputElement).checked)" />
            <bdi>{{ v.name }}</bdi>
          </label>
        </fieldset>
      </details>
      <template v-if="canPickOwner">
        <div v-if="state.owner" class="field">
          <span class="label">{{ t("services.filter.owner") }}</span>
          <span class="filter-token">
            <bdi>{{ ownerName }}</bdi>
            <button type="button" class="btn btn-sm" :aria-label="t('services.filter.owner.clear', { name: ownerName })" @click="update({ owner: '' })">×</button>
          </span>
        </div>
        <PrincipalCombobox v-else :label="t('services.filter.owner')" @select="pickOwner" />
      </template>
      <div class="field">
        <span class="label">{{ t("services.filter.mine") }}</span>
        <label class="checkbox-row">
          <input id="svc-mine" type="checkbox" :checked="state.mine" @change="update({ mine: ($event.target as HTMLInputElement).checked })" />
          {{ t("services.filter.mineLabel") }}
        </label>
      </div>
      <div v-if="state.owner || state.mine" class="field">
        <label for="svc-role">{{ t("services.filter.role") }}</label>
        <select id="svc-role" :value="state.role" @change="update({ role: ($event.target as HTMLSelectElement).value as ServiceListState['role'] })">
          <option value="">{{ t("services.filter.role.any") }}</option>
          <option value="technical">{{ t("services.owners.technical") }}</option>
          <option value="business">{{ t("services.owners.business") }}</option>
        </select>
      </div>
      <div class="field">
        <label for="svc-owner-state">{{ t("services.filter.ownerState") }}</label>
        <select id="svc-owner-state" :value="state.ownerState" @change="update({ ownerState: ($event.target as HTMLSelectElement).value as ServiceListState['ownerState'] })">
          <option value="">{{ t("services.filter.any") }}</option>
          <option value="none">{{ t("services.filter.ownerState.none") }}</option>
          <option value="disabled">{{ t("services.filter.ownerState.disabled") }}</option>
        </select>
      </div>
      <div class="field">
        <span class="label">{{ t("services.filter.validity") }}</span>
        <label class="checkbox-row">
          <input id="svc-inactive" type="checkbox" :checked="state.includeInactive" @change="update({ includeInactive: ($event.target as HTMLInputElement).checked })" />
          {{ t("services.filter.includeInactive") }}
        </label>
      </div>
      <button v-if="filtered" type="button" class="btn" @click="clearFilters">{{ t("services.filter.clear") }}</button>
    </form>

    <div v-if="list.isError.value" class="panel-body">
      <ServiceError :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <div v-else-if="list.isPending.value" class="table-wrap" aria-busy="true">
      <span class="sr-only" role="status">{{ t("services.loading") }}</span>
      <div class="skeleton-table" aria-hidden="true"><div v-for="n in 8" :key="n" class="skeleton-row" /></div>
    </div>
    <EmptyState v-else-if="total === 0 && !filtered" :title="t('services.empty.title')">
      {{ canCreate ? t("services.empty.canCreate") : t("services.empty.cannotCreate") }}
      <template v-if="canCreate" #actions>
        <RouterLink class="btn btn-primary" :to="createTo">{{ t("services.create") }}</RouterLink>
      </template>
    </EmptyState>
    <EmptyState v-else-if="total === 0" :title="t('services.empty.noMatch')">
      <template #actions><button type="button" class="btn" @click="clearFilters">{{ t("services.filter.clear") }}</button></template>
    </EmptyState>
    <EmptyState v-else-if="pastEnd" :title="t('services.pastEnd')">
      <template #actions><button type="button" class="btn" @click="update({})">{{ t("services.firstPage") }}</button></template>
    </EmptyState>

    <template v-if="rows.length > 0 && !list.isError.value">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th
                v-for="[key, label] in COLUMNS"
                :key="key"
                scope="col"
                :class="{ num: numeric.has(key) }"
                :aria-sort="SORTABLE[key] ? ariaSort(SORTABLE[key]) : undefined"
              >
                <button v-if="SORTABLE[key]" type="button" class="sort" @click="toggleSort(SORTABLE[key])">{{ t(label) }} {{ indicator(SORTABLE[key]) }}</button>
                <template v-else>{{ t(label) }}</template>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in rows" :key="s.id">
              <td><RouterLink :to="`/services/${s.id}`" dir="auto">{{ s.name }}</RouterLink></td>
              <td class="mono">{{ s.ident }}</td>
              <td><CriticalityBadge :value="s.criticality" show-unset /></td>
              <td v-for="role in ['technical', 'business'] as const" :key="role" class="owner-cell">
                <template v-if="s.owners[role].length">
                  <template v-for="(o, i) in cell(s, role).shown" :key="o.id">
                    <template v-if="i > 0">, </template><bdi>{{ ownerText(o) }}</bdi>
                  </template>
                  <span v-if="cell(s, role).more" class="more" :title="s.owners[role].slice(2).map(ownerText).join(', ')">
                    +{{ cell(s, role).more }}<span class="sr-only"> {{ t("services.owners.more", { n: cell(s, role).more }) }}</span>
                  </span>
                </template>
                <span v-else class="muted">{{ t("services.owners.noneCell") }}</span>
              </td>
              <td class="num">{{ s.memberCount.toLocaleString() }}</td>
              <td class="num">{{ s.serviceMemberCount.toLocaleString() }}</td>
              <td>{{ s.active ? t("common.yes") : t("common.no") }}</td>
              <td>{{ formatDateTime(s.updatedAt) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar
        :total="total"
        :limit="state.limit"
        :offset="(state.page - 1) * state.limit"
        @change="(p) => update({ page: Math.floor(p.offset / p.limit) + 1, limit: p.limit }, { keepPage: true })"
      />
    </template>
  </section>
</template>
