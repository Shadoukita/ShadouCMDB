<script setup lang="ts">
import { adminCrumbs } from "./sections";
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useProfileList, type PermissionProfile, type ProfileListQuery } from "../../api/admin";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import KeyboardHints from "../../components/KeyboardHints.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import RowMenu, { type RowMenuItem } from "../../components/RowMenu.vue";
import SkeletonRows from "../../components/SkeletonRows.vue";
import { formatNumber, t } from "../../i18n";
import { useDebounced, useDocumentTitle } from "../../lib/composables";
import { formatDateTime, formatRelative } from "../../lib/format";
import { onRowKeydown } from "../../lib/rowKeyboard";
import { useListQuery } from "../../lib/listQuery";
import { useSessionStore } from "../../stores/session";
import CloneProfileDialog from "./CloneProfileDialog.vue";
import { summarise } from "./profileSummary";

/**
 * Administration › Permission profiles, an explorer list (design §2.7). Users with only users.manage see it
 * read-only: no New and no Clone.
 */
useDocumentTitle(() => t("admin.section.profiles"));
type SortField = NonNullable<ProfileListQuery["sort"]>;
const session = useSessionStore();
const canManage = computed(() => session.can("profiles.manage"));

const lq = useListQuery({ sort: "name" });
const { get, limit, offset, update } = lq;
const query = computed<ProfileListQuery>(() => ({
  q: get("q") || undefined,
  sort: lq.sort.value as SortField,
  limit: limit.value,
  offset: offset.value,
}));
const list = useProfileList(query);
const qText = ref(get("q"));
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => v !== get("q") && update({ q: v || undefined }));
watch(
  () => get("q"),
  (v) => (qText.value = v),
);
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);
const cloning = ref<PermissionProfile | null>(null);
const pastEnd = computed(() => !!list.data.value && total.value > 0 && rows.value.length === 0);
function clearSearch() {
  qText.value = "";
  update({ q: undefined });
}
const rowMenu = (p: PermissionProfile): RowMenuItem[] => [
  { label: t("inventory.row.open"), to: `/admin/profiles/${p.id}` },
  ...(session.can("users.manage") ? [{ label: t("admin.profiles.row.users"), to: { path: "/admin/users", query: { profileId: p.id } } }] : []),
  ...(canManage.value ? [{ label: t("admin.profiles.row.clone"), action: () => (cloning.value = p) }] : []),
];
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="adminCrumbs('profiles')" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("admin.section.profiles") }}</h1>
        <span v-if="list.data.value" class="count mono">{{ t("common.total", { n: formatNumber(total) }) }}</span>
        <span v-if="list.isFetching.value && !list.isPending.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div v-if="canManage" class="actions">
        <RouterLink class="btn btn-primary" to="/admin/profiles/new"><Icon name="plus" />{{ t("admin.profiles.new") }}</RouterLink>
      </div>
    </div>
    <p class="page-intro">{{ t("admin.profiles.intro") }}</p>
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="p-q">{{ t("admin.search") }}</label>
        <div class="input-icon">
          <Icon name="search" />
          <input id="p-q" v-model="qText" type="search" :placeholder="t('admin.profiles.searchPlaceholder')" />
        </div>
      </div>
      <div class="field">
        <label for="p-sort">{{ t("admin.profiles.sort") }}</label>
        <select id="p-sort" :value="lq.sort.value" @change="update({ sort: ($event.target as HTMLSelectElement).value })">
          <option value="name">{{ t("admin.profiles.sort.name") }}</option>
          <option value="-updatedAt">{{ t("admin.profiles.sort.updated") }}</option>
          <option value="-createdAt">{{ t("admin.profiles.sort.created") }}</option>
        </select>
      </div>
      <button v-if="get('q')" type="button" class="btn btn-ghost" @click="clearSearch"><Icon name="x" />{{ t("groups.clearSearch") }}</button>
    </form>
  </div>

  <section class="panel explorer" :aria-label="t('admin.section.profiles')">
    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <SkeletonRows v-else-if="list.isPending.value" :label="t('admin.profiles.loading')" />
    <EmptyState v-else-if="total === 0 && get('q')" icon="search" :title="t('admin.profiles.noMatch')">
      {{ t("admin.filter.noMatchBody") }}
      <template #actions><button type="button" class="btn" @click="clearSearch">{{ t("groups.clearSearch") }}</button></template>
    </EmptyState>
    <!-- The built-in Administrator profile always exists; an empty list means the API returned none visible. -->
    <EmptyState v-else-if="total === 0" icon="lock" :title="t('admin.profiles.empty.title')">
      {{ t("admin.profiles.empty.body") }}
      <template v-if="canManage" #actions><RouterLink class="btn btn-primary" to="/admin/profiles/new"><Icon name="plus" />{{ t("admin.profiles.new") }}</RouterLink></template>
    </EmptyState>
    <EmptyState v-else-if="pastEnd" :title="t('common.pastEnd')">
      <template #actions><button type="button" class="btn" @click="update({})">{{ t("common.firstPage") }}</button></template>
    </EmptyState>

    <template v-if="rows.length > 0 && !list.isError.value">
      <div class="table-wrap table-scroll">
        <table :class="['data', 'list-table', 'profile-table', { loading: list.isPlaceholderData.value }]" aria-describedby="profiles-keys">
          <thead>
            <tr>
              <th scope="col">{{ t("admin.profiles.col.name") }}</th>
              <th scope="col">{{ t("admin.profiles.col.grants") }}</th>
              <th scope="col" class="num">{{ t("admin.profiles.col.users") }}</th>
              <th scope="col">{{ t("common.updated") }}</th>
              <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
            </tr>
          </thead>
          <tbody @keydown="onRowKeydown($event)">
            <tr v-for="p in rows" :key="p.id" :data-id="p.id">
              <td :title="p.description ?? undefined">
                <span class="name-badges">
                  <RouterLink class="list-name" :to="`/admin/profiles/${p.id}`" dir="auto">{{ p.name }}</RouterLink>
                  <span v-if="p.isBuiltin" class="badge">{{ t("admin.profiles.builtin") }}</span>
                  <span v-if="p.requireMfa" class="badge warn" :title="t('admin.profiles.mfaRequiredTitle')">{{ t("admin.profiles.mfaRequired") }}</span>
                </span>
              </td>
              <td :title="summarise(p)">{{ summarise(p) }}</td>
              <td class="num">
                <RouterLink v-if="session.can('users.manage') && p.userCount > 0" :to="{ path: '/admin/users', query: { profileId: p.id } }">
                  {{ formatNumber(p.userCount) }}
                </RouterLink>
                <template v-else>{{ formatNumber(p.userCount) }}</template>
              </td>
              <td><time :datetime="p.updatedAt" :title="formatDateTime(p.updatedAt)">{{ formatRelative(p.updatedAt) }}</time></td>
              <td class="row-actions">
                <RowMenu :label="t('inventory.rowMenu', { name: p.name })" :items="rowMenu(p)" />
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div class="table-footer">
        <PaginationBar numbered :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
      </div>
      <KeyboardHints id="profiles-keys" />
    </template>
  </section>
  <CloneProfileDialog :profile="cloning" @close="cloning = null" />
</template>
