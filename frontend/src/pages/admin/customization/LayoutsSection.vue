<script setup lang="ts">
import { useQueryClient } from "@tanstack/vue-query";
import { computed, nextTick, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { fetchCi, fetchCiList, useCiClasses } from "../../../api/queries";
import { useLayoutTemplateUsage, type UiLayoutTemplate, type UiSettingsDocument } from "../../../api/uiSettings";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import PaginationBar from "../../../components/PaginationBar.vue";
import { t } from "../../../i18n";
import { useDebounced } from "../../../lib/composables";
import { openLayoutEditor } from "../../../lib/layoutEditor";
import {
  addTemplate,
  classesUsing,
  classRows,
  classTemplateKey,
  deletable,
  freeTemplateName,
  ownLayoutLink,
  setClassTemplate,
  STANDARD_TEMPLATE,
  type TemplateUsers,
} from "../../../lib/layoutTemplates";
import { useListQuery } from "../../../lib/listQuery";
import ClassCiEditor from "./ClassCiEditor.vue";
import TemplateDialog from "./TemplateDialog.vue";

/**
 * Customization › Layouts: layout templates (SHAA-1472) and which one each
 * class uses.
 *
 * - Classes: every class with its default template (an inline select, part of
 *   this page's draft, saved with it), searchable, filterable by template and
 *   sortable, with the filter in the URL; the number of its CIs with a layout of
 *   their own links to them in the inventory. "Edit CI…" opens the class's panel
 *   to edit its default template on one of its CIs (ClassCiEditor).
 * - Templates: who uses each (classes from the draft, CIs from the API) and
 *   Edit (the layout editor on a CI that shows it, else on a CI of a class that
 *   uses it, else on the most recently updated CI), Rename, Duplicate and Delete
 *   (only when nobody uses it), plus New template.
 *
 * Template changes and class defaults go into the draft and are saved with the
 * page as one settings version; the layout itself is edited (and saved) in the
 * layout editor.
 */
const props = defineProps<{ doc: UiSettingsDocument; error?: unknown }>();
const route = useRoute();
const router = useRouter();
const qc = useQueryClient();
const classes = useCiClasses();
const usage = useLayoutTemplateUsage();
const allClasses = computed(() => classes.data.value ?? []);
const classByKey = computed(() => new Map(allClasses.value.map((c) => [c.key, c])));
const classById = computed(() => new Map(allClasses.value.map((c) => [c.id, c])));

// ---------- Classes ----------
const lq = useListQuery({ sort: "class" });
const search = ref(lq.get("q"));
const typed = useDebounced(search, 250);
watch(typed, (q) => {
  if (q !== lq.get("q")) lq.update({ q: q || undefined });
});
watch(
  () => lq.get("q"),
  (q) => {
    if (q !== typed.value) search.value = q;
  },
);
const uses = computed(() => lq.get("uses"));
/** CIs with a layout of their own, per class key (undefined while counting). */
const owned = computed(() => (usage.data.value ? new Map(usage.data.value.classes.map((c) => [c.classKey, c.ownLayoutCount])) : undefined));
const rows = computed(() => classRows(allClasses.value, props.doc, { q: lq.get("q"), uses: uses.value, sort: lq.sort.value, owned: owned.value }));
const page = computed(() => rows.value.slice(lq.offset.value, lq.offset.value + lq.limit.value));
const filtered = computed(() => !!lq.get("q") || !!uses.value);
function clearFilters() {
  search.value = "";
  lq.update({ q: undefined, uses: undefined });
}
const CLASS_COLUMNS = [
  { key: "class", label: () => t("customization.layouts.colClass"), sort: "class" },
  { key: "template", label: () => t("customization.layouts.colTemplate"), sort: "template" },
  { key: "owned", label: () => t("customization.layouts.colOwnLayout"), sort: "owned" },
  { key: "actions", label: () => t("customization.layouts.colActions"), sort: "" },
] as const;

function setDefault(classKey: string, templateKey: string) {
  setClassTemplate(props.doc, classKey, templateKey);
}

/** The class whose panel is open (?class=<key>, so it survives a reload). */
const selected = computed(() => {
  const k = route.query.class;
  return typeof k === "string" ? classByKey.value.get(k) : undefined;
});
async function openClass(key: string) {
  await router.replace({ query: { ...route.query, class: key } });
  await nextTick();
  document.querySelector<HTMLElement>("[data-testid=layout-class-panel] select, [data-testid=layout-class-panel] a")?.focus();
}
/** The open class's default template (in the draft); the editor opens on it once it is saved. */
const selectedTemplate = computed(() => (selected.value ? classTemplateKey(props.doc, selected.value.key) : STANDARD_TEMPLATE));
const closeClass = () => router.replace({ query: { ...route.query, class: undefined } });

// ---------- Templates ----------
/** Templates in the saved settings (the usage counts and the editor know only those). */
const saved = computed(() => new Set((usage.data.value?.templates ?? []).map((u) => u.key)));
const templateName = (key: string) => props.doc.layoutTemplates.find((x) => x.key === key)?.name ?? key;
function users(key: string): TemplateUsers {
  const u = usage.data.value?.templates.find((x) => x.key === key);
  return {
    classKeys: classesUsing(props.doc, key, allClasses.value.map((c) => c.key)),
    ciCount: u ? u.overrideCount : usage.data.value ? 0 : undefined,
  };
}
const className = (key: string) => classByKey.value.get(key)?.name ?? key;
/** Who uses a template, for the Delete button's explanation. */
function inUse(key: string): string {
  if (key === STANDARD_TEMPLATE) return t("layoutTemplates.deleteStandard");
  const u = users(key);
  const names = u.classKeys.map(className);
  const list = names.length > 5 ? `${names.slice(0, 5).join(", ")} ${t("layoutTemplates.andMore", { n: names.length - 5 })}` : names.join(", ");
  if (u.ciCount === undefined) return t("layoutTemplates.deleteCounting");
  return t(u.ciCount === null ? "layoutTemplates.inUseHidden" : "layoutTemplates.inUse", { classes: names.length, list, cis: u.ciCount ?? 0 });
}
const ciCell = (key: string) => {
  const n = users(key).ciCount;
  return n === undefined ? "…" : n === null ? "?" : n.toLocaleString();
};

const dialog = ref<{ mode: "new" | "rename"; initial: { key?: string; name: string; description?: string; from?: string } } | null>(null);
function newTemplate() {
  dialog.value = { mode: "new", initial: { name: "" } };
}
function duplicate(tp: UiLayoutTemplate) {
  dialog.value = { mode: "new", initial: { name: freeTemplateName(t("layoutTemplates.copyName", { name: tp.name }), props.doc.layoutTemplates), description: tp.description, from: tp.key } };
}
function rename(tp: UiLayoutTemplate) {
  dialog.value = { mode: "rename", initial: { key: tp.key, name: tp.name, description: tp.description } };
}
function onDialog(v: { name: string; description: string; from: string }) {
  const d = dialog.value;
  dialog.value = null;
  if (!d) return;
  if (d.mode === "new") {
    const from = v.from ? props.doc.layoutTemplates.find((x) => x.key === v.from)?.layout : undefined;
    addTemplate(props.doc, v.name, from, v.description);
    return;
  }
  const tp = props.doc.layoutTemplates.find((x) => x.key === d.initial.key);
  if (!tp) return;
  tp.name = v.name;
  if (v.description) tp.description = v.description;
  else delete tp.description;
}

const removing = ref<UiLayoutTemplate | null>(null);
function remove() {
  const key = removing.value?.key;
  removing.value = null;
  if (key) props.doc.layoutTemplates = props.doc.layoutTemplates.filter((x) => x.key !== key);
}

/**
 * Opens the layout editor on the template: on a CI that shows it as its own layout
 * (`sampleCiId`), else on a CI of a class that uses it, else on the most recently
 * updated CI, else on a create form.
 */
const opening = ref<string | null>(null);
const openError = ref<unknown>(null);
async function edit(key: string) {
  opening.value = key;
  openError.value = null;
  try {
    const sample = usage.data.value?.templates.find((x) => x.key === key)?.sampleCiId;
    if (sample) {
      const ci = await fetchCi(qc, sample);
      const cls = classById.value.get(ci.classId);
      if (cls) return void openLayoutEditor(router, { path: `/cis/${ci.id}` }, cls.key, key);
    }
    const using = users(key).classKeys.map((k) => classByKey.value.get(k)).filter((c) => !!c);
    for (const c of using.slice(0, 5)) {
      const ci = (await fetchCiList(qc, { classId: c.id, limit: 1, sort: "-updatedAt" })).data[0];
      if (ci) return void openLayoutEditor(router, { path: `/cis/${ci.id}` }, c.key, key);
    }
    const any = (await fetchCiList(qc, { limit: 1, sort: "-updatedAt" })).data[0];
    const cls = any ? classById.value.get(any.classId) : undefined;
    if (any && cls) return void openLayoutEditor(router, { path: `/cis/${any.id}` }, cls.key, key);
    const target = using[0] ?? allClasses.value.find((c) => c.isActive);
    if (target) openLayoutEditor(router, { path: "/cis/new", query: { classId: target.id } }, target.key, key);
  } catch (e) {
    openError.value = e;
  } finally {
    opening.value = null;
  }
}
</script>

<template>
  <section class="panel">
    <div class="panel-header">
      <h2>{{ t("customization.layouts.classesTitle") }}</h2>
      <span class="muted">{{ t("customization.layouts.classesSubtitle") }}</span>
    </div>
    <div class="panel-body stack">
      <LoadingState v-if="classes.isLoading.value" :label="t('customization.layouts.loadingClasses')" />
      <ErrorAlert v-else-if="classes.isError.value" :error="classes.error.value" :on-retry="() => classes.refetch()" />
      <template v-else>
        <div class="toolbar" role="search" :aria-label="t('customization.layouts.filterLabel')">
          <div class="inline-control">
            <label for="layout-class-q">{{ t("customization.layouts.search") }}</label>
            <input id="layout-class-q" v-model="search" type="search" :placeholder="t('customization.layouts.searchClasses')" @keydown.enter.prevent />
          </div>
          <div class="inline-control">
            <label for="layout-class-uses">{{ t("customization.layouts.usesFilter") }}</label>
            <select id="layout-class-uses" :value="uses" @change="lq.update({ uses: ($event.target as HTMLSelectElement).value || undefined })">
              <option value="">{{ t("customization.layouts.anyTemplate") }}</option>
              <option v-for="tp in doc.layoutTemplates" :key="tp.key" :value="tp.key">{{ tp.name }}</option>
            </select>
          </div>
          <span class="muted" role="status">{{ t("customization.layouts.classCount", { n: rows.length, total: allClasses.length }) }}</span>
        </div>

        <EmptyState v-if="allClasses.length === 0" :title="t('customization.layouts.noClasses')" />
        <EmptyState v-else-if="rows.length === 0 && filtered" :title="t('customization.layouts.noMatchClasses')">
          <template #actions><button type="button" class="btn" @click="clearFilters">{{ t("customization.layouts.clearFilters") }}</button></template>
        </EmptyState>
        <template v-else>
          <div class="table-wrap">
            <table class="data compact" data-testid="layout-classes">
              <caption class="sr-only">{{ t("customization.layouts.classesCaption") }}</caption>
              <thead>
                <tr>
                  <th v-for="c in CLASS_COLUMNS" :key="c.key" scope="col" :class="{ num: c.key === 'owned' }" :aria-sort="c.sort ? lq.ariaSort(c.sort) : undefined">
                    <button v-if="c.sort" type="button" class="sort" @click="lq.toggleSort(c.sort)">{{ c.label() }} {{ lq.sortIndicator(c.sort) }}</button>
                    <template v-else>{{ c.label() }}</template>
                  </th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="r in page" :key="r.key" :class="{ selected: selected?.key === r.key }">
                  <th scope="row">
                    {{ r.name }} <span class="mono muted">{{ r.key }}</span>
                    <span v-if="!r.isActive" class="badge">{{ t("customization.layouts.archived") }}</span>
                  </th>
                  <td>
                    <label class="sr-only" :for="`layout-default-${r.key}`">{{ t("customization.layouts.defaultFor", { class: r.name }) }}</label>
                    <select :id="`layout-default-${r.key}`" :value="r.templateKey" @change="setDefault(r.key, ($event.target as HTMLSelectElement).value)">
                      <option v-for="tp in doc.layoutTemplates" :key="tp.key" :value="tp.key">{{ tp.name }}</option>
                    </select>
                  </td>
                  <td class="num" data-testid="own-layout-count">
                    <template v-if="r.ownLayoutCount === undefined">…</template>
                    <span v-else-if="r.ownLayoutCount === null" :title="t('customization.layouts.ownLayoutHidden')">?</span>
                    <template v-else-if="r.ownLayoutCount === 0">0</template>
                    <RouterLink v-else :to="ownLayoutLink(r.id)" :aria-label="t('customization.layouts.ownLayoutLink', { n: r.ownLayoutCount, class: r.name })">
                      {{ r.ownLayoutCount.toLocaleString() }}
                    </RouterLink>
                  </td>
                  <td>
                    <button type="button" class="btn btn-sm" :aria-label="t('customization.layouts.editOnCiFor', { class: r.name })" @click="openClass(r.key)">
                      {{ t("customization.layouts.editOnCi") }}
                    </button>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <PaginationBar :total="rows.length" :limit="lq.limit.value" :offset="lq.offset.value" @change="lq.onPage" />
        </template>
        <p class="hint">{{ t("customization.layouts.classesHint") }}</p>
      </template>
    </div>
  </section>

  <ClassCiEditor
    v-if="selected"
    :cls="selected"
    :template-key="saved.has(selectedTemplate) ? selectedTemplate : undefined"
    :template-name="templateName(selectedTemplate)"
    :ready="!usage.isLoading.value"
    @close="closeClass"
  />

  <section class="panel">
    <div class="panel-header">
      <h2>{{ t("customization.layouts.templatesTitle") }}</h2>
      <button type="button" class="btn btn-primary btn-sm" @click="newTemplate">{{ t("layoutTemplates.new") }}</button>
    </div>
    <div class="panel-body stack">
      <ErrorAlert v-if="usage.isError.value" :error="usage.error.value" :title="t('layoutTemplates.usageError')" :on-retry="() => usage.refetch()" />
      <ErrorAlert v-if="openError" :error="openError" :title="t('layoutTemplates.openError')" />
      <div class="table-wrap">
        <table class="data compact" data-testid="layout-templates">
          <caption class="sr-only">{{ t("customization.layouts.templatesCaption") }}</caption>
          <thead>
            <tr>
              <th scope="col">{{ t("layoutTemplates.name") }}</th>
              <th scope="col">{{ t("layoutTemplates.description") }}</th>
              <th scope="col" class="num">{{ t("layoutTemplates.usedByClasses") }}</th>
              <th scope="col" class="num">{{ t("layoutTemplates.usedByCis") }}</th>
              <th scope="col">{{ t("customization.layouts.colActions") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="tp in doc.layoutTemplates" :key="tp.key" :data-template="tp.key">
              <th scope="row">
                {{ tp.name }}
                <span v-if="tp.key === STANDARD_TEMPLATE" class="badge">{{ t("layoutTemplates.builtIn") }}</span>
                <span v-if="usage.data.value && !saved.has(tp.key)" class="badge warn">{{ t("layoutTemplates.unsaved") }}</span>
              </th>
              <td :title="tp.description">{{ tp.description ?? "" }}</td>
              <td class="num" :title="users(tp.key).classKeys.map(className).join(', ') || undefined">{{ users(tp.key).classKeys.length.toLocaleString() }}</td>
              <td class="num" :title="users(tp.key).ciCount === null ? t('layoutTemplates.cisHidden') : undefined">{{ ciCell(tp.key) }}</td>
              <td class="tpl-actions">
                <button
                  type="button"
                  class="btn btn-sm"
                  :disabled="!saved.has(tp.key) || opening !== null"
                  :title="saved.has(tp.key) ? t('layoutTemplates.editHint') : t('layoutTemplates.editUnsaved')"
                  :aria-label="t('layoutTemplates.editFor', { name: tp.name })"
                  @click="edit(tp.key)"
                >
                  {{ opening === tp.key ? t("layoutTemplates.opening") : t("common.edit") }}
                </button>
                <button type="button" class="btn btn-sm" :aria-label="t('layoutTemplates.renameFor', { name: tp.name })" @click="rename(tp)">{{ t("layoutTemplates.rename") }}</button>
                <button type="button" class="btn btn-sm" :aria-label="t('layoutTemplates.duplicateFor', { name: tp.name })" @click="duplicate(tp)">{{ t("layoutTemplates.duplicate") }}</button>
                <button
                  type="button"
                  class="btn btn-sm btn-danger"
                  :aria-disabled="!deletable(tp.key, users(tp.key))"
                  :aria-label="t('layoutTemplates.deleteFor', { name: tp.name })"
                  :aria-describedby="deletable(tp.key, users(tp.key)) ? undefined : `tpl-in-use-${tp.key}`"
                  :title="deletable(tp.key, users(tp.key)) ? undefined : inUse(tp.key)"
                  @click="deletable(tp.key, users(tp.key)) && (removing = tp)"
                >
                  {{ t("common.delete") }}
                </button>
                <span v-if="!deletable(tp.key, users(tp.key))" :id="`tpl-in-use-${tp.key}`" class="sr-only">{{ inUse(tp.key) }}</span>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <p class="hint">{{ t("customization.layouts.templatesHint") }}</p>
    </div>
  </section>

  <TemplateDialog
    :open="!!dialog"
    :mode="dialog?.mode ?? 'new'"
    :templates="doc.layoutTemplates"
    :initial="dialog?.initial ?? { name: '' }"
    @submit="onDialog"
    @cancel="dialog = null"
  />
  <ConfirmDialog
    :open="!!removing"
    :title="t('layoutTemplates.deleteTitle', { name: removing?.name ?? '' })"
    :confirm-label="t('layoutTemplates.deleteConfirm')"
    @confirm="remove"
    @cancel="removing = null"
  >
    {{ t("layoutTemplates.deleteBody", { name: removing?.name ?? "" }) }}
  </ConfirmDialog>
</template>

<style scoped>
.toolbar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--sp-2) var(--sp-4);
}
.toolbar input[type="search"] {
  max-width: 260px;
}
table.data select {
  max-width: 280px;
}
table.data tbody th {
  font-weight: 600;
  text-align: left;
}
tr.selected {
  background: var(--c-surface-alt);
}
.tpl-actions {
  white-space: nowrap;
}
.tpl-actions .btn + .btn {
  margin-left: 4px;
}
.tpl-actions [aria-disabled="true"] {
  opacity: 0.55;
  cursor: not-allowed;
}
</style>
