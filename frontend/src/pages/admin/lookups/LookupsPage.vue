<script setup lang="ts">
import { computed } from "vue";
import { RouterLink, useRoute } from "vue-router";
import { useEnvironmentsAdmin, useStatusesAdmin } from "../../../api/datamodel";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import type { FieldSpec } from "../../../components/RecordDialog.vue";
import { useDocumentTitle } from "../../../lib/composables";
import LocationsTable from "./LocationsTable.vue";
import LookupListsPanel from "./LookupListsPanel.vue";
import OrderedLookupTable, { type Row } from "./OrderedLookupTable.vue";
import OwnersTable from "./OwnersTable.vue";

/**
 * Administration › Data model › Lookups: the values CIs pick from. Lists are
 * the administrator's own, used by "Lookup list" attributes (status, environment,
 * owner and location included). The statuses, environments, locations and owners
 * tabs keep the older tables, which CIs no longer refer to. The tab is the path
 * (/admin/lookups/statuses…), so each keeps its own URL state.
 */
const TABS = [
  { kind: "statuses", label: "Statuses" },
  { kind: "environments", label: "Environments" },
  { kind: "locations", label: "Locations" },
  { kind: "owners", label: "Owners" },
  { kind: "lists", label: "Lists" },
] as const;

const route = useRoute();
const kind = computed(() => String(route.params.kind ?? "statuses"));
const tab = computed(() => TABS.find((t) => t.kind === kind.value));
useDocumentTitle(() => `${tab.value?.label ?? "Lookups"} · Lookups`);

const statuses = useStatusesAdmin();
const environments = useEnvironmentsAdmin();

const STATUS_FIELDS: FieldSpec[] = [
  { name: "name", label: "Name", type: "text", required: true, hint: "e.g. In service" },
  { name: "key", label: "Key", type: "key", from: "name" },
  { name: "isOperational", label: "Operational", type: "checkbox", text: "Counts as live (in service, maintenance)" },
  { name: "description", label: "Description", type: "textarea" },
];
const ENVIRONMENT_FIELDS: FieldSpec[] = [
  { name: "name", label: "Name", type: "text", required: true, hint: "e.g. Production" },
  { name: "key", label: "Key", type: "key", from: "name" },
  { name: "description", label: "Description", type: "textarea" },
];
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'Data model' }, { label: 'Lookups', to: '/admin/lookups' }, { label: tab?.label ?? kind }]" />
  <div class="page-header">
    <div class="title"><h1>Lookups</h1></div>
  </div>
  <nav class="tabs" aria-label="Lookups">
    <RouterLink v-for="t in TABS" :key="t.kind" :to="`/admin/lookups/${t.kind}`" :aria-current="t.kind === kind ? 'page' : undefined">{{ t.label }}</RouterLink>
  </nav>

  <div v-if="tab && kind !== 'lists'" class="alert alert-warn" role="note">
    Configuration items no longer use these {{ tab.label.toLowerCase() }}: status, environment, owner and location are lookup
    attributes of the CI classes, and their values are kept under <RouterLink to="/admin/lookups/lists">Lists</RouterLink>.
    This table is kept for older data.
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
    :fields="STATUS_FIELDS"
    :columns="[{ key: 'isOperational', label: 'Operational' }]"
    empty-hint="Statuses of CIs are now values of the status lookup list."
  >
    <template #cell="{ row }"><span v-if="row.isOperational" class="badge ok">Operational</span></template>
    <template #empty><RouterLink class="btn" to="/admin/templates">Install the IT infrastructure starter</RouterLink></template>
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
    :fields="ENVIRONMENT_FIELDS"
    empty-hint="Environments separate production from test, staging and development."
  />
  <LocationsTable v-else-if="kind === 'locations'" />
  <OwnersTable v-else-if="kind === 'owners'" />
  <LookupListsPanel v-else-if="kind === 'lists'" />
  <div v-else class="alert alert-error" role="alert">
    There is no lookup called <code>{{ kind }}</code>. <RouterLink to="/admin/lookups/statuses">Open statuses</RouterLink>.
  </div>
</template>
