<script setup lang="ts">
import { computed, ref } from "vue";
import { useAdminDelegations, useMyDelegations, type AdminDelegationQuery, type MyDelegationQuery } from "../../api/approvals";
import Breadcrumbs from "../../components/Breadcrumbs.vue";
import EmptyState from "../../components/EmptyState.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import Icon from "../../components/Icon.vue";
import LoadingState from "../../components/LoadingState.vue";
import PaginationBar from "../../components/PaginationBar.vue";
import { formatNumber, t } from "../../i18n";
import { useDocumentTitle } from "../../lib/composables";
import { useListQuery } from "../../lib/listQuery";
import { adminCrumbs } from "../admin/sections";
import DelegationDialog from "./DelegationDialog.vue";
import DelegationTable from "./DelegationTable.vue";

/**
 * Approval delegations. "My delegations" (`admin` false, under the user menu): the delegations of your approvals,
 * including those an administrator made for you, and those that let you decide for someone. The admin page
 * (users.manage): every delegation, to set one up for an absent approver or end one early. Your side, current or
 * past, and the page live in the URL; the API filters and pages.
 */
const props = defineProps<{ admin: boolean }>();
const title = computed(() => (props.admin ? t("delegations.adminTitle") : t("delegations.title")));
useDocumentTitle(() => title.value);
const lq = useListQuery({ sort: "" });
const { get, limit, offset, update } = lq;
const active = computed(() => (get("active") === "true" || get("active") === "false" ? (get("active") as "true" | "false") : undefined));
const role = computed(() => (get("role") === "principal" || get("role") === "delegate" ? (get("role") as "principal" | "delegate") : undefined));

// Only the page's own list is fetched: the admin one needs users.manage.
const mine = useMyDelegations(
  computed<MyDelegationQuery>(() => ({ role: role.value, active: active.value, limit: limit.value, offset: offset.value })),
  () => !props.admin,
);
const all = useAdminDelegations(
  computed<AdminDelegationQuery>(() => ({ active: active.value, limit: limit.value, offset: offset.value })),
  () => props.admin,
);
const list = computed(() => (props.admin ? all : mine));
const rows = computed(() => list.value.data.value?.data ?? []);
const total = computed(() => list.value.data.value?.page.total ?? 0);
const filtered = computed(() => !!(active.value || role.value));
const creating = ref(false);
</script>

<template>
  <div class="list-head">
    <Breadcrumbs :items="admin ? adminCrumbs('approval-delegations') : [{ label: t('account.title'), to: '/account' }, { label: title }]" />
    <div class="page-header">
      <div class="title">
        <h1>{{ title }}</h1>
        <span v-if="list.data.value" class="count mono">{{ t("common.total", { n: formatNumber(total) }) }}</span>
        <span v-if="list.isFetching.value && !list.isPending.value" class="spinner" :aria-label="t('common.refreshing')" />
      </div>
      <div class="actions">
        <button type="button" class="btn btn-primary" data-testid="delegation-new" @click="creating = true">
          <Icon name="plus" />{{ admin ? t("delegations.new.adminAction") : t("delegations.new.action") }}
        </button>
      </div>
    </div>
    <p class="page-intro">{{ admin ? t("delegations.adminIntro") : t("delegations.intro") }}</p>
  </div>

  <section class="panel" :aria-label="title">
    <form class="toolbar" role="search" @submit.prevent>
      <div v-if="!admin" class="field">
        <label for="delegations-role">{{ t("delegations.filter.role") }}</label>
        <select id="delegations-role" :value="get('role')" @change="update({ role: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("delegations.filter.roleAny") }}</option>
          <option value="principal">{{ t("delegations.filter.rolePrincipal") }}</option>
          <option value="delegate">{{ t("delegations.filter.roleDelegate") }}</option>
        </select>
      </div>
      <div class="field">
        <label for="delegations-active">{{ t("delegations.filter.active") }}</label>
        <select id="delegations-active" :value="get('active')" @change="update({ active: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("delegations.filter.activeAny") }}</option>
          <option value="true">{{ t("delegations.filter.activeTrue") }}</option>
          <option value="false">{{ t("delegations.filter.activeFalse") }}</option>
        </select>
      </div>
      <button v-if="filtered" type="button" class="btn" @click="update({ role: undefined, active: undefined })">{{ t("approvals.clearFilters") }}</button>
    </form>

    <div v-if="list.isError.value" class="panel-body">
      <ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" />
    </div>
    <LoadingState v-else-if="list.isLoading.value" :label="t('delegations.loading')" />
    <EmptyState v-else-if="list.data.value && total === 0" :title="filtered ? t('delegations.emptyFiltered') : t('delegations.empty')" data-testid="delegations-empty">
      {{ admin ? t("delegations.emptyAdminBody") : t("delegations.emptyBody") }}
    </EmptyState>
    <template v-else-if="rows.length > 0">
      <DelegationTable :rows="rows" :admin="admin" :loading="list.isPlaceholderData.value" :label="title" />
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
    </template>
  </section>

  <DelegationDialog :open="creating" :admin="admin" @close="creating = false" />
</template>
