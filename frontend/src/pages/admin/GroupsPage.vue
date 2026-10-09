<script setup lang="ts">
import { adminCrumbs } from "./sections";
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useGroupList, type GroupListQuery, type UserGroup } from "../../api/groups";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import KeyboardHints from "../../components/KeyboardHints.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import RowMenu from "../../components/RowMenu.vue";
import SkeletonRows from "../../components/SkeletonRows.vue";
import { formatNumber, t } from "../../i18n";
import { useDebounced, useDocumentTitle } from "../../lib/composables";
import { formatDateTime, formatRelative } from "../../lib/format";
import { onRowKeydown } from "../../lib/rowKeyboard";
import { useListQuery } from "../../lib/listQuery";
import SortIcon from "../../components/SortIcon.vue";

/** Administration › Groups, an explorer list (design §2.7): search, sort and page live in the URL; the API searches and pages. */
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

const pastEnd = computed(() => !!list.data.value && total.value > 0 && rows.value.length === 0);
const rowMenu = (g: UserGroup) => [{ label: t("inventory.row.open"), to: `/admin/groups/${g.id}` }];

function clearSearch() {
  qText.value = "";
  update({ q: undefined });
}
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="adminCrumbs('groups')" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("groups.title") }}</h1>
        <span v-if="list.data.value" class="count mono">{{ t("common.total", { n: formatNumber(total) }) }}</span>
        <span v-if="list.isFetching.value && !list.isPending.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div class="actions">
        <RouterLink class="btn btn-primary" to="/admin/groups/new"><Icon name="plus" />{{ t("groups.create") }}</RouterLink>
      </div>
    </div>
    <p class="page-intro">{{ t("admin.groups.intro") }}</p>
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="g-q">{{ t("groups.search") }}</label>
        <div class="input-icon">
          <Icon name="search" />
          <input id="g-q" v-model="qText" type="search" :placeholder="t('groups.searchPlaceholder')" />
        </div>
      </div>
      <button v-if="filtered" type="button" class="btn btn-ghost" @click="clearSearch"><Icon name="x" />{{ t("groups.clearSearch") }}</button>
    </form>
  </div>

  <section class="panel explorer" :aria-label="t('groups.title')">
    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <SkeletonRows v-else-if="list.isPending.value" :label="t('groups.loading')" />
    <EmptyState v-else-if="total === 0 && filtered" icon="search" :title="t('groups.noMatch')">
      <template #actions><button type="button" class="btn" @click="clearSearch">{{ t("groups.clearSearch") }}</button></template>
    </EmptyState>
    <EmptyState v-else-if="total === 0" icon="user" :title="t('admin.groups.empty.title')" data-testid="groups-empty">
      {{ t("groups.empty") }}
      <template #actions><RouterLink class="btn btn-primary" to="/admin/groups/new"><Icon name="plus" />{{ t("groups.create") }}</RouterLink></template>
    </EmptyState>
    <EmptyState v-else-if="pastEnd" :title="t('common.pastEnd')">
      <template #actions><button type="button" class="btn" @click="update({})">{{ t("common.firstPage") }}</button></template>
    </EmptyState>

    <template v-if="rows.length > 0 && !list.isError.value">
      <div class="table-wrap table-scroll" role="region" tabindex="0" :aria-label="t('groups.table')">
        <table :class="['data', 'list-table', { loading: list.isPlaceholderData.value }]" aria-describedby="groups-keys">
          <thead>
            <tr>
              <th v-for="c in COLUMNS" :key="c.key" scope="col" :class="{ num: c.num }" :aria-sort="c.sort ? lq.ariaSort(c.sort) : undefined">
                <button v-if="c.sort" type="button" class="sort" @click="lq.toggleSort(c.sort)">
                  {{ c.label }} <SortIcon :dir="lq.ariaSort(c.sort)" />
                </button>
                <template v-else>{{ c.label }}</template>
              </th>
              <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
            </tr>
          </thead>
          <tbody @keydown="onRowKeydown($event)">
            <tr v-for="g in rows" :key="g.id" :data-id="g.id">
              <td><RouterLink class="list-name" :to="`/admin/groups/${g.id}`" dir="auto">{{ g.name }}</RouterLink></td>
              <td :title="g.description ?? undefined" dir="auto">{{ g.description ?? "" }}</td>
              <td class="num">{{ formatNumber(g.memberCount) }}</td>
              <td><time :datetime="g.updatedAt" :title="formatDateTime(g.updatedAt)">{{ formatRelative(g.updatedAt) }}</time></td>
              <td class="row-actions">
                <RowMenu :label="t('inventory.rowMenu', { name: g.name })" :items="rowMenu(g)" />
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div class="table-footer">
        <PaginationBar numbered :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
      </div>
      <KeyboardHints id="groups-keys" />
    </template>
  </section>
</template>
