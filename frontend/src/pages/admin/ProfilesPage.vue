<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useProfileList, type PermissionProfile, type ProfileListQuery } from "../../api/admin";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { useDebounced, useDocumentTitle } from "../../lib/composables";
import { formatRelative } from "../../lib/format";
import { useListQuery } from "../../lib/listQuery";
import { useSessionStore } from "../../stores/session";
import CloneProfileDialog from "./CloneProfileDialog.vue";
import { summarise } from "./profileSummary";

/** Administration › Permission profiles. Users with only users.manage see the list read-only. */
useDocumentTitle("Permission profiles");
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
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'Permission profiles' }]" />
  <div class="page-header">
    <div class="title">
      <h1>Permission profiles</h1>
      <span v-if="list.data.value" class="muted">{{ total.toLocaleString() }} total</span>
    </div>
    <div v-if="canManage" class="actions">
      <RouterLink class="btn btn-primary" to="/admin/profiles/new">+ New profile</RouterLink>
    </div>
  </div>
  <p class="muted" style="margin-top: 0">
    A profile grants global rights and view/create/edit/delete rights per CI class. A user may do what any of their
    profiles allows.
  </p>

  <section class="panel" aria-label="Permission profiles">
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="p-q">Search</label>
        <input id="p-q" v-model="qText" type="search" placeholder="Name or description…" />
      </div>
      <div class="field">
        <label for="p-sort">Sort</label>
        <select id="p-sort" :value="lq.sort.value" @change="update({ sort: ($event.target as HTMLSelectElement).value })">
          <option value="name">Name</option>
          <option value="-updatedAt">Recently changed</option>
          <option value="-createdAt">Newest</option>
        </select>
      </div>
    </form>

    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <LoadingState v-if="list.isLoading.value" label="Loading profiles…" />
    <EmptyState v-if="list.data.value && total === 0" title="No profiles match">Clear the search above.</EmptyState>

    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Grants</th>
              <th scope="col" class="num">Users</th>
              <th scope="col">Updated</th>
              <th v-if="canManage" scope="col"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="p in rows" :key="p.id">
              <td :title="p.description ?? undefined">
                <RouterLink :to="`/admin/profiles/${p.id}`">{{ p.name }}</RouterLink>
                <span v-if="p.isBuiltin" class="badge" style="margin-left: 6px">Built-in</span>
                <span v-if="p.requireMfa" class="badge warn" style="margin-left: 6px" title="Holders must set up two-factor authentication">Two-factor required</span>
              </td>
              <td :title="summarise(p)">{{ summarise(p) }}</td>
              <td class="num">
                <RouterLink v-if="session.can('users.manage') && p.userCount > 0" :to="{ path: '/admin/users', query: { profileId: p.id } }">
                  {{ p.userCount }}
                </RouterLink>
                <template v-else>{{ p.userCount }}</template>
              </td>
              <td :title="p.updatedAt">{{ formatRelative(p.updatedAt) }}</td>
              <td v-if="canManage" class="num">
                <button type="button" class="btn-link" :aria-label="`Clone ${p.name}`" @click="cloning = p">Clone</button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
    </template>
  </section>
  <CloneProfileDialog :profile="cloning" @close="cloning = null" />
</template>
