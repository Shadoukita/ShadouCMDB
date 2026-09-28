<script setup lang="ts">
import { computed } from "vue";
import { RouterLink, useRoute } from "vue-router";
import { useEnvironmentsAdmin, useLookupLists, useStatusesAdmin } from "../../../api/datamodel";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import { useDocumentTitle } from "../../../lib/composables";
import LocationsTable from "./LocationsTable.vue";
import OrderedLookupTable, { type Row } from "./OrderedLookupTable.vue";
import OwnersTable from "./OwnersTable.vue";

/**
 * Administration › Data model › Lookups: the older status, environment, location and
 * owner tables, which CIs no longer refer to (migration 0016). Read only, for history:
 * status, environment, owner and location are lookup attributes of the CI classes, and
 * their values are edited under Dropdowns, in the list each tab links to. The tab is
 * the path (/admin/lookups/statuses…), so each keeps its own URL state.
 */
const TABS = [
  { kind: "statuses", label: "Statuses", list: "status" },
  { kind: "environments", label: "Environments", list: "environment" },
  { kind: "locations", label: "Locations", list: "location" },
  { kind: "owners", label: "Owners", list: "owner" },
] as const;

const route = useRoute();
const kind = computed(() => String(route.params.kind ?? "statuses"));
const tab = computed(() => TABS.find((t) => t.kind === kind.value));
useDocumentTitle(() => `${tab.value?.label ?? "Lookups"} · Lookups`);

const statuses = useStatusesAdmin();
const environments = useEnvironmentsAdmin();

/** The Dropdowns list that replaced the tab's table: the starter template's key, else Dropdowns itself. */
const lists = useLookupLists();
const replacement = computed(() => lists.data.value?.find((l) => l.key === tab.value?.list));
const replacementTo = computed(() => (replacement.value ? { path: "/admin/dropdowns", query: { list: replacement.value.id } } : "/admin/dropdowns"));
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'Data model' }, { label: 'Lookups', to: '/admin/lookups' }, { label: tab?.label ?? kind }]" />
  <div class="page-header">
    <div class="title"><h1>Lookups</h1></div>
  </div>
  <nav class="tabs" aria-label="Lookups">
    <RouterLink v-for="t in TABS" :key="t.kind" :to="`/admin/lookups/${t.kind}`" :aria-current="t.kind === kind ? 'page' : undefined">{{ t.label }}</RouterLink>
  </nav>

  <div v-if="tab" class="alert alert-warn" role="note">
    Read only. Configuration items no longer use these {{ tab.label.toLowerCase() }}: status, environment, owner and location
    are lookup attributes of the CI classes, and their values are edited under Dropdowns. This table is kept for older data.
    <RouterLink :to="replacementTo">{{ replacement ? `Edit the “${replacement.name}” list under Dropdowns` : "Open Dropdowns" }}</RouterLink>.
  </div>
  <OrderedLookupTable
    v-if="kind === 'statuses'"
    resource="statuses"
    noun="status"
    title="Statuses"
    :rows="statuses.data.value?.data as Row[] | undefined"
    :loading="statuses.isLoading.value"
    :error="statuses.error.value"
    :refetch="() => statuses.refetch()"
    :columns="[{ key: 'isOperational', label: 'Operational' }]"
    empty-hint="The former statuses table has no rows."
    readonly
  >
    <template #cell="{ row }"><span v-if="row.isOperational" class="badge ok">Operational</span></template>
  </OrderedLookupTable>
  <OrderedLookupTable
    v-else-if="kind === 'environments'"
    resource="environments"
    noun="environment"
    title="Environments"
    :rows="environments.data.value?.data as Row[] | undefined"
    :loading="environments.isLoading.value"
    :error="environments.error.value"
    :refetch="() => environments.refetch()"
    empty-hint="The former environments table has no rows."
    readonly
  />
  <LocationsTable v-else-if="kind === 'locations'" />
  <OwnersTable v-else-if="kind === 'owners'" />
  <div v-else class="alert alert-error" role="alert">
    There is no lookup called <code>{{ kind }}</code>. <RouterLink to="/admin/lookups/statuses">Open statuses</RouterLink>.
  </div>
</template>
