<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useAllProfiles, useUserList, type UserListQuery } from "../../api/admin";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { useDebounced, useDocumentTitle } from "../../lib/composables";
import { formatRelative } from "../../lib/format";
import { useListQuery } from "../../lib/listQuery";
import { parseSignInStatus, SIGN_IN_STATUSES, signInStatusLabel } from "../../lib/people";
import { t } from "../../i18n";
import SortIcon from "../../components/SortIcon.vue";

/** Administration › Users. Search, filters, sort and page live in the URL; the API filters and pages. */
useDocumentTitle("Users");
type SortField = NonNullable<UserListQuery["sort"]>;

const COLUMNS: { key: string; label: string; sort?: string }[] = [
  { key: "username", label: "Username", sort: "username" },
  { key: "displayName", label: "Display name", sort: "displayName" },
  { key: "email", label: "Email" },
  { key: "person", label: t("people.users.col.person") },
  { key: "profiles", label: "Permission profiles" },
  { key: "status", label: "Status" },
  { key: "signIn", label: "Signs in with" },
  { key: "mfa", label: "Two-factor" },
  { key: "lastLogin", label: "Last sign-in", sort: "lastLoginAt" },
  { key: "created", label: "Created", sort: "createdAt" },
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
// Set by the user page after a delete.
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

function clearFilters() {
  qText.value = "";
  update({ q: undefined, isActive: undefined, profileId: undefined, signInStatus: undefined });
}
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'Users' }]" />
  <div class="page-header">
    <div class="title">
      <h1>Users</h1>
      <span v-if="list.data.value" class="muted">{{ total.toLocaleString() }} total</span>
      <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" aria-label="Refreshing" />
    </div>
    <div class="actions">
      <RouterLink class="btn btn-primary" to="/admin/users/new">+ New user</RouterLink>
    </div>
  </div>


  <section class="panel" aria-label="Users">
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="u-q">Search</label>
        <input id="u-q" v-model="qText" type="search" placeholder="Username, name, email…" />
      </div>
      <div class="field">
        <label for="u-status">Status</label>
        <select id="u-status" :value="get('isActive')" @change="update({ isActive: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Any status</option>
          <option value="true">Active</option>
          <option value="false">Disabled</option>
        </select>
      </div>
      <div class="field">
        <label for="u-profile">Profile</label>
        <select id="u-profile" :value="get('profileId')" @change="update({ profileId: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Any profile</option>
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
      <button v-if="filtered" type="button" class="btn" @click="clearFilters">Clear filters</button>
    </form>

    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <LoadingState v-if="list.isLoading.value" label="Loading users…" />
    <EmptyState v-if="list.data.value && total === 0" :title="filtered ? 'No users match these filters' : 'No users yet'">
      {{ filtered ? "Adjust or clear the filters above." : "Create a user and give them a permission profile." }}
    </EmptyState>
    <EmptyState v-if="list.data.value && total > 0 && rows.length === 0" title="This page is past the end of the results">
      <template #actions><button class="btn" @click="update({})">Go to first page</button></template>
    </EmptyState>

    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th v-for="c in COLUMNS" :key="c.key" scope="col" :aria-sort="c.sort ? lq.ariaSort(c.sort) : undefined">
                <button v-if="c.sort" type="button" class="sort" @click="lq.toggleSort(c.sort)">
                  {{ c.label }} <SortIcon :dir="lq.ariaSort(c.sort)" />
                </button>
                <template v-else>{{ c.label }}</template>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="u in rows" :key="u.id" :class="{ disabled: !u.isActive }">
              <td><RouterLink :to="`/admin/users/${u.id}`">{{ u.username }}</RouterLink></td>
              <td>{{ u.displayName }}</td>
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
                <span v-if="u.profiles.length === 0" class="muted">None — cannot see any CI</span>
                <template v-for="(p, i) in u.profiles" :key="p.id"><template v-if="i > 0">, </template>{{ p.name }}</template>
              </td>
              <td>
                <span v-if="u.isActive" class="badge ok">Active</span>
                <span v-else class="badge off">Disabled</span>
              </td>
              <td>
                <span v-if="u.identityProvider" class="badge" :title="`Account of ${u.identityProvider.name}: name, e-mail and profiles come from it`">
                  {{ u.identityProvider.name }}
                </span>
                <span v-else class="muted">Local password</span>
              </td>
              <td>
                <span v-if="u.mfaEnabled" class="badge ok">On</span>
                <span v-else class="muted">Off</span>
              </td>
              <td :title="u.lastLoginAt ?? undefined">
                <template v-if="u.lastLoginAt">{{ formatRelative(u.lastLoginAt) }}</template><span v-else class="muted">Never</span>
              </td>
              <td :title="u.createdAt">{{ formatRelative(u.createdAt) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
    </template>
  </section>
</template>
