<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useApiTokenList, useRevokeApiToken, useUserList, type ApiToken, type ApiTokenListQuery } from "../../api/admin";
import { MAX_PAGE } from "../../api/queries";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { useDebounced, useDocumentTitle } from "../../lib/composables";
import { formatDate, formatDateTime, formatRelative } from "../../lib/format";
import { useListQuery } from "../../lib/listQuery";
import CreateApiTokenDialog from "./CreateApiTokenDialog.vue";

/**
 * Administration › API tokens. Search, filters, sort and page live in the URL;
 * the API filters and pages. A token's secret is only ever shown by the create
 * dialog; this list knows tokens by name and prefix.
 */
useDocumentTitle("API tokens");
type SortField = NonNullable<ApiTokenListQuery["sort"]>;
type Status = NonNullable<ApiTokenListQuery["status"]>;
const STATUSES: { value: Status; label: string; badge: string }[] = [
  { value: "active", label: "Active", badge: "ok" },
  { value: "expired", label: "Expired", badge: "off" },
  { value: "revoked", label: "Revoked", badge: "danger" },
];

const COLUMNS: { key: string; label: string; sort?: string }[] = [
  { key: "name", label: "Name", sort: "name" },
  { key: "prefix", label: "Prefix" },
  { key: "owner", label: "Owner" },
  { key: "profile", label: "Profile" },
  { key: "status", label: "Status" },
  { key: "expires", label: "Expires", sort: "expiresAt" },
  { key: "lastUsed", label: "Last used", sort: "lastUsedAt" },
  { key: "created", label: "Created", sort: "createdAt" },
  { key: "actions", label: "" },
];

const lq = useListQuery({ sort: "-createdAt" });
const { get, limit, offset, update } = lq;
const status = computed(() => STATUSES.find((s) => s.value === get("status"))?.value);
/** Only the working tokens refused because their owner must use two-factor authentication (GH#200). */
const refusedOnly = computed(() => get("refusedForMfa") === "true");
const query = computed<ApiTokenListQuery>(() => ({
  q: get("q") || undefined,
  status: status.value,
  refusedForMfa: refusedOnly.value ? "true" : undefined,
  userId: get("userId") || undefined,
  sort: lq.sort.value as SortField,
  limit: limit.value,
  offset: offset.value,
}));
const list = useApiTokenList(query);
const users = useUserList({ sort: "username", limit: MAX_PAGE });

const qText = ref(get("q"));
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => v !== get("q") && update({ q: v || undefined }));
watch(
  () => get("q"),
  (v) => (qText.value = v),
);

const filtered = computed(() => !!(get("q") || get("status") || get("userId") || refusedOnly.value));
const total = computed(() => list.data.value?.page.total ?? 0);
const rows = computed(() => list.data.value?.data ?? []);

function clearFilters() {
  qText.value = "";
  update({ q: undefined, status: undefined, userId: undefined, refusedForMfa: undefined });
}

const REFUSED_TITLE =
  "The owner must use two-factor authentication, and this token was not created from a session signed in with a second factor, " +
  "so every request with it is refused. Create a new token from a session signed in with a second factor, then revoke this one.";

const creating = ref(false);
const statusBadge = (s: Status) => STATUSES.find((x) => x.value === s)!;

// ---------- Revoke ----------
const revoke = useRevokeApiToken();
const revoking = ref<ApiToken | null>(null);
const notice = ref("");

function askRevoke(t: ApiToken) {
  revoke.reset();
  revoking.value = t;
}

function confirmRevoke() {
  const t = revoking.value;
  if (!t) return;
  revoke.mutate(t.id, {
    onSuccess: () => {
      notice.value = `Revoked API token ${t.name} (owner ${t.username}).`;
      revoking.value = null;
    },
  });
}

function revokedTitle(t: ApiToken): string | undefined {
  if (t.status !== "revoked") return undefined;
  return `Revoked ${formatDateTime(t.revokedAt)}${t.revokedBy ? ` by ${t.revokedBy}` : ""}`;
}
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'API tokens' }]" />
  <div class="page-header">
    <div class="title">
      <h1>API tokens</h1>
      <span v-if="list.data.value" class="muted">{{ total.toLocaleString() }} total</span>
      <span v-if="list.isFetching.value && !list.isLoading.value" class="spinner" aria-label="Refreshing" />
    </div>
    <div class="actions">
      <button type="button" class="btn btn-primary" @click="creating = true">+ New API token</button>
    </div>
  </div>
  <div v-if="notice" class="alert" role="status">{{ notice }}</div>

  <section class="panel" aria-label="API tokens">
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="t-q">Search</label>
        <input id="t-q" v-model="qText" type="search" placeholder="Name, prefix, owner…" />
      </div>
      <div class="field">
        <label for="t-status">Status</label>
        <select id="t-status" :value="status ?? ''" @change="update({ status: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Any status</option>
          <option v-for="s in STATUSES" :key="s.value" :value="s.value">{{ s.label }}</option>
        </select>
      </div>
      <div class="field">
        <label for="t-owner">Owner</label>
        <select id="t-owner" :value="get('userId')" @change="update({ userId: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Any owner</option>
          <option v-for="u in users.data.value?.data ?? []" :key="u.id" :value="u.id">{{ u.username }}</option>
        </select>
      </div>
      <label class="checkbox-row" :title="REFUSED_TITLE">
        <input type="checkbox" :checked="refusedOnly" @change="update({ refusedForMfa: ($event.target as HTMLInputElement).checked ? 'true' : undefined })" />
        Refused for two-factor only
      </label>
      <button v-if="filtered" type="button" class="btn" @click="clearFilters">Clear filters</button>
    </form>

    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <LoadingState v-if="list.isLoading.value" label="Loading API tokens…" />
    <EmptyState v-if="list.data.value && total === 0" :title="filtered ? 'No API tokens match these filters' : 'No API tokens yet'">
      <template v-if="refusedOnly && !get('q') && !get('status') && !get('userId')">
        No working token is refused for two-factor authentication: every owner who must use a second factor has tokens created from a
        session signed in with one.
      </template>
      <template v-else-if="filtered">Adjust or clear the filters above.</template>
      <template v-else>
        A token lets a script or integration call the API as its owner, limited to a permission profile. Its secret is shown once, when
        you create it.
      </template>
      <template v-if="!filtered" #actions>
        <button type="button" class="btn btn-primary" @click="creating = true">+ New API token</button>
      </template>
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
                  {{ c.label }} {{ lq.sortIndicator(c.sort) }}
                </button>
                <template v-else-if="c.label">{{ c.label }}</template>
                <span v-else class="sr-only">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="t in rows" :key="t.id" :class="{ disabled: t.status !== 'active' }">
              <td>{{ t.name }}</td>
              <td><code>{{ t.tokenPrefix }}…</code></td>
              <td>
                <RouterLink :to="`/admin/users/${t.userId}`">{{ t.username }}</RouterLink>
                <span v-if="!t.ownerIsActive" class="badge off" title="A disabled owner's tokens are refused"> Owner disabled</span>
              </td>
              <td>
                <RouterLink v-if="t.profile" :to="`/admin/profiles/${t.profile.id}`">{{ t.profile.name }}</RouterLink>
                <span v-else class="muted" title="The profile was deleted; the token is refused">Profile deleted</span>
              </td>
              <td :title="revokedTitle(t)">
                <span :class="['badge', statusBadge(t.status).badge]">{{ statusBadge(t.status).label }}</span>
                <span v-if="t.status === 'revoked' && t.revokedBy" class="muted"> by {{ t.revokedBy }}</span>
                <span v-if="t.refusedForMfa" class="badge danger" :title="REFUSED_TITLE"> Refused: owner requires MFA</span>
              </td>
              <td :title="formatDateTime(t.expiresAt)">{{ formatDate(t.expiresAt) }}</td>
              <td :title="t.lastUsedAt ? `${formatDateTime(t.lastUsedAt)}${t.lastUsedIp ? ` from ${t.lastUsedIp}` : ''}` : undefined">
                <template v-if="t.lastUsedAt">{{ formatRelative(t.lastUsedAt) }}</template>
                <span v-else class="muted">Never</span>
              </td>
              <td :title="`${formatDateTime(t.createdAt)}${t.createdBy ? ` by ${t.createdBy}` : ''}`">{{ formatRelative(t.createdAt) }}</td>
              <td class="row-actions">
                <button v-if="t.status === 'active'" type="button" class="btn btn-sm btn-quiet-danger" :aria-label="`Revoke ${t.name}`" @click="askRevoke(t)">
                  Revoke
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
    </template>
  </section>

  <CreateApiTokenDialog :open="creating" @close="creating = false" />

  <ConfirmDialog
    :open="!!revoking"
    :title="`Revoke API token “${revoking?.name ?? ''}”?`"
    confirm-label="Revoke token"
    :busy="revoke.isPending.value"
    @cancel="revoking = null"
    @confirm="confirmRevoke"
  >
    <template v-if="revoking">
      <ErrorAlert v-if="revoke.isError.value" :error="revoke.error.value" title="Not revoked" />
      <p>
        Token <code>{{ revoking.tokenPrefix }}…</code>, owned by <strong>{{ revoking.username }}</strong>
        <template v-if="revoking.profile"> with profile {{ revoking.profile.name }}</template>.
      </p>
      <p>
        Every script or integration that sends it is refused from now on
        <template v-if="revoking.lastUsedAt">(last used {{ formatRelative(revoking.lastUsedAt) }}<template v-if="revoking.lastUsedIp"> from {{ revoking.lastUsedIp }}</template>)</template>.
        This cannot be undone; the token stays listed as revoked.
      </p>
    </template>
  </ConfirmDialog>
</template>
