<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useGroupList, type GroupListQuery } from "../../api/groups";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { t } from "../../i18n";
import { useDebounced, useDocumentTitle } from "../../lib/composables";
import { formatRelative } from "../../lib/format";
import { useListQuery } from "../../lib/listQuery";
import { useFlashStore } from "../../stores/flash";

/** Administration › Groups. Search, sort and page live in the URL; the API searches and pages. */
useDocumentTitle(t("groups.title"));
type SortField = NonNullable<GroupListQuery["sort"]>;

const COLUMNS: { key: string; label: string; sort?: string; num?: boolean }[] = [
  { key: "name", label: t("groups.col.name"), sort: "name" },
  { key: "description", label: t("groups.col.description") },
  { key: "members", label: t("groups.col.members"), sort: "memberCount", num: true },
  { key: "updated", label: t("groups.col.updated") },
];

const lq = useListQuery({ sort: "name" });
const { get, limit, offset, update } = lq;
const query = computed<GroupListQuery>(() => ({
  q: get("q") || undefined,
  sort: lq.sort.value as SortField,
  limit: limit.value,
  offset: offset.value,
}));
const list = useGroupList(query);
// Set by the edit page after a delete.
const flash = useFlashStore();
const flashText = computed(() => flash.forCi("groups"));

const qText = ref(get("q"));
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => v !== get("q") && update({ q: v || undefined }));
watch(
  () => get("q"),
  (v) => (qText.value = v),
);

const filtered = computed(() => !!get("q"));
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);

function clearSearch() {
  qText.value = "";
  update({ q: undefined });
}
</script>

<template>
  <Breadcrumbs :items="[{ label: t('common.administration'), to: '/admin' }, { label: t('groups.title') }]" />
  <div class="page-header">
    <div class="title">
      <h1>{{ t("groups.title") }}</h1>
      <span v-if="list.data.value" class="muted">{{ t("common.total", { n: total.toLocaleString() }) }}</span>
      <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" :aria-label="t('common.refreshing')" />
    </div>
    <div class="actions">
      <RouterLink class="btn btn-primary" to="/admin/groups/new">+ {{ t("groups.create") }}</RouterLink>
    </div>
  </div>
  <div v-if="flashText" class="alert" role="status">{{ flashText }}</div>

  <section class="panel" :aria-label="t('groups.title')">
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="g-q">{{ t("groups.search") }}</label>
        <input id="g-q" v-model="qText" type="search" :placeholder="t('groups.searchPlaceholder')" />
      </div>
      <button v-if="filtered" type="button" class="btn" @click="clearSearch">{{ t("groups.clearSearch") }}</button>
    </form>

    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <LoadingState v-if="list.isLoading.value" :label="t('groups.loading')" />
    <EmptyState v-if="list.data.value && total === 0 && filtered" :title="t('groups.noMatch')">
      <template #actions><button type="button" class="btn" @click="clearSearch">{{ t("groups.clearSearch") }}</button></template>
    </EmptyState>
    <EmptyState v-else-if="list.data.value && total === 0" data-testid="groups-empty">
      {{ t("groups.empty") }}
      <template #actions><RouterLink class="btn btn-primary" to="/admin/groups/new">{{ t("groups.create") }}</RouterLink></template>
    </EmptyState>
    <EmptyState v-if="list.data.value && total > 0 && rows.length === 0" :title="t('common.pastEnd')">
      <template #actions><button type="button" class="btn" @click="update({})">{{ t("common.firstPage") }}</button></template>
    </EmptyState>

    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th v-for="c in COLUMNS" :key="c.key" scope="col" :class="{ num: c.num }" :aria-sort="c.sort ? lq.ariaSort(c.sort) : undefined">
                <button v-if="c.sort" type="button" class="sort" @click="lq.toggleSort(c.sort)">
                  {{ c.label }} {{ lq.sortIndicator(c.sort) }}
                </button>
                <template v-else>{{ c.label }}</template>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="g in rows" :key="g.id">
              <td><RouterLink :to="`/admin/groups/${g.id}`">{{ g.name }}</RouterLink></td>
              <td :title="g.description ?? undefined">{{ g.description ?? "" }}</td>
              <td class="num">{{ g.memberCount.toLocaleString() }}</td>
              <td :title="g.updatedAt">{{ formatRelative(g.updatedAt) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
    </template>
  </section>
</template>
