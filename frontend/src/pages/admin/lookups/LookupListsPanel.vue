<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useCreateLookupList, useLookupListValues, useLookupLists, usePatch, type LookupList } from "../../../api/datamodel";
import ClassBadge from "../../../components/ClassBadge.vue";
import DeleteRowButton from "../../../components/DeleteRowButton.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import RecordDialog, { type FieldSpec } from "../../../components/RecordDialog.vue";
import { useListQuery } from "../../../lib/listQuery";
import OrderedLookupTable, { type Row } from "./OrderedLookupTable.vue";

/**
 * Admin-defined lookup lists ("Support contract": Gold, Silver, Bronze). A class
 * attribute of type "Lookup list" stores one value of a list. The selected list
 * (?list=…, in the URL) shows its values below.
 */
const lq = useListQuery({ sort: "sortOrder" });
const lists = useLookupLists();
const create = useCreateLookupList();
const patch = usePatch<LookupList>("lookup-lists");
const notice = ref<string | null>(null);

const selectedId = computed(() => lq.get("list") || lists.data.value?.[0]?.id || "");
const selected = computed(() => lists.data.value?.find((l) => l.id === selectedId.value));
const values = useLookupListValues(selectedId);

const LIST_FIELDS: FieldSpec[] = [
  { name: "name", label: "Name", type: "text", required: true, hint: "e.g. Support contract" },
  { name: "key", label: "Key", type: "key", from: "name" },
  { name: "description", label: "Description", type: "textarea" },
];
const VALUE_FIELDS: FieldSpec[] = [
  { name: "name", label: "Name", type: "text", required: true },
  { name: "key", label: "Key", type: "key", from: "name" },
  { name: "color", label: "Colour", type: "color", hint: "Shown next to the value on CI pages" },
  { name: "description", label: "Description", type: "textarea" },
];

const dialogOpen = ref(false);
const editing = ref<LookupList | null>(null);
function open(l: LookupList | null) {
  editing.value = l;
  dialogOpen.value = true;
}
async function save(body: Record<string, unknown>, isNew: boolean): Promise<string> {
  if (isNew) {
    const last = Math.max(0, ...(lists.data.value ?? []).map((l) => l.sortOrder));
    const created = await create.mutateAsync({ ...(body as { key: string; name: string }), sortOrder: last + 10 });
    lq.update({ list: created.id });
    return `Created list ${created.name}. Add its values below, then use it in a “Lookup list” attribute.`;
  }
  const saved = await patch.mutateAsync({ id: editing.value!.id, body });
  return `Saved list ${saved.name}.`;
}
function setActive(l: LookupList, isActive: boolean) {
  notice.value = null;
  patch.mutate(
    { id: l.id, body: { isActive } },
    { onSuccess: () => (notice.value = isActive ? `Restored list ${l.name}.` : `Archived list ${l.name}: it can no longer be chosen for new attributes.`) },
  );
}
</script>

<template>
  <section class="panel" aria-label="Lookup lists">
    <div class="panel-header">
      <h2>Lists</h2>
      <span v-if="lists.data.value" class="muted">{{ lists.data.value.length }}</span>
      <span class="muted">Your own value lists, used by “Lookup list” attributes.</span>
      <button type="button" class="btn btn-primary btn-sm" style="margin-left: auto" @click="open(null)">+ New list</button>
    </div>
    <div v-if="notice || patch.isError.value" class="panel-body">
      <div v-if="notice" class="alert" role="status">{{ notice }}</div>
      <ErrorAlert v-if="patch.isError.value" :error="patch.error.value" title="Not saved" />
    </div>
    <div v-if="lists.isError.value" class="panel-body"><ErrorAlert :error="lists.error.value" :on-retry="() => lists.refetch()" /></div>
    <LoadingState v-else-if="lists.isLoading.value" />
    <EmptyState v-else-if="lists.data.value?.length === 0" title="No lookup lists yet">
      Create a list (e.g. “Support contract” with Gold, Silver and Bronze), then add an attribute of type “Lookup list” to a
      class to let operators pick from it.
      <template #actions><button type="button" class="btn btn-primary" @click="open(null)">+ New list</button></template>
    </EmptyState>
    <div v-else class="table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col">Key</th>
            <th scope="col">Description</th>
            <th scope="col">Status</th>
            <th scope="col"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="l in lists.data.value" :key="l.id" :class="{ disabled: !l.isActive, selected: l.id === selectedId }" :aria-selected="l.id === selectedId">
            <td><RouterLink :to="{ query: { list: l.id } }">{{ l.name }}</RouterLink></td>
            <td class="mono">{{ l.key }}</td>
            <td class="muted">{{ l.description ?? "" }}</td>
            <td>
              <span v-if="l.isActive" class="badge ok">Active</span>
              <span v-else class="badge off">Archived</span>
            </td>
            <td class="row-actions">
              <button type="button" class="btn btn-sm" :aria-label="`Edit list ${l.name}`" @click="open(l)">Edit</button>
              <button v-if="l.isActive" type="button" class="btn btn-sm" :disabled="patch.isPending.value" :aria-label="`Archive ${l.name}`" @click="setActive(l, false)">Archive</button>
              <button v-else type="button" class="btn btn-sm" :disabled="patch.isPending.value" :aria-label="`Restore ${l.name}`" @click="setActive(l, true)">Restore</button>
              <DeleteRowButton
                resource="lookup-lists"
                :id="l.id"
                :label="`list “${l.name}”`"
                archivable
                :archived="!l.isActive"
                small
                @archive="setActive(l, false)"
                @deleted="(notice = `Deleted list ${l.name}.`), lq.update({ list: undefined })"
              />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <OrderedLookupTable
    v-if="selected"
    :key="selected.id"
    resource="lookup-list-values"
    noun="value"
    :title="`Values of “${selected.name}”`"
    :rows="values.data.value as Row[] | undefined"
    :loading="values.isLoading.value"
    :error="values.error.value"
    :refetch="() => values.refetch()"
    :fields="VALUE_FIELDS"
    :columns="[{ key: 'color', label: 'Colour' }]"
    :create-extra="{ listId: selected.id }"
    empty-hint="Add the values operators can choose from."
  >
    <template #cell="{ row }">
      <ClassBadge v-if="row.color" :color="String(row.color)" :name="String(row.color)" />
    </template>
  </OrderedLookupTable>

  <RecordDialog
    :open="dialogOpen"
    :title="editing ? `Edit list “${editing.name}”` : 'New lookup list'"
    :submit-label="editing ? 'Save' : 'Create list'"
    :fields="LIST_FIELDS"
    :record="editing"
    :save="save"
    id-prefix="ll"
    @close="dialogOpen = false"
    @saved="(m) => (notice = m)"
  />
</template>
