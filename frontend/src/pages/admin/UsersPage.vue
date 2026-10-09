<script setup lang="ts">
import { adminCrumbs } from "./sections";
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useAllProfiles, useUserList, type UserListQuery, type User } from "../../api/admin";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import KeyboardHints from "../../components/KeyboardHints.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import RowMenu from "../../components/RowMenu.vue";
import SkeletonRows from "../../components/SkeletonRows.vue";
import { useDebounced, useDocumentTitle } from "../../lib/composables";
import { formatDateTime, formatRelative } from "../../lib/format";
import { onRowKeydown } from "../../lib/rowKeyboard";
import { useListQuery } from "../../lib/listQuery";
import { parseSignInStatus, SIGN_IN_STATUSES, signInStatusLabel } from "../../lib/people";
import { formatNumber, t } from "../../i18n";
import SortIcon from "../../components/SortIcon.vue";
import { useSessionStore } from "../../stores/session";

/**
 * Administration › Users, an explorer list (design §2.7; audit A4–A6): search, filters, sort and page live in
 * the URL and the API filters and pages; a row menu and keyboard rows (↑/↓, Enter) as on the inventory.
 */
useDocumentTitle(() => t("admin.section.users"));
type SortField = NonNullable<UserListQuery["sort"]>;
const session = useSessionStore();

const COLUMNS: { key: string; label: string; sort?: string }[] = [
  { key: "username", label: t("admin.users.col.username"), sort: "username" },
  { key: "displayName", label: t("admin.users.col.displayName"), sort: "displayName" },
  { key: "email", label: t("admin.users.col.email") },
  { key: "person", label: t("people.users.col.person") },
  { key: "profiles", label: t("admin.section.profiles") },
  { key: "status", label: t("admin.col.status") },
  { key: "signIn", label: t("admin.users.col.signIn") },
  { key: "mfa", label: t("admin.users.col.mfa") },
  { key: "lastLogin", label: t("admin.users.col.lastLogin"), sort: "lastLoginAt" },
  { key: "created", label: t("common.created"), sort: "createdAt" },
];

const lq = useListQuery({ sort: "username" });
const { get, limit, offset, update } = lq;
const query = computed<UserListQuery>(() => ({
  q: get("q") || undefined,
  isActive: get("isActive") === "true" || get("isActive") === "false" ? (get("isActive") as "true" | "false") : undefined,
  profileId: get("profileId") || undefined,
  signInStatus: parseSignInStatus(get("signInStatus")),
  sort: lq.sort.value as SortField,
  limit: limit.value,
  offset: offset.value,
}));
const list = useUserList(query);
const profiles = useAllProfiles();

const qText = ref(get("q"));
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => v !== get("q") && update({ q: v || undefined }));
watch(
  () => get("q"),
  (v) => (qText.value = v),
);

const filtered = computed(() => !!(get("q") || get("isActive") || get("profileId") || query.value.signInStatus));
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);

const pastEnd = computed(() => !!list.data.value && total.value > 0 && rows.value.length === 0);
const rowMenu = (u: User) => [
  { label: t("inventory.row.open"), to: `/admin/users/${u.id}` },
  { label: t("admin.users.row.tokens"), to: { path: "/admin/api-tokens", query: { userId: u.id } } },
  ...(session.can("audit.view") ? [{ label: t("admin.users.row.audit"), to: { path: "/admin/audit", query: { actorId: u.id } } }] : []),
];

function clearFilters() {
  qText.value = "";
  update({ q: undefined, isActive: undefined, profileId: undefined, signInStatus: undefined });
}
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="adminCrumbs('users')" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("admin.section.users") }}</h1>
        <span v-if="list.data.value" class="count mono">{{ t("common.total", { n: formatNumber(total) }) }}</span>
        <span v-if="list.isFetching.value && !list.isPending.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div class="actions">
        <RouterLink class="btn btn-primary" to="/admin/users/new"><Icon name="plus" />{{ t("admin.users.new") }}</RouterLink>
      </div>
    </div>
    <p class="page-intro">{{ t("admin.users.intro") }}</p>
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="u-q">{{ t("admin.search") }}</label>
        <div class="input-icon">
          <Icon name="search" />
          <input id="u-q" v-model="qText" type="search" :placeholder="t('admin.users.searchPlaceholder')" />
        </div>
      </div>
      <div class="field">
        <label for="u-status">{{ t("admin.col.status") }}</label>
        <select id="u-status" :value="get('isActive')" @change="update({ isActive: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("admin.filter.anyStatus") }}</option>
          <option value="true">{{ t("common.active") }}</option>
          <option value="false">{{ t("common.disabled") }}</option>
        </select>
      </div>
      <div class="field">
        <label for="u-profile">{{ t("admin.users.filter.profile") }}</label>
        <select id="u-profile" :value="get('profileId')" @change="update({ profileId: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("admin.users.filter.anyProfile") }}</option>
          <option v-for="p in profiles.data.value?.data ?? []" :key="p.id" :value="p.id">{{ p.name }}</option>
        </select>
      </div>
      <div class="field">
        <label for="u-sign-in">{{ t("people.users.filter.label") }}</label>
        <select
          id="u-sign-in"
          :value="query.signInStatus ?? ''"
          @change="update({ signInStatus: ($event.target as HTMLSelectElement).value || undefined })"
        >
          <option value="">{{ t("people.users.filter.any") }}</option>
          <option v-for="s in SIGN_IN_STATUSES" :key="s" :value="s">{{ signInStatusLabel(s) }}</option>
        </select>
      </div>
      <button v-if="filtered" type="button" class="btn btn-ghost" @click="clearFilters"><Icon name="x" />{{ t("admin.filter.clear") }}</button>
    </form>
  </div>

  <section class="panel explorer" :aria-label="t('admin.section.users')">
    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <SkeletonRows v-else-if="list.isPending.value" :label="t('admin.users.loading')" />
    <EmptyState v-else-if="total === 0 && filtered" icon="search" :title="t('admin.users.noMatch')">
      {{ t("admin.filter.noMatchBody") }}
      <template #actions><button type="button" class="btn" @click="clearFilters">{{ t("admin.filter.clear") }}</button></template>
    </EmptyState>
    <EmptyState v-else-if="total === 0" icon="user" :title="t('admin.users.empty.title')">
      {{ t("admin.users.empty.body") }}
      <template #actions><RouterLink class="btn btn-primary" to="/admin/users/new"><Icon name="plus" />{{ t("admin.users.new") }}</RouterLink></template>
    </EmptyState>
    <EmptyState v-else-if="pastEnd" :title="t('common.pastEnd')">
      <template #actions><button type="button" class="btn" @click="update({})">{{ t("common.firstPage") }}</button></template>
    </EmptyState>

    <template v-if="rows.length > 0 && !list.isError.value">
      <div class="table-wrap table-scroll" role="region" tabindex="0" :aria-label="t('admin.users.table')">
        <table :class="['data', 'list-table', { loading: list.isPlaceholderData.value }]" aria-describedby="users-keys">
          <thead>
            <tr>
              <th v-for="c in COLUMNS" :key="c.key" scope="col" :aria-sort="c.sort ? lq.ariaSort(c.sort) : undefined">
                <button v-if="c.sort" type="button" class="sort" @click="lq.toggleSort(c.sort)">
                  {{ c.label }} <SortIcon :dir="lq.ariaSort(c.sort)" />
                </button>
                <template v-else>{{ c.label }}</template>
              </th>
              <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
            </tr>
          </thead>
          <tbody @keydown="onRowKeydown($event)">
            <tr v-for="u in rows" :key="u.id" :data-id="u.id" :class="{ disabled: !u.isActive }">
              <td><RouterLink class="list-name" :to="`/admin/users/${u.id}`">{{ u.username }}</RouterLink></td>
              <td dir="auto">{{ u.displayName }}</td>
              <td>
                <template v-if="u.email">{{ u.email }}</template>
                <span v-else class="badge warn" :title="t('people.users.emailRequiredTitle')" data-testid="email-required">
                  {{ signInStatusLabel("email_required") }}
                </span>
              </td>
              <td>
                <RouterLink v-if="u.person" :to="`/cis/${u.person.id}`" dir="auto">{{ u.person.label }}</RouterLink>
                <span v-else-if="u.signInStatus === 'person_missing'" class="badge danger" :title="t('people.users.incompleteTitle')" data-testid="account-incomplete">
                  {{ signInStatusLabel("person_missing") }}
                </span>
                <span v-else class="muted">{{ t("people.users.noPerson") }}</span>
              </td>
              <td :title="u.profiles.map((p) => p.name).join(', ')">
                <span v-if="u.profiles.length === 0" class="muted">{{ t("admin.users.noProfiles") }}</span>
                <template v-for="(p, i) in u.profiles" :key="p.id"><template v-if="i > 0">, </template>{{ p.name }}</template>
              </td>
              <td>
                <span v-if="u.isActive" class="badge ok"><span class="status-dot" aria-hidden="true" />{{ t("common.active") }}</span>
                <span v-else class="badge off"><span class="status-dot" aria-hidden="true" />{{ t("common.disabled") }}</span>
              </td>
              <td>
                <span v-if="u.identityProvider" class="badge" :title="t('admin.users.idpTitle', { name: u.identityProvider.name })">
                  {{ u.identityProvider.name }}
                </span>
                <span v-else class="muted">{{ t("admin.users.localPassword") }}</span>
              </td>
              <td>
                <span v-if="u.mfaEnabled" class="badge ok">{{ t("admin.users.mfaOn") }}</span>
                <span v-else class="muted">{{ t("admin.users.mfaOff") }}</span>
              </td>
              <td>
                <time v-if="u.lastLoginAt" :datetime="u.lastLoginAt" :title="formatDateTime(u.lastLoginAt)">{{ formatRelative(u.lastLoginAt) }}</time>
                <span v-else class="muted">{{ t("admin.never") }}</span>
              </td>
              <td><time :datetime="u.createdAt" :title="formatDateTime(u.createdAt)">{{ formatRelative(u.createdAt) }}</time></td>
              <td class="row-actions">
                <RowMenu :label="t('inventory.rowMenu', { name: u.username })" :items="rowMenu(u)" />
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div class="table-footer">
        <PaginationBar numbered :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
      </div>
      <KeyboardHints id="users-keys" />
    </template>
  </section>
</template>
