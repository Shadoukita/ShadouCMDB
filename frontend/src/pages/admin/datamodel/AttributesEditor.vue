<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useLookupLists, useOwnAttributes, usePatch, useRemove, useReorder, type AttributeDefinition } from "../../../api/datamodel";
import { usePurge } from "../../../api/schemaChanges";
import { useCiClasses, useClassAttributes, type CiClass } from "../../../api/queries";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import { t, type MessageKey } from "../../../i18n";
import LoadingState from "../../../components/LoadingState.vue";
import LookupValueName from "../../../components/LookupValueName.vue";
import SchemaChangeDialog from "../../../components/SchemaChangeDialog.vue";
import { GENERAL_SECTION, groupAttributes } from "../../../lib/attributes";
import RowMenu, { type RowMenuItem } from "../../../components/RowMenu.vue";
import { DATA_TYPES } from "../../../lib/dataTypes";
import { formatDate, formatDateTime } from "../../../lib/format";
import { moveItem, useDragReorder } from "../../../lib/reorder";
import { useSchemaChangeFlow } from "../../../lib/schemaChange";
import AttributeDialog from "./AttributeDialog.vue";
import Icon from "../../../components/Icon.vue";
import { useFlashStore } from "../../../stores/flash";

/**
 * The attributes defined on one class, by form section, in form order. Drag a
 * row (or use the arrows) to reorder; dropping it into another section moves it
 * there. Attributes inherited from parent classes are listed read-only below.
 * Each attribute is a column of the class's table: archiving hides it and keeps
 * the column; purging (typed to confirm) drops the column and its values. Row
 * actions sit in a RowMenu; confirmations are toasts, errors stay inline.
 */
const props = defineProps<{ cls: CiClass }>();
const own = useOwnAttributes(() => props.cls.id);
const effective = useClassAttributes(() => props.cls.id);
const classes = useCiClasses();
const lists = useLookupLists();
const reorder = useReorder("attribute-definitions");
const patch = usePatch<AttributeDefinition>("attribute-definitions");
const remove = useRemove("attribute-definitions");
const purge = usePurge("attribute-definitions");
const flow = useSchemaChangeFlow();
const flash = useFlashStore();
const failure = ref<unknown>(null);

type Placed = { id: string; groupName: string | null };
/** The order and sections being saved, shown until the refetch arrives. */
const pending = ref<Placed[] | null>(null);

const defs = computed<AttributeDefinition[]>(() => {
  const list = own.data.value?.data ?? [];
  if (!pending.value) return list;
  const at = new Map(pending.value.map((p, i) => [p.id, { i, groupName: p.groupName }]));
  return list.map((d) => ({ ...d, sortOrder: (at.get(d.id)?.i ?? 0) * 10, groupName: at.get(d.id)?.groupName ?? null }));
});
const groups = computed(() => groupAttributes(defs.value));
const flat = computed(() => groups.value.flatMap(([, items]) => items));
const sections = computed(() => [
  ...new Set([...groups.value.map(([g]) => g), ...(effective.data.value ?? []).map((d) => d.groupName ?? GENERAL_SECTION)]),
]);
const inherited = computed(() => (effective.data.value ?? []).filter((d) => d.inherited));
const nextSortOrder = computed(() => Math.max(0, ...defs.value.map((d) => d.sortOrder)) + 10);
const sectionOf = (d: AttributeDefinition) => d.groupName || GENERAL_SECTION;
/** The groupName to store for a section heading (the default section is "no section"). */
const storedSection = (section: string) => (section === GENERAL_SECTION ? null : section);

function save(order: AttributeDefinition[], moved: AttributeDefinition, toSection: string) {
  const changedSection = sectionOf(moved) !== toSection;
  pending.value = order.map((d) => ({ id: d.id, groupName: d.id === moved.id ? storedSection(toSection) : d.groupName }));
  reorder.mutate(
    order.map((d) => ({
      id: d.id,
      sortOrder: d.sortOrder,
      extra: d.id === moved.id && changedSection ? { groupName: storedSection(toSection) } : undefined,
    })),
    {
      onSuccess: () =>
        flash.show(changedSection ? t("dm.attr.toast.movedTo", { name: moved.label, section: toSection }) : t("dm.attr.toast.moved", { name: moved.label })),
      onSettled: () => (pending.value = null),
    },
  );
}

/** Drop on a row: take its position and its section. Drop on a section heading: become the section's first attribute. */
function commit(dragId: string, targetId: string) {
  const list = flat.value;
  const drag = list.find((d) => d.id === dragId);
  if (!drag) return;
  if (targetId.startsWith("section:")) {
    const section = targetId.slice(8);
    const first = list.findIndex((d) => sectionOf(d) === section);
    if (first < 0) return;
    const from = list.indexOf(drag);
    save(moveItem(list, from, from < first ? first - 1 : first), drag, section);
    return;
  }
  const target = list.find((d) => d.id === targetId);
  if (!target) return;
  save(moveItem(list, list.indexOf(drag), list.indexOf(target)), drag, sectionOf(target));
}

/** Arrow buttons: move one place; at a section boundary, first move into the neighbouring section. */
function step(d: AttributeDefinition, delta: -1 | 1) {
  const list = flat.value;
  const neighbour = list[list.indexOf(d) + delta];
  if (!neighbour) return;
  if (sectionOf(neighbour) !== sectionOf(d)) save(list, d, sectionOf(neighbour));
  else commit(d.id, neighbour.id);
}

const dnd = useDragReorder(commit, () => !reorder.isPending.value);

async function setActive(d: AttributeDefinition, isActive: boolean) {
  failure.value = null;
  const outcome = isActive
    ? await flow.run({
        title: t("dm.attr.restore.title", { name: d.label }),
        preview: { operation: "updateField", id: d.id, body: { isActive: true } },
        apply: () => patch.mutateAsync({ id: d.id, body: { isActive: true } }),
        applyLabel: t("dm.attr.restore.apply"),
      })
    : await flow.run({
        title: t("dm.attr.archive.title", { name: d.label }),
        intro: t("dm.attr.archive.intro", { column: `${props.cls.tableName}.${d.key}` }),
        preview: { operation: "deleteField", id: d.id },
        apply: () => remove.mutateAsync(d.id),
        applyLabel: t("dm.attr.archive.apply"),
        alwaysShow: true,
      });
  if (outcome.status === "applied") flash.show(t(isActive ? "dm.attr.toast.restored" : "dm.attr.toast.archived", { name: d.label }));
  else if (outcome.status === "refused") failure.value = outcome.error;
}

async function purgeField(d: AttributeDefinition) {
  failure.value = null;
  const outcome = await flow.run({
    title: t("dm.attr.purge.title", { name: d.label }),
    intro: t("dm.attr.purge.intro", { column: `${props.cls.tableName}.${d.key}` }),
    preview: { operation: "purgeField", id: d.id, body: { confirm: d.key } },
    apply: (confirm) => purge.mutateAsync({ id: d.id, confirm }),
    applyLabel: t("dm.attr.purge.apply"),
    danger: true,
    confirmName: d.key,
  });
  if (outcome.status === "applied") flash.show(t("dm.attr.toast.purged", { name: d.label, key: d.key }));
  else if (outcome.status === "refused") failure.value = outcome.error;
}

// ---------- Add / edit dialog ----------
const dialogOpen = ref(false);
const editing = ref<AttributeDefinition | null>(null);
const dialogSection = ref<string | undefined>(undefined);

function openNew(section?: string) {
  editing.value = null;
  dialogSection.value = section && section !== GENERAL_SECTION ? section : undefined;
  dialogOpen.value = true;
}
function openEdit(d: AttributeDefinition) {
  editing.value = d;
  dialogOpen.value = true;
}

/** Edit first, then Archive or Restore, then Purge. System attributes (SHAA-1505) cannot be archived. */
function rowMenu(d: AttributeDefinition): RowMenuItem[] {
  const items: RowMenuItem[] = [{ label: t("common.edit"), action: () => openEdit(d) }];
  if (d.isActive) {
    if (!d.systemRole) items.push({ label: t("dm.attr.row.archive"), action: () => setActive(d, false) });
  } else {
    items.push({ label: t("dm.attr.row.restore"), action: () => setActive(d, true) });
    items.push({ label: t("dm.attr.row.purge"), action: () => purgeField(d), danger: true });
  }
  return items;
}

// ---------- Display helpers ----------
const className = (id: string | null) => (id ? (classes.data.value?.find((c) => c.id === id)?.name ?? t("dm.attr.unknownClass")) : "");
const listName = (id: string | null) => (id ? (lists.data.value?.find((l) => l.id === id)?.name ?? t("dm.attr.unknownList")) : "");
/** The operator-facing name of a data type (lib/dataTypes keys, translated). */
const typeName = (key: string) => (DATA_TYPES.some((d) => d.key === key) ? t(`dm.attr.type.${key}` as MessageKey) : key);

function typeDetail(d: { dataType: string; enumValues: string[] | null; referenceClassId: string | null; lookupListId: string | null }): string {
  if (d.dataType === "enum") return (d.enumValues ?? []).join(", ");
  if (d.dataType === "reference") return `→ ${className(d.referenceClassId)}`;
  if (d.dataType === "lookup") return listName(d.lookupListId);
  return "";
}

function defaultText(d: AttributeDefinition): string {
  const v = d.defaultValue;
  if (v === null || v === undefined || v === "") return "";
  if (d.dataType === "boolean") return v ? t("common.yes") : t("common.no");
  if (d.dataType === "date") return formatDate(String(v));
  if (d.dataType === "datetime") return formatDateTime(String(v));
  return String(v);
}
</script>

<template>
  <section class="panel" aria-labelledby="attrs-title">
    <div class="panel-header">
      <h2 id="attrs-title">{{ t("dm.attr.title", { name: cls.name }) }}</h2>
      <span v-if="reorder.isPending.value || patch.isPending.value" class="spinner" :aria-label="t('common.saving')" />
      <p v-if="defs.length > 1" class="toolbar-hint">{{ t("dm.attr.hint") }}</p>
      <button type="button" class="btn btn-primary btn-sm attrs-add" @click="openNew()"><Icon name="plus" />{{ t("dm.attr.add") }}</button>
    </div>
    <div v-if="reorder.isError.value || failure" class="panel-body">
      <ErrorAlert v-if="reorder.isError.value" :error="reorder.error.value" :title="t('dm.attr.reorderFailed')" />
      <ErrorAlert v-if="failure" :error="failure" :title="t('dm.attr.notSaved')" />
    </div>
    <LoadingState v-if="own.isLoading.value" :label="t('dm.attr.loading')" />
    <div v-else-if="own.isError.value" class="panel-body">
      <ErrorAlert :error="own.error.value" :on-retry="() => own.refetch()" />
    </div>
    <EmptyState v-else-if="defs.length === 0" icon="columns-3" :title="t('dm.attr.empty.title', { name: cls.name })" data-testid="attributes-empty">
      {{ inherited.length ? t("dm.attr.empty.bodyInherited") : t("dm.attr.empty.body") }}
      <template #actions>
        <button type="button" class="btn btn-primary" @click="openNew()"><Icon name="plus" />{{ t("dm.attr.empty.action") }}</button>
      </template>
    </EmptyState>
    <div v-else class="table-wrap">
      <table class="data reorderable attributes">
        <thead>
          <tr>
            <th scope="col" class="drag-col"><span class="sr-only">{{ t("dm.attr.dragToReorder") }}</span></th>
            <th scope="col">{{ t("dm.attr.col.label") }}</th>
            <th scope="col">{{ t("dm.attr.col.column") }}</th>
            <th scope="col">{{ t("dm.attr.col.type") }}</th>
            <th scope="col">{{ t("common.required") }}</th>
            <th scope="col">{{ t("dm.attr.col.default") }}</th>
            <th scope="col">{{ t("dm.attr.col.order") }}</th>
            <th scope="col" class="row-actions"><span class="sr-only">{{ t("inventory.actions") }}</span></th>
          </tr>
        </thead>
        <tbody v-for="[section, items] in groups" :key="section">
          <tr class="section-row" v-bind="dnd.row(`section:${section}`)">
            <th colspan="7" scope="colgroup">{{ section }}</th>
            <td class="row-actions">
              <button type="button" class="btn-link" :aria-label="t('dm.attr.addTo', { section })" @click="openNew(section)">
                <Icon name="plus" />{{ t("dm.attr.addHere") }}
              </button>
            </td>
          </tr>
          <tr v-for="d in items" :key="d.id" v-bind="dnd.row(d.id)" :class="{ disabled: !d.isActive }">
            <td class="drag-handle" aria-hidden="true" :title="t('dm.attr.dragToReorder')"><Icon name="grip-vertical" /></td>
            <td>
              <button type="button" class="btn-link" :title="t('dm.attr.editTitle', { name: d.label })" @click="openEdit(d)">{{ d.label }}</button>
              <span v-if="!d.isActive" class="badge off" :title="t('dm.attr.archivedTitle')">{{ t("dm.attr.archived") }}</span>
              <span v-if="d.systemRole" class="badge" :title="t('people.datamodel.systemTitle')" data-testid="system-attribute">{{ t("people.datamodel.system") }}</span>
              <div v-if="d.helpText" class="muted cell-note">{{ d.helpText }}</div>
            </td>
            <td class="mono">{{ d.key }}</td>
            <td>
              {{ typeName(d.dataType) }}
              <div v-if="typeDetail(d)" class="muted cell-note" :title="typeDetail(d)">{{ typeDetail(d) }}</div>
            </td>
            <td><span v-if="d.isRequired" class="badge warn">{{ t("common.required") }}</span></td>
            <td>
              <LookupValueName v-if="d.dataType === 'lookup' && d.defaultValue" :list-id="d.lookupListId" :value-id="String(d.defaultValue)" />
              <template v-else>{{ defaultText(d) }}</template>
            </td>
            <td class="order-buttons">
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || flat.indexOf(d) === 0" :aria-label="t('dm.attr.moveUp', { name: d.label })" @click="step(d, -1)"><Icon name="arrow-up" /></button>
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || flat.indexOf(d) === flat.length - 1" :aria-label="t('dm.attr.moveDown', { name: d.label })" @click="step(d, 1)"><Icon name="arrow-down" /></button>
            </td>
            <td class="row-actions">
              <RowMenu :label="t('inventory.rowMenu', { name: d.label })" :items="rowMenu(d)" />
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <section v-if="inherited.length > 0" class="panel" aria-labelledby="inherited-title">
    <div class="panel-header">
      <h2 id="inherited-title">{{ t("dm.attr.inherited.title") }}</h2>
      <span class="meta">{{ t("dm.attr.inherited.hint") }}</span>
    </div>
    <div class="table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th scope="col">{{ t("dm.attr.col.label") }}</th>
            <th scope="col">{{ t("dm.attr.col.key") }}</th>
            <th scope="col">{{ t("dm.attr.col.type") }}</th>
            <th scope="col">{{ t("dm.attr.col.section") }}</th>
            <th scope="col">{{ t("common.required") }}</th>
            <th scope="col">{{ t("dm.attr.col.definedOn") }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="d in inherited" :key="d.id" :class="{ disabled: !d.isActive }">
            <td>{{ d.label }}</td>
            <td class="mono">{{ d.key }}</td>
            <td>{{ typeName(d.dataType) }}<span v-if="typeDetail(d)" class="muted"> · {{ typeDetail(d) }}</span></td>
            <td>{{ d.groupName ?? GENERAL_SECTION }}</td>
            <td><span v-if="d.isRequired" class="badge warn">{{ t("common.required") }}</span></td>
            <td><RouterLink :to="`/admin/classes/${d.definedOn.id}`">{{ d.definedOn.name }}</RouterLink></td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <AttributeDialog
    :open="dialogOpen"
    :cls="cls"
    :def="editing"
    :sections="sections"
    :default-section="dialogSection"
    :next-sort-order="nextSortOrder"
    @close="dialogOpen = false"
    @saved="(m) => flash.show(m)"
  />
  <SchemaChangeDialog :flow="flow" />
</template>
