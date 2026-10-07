<script setup lang="ts">
import { adminCrumbs } from "../sections";
import { computed, nextTick, ref } from "vue";
import { RouterLink } from "vue-router";
import { useCreateLookupList, useLookupListValues, useLookupLists, usePatch, type LookupList } from "../../../api/datamodel";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import ClassBadge from "../../../components/ClassBadge.vue";
import DeleteRowButton from "../../../components/DeleteRowButton.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import Icon from "../../../components/Icon.vue";
import RecordDialog, { type FieldSpec } from "../../../components/RecordDialog.vue";
import RowMenu, { type RowMenuItem } from "../../../components/RowMenu.vue";
import { formatNumber, t } from "../../../i18n";
import { useDocumentTitle } from "../../../lib/composables";
import { useListQuery } from "../../../lib/listQuery";
import { useFlashStore } from "../../../stores/flash";
import OrderedLookupTable, { type Row } from "./OrderedLookupTable.vue";

/**
 * Administration › Data model › Dropdowns: admin-defined lookup lists ("Support
 * contract": Gold, Silver, Bronze) used by "Lookup list" attributes. A list can
 * depend on a parent list ("Model" on "Manufacturer"): each of its values then
 * names the parent value it belongs to, and CI forms offer only the values of
 * the chosen parent. The selected list (?list=…) and the parent value its values
 * are filtered by (?parent=<id>|none) are in the URL.
 */
useDocumentTitle(() => t("dm.dropdowns.title"));
const flash = useFlashStore();
const lq = useListQuery({ sort: "sortOrder" });
const lists = useLookupLists();
const create = useCreateLookupList();
const patch = usePatch<LookupList>("lookup-lists");

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
    { name: "name", label: t("dm.lookup.col.name"), type: "text", required: true, hint: t("dm.dropdowns.field.nameHint") },
    { name: "key", label: t("dm.lookup.col.key"), type: "key", from: "name" },
    {
      name: "parentListId",
      label: t("dm.dropdowns.col.parent"),
      type: "select",
      options: [
        { value: "", label: t("dm.dropdowns.field.noParent") },
        ...(lists.data.value ?? [])
          .filter((l) => !excluded.has(l.id))
          .map((l) => ({ value: l.id, label: l.isActive ? l.name : t("dm.dropdowns.archivedName", { name: l.name }) })),
      ],
      hint: own ? t("dm.dropdowns.field.parentHintEdit") : t("dm.dropdowns.field.parentHintNew"),
    },
    { name: "description", label: t("dm.lookup.col.description"), type: "textarea", wide: true },
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
    return t("dm.dropdowns.created", { name: created.name });
  }
  const before = editing.value!;
  const saved = await patch.mutateAsync({ id: before.id, body });
  if (saved.parentListId !== before.parentListId) {
    lq.update({ list: saved.id, parent: undefined });
    return saved.parentListId
      ? t("dm.dropdowns.savedParent", { name: saved.name, parent: listName(saved.parentListId) })
      : t("dm.dropdowns.savedNoParent", { name: saved.name });
  }
  return t("dm.dropdowns.saved", { name: saved.name });
}
function setActive(l: LookupList, isActive: boolean) {
  patch.mutate(
    { id: l.id, body: { isActive } },
    { onSuccess: () => flash.show(t(isActive ? "dm.dropdowns.restored" : "dm.dropdowns.archived", { name: l.name })) },
  );
}

// One delete dialog for the lists, opened from a row's menu.
const deleting = ref<LookupList | null>(null);
const deleteDialog = ref<InstanceType<typeof DeleteRowButton>>();
async function askDelete(l: LookupList) {
  deleting.value = l;
  await nextTick();
  deleteDialog.value?.open();
}
function onDeleted() {
  flash.show(t("dm.dropdowns.deleted", { name: deleting.value?.name ?? "" }));
  lq.update({ list: undefined, parent: undefined });
}
const rowMenu = (l: LookupList): RowMenuItem[] => [
  { label: t("common.edit"), action: () => open(l) },
  l.isActive ? { label: t("dm.lookup.archive"), action: () => setActive(l, false) } : { label: t("dm.lookup.restore"), action: () => setActive(l, true) },
  { label: t("common.delete"), action: () => void askDelete(l), danger: true },
];

const valueFields = computed<FieldSpec[]>(() => [
  { name: "name", label: t("dm.lookup.col.name"), type: "text", required: true },
  { name: "key", label: t("dm.lookup.col.key"), type: "key", from: "name" },
  ...(parentList.value
    ? [
        {
          name: "parentValueId",
          label: t("dm.dropdowns.belongsTo", { list: parentList.value.name }),
          type: "select",
          options: [
            { value: "", label: t("dm.dropdowns.field.notAssigned") },
            ...(parentValues.data.value ?? []).map((v) => ({ value: v.id, label: v.isActive ? v.name : t("dm.dropdowns.retiredName", { name: v.name }) })),
          ],
          hint: t("dm.dropdowns.field.belongsToHint", { list: parentList.value.name }),
        } satisfies FieldSpec,
      ]
    : []),
  { name: "color", label: t("dm.dropdowns.col.colour"), type: "color", hint: t("dm.dropdowns.field.colourHint") },
  { name: "description", label: t("dm.lookup.col.description"), type: "textarea" },
]);
const valueColumns = computed(() => [
  ...(parentList.value ? [{ key: "parentValueId", label: t("dm.dropdowns.belongsTo", { list: parentList.value.name }) }] : []),
  { key: "color", label: t("dm.dropdowns.col.colour") },
]);
const valuesEmptyHint = computed(() => {
  if (parentFilter.value === "none") return t("dm.dropdowns.valuesEmpty.noneUnassigned");
  if (parentFilterName.value) return t("dm.dropdowns.valuesEmpty.noneOf", { name: parentFilterName.value });
  return parentList.value ? t("dm.dropdowns.valuesEmpty.withParent", { list: parentList.value.name }) : t("dm.dropdowns.valuesEmpty.plain");
});
</script>

<template>
  <Breadcrumbs :items="adminCrumbs('dropdowns')" />
  <div class="page-header">
    <div class="title">
      <h1>{{ t("dm.dropdowns.title") }}</h1>
      <span v-if="lists.data.value" class="muted count">{{ t("common.total", { n: formatNumber(lists.data.value.length) }) }}</span>
      <span v-if="patch.isPending.value" class="spinner" :aria-label="t('common.saving')" />
    </div>
    <div class="actions">
      <button type="button" class="btn btn-primary" @click="open(null)"><Icon name="plus" />{{ t("dm.dropdowns.create") }}</button>
    </div>
  </div>
  <p class="page-intro">{{ t("dm.dropdowns.intro") }}</p>
  <ErrorAlert v-if="patch.isError.value" :error="patch.error.value" :title="t('formError.notSaved')" />

  <section class="panel explorer" :aria-label="t('dm.dropdowns.lists')">
    <div v-if="lists.isError.value" class="panel-body"><ErrorAlert :error="lists.error.value" :on-retry="() => lists.refetch()" /></div>
    <LoadingState v-else-if="lists.isLoading.value" :label="t('dm.dropdowns.loading')" />
    <EmptyState v-else-if="lists.data.value?.length === 0" icon="list" :title="t('dm.dropdowns.empty.title')">
      {{ t("dm.dropdowns.empty.body") }}
      <template #actions>
        <button type="button" class="btn btn-primary" @click="open(null)"><Icon name="plus" />{{ t("dm.dropdowns.create") }}</button>
      </template>
    </EmptyState>
    <div v-else class="table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th scope="col">{{ t("dm.lookup.col.name") }}</th>
            <th scope="col">{{ t("dm.lookup.col.key") }}</th>
            <th scope="col">{{ t("dm.dropdowns.col.parent") }}</th>
            <th scope="col">{{ t("dm.lookup.col.description") }}</th>
            <th scope="col">{{ t("admin.col.status") }}</th>
            <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
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
              <span v-if="l.isActive" class="badge ok">{{ t("common.active") }}</span>
              <span v-else class="badge off">{{ t("dm.lookup.archivedBadge") }}</span>
            </td>
            <td class="row-actions">
              <RowMenu :label="t('inventory.rowMenu', { name: l.name })" :items="rowMenu(l)" />
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
    :title="t('dm.dropdowns.valuesTitle', { name: selected.name })"
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
        <label for="llv-parent">{{ t("dm.dropdowns.belongsTo", { list: parentList.name }) }}</label>
        <select id="llv-parent" :value="parentFilter" @change="lq.update({ parent: ($event.target as HTMLSelectElement).value || undefined })">
          <option value="">{{ t("dm.dropdowns.filter.all") }}</option>
          <option value="none">{{ t("dm.dropdowns.notAssigned") }}</option>
          <option v-for="v in parentValues.data.value ?? []" :key="v.id" :value="v.id">{{ v.isActive ? v.name : t("dm.dropdowns.retiredName", { name: v.name }) }}</option>
        </select>
      </div>
      <span v-if="parentValues.isError.value" class="error">{{ t("dm.dropdowns.filter.loadFailed", { name: parentList.name }) }}</span>
    </template>
    <template #cell="{ row, column }">
      <template v-if="column === 'parentValueId'">
        <template v-if="row.parentValueId">{{ parentValueName(row.parentValueId) ?? "…" }}</template>
        <span v-else class="badge warn" :title="t('dm.dropdowns.notAssignedTitle')">{{ t("dm.dropdowns.notAssigned") }}</span>
      </template>
      <ClassBadge v-else-if="row.color" :color="String(row.color)" :name="String(row.color)" />
    </template>
  </OrderedLookupTable>

  <DeleteRowButton
    ref="deleteDialog"
    headless
    resource="lookup-lists"
    :id="deleting?.id ?? ''"
    :label="t('dm.dropdowns.deleteLabel', { name: deleting?.name ?? '' })"
    archivable
    :archived="!deleting?.isActive"
    @archive="deleting && setActive(deleting, false)"
    @deleted="onDeleted"
  />

  <RecordDialog
    :open="dialogOpen"
    :title="editing ? t('dm.dropdowns.dialog.editTitle', { name: editing.name }) : t('dm.dropdowns.dialog.newTitle')"
    :submit-label="editing ? t('record.save.save') : t('dm.dropdowns.dialog.create')"
    :fields="listFields"
    :record="editing"
    :save="save"
    id-prefix="ll"
    @close="dialogOpen = false"
    @saved="(m) => flash.show(m)"
  />
</template>
