<script setup lang="ts">
import { computed, ref } from "vue";
import { RouterLink } from "vue-router";
import { useLookupLists, useOwnAttributes, usePatch, useRemove, useReorder, type AttributeDefinition } from "../../../api/datamodel";
import { usePurge } from "../../../api/schemaChanges";
import { useCiClasses, useClassAttributes, type CiClass } from "../../../api/queries";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import { t } from "../../../i18n";
import LoadingState from "../../../components/LoadingState.vue";
import LookupValueName from "../../../components/LookupValueName.vue";
import SchemaChangeDialog from "../../../components/SchemaChangeDialog.vue";
import { GENERAL_SECTION, groupAttributes } from "../../../lib/attributes";
import { dataTypeLabel } from "../../../lib/dataTypes";
import { formatDate, formatDateTime } from "../../../lib/format";
import { moveItem, useDragReorder } from "../../../lib/reorder";
import { useSchemaChangeFlow } from "../../../lib/schemaChange";
import AttributeDialog from "./AttributeDialog.vue";
import Icon from "../../../components/Icon.vue";

/**
 * The attributes defined on one class, by form section, in form order. Drag a
 * row (or use the arrows) to reorder; dropping it into another section moves it
 * there. Attributes inherited from parent classes are listed read-only below.
 * Each attribute is a column of the class's table: archiving hides it and keeps
 * the column; purging (typed to confirm) drops the column and its values.
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
const notice = ref<string | null>(null);
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
  notice.value = null;
  reorder.mutate(
    order.map((d) => ({
      id: d.id,
      sortOrder: d.sortOrder,
      extra: d.id === moved.id && changedSection ? { groupName: storedSection(toSection) } : undefined,
    })),
    {
      onSuccess: () => (notice.value = `Moved ${moved.label}${changedSection ? ` to section “${toSection}”` : ""}.`),
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
  notice.value = null;
  failure.value = null;
  const outcome = isActive
    ? await flow.run({
        title: `Restore attribute “${d.label}”`,
        preview: { operation: "updateField", id: d.id, body: { isActive: true } },
        apply: () => patch.mutateAsync({ id: d.id, body: { isActive: true } }),
        applyLabel: "Restore attribute",
      })
    : await flow.run({
        title: `Archive attribute “${d.label}”?`,
        intro: `The column ${props.cls.tableName}.${d.key} and its stored values are kept and still shown on CIs that have one, but the attribute leaves the forms and accepts no new values. Only a purge drops the column.`,
        preview: { operation: "deleteField", id: d.id },
        apply: () => remove.mutateAsync(d.id),
        applyLabel: "Archive attribute",
        alwaysShow: true,
      });
  if (outcome.status === "applied")
    notice.value = isActive
      ? `Restored ${d.label}: it shows on forms again.`
      : `Archived ${d.label}: stored values are kept and still shown, but it is no longer on forms.`;
  else if (outcome.status === "refused") failure.value = outcome.error;
}

async function purgeField(d: AttributeDefinition) {
  notice.value = null;
  failure.value = null;
  const outcome = await flow.run({
    title: `Purge attribute “${d.label}”?`,
    intro: `Drops the column ${props.cls.tableName}.${d.key} with every value stored in it, and rebuilds the reporting views. The audit log keeps the history of past values.`,
    preview: { operation: "purgeField", id: d.id, body: { confirm: d.key } },
    apply: (confirm) => purge.mutateAsync({ id: d.id, confirm }),
    applyLabel: "Purge attribute",
    danger: true,
    confirmName: d.key,
  });
  if (outcome.status === "applied") notice.value = `Purged ${d.label}: column ${d.key} was dropped.`;
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

// ---------- Display helpers ----------
const className = (id: string | null) => (id ? (classes.data.value?.find((c) => c.id === id)?.name ?? "unknown class") : "");
const listName = (id: string | null) => (id ? (lists.data.value?.find((l) => l.id === id)?.name ?? "unknown list") : "");

function typeDetail(d: { dataType: string; enumValues: string[] | null; referenceClassId: string | null; lookupListId: string | null }): string {
  if (d.dataType === "enum") return (d.enumValues ?? []).join(", ");
  if (d.dataType === "reference") return `→ ${className(d.referenceClassId)}`;
  if (d.dataType === "lookup") return listName(d.lookupListId);
  return "";
}

function defaultText(d: AttributeDefinition): string {
  const v = d.defaultValue;
  if (v === null || v === undefined || v === "") return "";
  if (d.dataType === "boolean") return v ? "Yes" : "No";
  if (d.dataType === "date") return formatDate(String(v));
  if (d.dataType === "datetime") return formatDateTime(String(v));
  return String(v);
}
</script>

<template>
  <section class="panel" aria-labelledby="attrs-title">
    <div class="panel-header">
      <h2 id="attrs-title">Attributes of {{ cls.name }}</h2>
      <span v-if="reorder.isPending.value || patch.isPending.value" class="spinner" aria-label="Saving" />
      <span class="muted">Drag a row, or use the arrows, to change the form order. Drop on a section to move it there.</span>
      <button type="button" class="btn btn-primary btn-sm" style="margin-left: auto" @click="openNew()">+ Add attribute</button>
    </div>
    <div v-if="notice || reorder.isError.value || failure" class="panel-body">
      <div v-if="notice" class="alert alert-success" role="status">{{ notice }}</div>
      <ErrorAlert v-if="reorder.isError.value" :error="reorder.error.value" title="The new order was not saved completely" />
      <ErrorAlert v-if="failure" :error="failure" title="Not saved" />
    </div>
    <LoadingState v-if="own.isLoading.value" label="Loading attributes…" />
    <div v-else-if="own.isError.value" class="panel-body">
      <ErrorAlert :error="own.error.value" :on-retry="() => own.refetch()" />
    </div>
    <div v-else-if="defs.length === 0" class="panel-body">
      <p class="muted" style="margin: 0">
        {{ cls.name }} defines no attributes of its own yet{{ inherited.length ? "; its CIs carry the inherited ones listed below" : "" }}.
        CIs always have an ident and a validity period; everything else, name and status included, is an attribute.
      </p>
    </div>
    <div v-else class="table-wrap">
      <table class="data reorderable attributes">
        <thead>
          <tr>
            <th scope="col" class="drag-col"><span class="sr-only">Drag to reorder</span></th>
            <th scope="col">Label</th>
            <th scope="col">Column</th>
            <th scope="col">Type</th>
            <th scope="col">Required</th>
            <th scope="col">Default</th>
            <th scope="col">Order</th>
            <th scope="col"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody v-for="[section, items] in groups" :key="section">
          <tr class="section-row" v-bind="dnd.row(`section:${section}`)">
            <th colspan="7" scope="colgroup">{{ section }}</th>
            <td class="row-actions">
              <button type="button" class="btn-link" :aria-label="`Add an attribute to ${section}`" @click="openNew(section)">+ Add here</button>
            </td>
          </tr>
          <tr v-for="d in items" :key="d.id" v-bind="dnd.row(d.id)" :class="{ disabled: !d.isActive }">
            <td class="drag-handle" aria-hidden="true" title="Drag to reorder"><Icon name="grip-vertical" /></td>
            <td>
              <button type="button" class="btn-link" :title="`Edit ${d.label}`" @click="openEdit(d)">{{ d.label }}</button>
              <span v-if="!d.isActive" class="badge off" title="Kept on CIs that have a value; not on forms">archived</span>
              <span v-if="d.systemRole" class="badge" :title="t('people.datamodel.systemTitle')" data-testid="system-attribute">{{ t("people.datamodel.system") }}</span>
              <div v-if="d.helpText" class="muted cell-note">{{ d.helpText }}</div>
            </td>
            <td class="mono">{{ d.key }}</td>
            <td>
              {{ dataTypeLabel(d.dataType) }}
              <div v-if="typeDetail(d)" class="muted cell-note" :title="typeDetail(d)">{{ typeDetail(d) }}</div>
            </td>
            <td><span v-if="d.isRequired" class="badge warn">Required</span></td>
            <td>
              <LookupValueName v-if="d.dataType === 'lookup' && d.defaultValue" :list-id="d.lookupListId" :value-id="String(d.defaultValue)" />
              <template v-else>{{ defaultText(d) }}</template>
            </td>
            <td class="order-buttons">
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || flat.indexOf(d) === 0" :aria-label="`Move ${d.label} up`" @click="step(d, -1)"><Icon name="arrow-up" /></button>
              <button type="button" class="btn btn-sm btn-icon" :disabled="reorder.isPending.value || flat.indexOf(d) === flat.length - 1" :aria-label="`Move ${d.label} down`" @click="step(d, 1)"><Icon name="arrow-down" /></button>
            </td>
            <td class="row-actions">
              <button
                v-if="d.isActive"
                type="button"
                class="btn btn-sm"
                :disabled="!!d.systemRole"
                :title="d.systemRole ? t('people.datamodel.systemTitle') : undefined"
                :aria-label="`Archive ${d.label}`"
                @click="setActive(d, false)"
              >
                Archive
              </button>
              <template v-else>
                <button type="button" class="btn btn-sm" :aria-label="`Restore ${d.label}`" @click="setActive(d, true)">Restore</button>
                <button type="button" class="btn btn-sm btn-quiet-danger" :aria-label="`Purge ${d.label}`" @click="purgeField(d)">Purge…</button>
              </template>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>

  <section v-if="inherited.length > 0" class="panel" aria-labelledby="inherited-title">
    <div class="panel-header">
      <h2 id="inherited-title">Inherited attributes</h2>
      <span class="muted">Defined on a parent class; edit them there</span>
    </div>
    <div class="table-wrap">
      <table class="data">
        <thead>
          <tr>
            <th scope="col">Label</th>
            <th scope="col">Key</th>
            <th scope="col">Type</th>
            <th scope="col">Section</th>
            <th scope="col">Required</th>
            <th scope="col">Defined on</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="d in inherited" :key="d.id" :class="{ disabled: !d.isActive }">
            <td>{{ d.label }}</td>
            <td class="mono">{{ d.key }}</td>
            <td>{{ dataTypeLabel(d.dataType) }}<span v-if="typeDetail(d)" class="muted"> · {{ typeDetail(d) }}</span></td>
            <td>{{ d.groupName ?? GENERAL_SECTION }}</td>
            <td><span v-if="d.isRequired" class="badge warn">Required</span></td>
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
    @saved="(m) => (notice = m)"
  />
  <SchemaChangeDialog :flow="flow" />
</template>
