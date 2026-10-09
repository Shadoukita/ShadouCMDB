<script setup lang="ts">
import { adminCrumbs } from "./sections";
import { computed, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { useApiTokenList, useRevokeApiToken, useUserList, type ApiToken, type ApiTokenListQuery } from "../../api/admin";
import { MAX_PAGE } from "../../api/queries";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import ConfirmDialog from "../../components/ConfirmDialog.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import KeyboardHints from "../../components/KeyboardHints.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import RowMenu, { type RowMenuItem } from "../../components/RowMenu.vue";
import SkeletonRows from "../../components/SkeletonRows.vue";
import { formatNumber, t, type MessageKey } from "../../i18n";
import { useDebounced, useDocumentTitle } from "../../lib/composables";
import { formatDate, formatDateTime, formatRelative } from "../../lib/format";
import { onRowKeydown } from "../../lib/rowKeyboard";
import { useFlashStore } from "../../stores/flash";
import { useListQuery } from "../../lib/listQuery";
import CreateApiTokenDialog from "./CreateApiTokenDialog.vue";
import SortIcon from "../../components/SortIcon.vue";

/**
 * Administration › API tokens, an explorer list (design §2.7). Search, filters, sort and page live in the URL;
 * the API filters and pages. A token's secret is only ever shown by the create dialog; this list knows tokens
 * by name and prefix. Revoke sits in the row menu and asks first.
 */
useDocumentTitle(() => t("admin.section.apiTokens"));
type SortField = NonNullable<ApiTokenListQuery["sort"]>;
type Status = NonNullable<ApiTokenListQuery["status"]>;
const STATUSES: { value: Status; label: string; badge: string }[] = [
  { value: "active", label: t("common.active"), badge: "ok" },
  { value: "expired", label: t("admin.tokens.status.expired"), badge: "off" },
  { value: "revoked", label: t("admin.tokens.status.revoked"), badge: "danger" },
];

const COLUMNS: { key: string; label: MessageKey; sort?: string }[] = [
  { key: "name", label: "admin.tokens.col.name", sort: "name" },
  { key: "prefix", label: "admin.tokens.col.prefix" },
  { key: "owner", label: "admin.tokens.col.owner" },
  { key: "profile", label: "admin.tokens.col.profile" },
  { key: "status", label: "admin.col.status" },
  { key: "expires", label: "admin.tokens.col.expires", sort: "expiresAt" },
  { key: "lastUsed", label: "admin.tokens.col.lastUsed", sort: "lastUsedAt" },
  { key: "created", label: "common.created", sort: "createdAt" },
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

const REFUSED_TITLE = t("admin.tokens.refusedTitle");
const pastEnd = computed(() => !!list.data.value && total.value > 0 && rows.value.length === 0);

const creating = ref(false);
const statusBadge = (s: Status) => STATUSES.find((x) => x.value === s)!;

// ---------- Revoke ----------
const revoke = useRevokeApiToken();
const revoking = ref<ApiToken | null>(null);
const flash = useFlashStore();

function askRevoke(tok: ApiToken) {
  revoke.reset();
  revoking.value = tok;
}

function confirmRevoke() {
  const tok = revoking.value;
  if (!tok) return;
  revoke.mutate(tok.id, {
    onSuccess: () => {
      flash.show(t("admin.tokens.revoked", { name: tok.name, owner: tok.username }));
      revoking.value = null;
    },
  });
}

function revokedTitle(tok: ApiToken): string | undefined {
  if (tok.status !== "revoked") return undefined;
  return tok.revokedBy
    ? t("admin.tokens.revokedAtBy", { at: formatDateTime(tok.revokedAt), by: tok.revokedBy })
    : t("admin.tokens.revokedAt", { at: formatDateTime(tok.revokedAt) });
}

function lastUsedTitle(tok: ApiToken): string | undefined {
  if (!tok.lastUsedAt) return undefined;
  return tok.lastUsedIp ? t("admin.tokens.lastUsedFrom", { at: formatDateTime(tok.lastUsedAt), ip: tok.lastUsedIp }) : formatDateTime(tok.lastUsedAt);
}

const rowMenu = (tok: ApiToken): RowMenuItem[] => [
  { label: t("admin.tokens.row.owner"), to: `/admin/users/${tok.userId}` },
  ...(tok.profile ? [{ label: t("admin.tokens.row.profile"), to: `/admin/profiles/${tok.profile.id}` }] : []),
  ...(tok.status === "active" ? [{ label: t("admin.tokens.row.revoke"), action: () => askRevoke(tok), danger: true }] : []),
];
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="adminCrumbs('api-tokens')" />
    <div class="page-header">
      <div class="title">
        <h1>{{ t("admin.section.apiTokens") }}</h1>
        <span v-if="list.data.value" class="count mono">{{ t("common.total", { n: formatNumber(total) }) }}</span>
        <span v-if="list.isFetching.value && !list.isPending.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div class="actions">
        <button type="button" class="btn btn-primary" @click="creating = true"><Icon name="plus" />{{ t("admin.tokens.new") }}</button>
      </div>
    </div>
    <p class="page-intro">{{ t("admin.tokens.intro") }}</p>
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="t-q">{{ t("admin.search") }}</label>
        <div class="input-icon">
          <Icon name="search" />
          <input id="t-q" v-model="qText" type="search" :placeholder="t('admin.tokens.searchPlaceholder')" />
        </div>
      </div>
      <div class="field">
        <label for="t-status">{{ t("admin.col.status") }}</label>
        <select id="t-status" :value="status ?? ''" @change="update({ status: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("admin.filter.anyStatus") }}</option>
          <option v-for="s in STATUSES" :key="s.value" :value="s.value">{{ s.label }}</option>
        </select>
      </div>
      <div class="field">
        <label for="t-owner">{{ t("admin.tokens.col.owner") }}</label>
        <select id="t-owner" :value="get('userId')" @change="update({ userId: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("admin.tokens.filter.anyOwner") }}</option>
          <option v-for="u in users.data.value?.data ?? []" :key="u.id" :value="u.id">{{ u.username }}</option>
        </select>
      </div>
      <div class="field">
        <span class="label">{{ t("admin.tokens.filter.mfa") }}</span>
        <label class="checkbox-row" :title="REFUSED_TITLE">
          <input type="checkbox" :checked="refusedOnly" @change="update({ refusedForMfa: ($event.target as HTMLInputElement).checked ? 'true' : undefined })" />
          {{ t("admin.tokens.filter.refusedOnly") }}
        </label>
      </div>
      <button v-if="filtered" type="button" class="btn btn-ghost" @click="clearFilters"><Icon name="x" />{{ t("admin.filter.clear") }}</button>
    </form>
  </div>

  <section class="panel explorer" :aria-label="t('admin.section.apiTokens')">
    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <SkeletonRows v-else-if="list.isPending.value" :label="t('admin.tokens.loading')" />
    <EmptyState v-else-if="total === 0 && filtered" icon="search" :title="t('admin.tokens.noMatch')">
      <template v-if="refusedOnly && !get('q') && !get('status') && !get('userId')">{{ t("admin.tokens.noneRefused") }}</template>
      <template v-else>{{ t("admin.filter.noMatchBody") }}</template>
      <template #actions><button type="button" class="btn" @click="clearFilters">{{ t("admin.filter.clear") }}</button></template>
    </EmptyState>
    <EmptyState v-else-if="total === 0" icon="lock" :title="t('admin.tokens.empty.title')">
      {{ t("admin.tokens.empty.body") }}
      <template #actions>
        <button type="button" class="btn btn-primary" @click="creating = true"><Icon name="plus" />{{ t("admin.tokens.new") }}</button>
      </template>
    </EmptyState>
    <EmptyState v-else-if="pastEnd" :title="t('common.pastEnd')">
      <template #actions><button type="button" class="btn" @click="update({})">{{ t("common.firstPage") }}</button></template>
    </EmptyState>

    <template v-if="rows.length > 0 && !list.isError.value">
      <div class="table-wrap table-scroll" role="region" tabindex="0" :aria-label="t('admin.tokens.table')">
        <table :class="['data', 'list-table', 'token-table', { loading: list.isPlaceholderData.value }]" aria-describedby="tokens-keys">
          <thead>
            <tr>
              <th v-for="c in COLUMNS" :key="c.key" scope="col" :aria-sort="c.sort ? lq.ariaSort(c.sort) : undefined">
                <button v-if="c.sort" type="button" class="sort" @click="lq.toggleSort(c.sort)">
                  {{ t(c.label) }} <SortIcon :dir="lq.ariaSort(c.sort)" />
                </button>
                <template v-else>{{ t(c.label) }}</template>
              </th>
              <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
            </tr>
          </thead>
          <tbody @keydown="onRowKeydown($event)">
            <tr v-for="tok in rows" :key="tok.id" :data-id="tok.id" :class="{ disabled: tok.status !== 'active' }">
              <td dir="auto">{{ tok.name }}</td>
              <td><code>{{ tok.tokenPrefix }}…</code></td>
              <td>
                <span class="name-badges">
                  <RouterLink :to="`/admin/users/${tok.userId}`" class="list-name">{{ tok.username }}</RouterLink>
                  <span v-if="!tok.ownerIsActive" class="badge off" :title="t('admin.tokens.ownerDisabledTitle')">{{ t("admin.tokens.ownerDisabled") }}</span>
                </span>
              </td>
              <td>
                <RouterLink v-if="tok.profile" :to="`/admin/profiles/${tok.profile.id}`" dir="auto">{{ tok.profile.name }}</RouterLink>
                <span v-else class="muted" :title="t('admin.tokens.profileDeletedTitle')">{{ t("admin.tokens.profileDeleted") }}</span>
              </td>
              <td :title="revokedTitle(tok)">
                <span class="name-badges">
                  <span :class="['badge', statusBadge(tok.status).badge]"><span class="status-dot" aria-hidden="true" />{{ statusBadge(tok.status).label }}</span>
                  <span v-if="tok.status === 'revoked' && tok.revokedBy" class="muted">{{ t("admin.tokens.by", { name: tok.revokedBy }) }}</span>
                  <span v-if="tok.refusedForMfa" class="badge danger" :title="REFUSED_TITLE">{{ t("admin.tokens.refused") }}</span>
                </span>
              </td>
              <td><time :datetime="tok.expiresAt" :title="formatDateTime(tok.expiresAt)">{{ formatDate(tok.expiresAt) }}</time></td>
              <td>
                <time v-if="tok.lastUsedAt" :datetime="tok.lastUsedAt" :title="lastUsedTitle(tok)">{{ formatRelative(tok.lastUsedAt) }}</time>
                <span v-else class="muted">{{ t("admin.never") }}</span>
              </td>
              <td>
                <time
                  :datetime="tok.createdAt"
                  :title="tok.createdBy ? t('admin.tokens.createdAtBy', { at: formatDateTime(tok.createdAt), by: tok.createdBy }) : formatDateTime(tok.createdAt)"
                >{{ formatRelative(tok.createdAt) }}</time>
              </td>
              <td class="row-actions">
                <RowMenu :label="t('inventory.rowMenu', { name: tok.name })" :items="rowMenu(tok)" row-focus />
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div class="table-footer">
        <PaginationBar numbered :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
      </div>
      <KeyboardHints id="tokens-keys" />
    </template>
  </section>

  <CreateApiTokenDialog :open="creating" @close="creating = false" />

  <ConfirmDialog
    :open="!!revoking"
    :title="t('admin.tokens.revoke.title', { name: revoking?.name ?? '' })"
    :confirm-label="t('admin.tokens.revoke.confirm')"
    :busy="revoke.isPending.value"
    @cancel="revoking = null"
    @confirm="confirmRevoke"
  >
    <template v-if="revoking">
      <ErrorAlert v-if="revoke.isError.value" :error="revoke.error.value" :title="t('admin.tokens.revoke.failed')" />
      <p>
        {{ t(revoking.profile ? "admin.tokens.revoke.whoProfile" : "admin.tokens.revoke.who", { prefix: `${revoking.tokenPrefix}…`, owner: revoking.username, profile: revoking.profile?.name }) }}
      </p>
      <p>
        {{ revoking.lastUsedAt
          ? t(revoking.lastUsedIp ? "admin.tokens.revoke.effectUsedFrom" : "admin.tokens.revoke.effectUsed", { when: formatRelative(revoking.lastUsedAt), ip: revoking.lastUsedIp })
          : t("admin.tokens.revoke.effect") }}
      </p>
    </template>
  </ConfirmDialog>
</template>
