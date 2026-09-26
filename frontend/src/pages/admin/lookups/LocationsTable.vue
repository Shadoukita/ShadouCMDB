<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useCreateLookup, useLocationsAdmin, usePatch, type LocationListQuery } from "../../../api/datamodel";
import { useLookup, type Location } from "../../../api/queries";
import DeleteRowButton from "../../../components/DeleteRowButton.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import PaginationBar from "../../../components/PaginationBar.vue";
import RecordDialog, { type FieldSpec } from "../../../components/RecordDialog.vue";
import { useDebounced } from "../../../lib/composables";
import { useListQuery } from "../../../lib/listQuery";

/** Locations: a tree (region › site › building › room › rack). Searched, filtered, sorted and paged by the API; state in the URL. */
const LOCATION_TYPES = [
  { value: "region", label: "Region" },
  { value: "site", label: "Site" },
  { value: "building", label: "Building" },
  { value: "floor", label: "Floor" },
  { value: "room", label: "Room" },
  { value: "rack", label: "Rack" },
  { value: "cloud_region", label: "Cloud region" },
  { value: "other", label: "Other" },
];
const typeLabel = (t: string) => LOCATION_TYPES.find((x) => x.value === t)?.label ?? t;

const lq = useListQuery({ sort: "name" });
const { get, limit, offset, update } = lq;
const query = computed<LocationListQuery>(() => ({
  q: get("q") || undefined,
  locationType: (get("locationType") || undefined) as LocationListQuery["locationType"],
  isActive: get("isActive") === "true" || get("isActive") === "false" ? (get("isActive") as "true" | "false") : undefined,
  sort: lq.sort.value as LocationListQuery["sort"],
  limit: limit.value,
  offset: offset.value,
}));
const list = useLocationsAdmin(query);
/** Every location (up to the API's page limit) for parent names and the parent picker. */
const tree = useLookup("locations");
const nameOf = (id: string | null) => (id ? (tree.data.value?.find((l) => l.id === id)?.name ?? "") : "");
const rows = computed(() => list.data.value?.data ?? []);
const total = computed(() => list.data.value?.page.total ?? 0);
const filtered = computed(() => !!(get("q") || get("locationType") || get("isActive")));

const qText = ref(get("q"));
const debouncedQ = useDebounced(qText, 300);
watch(debouncedQ, (v) => v !== get("q") && update({ q: v || undefined }));
watch(
  () => get("q"),
  (v) => (qText.value = v),
);

const create = useCreateLookup("locations");
const patch = usePatch<Location>("locations");
const notice = ref<string | null>(null);
const dialogOpen = ref(false);
const editing = ref<Location | null>(null);

const fields = computed<FieldSpec[]>(() => {
  const exclude = editing.value?.id;
  return [
    { name: "name", label: "Name", type: "text", required: true },
    { name: "key", label: "Key", type: "key", from: "name" },
    { name: "locationType", label: "Type", type: "select", required: true, options: LOCATION_TYPES },
    {
      name: "parentId",
      label: "Inside",
      type: "select",
      options: [
        { value: "", label: "— top level —" },
        ...(tree.data.value ?? []).filter((l) => l.id !== exclude).map((l) => ({ value: l.id, label: l.name, depth: l.depth })),
      ],
    },
    { name: "address", label: "Address", type: "text", wide: true },
    { name: "description", label: "Description", type: "textarea" },
  ];
});

function open(l: Location | null) {
  editing.value = l;
  dialogOpen.value = true;
}
async function save(body: Record<string, unknown>, isNew: boolean): Promise<string> {
  if (isNew) {
    const created = (await create.mutateAsync(body)) as { name: string };
    return `Added location ${created.name}.`;
  }
  const saved = await patch.mutateAsync({ id: editing.value!.id, body });
  return `Saved location ${saved.name}.`;
}
function setActive(l: Location, isActive: boolean) {
  notice.value = null;
  patch.mutate(
    { id: l.id, body: { isActive } },
    { onSuccess: () => (notice.value = isActive ? `Restored ${l.name}.` : `Archived ${l.name}: CIs keep it, but it can no longer be chosen.`) },
  );
}
</script>

<template>
  <section class="panel" aria-label="Locations">
    <div class="panel-header">
      <h2>Locations</h2>
      <span v-if="list.data.value" class="muted">{{ total.toLocaleString() }}</span>
      <span v-if="patch.isPending.value || (list.isFetching.value && !list.isLoading.value)" class="spinner" aria-label="Refreshing" />
      <button type="button" class="btn btn-primary btn-sm" style="margin-left: auto" @click="open(null)">+ Add location</button>
    </div>
    <form class="toolbar" role="search" @submit.prevent>
      <div class="field search">
        <label for="loc-q">Search</label>
        <input id="loc-q" v-model="qText" type="search" placeholder="Name, key, address…" />
      </div>
      <div class="field">
        <label for="loc-type">Type</label>
        <select id="loc-type" :value="get('locationType')" @change="update({ locationType: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Any type</option>
          <option v-for="t in LOCATION_TYPES" :key="t.value" :value="t.value">{{ t.label }}</option>
        </select>
      </div>
      <div class="field">
        <label for="loc-active">Status</label>
        <select id="loc-active" :value="get('isActive')" @change="update({ isActive: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">Any status</option>
          <option value="true">Active</option>
          <option value="false">Archived</option>
        </select>
      </div>
      <button v-if="filtered" type="button" class="btn" @click="(qText = ''), update({ q: undefined, locationType: undefined, isActive: undefined })">Clear filters</button>
    </form>
    <div v-if="notice || patch.isError.value" class="panel-body">
      <div v-if="notice" class="alert" role="status">{{ notice }}</div>
      <ErrorAlert v-if="patch.isError.value" :error="patch.error.value" title="Not saved" />
    </div>
    <div v-if="list.isError.value" class="panel-body"><ErrorAlert :error="list.error.value" :on-retry="() => list.refetch()" /></div>
    <LoadingState v-if="list.isLoading.value" />
    <EmptyState v-if="list.data.value && total === 0" :title="filtered ? 'No locations match these filters' : 'No locations yet'">
      {{ filtered ? "Adjust or clear the filters above." : "Locations say where CIs are: regions, sites, buildings, rooms and racks." }}
      <template v-if="!filtered" #actions><button type="button" class="btn btn-primary" @click="open(null)">+ Add location</button></template>
    </EmptyState>
    <template v-if="rows.length > 0">
      <div class="table-wrap">
        <table :class="['data', { loading: list.isPlaceholderData.value }]">
          <thead>
            <tr>
              <th scope="col" :aria-sort="lq.ariaSort('name')"><button type="button" class="sort" @click="lq.toggleSort('name')">Name {{ lq.sortIndicator("name") }}</button></th>
              <th scope="col" :aria-sort="lq.ariaSort('key')"><button type="button" class="sort" @click="lq.toggleSort('key')">Key {{ lq.sortIndicator("key") }}</button></th>
              <th scope="col">Type</th>
              <th scope="col">Inside</th>
              <th scope="col">Address</th>
              <th scope="col">Status</th>
              <th scope="col"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="l in rows" :key="l.id" :class="{ disabled: !l.isActive }">
              <td><button type="button" class="btn-link" @click="open(l)">{{ l.name }}</button></td>
              <td class="mono">{{ l.key }}</td>
              <td>{{ typeLabel(l.locationType) }}</td>
              <td>{{ nameOf(l.parentId) }}</td>
              <td class="muted">{{ l.address ?? "" }}</td>
              <td>
                <span v-if="l.isActive" class="badge ok">Active</span>
                <span v-else class="badge off">Archived</span>
              </td>
              <td class="row-actions">
                <button type="button" class="btn btn-sm" :aria-label="`Edit ${l.name}`" @click="open(l)">Edit</button>
                <button v-if="l.isActive" type="button" class="btn btn-sm" :disabled="patch.isPending.value" :aria-label="`Archive ${l.name}`" @click="setActive(l, false)">Archive</button>
                <button v-else type="button" class="btn btn-sm" :disabled="patch.isPending.value" :aria-label="`Restore ${l.name}`" @click="setActive(l, true)">Restore</button>
                <DeleteRowButton
                  resource="locations"
                  :id="l.id"
                  :label="`location “${l.name}”`"
                  archivable
                  :archived="!l.isActive"
                  small
                  @archive="setActive(l, false)"
                  @deleted="notice = `Deleted location ${l.name}.`"
                />
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <PaginationBar :total="total" :limit="limit" :offset="offset" @change="lq.onPage" />
    </template>
  </section>

  <RecordDialog
    :open="dialogOpen"
    :title="editing ? `Edit location “${editing.name}”` : 'New location'"
    :submit-label="editing ? 'Save' : 'Add location'"
    :fields="fields"
    :record="editing"
    :defaults="{ locationType: 'site' }"
    :save="save"
    id-prefix="loc"
    @close="dialogOpen = false"
    @saved="(m) => (notice = m)"
  />
</template>
