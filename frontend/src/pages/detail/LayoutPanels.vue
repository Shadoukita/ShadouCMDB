<script setup lang="ts">
import { computed } from "vue";
import type { Ci, EffectiveAttribute } from "../../api/queries";
import type { TrailStep } from "../../lib/trail";
import { attributeKey, cellClass, fieldLabel, freeAreaStyle, gridClass, sectionClass, sectionStyle, windowClass, windowStyle, type ResolvedSection } from "../../lib/uiSettings";
import type { CiDraft } from "../form/ciDraft";
import CiFieldInput from "../form/CiFieldInput.vue";
import AttributeValue from "./AttributeValue.vue";
import BlockContent from "./BlockContent.vue";
import CoreFieldValue from "./CoreFieldValue.vue";

/**
 * One tab of the detail page's fields (lib/uiSettings resolveLayout): its
 * sections as windows where the layout puts them (lib/freeLayout; stacked on
 * small screens), collapsed ones closed, each a grid of label/value cells as wide as
 * the class layout says. On the first tab the built-in sections follow: General,
 * the attribute groups and the record's class and timestamps. Notes and the
 * built-in panels a layout places are sections too (BlockContent). Values no current
 * definition describes (e.g. after a class change) are listed last on the first
 * tab (`orphans`), so nothing stored is hidden; values of archived fields
 * (`archived`) are listed there too, apart and labelled. The sections the layout
 * does not place follow below the windows at the full width.
 *
 * With a `draft` (ciDraft.ts) the fields are the CI's form (SHAA-1644): each field it may change is an
 * input bound to the draft; the others (the class, the timestamps, a read-only or managed field, or every
 * field of a CI the user may not edit) show their value in the same place, read-only.
 */
const props = defineProps<{
  ci: Ci;
  sections: ResolvedSection[];
  defs: EffectiveAttribute[];
  self: TrailStep;
  trail: TrailStep[];
  orphans?: boolean;
  /** Archived definitions this CI still holds a value for. */
  archived?: EffectiveAttribute[];
  /** The CI's values being edited; without it the fields are shown read-only as a list. */
  draft?: CiDraft;
}>();
const values = computed(() => props.ci.attributes as Record<string, unknown>);
const refs = computed(() => props.ci.attributeReferences);
const defFor = (field: string) => props.defs.find((d) => d.key === attributeKey(field));
const roId = (field: string) => `ro-${field.replace(/[^\w-]/g, "-")}`;
const roHint = (field: string) => props.draft?.readOnlyHint(field);
/** The windows, then everything the layout does not place. */
const groups = computed(() => {
  const windows = props.sections.filter((p) => p.frame);
  const flow = props.sections.filter((p) => !p.frame);
  return windows.length > 0 ? [{ free: true, items: windows }, { free: false, items: flow }] : [{ free: false, items: flow }];
});
const archivedDefs = computed(() => (props.orphans ? (props.archived ?? []) : []));
const orphanKeys = computed(() => {
  if (!props.orphans) return [];
  const known = new Set([...props.defs, ...(props.archived ?? [])].map((d) => d.key));
  return Object.keys(values.value).filter((k) => !known.has(k) && values.value[k] != null);
});
</script>

<template>
  <div class="layout-container">
    <div v-for="g in groups" :key="String(g.free)" :class="g.free ? 'lg-free' : 'layout-panels'" :style="g.free ? freeAreaStyle(g.items) : undefined">
      <details
        v-for="p in g.items"
        :key="p.key"
        :class="['panel', 'layout-panel', ...(p.frame ? [windowClass] : sectionClass(p))]"
        :style="p.frame ? windowStyle(p.frame) : sectionStyle(p)"
        :data-section="p.key"
        :open="!p.collapsed"
      >
        <summary class="panel-header"><h2>{{ p.label }}</h2></summary>
        <BlockContent v-if="p.kind !== 'fields'" :kind="p.kind" :text="p.text" :ci="ci" :self="self" :trail="trail" />
        <div v-else-if="draft" class="panel-body">
          <div :class="gridClass(p.columns)">
            <template v-for="{ field: f, width } in p.fields" :key="f">
              <CiFieldInput v-if="draft.editable(f)" :draft="draft" :f="f" :width="width" :columns="p.columns" :ci="ci" :self="self" :trail="trail" />
              <div v-else :class="['field', 'field-ro', cellClass(width, p.columns)]" :data-field="f">
                <span :id="roId(f)" class="label">{{ fieldLabel(f, defs) }}</span>
                <div class="ro-value" role="group" :aria-labelledby="roId(f)">
                  <AttributeValue v-if="defFor(f)" :def="defFor(f)!" :value="values[defFor(f)!.key]" :ref-info="refs[defFor(f)!.key]" :self="self" :trail="trail" />
                  <CoreFieldValue v-else :ci="ci" :field="f" />
                </div>
                <span v-if="roHint(f)" class="hint" dir="auto">{{ roHint(f) }}</span>
              </div>
            </template>
            <div v-if="p.key === '_record'" :class="['field', 'field-ro', cellClass(1, p.columns)]" data-field="id">
              <span id="ro-id" class="label">ID</span>
              <div class="ro-value mono" role="group" aria-labelledby="ro-id">{{ ci.id }}</div>
            </div>
          </div>
        </div>
        <div v-else class="panel-body">
          <dl :class="gridClass(p.columns)">
            <div v-for="{ field: f, width } in p.fields" :key="f" :class="['prop', cellClass(width, p.columns)]">
              <dt>{{ fieldLabel(f, defs) }}</dt>
              <dd>
                <AttributeValue v-if="defFor(f)" :def="defFor(f)!" :value="values[defFor(f)!.key]" :ref-info="refs[defFor(f)!.key]" :self="self" :trail="trail" />
                <CoreFieldValue v-else :ci="ci" :field="f" />
              </dd>
            </div>
            <div v-if="p.key === '_record'" :class="['prop', cellClass(1, p.columns)]">
              <dt>ID</dt>
              <dd class="mono">{{ ci.id }}</dd>
            </div>
          </dl>
        </div>
      </details>
      <section v-if="!g.free && archivedDefs.length > 0" class="panel" data-section="_archived">
        <div class="panel-header"><h2>Archived fields</h2></div>
        <div class="panel-body">
          <p class="muted">These fields are no longer in use. Their stored values are kept for reference and cannot be edited.</p>
          <dl class="props">
            <template v-for="d in archivedDefs" :key="d.key">
              <dt>{{ d.label }} <span class="muted">(archived)</span></dt>
              <dd><AttributeValue :def="d" :value="values[d.key]" :ref-info="refs[d.key]" :self="self" :trail="trail" /></dd>
            </template>
          </dl>
        </div>
      </section>
      <section v-if="!g.free && orphanKeys.length > 0" class="panel">
        <div class="panel-header"><h2>Not defined by this class</h2></div>
        <div class="panel-body">
          <dl class="props">
            <template v-for="k in orphanKeys" :key="k">
              <dt>{{ k }}</dt>
              <dd><span class="mono">{{ JSON.stringify(values[k]) }}</span></dd>
            </template>
          </dl>
        </div>
      </section>
    </div>
  </div>
</template>
