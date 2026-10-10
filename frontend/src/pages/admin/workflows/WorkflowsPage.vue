<script setup lang="ts">
import { adminCrumbs } from "../sections";
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useCiClasses } from "../../../api/queries";
import { useWorkflowList, type WorkflowListQuery } from "../../../api/workflows";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import Icon from "../../../components/Icon.vue";
import PaginationBar from "../../../components/PaginationBar.vue";
import SkeletonRows from "../../../components/SkeletonRows.vue";
import SortIcon from "../../../components/SortIcon.vue";
import { t, type MessageKey } from "../../../i18n";
import { useDebounced, useDocumentTitle } from "../../../lib/composables";
import { formatDateTime, formatRelative } from "../../../lib/format";
import { useListQuery } from "../../../lib/listQuery";

/**
 * Administration › Workflows: every workflow definition, filtered by the CI type it runs on.
 * Search, type, active flag, sort and page live in the URL; the API filters and pages. The page uses the
 * inventory's head band (breadcrumb, title with the count, intro, filters) above the table card.
 */
useDocumentTitle(() => t("admin.section.workflows"));
type SortField = NonNullable<WorkflowListQuery["sort"]>;

const COLUMNS: { key: string; label: MessageKey; sort?: string; num?: boolean }[] = [
  { key: "name", label: "wfAdmin.col.name", sort: "name" },
  { key: "key", label: "wfAdmin.col.key", sort: "key" },
  { key: "class", label: "wfAdmin.col.class" },
  { key: "stateField", label: "wfAdmin.col.stateField" },
  { key: "status", label: "wfAdmin.col.status" },
  { key: "version", label: "wfAdmin.col.version", num: true },
  { key: "draft", label: "wfAdmin.col.draft" },
  { key: "updated", label: "wfAdmin.col.updated", sort: "updatedAt" },
];

const lq = useListQuery({ sort: "name" });
const { get, limit, offset, update } = lq;
const query = computed<WorkflowListQuery>(() => ({
  q: get("q") || undefined,
  classKey: get("class") || undefined,
  active: (get("active") || undefined) as WorkflowListQuery["active"],
  sort: lq.sort.value as SortField,
  limit: limit.value,
  offset: offset.value,
}));
const list = useWorkflowList(query);
const classes = useCiClasses();
const classByKey = computed(() => new Map((classes.data.value ?? []).map((c) => [c.key, c])));

const qText = ref(get("q"));
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => v !== get("q") && update({ q: v || undefined }));
watch(
  () => get("q"),
  (v) => (qText.value = v),
);

const filtered = computed(() => !!(get("q") || get("class") || get("active")));
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);
const newHref = computed(() => {
  const c = classByKey.value.get(get("class"));
  return c ? `/admin/workflows/new?classId=${c.id}` : "/admin/workflows/new";
});

function clearFilters() {
  qText.value = "";
  update({ q: undefined, class: undefined, active: undefined });
}
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="adminCrumbs('workflows')" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("admin.section.workflows") }}</h1>
        <span v-if="list.data.value" class="count mono">{{ t("wfAdmin.list.count", { n: total }) }}</span>
        <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div class="actions">
        <RouterLink class="btn btn-primary" :to="newHref"><Icon name="plus" />{{ t("wfAdmin.new") }}</RouterLink>
      </div>
    </div>
    <p class="page-intro">{{ t("wfAdmin.list.intro") }}</p>
    <form class="toolbar" role="search" :aria-label="t('wfAdmin.list.filters')" @submit.prevent>
      <div class="field search">
        <label for="wf-q">{{ t("admin.search") }}</label>
        <div class="input-icon">
          <Icon name="search" />
          <input id="wf-q" v-model="qText" type="search" :placeholder="t('wfAdmin.list.searchPlaceholder')" />
        </div>
      </div>
      <div class="field">
        <label for="wf-class">{{ t("wfAdmin.col.class") }}</label>
        <select id="wf-class" :value="get('class')" @change="update({ class: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("wfAdmin.filter.allTypes") }}</option>
          <option v-for="c in classes.data.value ?? []" :key="c.id" :value="c.key" dir="auto">{{ c.name }}</option>
        </select>
      </div>
      <div class="field">
        <label for="wf-active">{{ t("wfAdmin.col.status") }}</label>
        <select id="wf-active" :value="get('active')" @change="update({ active: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("wfAdmin.filter.anyStatus") }}</option>
          <option value="true">{{ t("common.active") }}</option>
          <option value="false">{{ t("wfAdmin.inactive") }}</option>
        </select>
      </div>
      <button v-if="filtered" type="button" class="btn" @click="clearFilters">{{ t("inventory.clearFilters") }}</button>
    </form>
  </div>

  <section class="panel explorer" :aria-label="t('admin.section.workflows')">
    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <SkeletonRows v-else-if="list.isLoading.value" :label="t('wfAdmin.list.loading')" />
    <EmptyState v-else-if="list.data.value && total === 0 && filtered" icon="search" :title="t('wfAdmin.list.noMatch')">
      <template #actions><button type="button" class="btn" @click="clearFilters">{{ t("inventory.clearFilters") }}</button></template>
    </EmptyState>
    <EmptyState v-else-if="list.data.value && total === 0" icon="network" :title="t('wfAdmin.list.empty.title')" data-testid="workflows-empty">
      {{ t("wfAdmin.list.empty.body") }}
      <template #actions><RouterLink class="btn btn-primary" :to="newHref">{{ t("wfAdmin.new") }}</RouterLink></template>
    </EmptyState>
    <EmptyState v-else-if="list.data.value && rows.length === 0" :title="t('common.pastEnd')">
      <template #actions><button type="button" class="btn" @click="update({})">{{ t("common.firstPage") }}</button></template>
    </EmptyState>

    <template v-if="rows.length > 0 && !list.isError.value">
      <div class="table-wrap table-scroll">
        <table :class="['data', 'list-table', { loading: list.isPlaceholderData.value }]">
          <caption class="sr-only">{{ t("wfAdmin.list.caption") }}</caption>
          <thead>
            <tr>
              <th v-for="c in COLUMNS" :key="c.key" scope="col" :class="{ num: c.num }" :aria-sort="c.sort ? lq.ariaSort(c.sort) : undefined">
                <button v-if="c.sort" type="button" class="sort" @click="lq.toggleSort(c.sort)">{{ t(c.label) }} <SortIcon :dir="lq.ariaSort(c.sort)" /></button>
                <template v-else>{{ t(c.label) }}</template>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="w in rows" :key="w.id" :class="{ disabled: !w.isActive }">
              <td>
                <span class="cell-clip"><RouterLink class="list-name" :to="`/admin/workflows/${w.id}`" dir="auto">{{ w.name }}</RouterLink></span>
              </td>
              <td class="mono muted">{{ w.key }}</td>
              <td dir="auto">
                {{ classByKey.get(w.classKey)?.name ?? w.classKey }}
                <span v-if="w.includeSubclasses" class="muted">{{ t("wfAdmin.andSubtypes") }}</span>
              </td>
              <td :class="w.stateAttributeKey ? 'mono' : 'muted'">{{ w.stateAttributeKey ?? t("wfAdmin.none") }}</td>
              <td>
                <span v-if="w.isActive" class="badge ok"><span class="status-dot" aria-hidden="true" />{{ t("common.active") }}</span>
                <span v-else class="badge off"><span class="status-dot" aria-hidden="true" />{{ t("wfAdmin.inactive") }}</span>
              </td>
              <td class="num mono">{{ w.currentVersionNo ?? "–" }}</td>
              <td>
                <span v-if="w.draftVersionNo !== null" class="badge info">{{ t("wfAdmin.draftChip", { n: w.draftVersionNo }) }}</span>
                <span v-else class="muted">{{ t("wfAdmin.none") }}</span>
              </td>
              <td>
                <time :datetime="w.updatedAt" :title="t('wfAdmin.updatedTitle', { when: formatDateTime(w.updatedAt), name: w.updatedByName })">{{
                  formatRelative(w.updatedAt)
                }}</time>
              </td>
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
