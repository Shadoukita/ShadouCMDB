<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useCreateLookupList, useLookupListValues, useLookupLists, usePatch, type LookupList } from "../../../api/datamodel";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import ClassBadge from "../../../components/ClassBadge.vue";
import DeleteRowButton from "../../../components/DeleteRowButton.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import RecordDialog, { type FieldSpec } from "../../../components/RecordDialog.vue";
import { useDocumentTitle } from "../../../lib/composables";
import { useListQuery } from "../../../lib/listQuery";
import OrderedLookupTable, { type Row } from "../lookups/OrderedLookupTable.vue";

/**
 * Administration › Data model › Dropdowns: admin-defined lookup lists ("Support
 * contract": Gold, Silver, Bronze) used by "Lookup list" attributes. A list can
 * depend on a parent list ("Model" on "Manufacturer"): each of its values then
 * names the parent value it belongs to, and CI forms offer only the values of
 * the chosen parent. The selected list (?list=…) and the parent value its values
 * are filtered by (?parent=<id>|none) are in the URL.
 */
useDocumentTitle(() => "Dropdowns");
const lq = useListQuery({ sort: "sortOrder" });
const lists = useLookupLists();
const create = useCreateLookupList();
const patch = usePatch<LookupList>("lookup-lists");
const notice = ref<string | null>(null);

const selectedId = computed(() => lq.get("list") || lists.data.value?.[0]?.id || "");
const selected = computed(() => lists.data.value?.find((l) => l.id === selectedId.value));
const listName = (id: string | null) => (id ? (lists.data.value?.find((l) => l.id === id)?.name ?? "?") : "");

// The selected list's parent list, its values, and the filter by one of them.
const parentList = computed(() => lists.data.value?.find((l) => l.id === selected.value?.parentListId));
const parentValues = useLookupListValues(() => selected.value?.parentListId);
const parentFilter = computed(() => (parentList.value ? lq.get("parent") : ""));
const parentFilterName = computed(() =>
  parentFilter.value === "none" ? "" : (parentValues.data.value?.find((v) => v.id === parentFilter.value)?.name ?? ""),
);
const parentValueName = (id: unknown) => parentValues.data.value?.find((v) => v.id === id)?.name;
const values = useLookupListValues(selectedId, parentFilter);

/** Lists that depend on `id`, directly or further down: choosing one of them as its parent would be a cycle. */
function dependents(id: string): Set<string> {
  const out = new Set<string>();
  const walk = (p: string) => {
    for (const l of lists.data.value ?? []) {
      if (l.parentListId === p && !out.has(l.id)) {
        out.add(l.id);
        walk(l.id);
      }
    }
  };
  walk(id);
  return out;
}

const dialogOpen = ref(false);
const editing = ref<LookupList | null>(null);
const listFields = computed<FieldSpec[]>(() => {
  const own = editing.value?.id;
  const excluded = own ? dependents(own).add(own) : new Set<string>();
  return [
    { name: "name", label: "Name", type: "text", required: true, hint: "e.g. Support contract" },
    { name: "key", label: "Key", type: "key", from: "name" },
    {
      name: "parentListId",
      label: "Parent list",
      type: "select",
      options: [
        { value: "", label: "— none —" },
        ...(lists.data.value ?? [])
          .filter((l) => !excluded.has(l.id))
          .map((l) => ({ value: l.id, label: `${l.name}${l.isActive ? "" : " (archived)"}` })),
      ],
      hint: own
        ? "Changing it unassigns the parent value of every value, and the parent field of every attribute, that uses this list"
        : "e.g. Manufacturer for a Model list: each value then belongs to a value of the parent list",
    },
    { name: "description", label: "Description", type: "textarea", wide: true },
  ];
});
function open(l: LookupList | null) {
  editing.value = l;
  dialogOpen.value = true;
}
async function save(body: Record<string, unknown>, isNew: boolean): Promise<string> {
  if (isNew) {
    const last = Math.max(0, ...(lists.data.value ?? []).map((l) => l.sortOrder));
    const created = await create.mutateAsync({ ...(body as { key: string; name: string }), sortOrder: last + 10 });
    lq.update({ list: created.id, parent: undefined });
    return `Created list ${created.name}. Add its values below, then use it in a “Lookup list” attribute.`;
  }
  const before = editing.value!;
  const saved = await patch.mutateAsync({ id: before.id, body });
  if (saved.parentListId !== before.parentListId) {
    lq.update({ list: saved.id, parent: undefined });
    return saved.parentListId
      ? `Saved list ${saved.name}: it now depends on ${listName(saved.parentListId)}. Assign each value its parent value below, and each “${saved.name}” attribute its parent field.`
      : `Saved list ${saved.name}: it no longer depends on another list.`;
  }
  return `Saved list ${saved.name}.`;
}
function setActive(l: LookupList, isActive: boolean) {
  notice.value = null;
  patch.mutate(
    { id: l.id, body: { isActive } },
    { onSuccess: () => (notice.value = isActive ? `Restored list ${l.name}.` : `Archived list ${l.name}: it can no longer be chosen for new attributes.`) },
  );
}

const valueFields = computed<FieldSpec[]>(() => [
  { name: "name", label: "Name", type: "text", required: true },
  { name: "key", label: "Key", type: "key", from: "name" },
  ...(parentList.value
    ? [
        {
          name: "parentValueId",
          label: `Belongs to (${parentList.value.name})`,
          type: "select",
          options: [
            { value: "", label: "— not assigned —" },
            ...(parentValues.data.value ?? []).map((v) => ({ value: v.id, label: `${v.name}${v.isActive ? "" : " (retired)"}` })),
          ],
          hint: `Shown on CI forms only when this ${parentList.value.name} is chosen. Required for new values.`,
        } satisfies FieldSpec,
      ]
    : []),
  { name: "color", label: "Colour", type: "color", hint: "Shown next to the value on CI pages" },
  { name: "description", label: "Description", type: "textarea" },
]);
const valueColumns = computed(() => [
  ...(parentList.value ? [{ key: "parentValueId", label: `Belongs to (${parentList.value.name})` }] : []),
  { key: "color", label: "Colour" },
]);
const valuesEmptyHint = computed(() => {
  if (parentFilter.value === "none") return "Every value belongs to a parent value.";
  if (parentFilterName.value) return `No value belongs to ${parentFilterName.value} yet.`;
  return parentList.value
    ? `Add the values operators can choose from, each with the ${parentList.value.name} it belongs to.`
    : "Add the values operators can choose from.";
});
</script>

<template>
  <Breadcrumbs :items="[{ label: 'Administration', to: '/admin' }, { label: 'Data model' }, { label: 'Dropdowns' }]" />
  <div class="page-header">
    <div class="title">
      <h1>Dropdowns</h1>
      <span class="muted">Value lists for “Lookup list” attributes; a list can depend on a parent list.</span>
    </div>
  </div>

  <section class="panel" aria-label="Lookup lists">
    <div class="panel-header">
      <h2>Lists</h2>
      <span v-if="lists.data.value" class="muted">{{ lists.data.value.length }}</span>
      <span v-if="patch.isPending.value" class="spinner" aria-label="Saving" />
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
            <th scope="col">Parent list</th>
            <th scope="col">Description</th>
            <th scope="col">Status</th>
            <th scope="col"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="l in lists.data.value" :key="l.id" :class="{ disabled: !l.isActive, selected: l.id === selectedId }" :aria-selected="l.id === selectedId">
            <td><RouterLink :to="{ query: { list: l.id } }">{{ l.name }}</RouterLink></td>
            <td class="mono">{{ l.key }}</td>
            <td>
              <RouterLink v-if="l.parentListId" :to="{ query: { list: l.parentListId } }">{{ listName(l.parentListId) }}</RouterLink>
            </td>
            <td class="muted fill" :title="l.description ?? undefined">{{ l.description ?? "" }}</td>
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
                @deleted="(notice = `Deleted list ${l.name}.`), lq.update({ list: undefined, parent: undefined })"
              />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <OrderedLookupTable
    v-if="selected"
    :key="`${selected.id}:${selected.parentListId ?? ''}`"
    resource="lookup-list-values"
    noun="value"
    :title="`Values of “${selected.name}”`"
    :rows="values.data.value as Row[] | undefined"
    :loading="values.isLoading.value"
    :error="values.error.value"
    :refetch="() => values.refetch()"
    :fields="valueFields"
    :columns="valueColumns"
    :create-extra="{ listId: selected.id }"
    :create-defaults="parentFilter && parentFilter !== 'none' ? { parentValueId: parentFilter } : undefined"
    :partial="!!parentFilter"
    :empty-hint="valuesEmptyHint"
  >
    <template v-if="parentList" #toolbar>
      <div class="field">
        <label for="llv-parent">Belongs to ({{ parentList.name }})</label>
        <select id="llv-parent" :value="parentFilter" @change="lq.update({ parent: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">All values</option>
          <option value="none">Not assigned</option>
          <option v-for="v in parentValues.data.value ?? []" :key="v.id" :value="v.id">{{ v.name }}{{ v.isActive ? "" : " (retired)" }}</option>
        </select>
      </div>
      <span v-if="parentValues.isError.value" class="error">Could not load the values of {{ parentList.name }}.</span>
    </template>
    <template #cell="{ row, column }">
      <template v-if="column === 'parentValueId'">
        <template v-if="row.parentValueId">{{ parentValueName(row.parentValueId) ?? "…" }}</template>
        <span v-else class="badge warn" title="Cannot be chosen on attributes that have a parent field until assigned">Not assigned</span>
      </template>
      <ClassBadge v-else-if="row.color" :color="String(row.color)" :name="String(row.color)" />
    </template>
  </OrderedLookupTable>

  <RecordDialog
    :open="dialogOpen"
    :title="editing ? `Edit list “${editing.name}”` : 'New lookup list'"
    :submit-label="editing ? 'Save' : 'Create list'"
    :fields="listFields"
    :record="editing"
    :save="save"
    id-prefix="ll"
    @close="dialogOpen = false"
    @saved="(m) => (notice = m)"
  />
</template>
