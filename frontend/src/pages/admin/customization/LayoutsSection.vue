<script setup lang="ts">
import { computed, ref } from "vue";
import { useClassAttributes, type CiClass } from "../../../api/queries";
import type { UiSettingsDocument } from "../../../api/uiSettings";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { suggestKey } from "../../../lib/keys";
import { moveItem } from "../../../lib/reorder";
import { ATTRIBUTE_PREFIX, BUILTIN_FIELDS, CORE_FIELDS, DETAIL_CORE, DETAIL_RECORD, fieldLabel, resolveLayout } from "../../../lib/uiSettings";
import ClassPicker from "./ClassPicker.vue";
import FieldListEditor from "./FieldListEditor.vue";

/**
 * Customization › Detail and form layout: per class, panels of fields in order
 * (for the detail page and the CI form alike), fields hidden from both, and
 * fields read-only on the form. The preview shows the resulting panels.
 */
const props = defineProps<{ doc: UiSettingsDocument }>();
const cls = ref<CiClass>();
const layout = computed(() => (cls.value ? props.doc.layouts.find((l) => l.classKey === cls.value!.key) : undefined));
const attrs = useClassAttributes(() => cls.value?.id);
const defs = computed(() => (attrs.data.value ?? []).filter((d) => d.isActive));
const fields = computed(() => [
  ...BUILTIN_FIELDS.map((f) => ({ key: f.key, label: f.label, required: false, form: CORE_FIELDS.includes(f.key) })),
  ...defs.value.map((d) => ({ key: `${ATTRIBUTE_PREFIX}${d.key}`, label: `${d.label} (attribute)`, required: d.isRequired, form: true })),
]);
/** Fields another panel already holds cannot be added to this one. */
const optionsFor = (panelIndex: number) => {
  const elsewhere = new Set((layout.value?.panels ?? []).flatMap((p, i) => (i === panelIndex ? [] : p.fields ?? [])));
  return fields.value.filter((f) => !elsewhere.has(f.key));
};

function customize() {
  if (!cls.value) return;
  props.doc.layouts.push({ classKey: cls.value.key, panels: [], hiddenFields: [], readOnlyFields: [] });
}
function removeLayout() {
  props.doc.layouts = props.doc.layouts.filter((l) => l !== layout.value);
}

const newPanel = ref("");
function addPanel() {
  const l = layout.value;
  const label = newPanel.value.trim();
  if (!l || !label) return;
  const taken = new Set((l.panels ?? []).map((p) => p.key));
  let key = suggestKey(label) || "panel";
  for (let n = 2; taken.has(key); n++) key = `${suggestKey(label) || "panel"}_${n}`;
  l.panels = [...(l.panels ?? []), { key, label, fields: [], collapsed: false }];
  newPanel.value = "";
}
const movePanel = (i: number, to: number) => layout.value && (layout.value.panels = moveItem(layout.value.panels ?? [], i, to));
const removePanel = (i: number) => layout.value && (layout.value.panels = (layout.value.panels ?? []).filter((_, j) => j !== i));

function toggle(list: "hiddenFields" | "readOnlyFields", key: string, on: boolean) {
  const l = layout.value;
  if (!l) return;
  const cur = l[list] ?? [];
  l[list] = on ? [...cur, key] : cur.filter((k) => k !== key);
}
const isHidden = (k: string) => !!layout.value?.hiddenFields?.includes(k);
const isReadOnly = (k: string) => !!layout.value?.readOnlyFields?.includes(k);

const previewPanels = computed(() => (layout.value ? resolveLayout(layout.value, defs.value, DETAIL_CORE, DETAIL_RECORD) : []));
</script>

<template>
  <section class="panel">
    <div class="panel-header"><h2>Detail and form layout</h2><span class="muted">Panels, hidden and read-only fields of one class</span></div>
    <div class="panel-body">
      <ClassPicker v-model:selected="cls" :customized="doc.layouts.map((l) => l.classKey)" noun="layout" />
    </div>
    <div v-if="cls" class="panel-body">
      <template v-if="!layout">
        <p>
          {{ cls.name }} uses the built-in layout: a General panel with the ident, the validity period and the attributes
          without a group, then the class's other attribute groups.
        </p>
        <button type="button" class="btn btn-primary" @click="customize">Customize the {{ cls.name }} layout</button>
      </template>
      <template v-else>
        <LoadingState v-if="attrs.isLoading.value" label="Loading attributes…" />
        <ErrorAlert v-if="attrs.isError.value" :error="attrs.error.value" title="Could not load the class's attributes" :on-retry="() => attrs.refetch()" />

        <h3 class="subhead">Panels</h3>
        <p class="hint">
          Panels come first, in this order, on the detail page and the form. Fields no panel places follow in a General
          panel and in their attribute groups.
        </p>
        <div v-for="(p, i) in layout.panels" :key="p.key" class="panel layout-editor-panel">
          <div class="panel-header">
            <div class="inline-control">
              <label class="sr-only" :for="`panel-label-${p.key}`">Panel heading</label>
              <input :id="`panel-label-${p.key}`" v-model="p.label" type="text" maxlength="100" />
              <label class="check"><input v-model="p.collapsed" type="checkbox" /> Collapsed</label>
            </div>
            <span class="row-actions">
              <button type="button" class="btn btn-sm" :disabled="i === 0" :aria-label="`Move panel ${p.label} up`" @click="movePanel(i, i - 1)">↑</button>
              <button type="button" class="btn btn-sm" :disabled="i === (layout.panels?.length ?? 0) - 1" :aria-label="`Move panel ${p.label} down`" @click="movePanel(i, i + 1)">↓</button>
              <button type="button" class="btn btn-sm" @click="removePanel(i)">Remove panel</button>
            </span>
          </div>
          <div class="panel-body">
            <FieldListEditor v-model="p.fields!" :options="optionsFor(i)" :label="`Fields of ${p.label}`" :id-prefix="`panel-${p.key}`" empty-text="No fields yet." />
          </div>
        </div>
        <form class="inline-control" style="margin: var(--sp-3) 0 var(--sp-5)" @submit.prevent="addPanel">
          <label for="layout-new-panel">New panel</label>
          <input id="layout-new-panel" v-model="newPanel" type="text" maxlength="100" placeholder="e.g. Hardware" />
          <button type="submit" class="btn" :disabled="!newPanel.trim()">Add panel</button>
        </form>

        <h3 class="subhead">Hidden and read-only fields</h3>
        <table class="data">
          <thead>
            <tr>
              <th scope="col">Field</th>
              <th scope="col">Hidden</th>
              <th scope="col">Read-only on the form</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="f in fields" :key="f.key">
              <td>
                {{ f.label }} <code class="muted">{{ f.key }}</code>
                <span v-if="f.required" class="badge">required</span>
                <span v-if="f.required && (isHidden(f.key) || isReadOnly(f.key))" class="hint">
                  · stays editable on new CIs, which cannot be saved without it
                </span>
              </td>
              <td>
                <label class="check">
                  <input type="checkbox" :checked="isHidden(f.key)" @change="toggle('hiddenFields', f.key, ($event.target as HTMLInputElement).checked)" />
                  <span class="sr-only">Hide {{ f.label }}</span>
                </label>
              </td>
              <td>
                <label v-if="f.form" class="check">
                  <input type="checkbox" :checked="isReadOnly(f.key)" @change="toggle('readOnlyFields', f.key, ($event.target as HTMLInputElement).checked)" />
                  <span class="sr-only">Make {{ f.label }} read-only</span>
                </label>
                <span v-else class="muted">not on the form</span>
              </td>
            </tr>
          </tbody>
        </table>
        <div style="margin-top: var(--sp-4)">
          <button type="button" class="btn" @click="removeLayout">Use the built-in layout for {{ cls.name }}</button>
        </div>
      </template>
    </div>
  </section>

  <section v-if="cls && layout" class="preview-frame" style="margin-top: var(--sp-4)" aria-label="Layout preview">
    <p class="preview-label">Preview: the {{ cls.name }} detail page</p>
    <div class="layout-panels">
      <details v-for="p in previewPanels" :key="p.key" class="panel layout-panel" :open="!p.collapsed">
        <summary class="panel-header"><h2>{{ p.label }}</h2></summary>
        <div class="panel-body">
          <dl class="props">
            <template v-for="f in p.fields" :key="f">
              <dt>{{ fieldLabel(f, defs) }}</dt>
              <dd class="muted">{{ isReadOnly(f) ? "read-only on the form" : "…" }}</dd>
            </template>
          </dl>
        </div>
      </details>
    </div>
  </section>
</template>

<style scoped>
.subhead {
  font-size: var(--fs-md);
  margin: var(--sp-4) 0 var(--sp-2);
}
.layout-editor-panel {
  margin-bottom: var(--sp-3);
}
</style>
